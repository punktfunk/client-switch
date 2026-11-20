//! This client's typed view of the shared settings document (`pf_client_core::trust::Settings`),
//! the one settings type it stores and edits. Every row here is a shared field but Save Battery,
//! which only the console has and which lives in `extra` under an `nx.` prefix.

use pf_client_core::trust::{PresentPriority, Settings};
use punktfunk_core::config::GamepadPref;

use crate::core::model::{CodecPref, GamepadType};

/// This client's codec pick as punktfunk's wire name, and back.
fn shared_codec(c: CodecPref) -> &'static str {
    match c {
        CodecPref::Auto => "auto",
        CodecPref::PyroWave => "pyrowave",
        CodecPref::H264 => "h264",
        CodecPref::Hevc => "hevc",
    }
}

fn local_codec(name: &str) -> CodecPref {
    match name {
        "pyrowave" => CodecPref::PyroWave,
        "h264" => CodecPref::H264,
        "hevc" => CodecPref::Hevc,
        // "av1" too: the console cannot decode it, so it reads as the host's choice.
        _ => CodecPref::Auto,
    }
}

/// Save Battery's key in `extra`: on, the title leaves the clocks to the system.
pub const SAVE_BATTERY_KEY: &str = "nx.save_battery";

/// The typed reads and writes this client makes on the shared document.
pub trait NxSettings {
    /// Whether the title leaves the clocks to the system instead of holding its own. Off until
    /// the player turns it on.
    fn save_battery(&self) -> bool;
    fn set_save_battery(&mut self, on: bool);
    /// The shared rule but for Automatic smoothness, which takes one frame here, not two: on the
    /// console one frame presents as many pictures as two, a refresh sooner.
    fn nx_present_priority(&self) -> PresentPriority;
    fn codec_pref(&self) -> CodecPref;
    fn set_codec_pref(&mut self, codec: CodecPref);
    fn gamepad_type(&self) -> GamepadType;
    fn set_gamepad_type(&mut self, kind: GamepadType);
    /// Normalise to what this console can present (`core::caps`). Called on load and on every
    /// launch's resolved settings, so a profile override cannot put an unpresentable pick on
    /// the wire. HDR stays as the player set it: the display can change between launches, and
    /// `session::connect` asks for HDR only when the caps carry it.
    fn clamp_to_caps(&mut self);
}

impl NxSettings for Settings {
    fn save_battery(&self) -> bool {
        self.extra
            .get(SAVE_BATTERY_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn set_save_battery(&mut self, on: bool) {
        self.extra.insert(SAVE_BATTERY_KEY.into(), serde_json::Value::Bool(on));
    }

    fn nx_present_priority(&self) -> PresentPriority {
        match self.present_priority() {
            PresentPriority::Smooth { .. } if self.smooth_buffer == 0 => PresentPriority::Smooth { buffer: 1 },
            shared => shared,
        }
    }

    fn codec_pref(&self) -> CodecPref {
        local_codec(&self.codec)
    }

    fn set_codec_pref(&mut self, codec: CodecPref) {
        self.codec = shared_codec(codec).to_string();
    }

    fn gamepad_type(&self) -> GamepadType {
        GamepadPref::from_name(&self.gamepad)
            .and_then(GamepadType::from_core)
            .unwrap_or_default()
    }

    fn set_gamepad_type(&mut self, kind: GamepadType) {
        self.gamepad = kind.to_core().as_str().to_string();
    }

    fn clamp_to_caps(&mut self) {
        let caps = crate::core::caps::video_caps();
        let codecs = caps.codec_prefs();
        let codec = self.codec_pref();
        if !codecs.contains(&codec) {
            if let Some(&first) = codecs.first() {
                tracing::info!("settings: {codec:?} isn't decodable here — using {first:?}");
                self.set_codec_pref(first);
            }
        }
        if self.audio_channels > caps.max_channels {
            tracing::info!(
                "settings: {} audio channels is more than this console plays ({}) — clamping",
                self.audio_channels,
                caps.max_channels,
            );
            self.audio_channels = caps.max_channels;
        }
    }
}

/// The document a fresh install starts from.
pub fn default_document() -> Settings {
    Settings::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_through_the_shared_names() {
        let mut s = default_document();
        s.set_codec_pref(CodecPref::Hevc);
        s.set_gamepad_type(GamepadType::JoyConPair);
        assert_eq!(s.codec, "hevc");
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.codec_pref(), CodecPref::Hevc);
        assert_eq!(back.gamepad_type(), GamepadType::JoyConPair);
    }

    #[test]
    fn save_battery_starts_off_and_round_trips() {
        let mut s = default_document();
        assert!(!s.save_battery());
        s.set_save_battery(true);
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert!(back.save_battery());
        assert_eq!(back.extra[SAVE_BATTERY_KEY], serde_json::Value::Bool(true));
    }

    #[test]
    fn automatic_smoothness_takes_one_frame() {
        let mut s = default_document();
        s.present_priority = "smooth".into();
        s.smooth_buffer = 0;
        assert_eq!(s.nx_present_priority(), PresentPriority::Smooth { buffer: 1 });
        for buffer in 1..=3 {
            s.smooth_buffer = buffer;
            assert_eq!(s.nx_present_priority(), PresentPriority::Smooth { buffer });
        }
        s.present_priority = "latency".into();
        s.smooth_buffer = 0;
        assert_eq!(s.nx_present_priority(), PresentPriority::Latency);
    }

    #[test]
    fn clamp_keeps_hdr_and_narrows_channels() {
        let mut s = default_document();
        s.hdr_enabled = true;
        s.audio_channels = 8;
        s.codec = "av1".into();
        s.clamp_to_caps();
        assert!(s.hdr_enabled);
        assert_eq!(s.audio_channels, 2);
        assert_eq!(s.codec_pref(), CodecPref::Auto);
    }
}
