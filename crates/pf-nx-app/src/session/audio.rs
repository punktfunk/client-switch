//! The audio stage: transport packets → decoded PCM in the session's [`AudioSink`].
//!
//! libopus decodes here, with concealment written once into a reused buffer. **Nothing is
//! mixed down**: a layout the output cannot play is never requested (`core::caps`), so a width
//! mismatch is a bug and is reported as one.
use std::sync::Arc;

use anyhow::{bail, Result};
use punktfunk_core::audio::{layout_for, AudioGapTracker, AudioLayout};

use crate::core::media::{AudioSink, Samples};

/// 48 kHz, 5 ms frames — punktfunk's fixed audio framing.
pub const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: usize = 240;
/// One packet, in ms, as the concealment arithmetic needs it.
const FRAME_MS: i64 = 5;
/// Max channels punktfunk ever negotiates (7.1) — sizes the scratch decode buffer.
const MAX_CHANNELS: usize = 8;

/// Decodes one session's audio into its sink.
pub struct AudioStage {
    sink: Arc<dyn AudioSink>,
    decoder: opus::MSDecoder,
    /// Negotiated channel count — the decode width.
    channels: usize,
    /// Detects packets lost on the wire so they are concealed rather than skipped.
    gaps: AudioGapTracker,
    /// Concealment frames then the packet, filled only when there was concealment.
    f32: Vec<f32>,
    /// Samples in `pcm` from the last decode.
    last: usize,
    /// libopus's output, one frame at the widest layout. A field so it is not zeroed per packet.
    pcm: Box<[f32; SAMPLES_PER_FRAME * MAX_CHANNELS]>,
    /// The level of the host's packets since [`AudioStage::take_level`].
    meter: Meter,
}

/// Peak and RMS of decoded samples, full scale 1.0.
#[derive(Default)]
struct Meter {
    peak: f32,
    sum_sq: f64,
    samples: u64,
}

/// The audio's level over a window, in dBFS: 0 is full scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub peak_db: f32,
    pub rms_db: f32,
}

/// Silence reads as this rather than minus infinity.
const FLOOR_DB: f32 = -120.0;

