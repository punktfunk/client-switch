//! `PyroWave` beside NVDEC (plan D14, §10): the shared wavelet decoder on the title's NVK device.
//! The host picks `PyroWave` only when the player asks for it, so Automatic stays on NVDEC; the
//! codec row offers it only where the device probe found the compute features it needs.

use anyhow::Result;
use pf_nx_app::core::caps::VideoCaps;
use pf_nx_app::core::media::VideoSink;
use pf_nx_app::platform::{StreamSpec, VideoDecoders};
use pf_nx_vk::PyroLane;
use punktfunk_core::quic;

use crate::video::NxVideo;

pub struct WithPyro {
    nvdec: NxVideo,
    pyro: PyroLane,
}

impl WithPyro {
    pub fn new(nvdec: NxVideo, pyro: PyroLane) -> Self {
        tracing::info!("pyrowave: lane built, device capable {}", pyro.available());
        Self { nvdec, pyro }
    }
}

impl VideoDecoders for WithPyro {
    fn caps(&self) -> VideoCaps {
        VideoCaps {
            pyrowave: self.pyro.available(),
            ..self.nvdec.caps()
        }
    }

    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        if spec.codec == quic::CODEC_PYROWAVE {
            self.pyro.open(spec)
        } else {
            self.nvdec.open(spec)
        }
    }

    fn lane_name(&self, codec: u8) -> &'static str {
        if codec == quic::CODEC_PYROWAVE {
            "PyroWave (GPU)"
        } else {
            self.nvdec.lane_name(codec)
        }
    }
}
