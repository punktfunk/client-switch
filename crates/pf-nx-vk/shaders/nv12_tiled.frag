// NV12 where NVDEC wrote it → RGBA. Both planes are uniform texel buffers over the decoder's
// surface, 16Bx2 block-linear with two GOBs to a block: 64 bytes by 16 rows, 1 KiB, blocks in
// row order, `pitch` bytes a row. A byte's offset is in_place.rs's `block_linear`, a row part
// plus a column part, so a 2x2 footprint works out two of each instead of four sums. The colour
// rows and the chroma siting are nv12_csc.frag's; the lane sends only SDR here. A texel buffer
// has no sampler, so the bilinear filter is done by hand. A picture the size of the output
// (`surface.w` set) has a luma texel centred on each pixel: luma is read once, unfiltered.
//
// Regenerate: glslc -O nv12_tiled.frag -o nv12_tiled.frag.spv (the .spv is committed).
#version 450

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 frag;

layout(set = 0, binding = 0) uniform samplerBuffer u_y; // R8_UNORM: the luma plane
layout(set = 0, binding = 1) uniform samplerBuffer u_c; // R8G8_UNORM: the CbCr plane

layout(push_constant) uniform Csc {
    vec4 r0;
    vec4 r1;
    vec4 r2;
    vec4 params;   // nv12_csc.frag's; unused, the stream is SDR
    uvec4 surface; // x: pitch in bytes, y: width, z: height in luma texels, w: 1 at one to one
} pc;

uint row_part(uint y) {
    return (y >> 4) * (pc.surface.x << 4) + ((y >> 3) & 1u) * 512u + ((y >> 1) & 3u) * 64u
        + (y & 1u) * 16u;
}

uint col_part(uint x) {
    return (x >> 6) * 1024u + ((x >> 5) & 1u) * 256u + ((x >> 4) & 1u) * 32u + (x & 15u);
}

// The texels either side of `pos` (texel space, centres on .5) and the weight of the second;
// edges clamp, as the sampled path's CLAMP_TO_EDGE.
void footprint(vec2 pos, ivec2 size, out uvec2 a, out uvec2 b, out vec2 f) {
    vec2 p = pos - 0.5;
    f = fract(p);
    a = uvec2(clamp(ivec2(floor(p)), ivec2(0), size - 1));
    b = uvec2(clamp(ivec2(floor(p)) + 1, ivec2(0), size - 1));
}

float bilinear_y(vec2 pos, ivec2 size) {
    uvec2 a, b;
    vec2 f;
    footprint(pos, size, a, b, f);
    uint ra = row_part(a.y), rb = row_part(b.y), ca = col_part(a.x), cb = col_part(b.x);
    float aa = texelFetch(u_y, int(ra + ca)).r, ba = texelFetch(u_y, int(ra + cb)).r;
    float ab = texelFetch(u_y, int(rb + ca)).r, bb = texelFetch(u_y, int(rb + cb)).r;
    return mix(mix(aa, ba, f.x), mix(ab, bb, f.x), f.y);
}

// Chroma texel x is the byte pair at luma column 2x: both parts even, so the sum halves exactly.
vec2 bilinear_c(vec2 pos, ivec2 size) {
    uvec2 a, b;
    vec2 f;
    footprint(pos, size, a, b, f);
    uint ra = row_part(a.y), rb = row_part(b.y), ca = col_part(a.x * 2u), cb = col_part(b.x * 2u);
    vec2 aa = texelFetch(u_c, int((ra + ca) >> 1)).rg, ba = texelFetch(u_c, int((ra + cb) >> 1)).rg;
    vec2 ab = texelFetch(u_c, int((rb + ca) >> 1)).rg, bb = texelFetch(u_c, int((rb + cb) >> 1)).rg;
    return mix(mix(aa, ba, f.x), mix(ab, bb, f.x), f.y);
}

void main() {
    ivec2 size = ivec2(pc.surface.yz);
    ivec2 csize = (size + 1) / 2;
    vec2 pos = v_uv * vec2(size);
    float y;
    if (pc.surface.w != 0u) {
        uvec2 t = uvec2(clamp(ivec2(pos), ivec2(0), size - 1));
        y = texelFetch(u_y, int(row_part(t.y) + col_part(t.x))).r;
    } else {
        y = bilinear_y(pos, size);
    }
    // Left-cosited 4:2:0 chroma: a quarter chroma texel right of the luma position.
    vec2 cpos = v_uv * vec2(csize) + vec2(0.25, 0.0);
    vec3 yuv = vec3(y, bilinear_c(cpos, csize));
    vec3 rgb = vec3(
        dot(pc.r0.xyz, yuv) + pc.r0.w,
        dot(pc.r1.xyz, yuv) + pc.r1.w,
        dot(pc.r2.xyz, yuv) + pc.r2.w
    );
    frag = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
