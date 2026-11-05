// NVDEC through libavcodec's nvtegra hwaccel, as six calls the Rust side binds (src/av.rs).
// One thread uses a decoder at a time; pictures leave as pitch-linear NV12 via the VIC,
// through libavutil's own frame and a copy, or, when the caller opens with `direct`, straight
// into the caller's planes where the hwcontext can map them.
// Built for a Linux dev box (no __SWITCH__), the same calls decode in software against the
// system's libavcodec and interleave its 4:2:0 chroma into the same NV12 planes.
#include <errno.h>
#include <stdint.h>
#include <string.h>

#ifdef __SWITCH__
// nvtegra.h names libnx's NvChannel and MmuRequest without including them.
#include <switch.h>
#endif

#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/time.h>
#ifdef __SWITCH__
#include <libavutil/hwcontext_nvtegra.h>
#endif

// The shim's own errors, outside FFmpeg's AVERROR space; src/av.rs names them.
#define PF_AV_NOT_NV12 (-0x70660001)
#define PF_AV_TOO_SMALL (-0x70660002)

// How the last picture reached the caller's planes (src/av.rs `Route`).
enum {
    PF_AV_ROUTE_SOFTWARE = 0,
    // The hwcontext's transfer into libavutil's frame, then the shim's copy.
    PF_AV_ROUTE_VIC_THEN_COPY = 1,
    PF_AV_ROUTE_CPU_THEN_COPY = 2,
    // The hwcontext's transfer straight into the caller's planes.
    PF_AV_ROUTE_VIC = 3,
    PF_AV_ROUTE_CPU = 4,
};

typedef struct PfAvStats {
    uint64_t decoded;
    uint32_t in_flight, last_decode_us, last_transfer_us, last_copy_us;
    uint32_t route;
    // Why transfers stopped going straight into the caller's planes; 0 while they do.
    int32_t direct_err;
    // NVDEC's clock after the last picture, kHz; 0 off the console.
    uint32_t nvdec_khz;
} PfAvStats;

typedef struct PfAvPictureMeta {
    uint32_t width, height;
    uint64_t pts;
} PfAvPictureMeta;

typedef struct PfAvDecoder {
    AVCodecContext *ctx;
    AVBufferRef *device;
    AVPacket *pkt;
    AVFrame *hw;
    AVFrame *sw;
    // The caller's planes as the transfer's destination; NULL off the console.
    AVFrame *direct;
    int64_t sent_at;
    uint64_t taken;
    PfAvStats stats;
    // Hold NVDEC at its top clock; the clock last asked for, Hz.
    int pin_clock;
    uint32_t pinned_hz;
} PfAvDecoder;

void pf_av_close(PfAvDecoder *d) {
    if (!d) return;
    avcodec_free_context(&d->ctx);
    av_buffer_unref(&d->device);
    av_packet_free(&d->pkt);
    av_frame_free(&d->hw);
    av_frame_free(&d->sw);
    av_frame_free(&d->direct);
    av_free(d);
}

// codec: 0 H.264, 1 HEVC. The hwaccel is found by name, so no header needs its enum.
// `direct` non-zero tries the transfer straight into the caller's planes first; zero always
// goes through libavutil's frame and a copy.
// TODO(verify): the device type's name is "nvtegra" in the pinned tree (WP0.6).
int pf_av_open(int codec, int width, int height, int direct, PfAvDecoder **out) {
    *out = NULL;
    const AVCodec *c = avcodec_find_decoder(codec == 1 ? AV_CODEC_ID_HEVC : AV_CODEC_ID_H264);
    if (!c) return AVERROR_DECODER_NOT_FOUND;
#ifdef __SWITCH__
    enum AVHWDeviceType type = av_hwdevice_find_type_by_name("nvtegra");
    if (type == AV_HWDEVICE_TYPE_NONE) return AVERROR(ENOSYS);
#endif
    PfAvDecoder *d = av_mallocz(sizeof(*d));
    if (!d) return AVERROR(ENOMEM);
    int err = 0;
#ifdef __SWITCH__
    err = av_hwdevice_ctx_create(&d->device, type, NULL, NULL, 0);
    if (err < 0) goto fail;
#endif
    d->ctx = avcodec_alloc_context3(c);
    d->pkt = av_packet_alloc();
    d->hw = av_frame_alloc();
    d->sw = av_frame_alloc();
    err = AVERROR(ENOMEM);
    if (!d->ctx || !d->pkt || !d->hw || !d->sw)
        goto fail;
#ifdef __SWITCH__
    if (!(d->ctx->hw_device_ctx = av_buffer_ref(d->device)) || (direct && !(d->direct = av_frame_alloc())))
        goto fail;
#else
    (void)direct;
#endif
    // The default get_format picks the device's hw format when hw_device_ctx is set.
    d->ctx->width = width;
    d->ctx->height = height;
    d->ctx->flags |= AV_CODEC_FLAG_LOW_DELAY;
    d->ctx->thread_count = 1;
    err = avcodec_open2(d->ctx, c, NULL);
    if (err < 0) goto fail;
    *out = d;
    return 0;
fail:
    pf_av_close(d);
    return err;
}

