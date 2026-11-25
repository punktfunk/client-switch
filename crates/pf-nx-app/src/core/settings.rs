//! This client's typed view of the shared settings document (`pf_client_core::trust::Settings`),
//! the one settings type it stores and edits. Every row here is a shared field but one: a row
//! only the console has lives in `extra` under an `nx.` prefix, as `nx.handheld_gyro` and
//! `nx.face_layout` do.

use pf_client_core::trust::Settings;
use punktfunk_core::config::GamepadPref;

use crate::core::model::{CodecPref, FaceLayout, GamepadType, HandheldGyro};

/// The console-only row picking the face-button layout: `"auto"` (the default) or
/// `"position"`. No screen sets it yet; the player edits `settings.json` with the app closed.
pub const FACE_LAYOUT_KEY: &str = "nx.face_layout";

fn layout_name(l: FaceLayout) -> &'static str {
    match l {
        FaceLayout::Auto => "auto",
        FaceLayout::Position => "position",
    }
}

/// The console-only row picking the handheld gyro: `"console"` (the default) or `"controller"`.
/// No screen sets it yet; the shared settings screen is to grow a row writing these names.
pub const HANDHELD_GYRO_KEY: &str = "nx.handheld_gyro";

/// A file by this name in the app's data directory picks the Joy-Con's gyro whatever the
/// setting says, until a screen sets [`HANDHELD_GYRO_KEY`].
pub const GYRO_CONTROLLER_KEY: &str = "gyro_controller";

fn gyro_name(g: HandheldGyro) -> &'static str {
    match g {
        HandheldGyro::Console => "console",
        HandheldGyro::Controller => "controller",
    }
}

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

/// The typed reads and writes this client makes on the shared document.
pub trait NxSettings {
    fn codec_pref(&self) -> CodecPref;
    fn set_codec_pref(&mut self, codec: CodecPref);
    fn gamepad_type(&self) -> GamepadType;
    fn set_gamepad_type(&mut self, kind: GamepadType);
    /// The handheld gyro: the Joy-Con's when `key_file` ([`GYRO_CONTROLLER_KEY`]) is there,
    /// else [`HANDHELD_GYRO_KEY`]'s pick; an unknown name reads as the default.
    fn handheld_gyro(&self, key_file: bool) -> HandheldGyro;
    fn set_handheld_gyro(&mut self, gyro: HandheldGyro);
    /// The face-button layout [`FACE_LAYOUT_KEY`] picks; an unknown name reads as the default.
    fn face_layout(&self) -> FaceLayout;
    fn set_face_layout(&mut self, layout: FaceLayout);
    /// Normalise to what this console can present (`core::caps`). Called on load and on every
    /// launch's resolved settings, so a profile override cannot put an unpresentable pick on
    /// the wire. HDR stays as the player set it: the display can change between launches, and
    /// `session::connect` asks for HDR only when the caps carry it.
    fn clamp_to_caps(&mut self);
}

impl NxSettings for Settings {
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

    fn handheld_gyro(&self, key_file: bool) -> HandheldGyro {
        if key_file {
            return HandheldGyro::Controller;
        }
        match self.extra.get(HANDHELD_GYRO_KEY).and_then(|v| v.as_str()) {
            Some(name) if name == gyro_name(HandheldGyro::Controller) => HandheldGyro::Controller,
            _ => HandheldGyro::Console,
        }
    }

    fn set_handheld_gyro(&mut self, gyro: HandheldGyro) {
        self.extra.insert(HANDHELD_GYRO_KEY.into(), gyro_name(gyro).into());
    }

    fn face_layout(&self) -> FaceLayout {
        match self.extra.get(FACE_LAYOUT_KEY).and_then(|v| v.as_str()) {
            Some(name) if name == layout_name(FaceLayout::Position) => FaceLayout::Position,
            _ => FaceLayout::Auto,
        }
    }

    fn set_face_layout(&mut self, layout: FaceLayout) {
        self.extra.insert(FACE_LAYOUT_KEY.into(), layout_name(layout).into());
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
    fn the_handheld_gyro_is_the_console_unless_picked_or_keyed() {
        let mut s = default_document();
        assert_eq!(s.handheld_gyro(false), HandheldGyro::Console);
        assert_eq!(s.handheld_gyro(true), HandheldGyro::Controller, "the key file wins");
        s.set_handheld_gyro(HandheldGyro::Controller);
        assert_eq!(s.extra[HANDHELD_GYRO_KEY], "controller");
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.handheld_gyro(false), HandheldGyro::Controller);
        s.extra.insert(HANDHELD_GYRO_KEY.into(), "sideways".into());
        assert_eq!(s.handheld_gyro(false), HandheldGyro::Console);
    }

    #[test]
    fn the_face_layout_is_auto_unless_picked() {
        let mut s = default_document();
        assert_eq!(s.face_layout(), FaceLayout::Auto);
        s.set_face_layout(FaceLayout::Position);
        assert_eq!(s.extra[FACE_LAYOUT_KEY], "position");
        let back: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.face_layout(), FaceLayout::Position);
        // The key sits beside the shared fields, where a player editing the file finds it.
        let doc: Settings = serde_json::from_str(r#"{"nx.face_layout": "position"}"#).unwrap();
        assert_eq!(doc.face_layout(), FaceLayout::Position);
        s.extra.insert(FACE_LAYOUT_KEY.into(), "label".into());
        assert_eq!(s.face_layout(), FaceLayout::Auto);
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