fn dbfs(amplitude: f64) -> f32 {
    if amplitude > 0.0 {
        ((20.0 * amplitude.log10()) as f32).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

impl AudioStage {
    /// `channels` and `layout` are host-resolved: the decoder is built from what the handshake
    /// settled on, never from what was requested.
    pub fn new(sink: Arc<dyn AudioSink>, channels: u8, layout: u8) -> Result<Self> {
        let format = sink.format();
        if format.channels() != channels {
            bail!(
                "{} takes {} channel(s), the session negotiated {channels}",
                sink.name(),
                format.channels(),
            );
        }
        let Some(layout) = AudioLayout::from_wire(layout) else {
            bail!("the host encodes audio layout {layout}, which this client does not know");
        };
        let wire = layout_for(channels, layout);
        let decoder = opus::MSDecoder::new(SAMPLE_RATE, wire.streams, wire.coupled, wire.mapping)
            .map_err(|e| anyhow::anyhow!("opus MSDecoder::new: {e}"))?;
        Ok(Self {
            sink,
            decoder,
            channels: wire.channels as usize,
            gaps: AudioGapTracker::new(),
            f32: Vec::with_capacity(SAMPLES_PER_FRAME * MAX_CHANNELS * 2),
            last: 0,
            pcm: Box::new([0.0; SAMPLES_PER_FRAME * MAX_CHANNELS]),
            meter: Meter::default(),
        })
    }

    pub fn sink_name(&self) -> &'static str {
        self.sink.name()
    }

    /// One packet, concealment included, into the sink. Concealment fills the hole BEFORE the
    /// packet, so it is stamped that many milliseconds earlier than the packet.
    pub fn play(&mut self, seq: u32, pts_ns: u64, payload: &[u8]) -> Result<()> {
        let Self {
            decoder,
            f32,
            last,
            pcm,
            channels,
            gaps,
            sink,
            meter,
        } = self;
        let channels = *channels;
        let missing = gaps.missing_before(seq);
        f32.clear();
        // A concealment frame gets exactly one frame of buffer: with no packet libopus takes
        // `out.len() / channels` as the frame size and rejects an illegal one.
        let cap = |i: u32| {
            if i < missing {
                SAMPLES_PER_FRAME * channels
            } else {
                SAMPLES_PER_FRAME * MAX_CHANNELS
            }
        };
        let stamp = |i: u32| pts_ns.saturating_sub((i64::from(missing - i) * FRAME_MS) as u64 * 1_000_000);
        for i in 0..=missing {
            let input: &[u8] = if i < missing { &[] } else { payload };
            let frames = decoder
                .decode_float(input, &mut pcm[..cap(i)], false)
                .map_err(|e| anyhow::anyhow!("opus decode: {e}"))?;
            let decoded = &pcm[..frames * channels];
            *last = decoded.len();
            if missing > 0 {
                f32.extend_from_slice(decoded);
            }
        }
        for &s in &pcm[..*last] {
            meter.peak = meter.peak.max(s.abs());
            meter.sum_sq += f64::from(s) * f64::from(s);
        }
        meter.samples += *last as u64;
        if missing == 0 {
            return sink.feed(Samples::F32(&pcm[..*last]), stamp(0));
        }
        sink.feed(Samples::F32(f32), stamp(0))
    }

    /// The sink's queue depth in ms, where it knows one.
    pub fn depth_ms(&self) -> Option<i64> {
        self.sink.depth_ms()
    }

    /// Output-grain underruns, when the sink can report them.
    pub fn underruns(&self) -> Option<u64> {
        self.sink.underruns()
    }

    /// The level of the host's packets since the last call, concealment left out: tells "the
    /// host sends it quiet" from "the console plays it quiet".
    pub fn take_level(&mut self) -> Level {
        let m = std::mem::take(&mut self.meter);
        let mean_sq = if m.samples == 0 { 0.0 } else { m.sum_sq / m.samples as f64 };
        Level {
            peak_db: dbfs(f64::from(m.peak)),
            rms_db: dbfs(mean_sq.sqrt()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::core::media::AudioFormat;

    /// Keeps every buffer it is fed, with its stamp.
    struct Recorder(Mutex<Vec<(usize, u64)>>, u8);

    impl AudioSink for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }

        fn format(&self) -> AudioFormat {
            AudioFormat::PcmF32 {
                channels: self.1,
                sample_rate: SAMPLE_RATE,
            }
        }

        fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()> {
            let Samples::F32(pcm) = samples;
            self.0.lock().unwrap().push((pcm.len(), host_pts_ns));
            Ok(())
        }
    }

    fn packet(encoder: &mut opus::Encoder) -> Vec<u8> {
        let frame = vec![0.25f32; SAMPLES_PER_FRAME * 2];
        let mut out = [0u8; 1500];
        let n = encoder.encode_float(&frame, &mut out).unwrap();
        out[..n].to_vec()
    }

    /// A lost packet comes out as one concealed frame ahead of the next, stamped 5 ms earlier.
    #[test]
    fn a_gap_is_concealed_before_the_next_packet() {
        let sink = Arc::new(Recorder(Mutex::new(Vec::new()), 2));
        let mut stage = AudioStage::new(sink.clone(), 2, AudioLayout::Standard.wire()).unwrap();
        let mut encoder = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
        stage.play(0, 100_000_000, &packet(&mut encoder)).unwrap();
        stage.play(2, 110_000_000, &packet(&mut encoder)).unwrap();
        let fed = sink.0.lock().unwrap().clone();
        assert_eq!(fed[0], (SAMPLES_PER_FRAME * 2, 100_000_000));
        assert_eq!(fed[1], (SAMPLES_PER_FRAME * 2 * 2, 105_000_000));
        assert!(stage.take_level().peak_db > FLOOR_DB);
        assert_eq!(stage.take_level().peak_db, FLOOR_DB, "the window starts over");
    }

    #[test]
    fn levels_read_in_dbfs() {
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(0.5) + 6.02).abs() < 0.01);
        assert_eq!(dbfs(0.0), FLOOR_DB);
    }

    /// A width the sink does not play, or a layout this build does not know, is refused.
    #[test]
    fn mismatches_are_refused() {
        let stereo = Arc::new(Recorder(Mutex::new(Vec::new()), 2));
        assert!(AudioStage::new(stereo.clone(), 6, AudioLayout::Standard.wire()).is_err());
        assert!(AudioStage::new(stereo, 2, 9).is_err());
    }
}
