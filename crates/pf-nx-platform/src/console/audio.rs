//! `AudioOut` over audout: 48 kHz stereo S16, one 5 ms Opus frame per buffer, four buffers
//! (plan D8). A write blocks while every buffer is queued, which paces the audio thread.

use anyhow::{ensure, Result};
use pf_nx_app::platform::{AudioOut, AudioPort};
use pf_nx_sys::audout;

/// One 5 ms Opus frame at 48 kHz.
const GRAIN_FRAMES: usize = 240;
const BUFFERS: usize = 4;

pub struct NxAudio;

impl AudioOut for NxAudio {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>> {
        ensure!(
            channels == 2 && rate == 48_000,
            "audout plays 48 kHz stereo, not {channels} channels at {rate} Hz"
        );
        let out = audout::AudioOut::open(BUFFERS, GRAIN_FRAMES)?;
        tracing::info!("audio: audout 48 kHz stereo S16, {BUFFERS} buffers of {GRAIN_FRAMES} frames");
        Ok(Box::new(Port {
            out,
            pcm: vec![0; GRAIN_FRAMES * 2],
        }))
    }
}

struct Port {
    out: audout::AudioOut,
    pcm: Vec<i16>,
}

impl AudioPort for Port {
    fn grain_frames(&self) -> usize {
        GRAIN_FRAMES
    }

    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        ensure!(
            pcm.len() == self.pcm.len(),
            "grain of {} samples, the port takes {}",
            pcm.len(),
            self.pcm.len()
        );
        for (dst, src) in self.pcm.iter_mut().zip(pcm) {
            *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
        }
        self.out.push(&self.pcm)
    }
}