// 0 taken, 1 the decoder is full (receive first), < 0 an AVERROR. The packet is copied.
int pf_av_send(PfAvDecoder *d, const uint8_t *data, int len, uint64_t pts) {
    d->pkt->data = (uint8_t *)data;
    d->pkt->size = len;
    d->pkt->pts = (int64_t)pts;
    int64_t t = av_gettime_relative();
    int err = avcodec_send_packet(d->ctx, d->pkt);
    d->pkt->data = NULL;
    d->pkt->size = 0;
    if (err == AVERROR(EAGAIN)) return 1;
    if (err < 0) return err;
    d->sent_at = t;
    d->taken++;
    return 0;
}

// Non-zero holds NVDEC at its top clock from the next picture; zero hands it back to nvtegra's
// scaling, which takes over at its next rescale.
void pf_av_set_top_clock(PfAvDecoder *d, int on) {
    on = on != 0;
    if (d->pin_clock != on) {
        d->pin_clock = on;
        d->pinned_hz = 0;
    }
}

// nvtegra rescales NVDEC every 20 pictures so one takes about a frame interval: playback's
// rule, which puts a 1080p picture at about 8 ms. Asking the top clock again after each
// rescale holds it there; closing the decoder releases the request.
static void hold_clock(PfAvDecoder *d) {
#ifdef __SWITCH__
    AVHWDeviceContext *dev = (AVHWDeviceContext *)d->device->data;
    AVNVTegraChannel *ch = &((AVNVTegraDeviceContext *)dev->hwctx)->nvdec_channel;
    if (d->pin_clock && ch->clock != d->pinned_hz) {
        // nvtegra's own top request; mm rounds it down to the fastest step.
        if (R_SUCCEEDED(mmuRequestSetAndWait(&ch->mmu_request, UINT32_MAX - 999, -1)))
            mmuRequestGet(&ch->mmu_request, &ch->clock);
        d->pinned_hz = ch->clock;
    }
    d->stats.nvdec_khz = ch->clock / 1000;
#else
    (void)d;
#endif
}

// The caller's memory as a buffer the frame refers to; the caller owns it.
static void keep(void *opaque, uint8_t *data) {
    (void)opaque;
    (void)data;
}

// The hwcontext maps the destination's buffers into the VIC per transfer and writes the
// picture there: the copy out of libavutil's frame goes away. It needs 256-byte aligned
// addresses and pitches, else it detiles on the CPU, still into these planes. `rows` is the
// picture's height rounded up to even, which is what either path writes.
static int transfer_into(PfAvDecoder *d, uint8_t *y, size_t y_stride, size_t y_len, uint8_t *uv,
                         size_t uv_stride, size_t uv_len) {
    AVFrame *f = d->direct;
    f->format = AV_PIX_FMT_NV12;
    f->width = d->hw->width;
    f->height = d->hw->height;
    f->data[0] = y;
    f->data[1] = uv;
    f->linesize[0] = (int)y_stride;
    f->linesize[1] = (int)uv_stride;
    // One mapping when the planes are one block, as the staging buffer lays them out.
    int err = AVERROR(ENOMEM);
    if (uv == y + y_len) {
        f->buf[0] = av_buffer_create(y, y_len + uv_len, keep, NULL, 0);
        if (!f->buf[0]) goto out;
    } else {
        f->buf[0] = av_buffer_create(y, y_len, keep, NULL, 0);
        f->buf[1] = av_buffer_create(uv, uv_len, keep, NULL, 0);
        if (!f->buf[0] || !f->buf[1]) goto out;
    }
    err = av_hwframe_transfer_data(f, d->hw, 0);
out:
    av_frame_unref(f);
    return err;
}

static int aligned_256(const void *p, size_t stride) {
    return (((uintptr_t)p | stride) & 0xff) == 0;
}

static void copy_plane(uint8_t *dst, size_t dst_stride, const uint8_t *src, int src_stride,
                       size_t row, size_t rows) {
    for (size_t i = 0; i < rows; i++)
        memcpy(dst + i * dst_stride, src + i * (size_t)src_stride, row);
}

// A software decoder's planar 4:2:0 chroma, `pairs` U/V samples a row, as NV12's one plane.
static void interleave_uv(uint8_t *dst, size_t dst_stride, const AVFrame *src, size_t pairs,
                          size_t rows) {
    for (size_t i = 0; i < rows; i++) {
        const uint8_t *u = src->data[1] + i * (size_t)src->linesize[1];
        const uint8_t *v = src->data[2] + i * (size_t)src->linesize[2];
        uint8_t *out = dst + i * dst_stride;
        for (size_t j = 0; j < pairs; j++) {
            out[2 * j] = u[j];
            out[2 * j + 1] = v[j];
        }
    }
}

