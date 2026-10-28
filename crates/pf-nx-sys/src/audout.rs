//! Audio out: 48 kHz stereo S16 into a ring of page-aligned buffers. Externs from
//! `services/audout.h` and `audio/audio.h`, libnx 4.12.0.
//!
//! libnx's audout is one global device: one [`AudioOut`] at a time.

#[cfg(target_os = "horizon")]
use core::ffi::c_void;

/// `AudioOutBuffer`. audout keys a buffer by this struct's address and reads `buffer` until it
/// releases it, so neither moves while queued.
#[repr(C)]
#[derive(Debug)]
pub struct AudioOutBuffer {
    pub next: *mut Self,
    /// Sample memory, 0x1000-aligned.
    pub buffer: *mut core::ffi::c_void,
    /// A multiple of 0x1000.
    pub buffer_size: u64,
    pub data_size: u64,
    pub data_offset: u64,
}

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// `PcmFormat_Int16`.
pub const PCM_FORMAT_INT16: u32 = 2;
/// audout's alignment for sample memory and its size.
pub const BUFFER_ALIGN: usize = 0x1000;

/// Bytes a buffer of `frames` stereo S16 frames takes, rounded up to [`BUFFER_ALIGN`].
pub fn buffer_bytes(frames: usize) -> usize {
    (frames * CHANNELS as usize * 2).next_multiple_of(BUFFER_ALIGN)
}

#[cfg(target_os = "horizon")]
mod ffi {
    use super::AudioOutBuffer;

    extern "C" {
        pub fn audoutInitialize() -> u32;
        pub fn audoutExit();
        pub fn audoutStartAudioOut() -> u32;
        pub fn audoutStopAudioOut() -> u32;
        pub fn audoutAppendAudioOutBuffer(buffer: *mut AudioOutBuffer) -> u32;
        pub fn audoutWaitPlayFinish(released: *mut *mut AudioOutBuffer, released_count: *mut u32, timeout: u64) -> u32;
        pub fn audoutGetSampleRate() -> u32;
        pub fn audoutGetChannelCount() -> u32;
        pub fn audoutGetPcmFormat() -> u32;
    }
}

/// How long [`AudioOut::push`] waits for a buffer before it gives up: one second.
#[cfg(target_os = "horizon")]
const WAIT_NS: u64 = 1_000_000_000;

/// The open audio out device and its buffers.
#[cfg(target_os = "horizon")]
pub struct AudioOut {
    bufs: Box<[AudioOutBuffer]>,
    mem: *mut u8,
    layout: std::alloc::Layout,
    frames_per_buffer: usize,
    free: Vec<usize>,
    queued_frames: usize,
}

// SAFETY: the raw pointers point into memory this value owns; audout's state is process-global
// and taken by whichever thread holds the value.
#[cfg(target_os = "horizon")]
unsafe impl Send for AudioOut {}

#[cfg(target_os = "horizon")]
impl AudioOut {
    /// Opens the default device and starts it with `buffers` empty buffers of
    /// `frames_per_buffer` stereo frames each.
    pub fn open(buffers: usize, frames_per_buffer: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            buffers > 0 && frames_per_buffer > 0,
            "open audout with {buffers} buffers of {frames_per_buffer} frames"
        );
        // SAFETY: no arguments; balanced by audoutExit below or in Drop.
        let rc = unsafe { ffi::audoutInitialize() };
        if rc != 0 {
            return Err(crate::nx_error("initialize audout", rc));
        }
        // SAFETY: audout is initialised; these read the format it opened with.
        let (rate, ch, fmt) = unsafe {
            (
                ffi::audoutGetSampleRate(),
                ffi::audoutGetChannelCount(),
                ffi::audoutGetPcmFormat(),
            )
        };
        if (rate, ch, fmt) != (SAMPLE_RATE, CHANNELS, PCM_FORMAT_INT16) {
            // SAFETY: balances the audoutInitialize above.
            unsafe { ffi::audoutExit() };
            anyhow::bail!(
                "open audout: device runs {rate} Hz, {ch} channels, PCM format {fmt}, not 48000 Hz stereo S16"
            );
        }
        let stride = buffer_bytes(frames_per_buffer);
        let layout = std::alloc::Layout::from_size_align(stride * buffers, BUFFER_ALIGN)?;
        // SAFETY: a non-zero size.
        let mem = unsafe { std::alloc::alloc_zeroed(layout) };
        if mem.is_null() {
            // SAFETY: balances the audoutInitialize above.
            unsafe { ffi::audoutExit() };
            anyhow::bail!("allocate {} bytes of audout buffers", layout.size());
        }
        let bufs = (0..buffers)
            .map(|i| AudioOutBuffer {
                next: core::ptr::null_mut(),
                // SAFETY: `i * stride` lies inside the allocation of `buffers * stride` bytes.
                buffer: unsafe { mem.add(i * stride) }.cast::<c_void>(),
                buffer_size: stride as u64,
                data_size: 0,
                data_offset: 0,
            })
            .collect();
        let out = Self {
            bufs,
            mem,
            layout,
            frames_per_buffer,
            free: (0..buffers).rev().collect(),
            queued_frames: 0,
        };
        // SAFETY: audout is initialised.
        let rc = unsafe { ffi::audoutStartAudioOut() };
        if rc != 0 {
            return Err(crate::nx_error("start audout", rc)); // Drop exits audout and frees.
        }
        Ok(out)
    }

    /// Queues interleaved stereo S16, blocking while every buffer is queued.
    pub fn push(&mut self, pcm: &[i16]) -> anyhow::Result<()> {
        anyhow::ensure!(
            pcm.len() % 2 == 0,
            "queue {} samples: not whole stereo frames",
            pcm.len()
        );
        for chunk in pcm.chunks(self.frames_per_buffer * 2) {
            let i = self.take_free()?;
            let b = &mut self.bufs[i];
            // SAFETY: `buffer` holds `buffer_bytes(frames_per_buffer)` ≥ `chunk.len() * 2` bytes,
            // and audout released it, so nothing else reads it.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), b.buffer.cast::<i16>(), chunk.len()) };
            b.data_size = (chunk.len() * 2) as u64;
            b.data_offset = 0;
            // SAFETY: `b` and its memory stay put and untouched until audout releases them.
            let rc = unsafe { ffi::audoutAppendAudioOutBuffer(b) };
            if rc != 0 {
                self.free.push(i);
                return Err(crate::nx_error("append an audout buffer", rc));
            }
            self.queued_frames += chunk.len() / 2;
        }
        Ok(())
    }

    /// Frames appended and not yet reclaimed. A buffer audout has played counts until the next
    /// [`AudioOut::push`] reclaims it.
    pub fn queued_frames(&self) -> usize {
        self.queued_frames
    }

    fn take_free(&mut self) -> anyhow::Result<usize> {
        loop {
            if let Some(i) = self.free.pop() {
                return Ok(i);
            }
            let mut released: *mut AudioOutBuffer = core::ptr::null_mut();
            let mut count = 0u32;
            // SAFETY: both out-pointers are valid locals.
            let rc = unsafe { ffi::audoutWaitPlayFinish(&mut released, &mut count, WAIT_NS) };
            if rc != 0 {
                return Err(crate::nx_error("wait for a free audout buffer", rc));
            }
            if count == 0 {
                continue;
            }
            if let Some(i) = self.bufs.iter().position(|b| core::ptr::eq(b, released)) {
                self.queued_frames = self.queued_frames.saturating_sub(self.bufs[i].data_size as usize / 4);
                self.free.push(i);
            }
        }
    }
}

#[cfg(target_os = "horizon")]
impl Drop for AudioOut {
    fn drop(&mut self) {
        // SAFETY: audout was initialised by `open`; stopping and exiting releases every buffer
        // before the memory behind them is freed.
        unsafe {
            ffi::audoutStopAudioOut();
            ffi::audoutExit();
            std::alloc::dealloc(self.mem, self.layout);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn buffers_round_up_to_a_page() {
        // 5 ms at 48 kHz: 240 frames × 4 bytes = 960 → one page.
        assert_eq!(super::buffer_bytes(240), 0x1000);
        assert_eq!(super::buffer_bytes(1024), 0x1000);
        assert_eq!(super::buffer_bytes(1025), 0x2000);
    }
}