// 1 a picture written, 0 none ready, < 0 an AVERROR; the picture is dropped on error. `meta`
// is filled whenever a picture came out, so a too-small buffer reports the size it needs.
int pf_av_receive(PfAvDecoder *d, uint8_t *y, size_t y_stride, size_t y_len, uint8_t *uv,
                  size_t uv_stride, size_t uv_len, PfAvPictureMeta *meta) {
    int err = avcodec_receive_frame(d->ctx, d->hw);
    if (err == AVERROR(EAGAIN)) return 0;
    if (err < 0) return err;
    d->stats.decoded++;
    d->stats.last_decode_us = (uint32_t)(av_gettime_relative() - d->sent_at);
    hold_clock(d);
    meta->width = (uint32_t)d->hw->width;
    meta->height = (uint32_t)d->hw->height;
    meta->pts = (uint64_t)d->hw->pts;
    AVFrame *src = d->hw;
    d->stats.last_copy_us = 0;
    d->stats.route = PF_AV_ROUTE_SOFTWARE;
    if (d->hw->hw_frames_ctx) {
        size_t rows = ((size_t)d->hw->height + 1) & ~(size_t)1;
        size_t w = ((size_t)d->hw->width + 1) & ~(size_t)1;
        if (d->direct && d->stats.direct_err == 0 && y_stride >= w && uv_stride >= w &&
            y_len >= y_stride * rows && uv_len >= uv_stride * rows / 2) {
            int64_t t = av_gettime_relative();
            err = transfer_into(d, y, y_stride, y_len, uv, uv_stride, uv_len);
            d->stats.last_transfer_us = (uint32_t)(av_gettime_relative() - t);
            if (err >= 0) {
                d->stats.route = aligned_256(y, y_stride) && aligned_256(uv, uv_stride)
                                     ? PF_AV_ROUTE_VIC
                                     : PF_AV_ROUTE_CPU;
                err = 1;
                goto out;
            }
            // Never again for this decoder: the route through libavutil's frame is proven.
            d->stats.direct_err = err < 0 ? err : -1;
        }
        int64_t t = av_gettime_relative();
        d->sw->format = AV_PIX_FMT_NV12; // TODO(verify): the VIC transfer offers NV12 (WP0.6).
        err = av_hwframe_transfer_data(d->sw, d->hw, 0);
        d->stats.last_transfer_us = (uint32_t)(av_gettime_relative() - t);
        if (err < 0) goto out;
        src = d->sw;
        d->stats.route = aligned_256(src->data[0], (size_t)src->linesize[0]) &&
                                 aligned_256(src->data[1], (size_t)src->linesize[1])
                             ? PF_AV_ROUTE_VIC_THEN_COPY
                             : PF_AV_ROUTE_CPU_THEN_COPY;
    }
    // On the console a software frame means the hwaccel did not take the stream.
#ifdef __SWITCH__
    int planar = 0;
#else
    int planar = src->format == AV_PIX_FMT_YUV420P || src->format == AV_PIX_FMT_YUVJ420P;
#endif
    err = PF_AV_NOT_NV12;
    if (src->format != AV_PIX_FMT_NV12 && !planar)
        goto out;
    size_t w = (size_t)src->width, h = (size_t)src->height;
    size_t uw = (w + 1) & ~(size_t)1, uh = (h + 1) / 2;
    err = PF_AV_TOO_SMALL;
    if (y_stride < w || uv_stride < uw || y_len < y_stride * h || uv_len < uv_stride * uh)
        goto out;
    int64_t t = av_gettime_relative();
    copy_plane(y, y_stride, src->data[0], src->linesize[0], w, h);
    if (planar) {
        interleave_uv(uv, uv_stride, src, uw / 2, uh);
        d->stats.last_transfer_us = (uint32_t)(av_gettime_relative() - t);
    } else {
        copy_plane(uv, uv_stride, src->data[1], src->linesize[1], uw, uh);
        d->stats.last_copy_us = (uint32_t)(av_gettime_relative() - t);
    }
    err = 1;
out:
    av_frame_unref(d->hw);
    av_frame_unref(d->sw);
    return err;
}

void pf_av_flush(PfAvDecoder *d) {
    avcodec_flush_buffers(d->ctx);
    d->taken = d->stats.decoded;
}

void pf_av_stats(const PfAvDecoder *d, PfAvStats *out) {
    *out = d->stats;
    uint64_t n = d->taken > d->stats.decoded ? d->taken - d->stats.decoded : 0;
    out->in_flight = n > UINT32_MAX ? UINT32_MAX : (uint32_t)n;
}
