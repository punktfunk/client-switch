//! Plain domain data. No I/O — persistence lives in `crate::services`.
use serde::{Deserialize, Serialize};

use pf_client_core::presets::StreamPreset;

/// Stream connection target.
#[derive(Clone)]
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
    /// The pinned host fingerprint. Always present: the pin *is* the pair state, and
    /// every launch path bails on an unpaired host before building one of these.
    pub fingerprint: [u8; 32],
    /// Library entry id to launch, or `None` for desktop.
    pub launch: Option<String>,
    /// The host profile to play as; `None` lets the host choose.
    pub profile: Option<String>,
}

/// One saved host: the record every punktfunk client stores (`trust::KnownHost`, flattened
/// into the same object — plan D8) plus what only this console keeps: its power behaviour. Reads of the shared fields go through `Deref`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownHost {
    #[serde(flatten)]
    pub shared: pf_client_core::trust::KnownHost,
    /// Wake this host when it is picked and found asleep.
    pub wol_auto: bool,
    /// What to do to this host when the app exits (per-host, off by default; sits under
    /// `wol_auto` in Host power settings — the two are the same switch pointing opposite ways).
    pub exit_action: ExitAction,
}

impl std::ops::Deref for KnownHost {
    type Target = pf_client_core::trust::KnownHost;
    fn deref(&self) -> &Self::Target {
        &self.shared
    }
}

impl std::ops::DerefMut for KnownHost {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.shared
    }
}

/// The shared record derives no `PartialEq`; the store's writer compares documents, and the
/// serialized form is the one comparison that cannot miss a field. Bytes, not a `Value` tree: the
/// record's only map is a `BTreeMap`, so its encoding is deterministic.
impl PartialEq for KnownHost {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_vec(self).ok() == serde_json::to_vec(other).ok()
    }
}

/// What to do to a host when the app exits: nothing, or one of the host's power actions
/// (`services::power`, host-side from `punktfunk-core` 0.33.0), which need the pairing's Host
/// power grant. Defaults to [`Self::None`]: a client that quietly powered a machine down would be
/// worse than one that never offered to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitAction {
    #[default]
    None,
    Sleep,
    Shutdown,
}

impl ExitAction {
    /// The host action id to invoke, or `None` when there is nothing to do.
    pub fn action_id(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Sleep => Some("power.sleep"),
            Self::Shutdown => Some("power.shutdown"),
        }
    }
}

/// Pin ID for the "Desktop" card — a `game_presets` key like any game id.
pub const DESKTOP_PIN_ID: &str = "__desktop__";

impl KnownHost {
    /// Paired means a pinned certificate: the fingerprint IS the pair state.
    pub fn is_paired(&self) -> bool {
        !self.fp_hex.is_empty()
    }

    /// The pinned certificate fingerprint, decoded from the stored hex.
    pub fn fingerprint(&self) -> Option<[u8; 32]> {
        pf_client_core::trust::parse_hex32(&self.fp_hex)
    }

    /// Pins `fp` as the host's certificate: the record is paired from here on.
    pub fn set_fingerprint(&mut self, fp: [u8; 32]) {
        use std::fmt::Write;
        self.fp_hex = fp.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
        self.paired = true;
    }

    /// This game's bound profile id, if it has one (the shared `game_presets`).
    pub fn game_profile(&self, id: &str) -> Option<&str> {
        self.shared.preset_for_game(id)
    }
}

/// Upserts by `(addr, port)`, keeping the existing pin if the new record is unpaired (a fresh
/// mDNS discovery shouldn't clobber a paired host) — same reasoning for `mac` and `os`,
/// learned separately. The record's id, its profile bindings, its pins, `wol_auto` and the
/// exit action are *always* kept from the existing record: only their own screens change them, so no add/edit/re-pair flow may clobber any.
///
/// Returns the inserted record when it was genuinely new, so the caller can seed what a first
/// sighting gets ([`seed_new_host_profiles`]) without looking it up again. `None` for a merge:
/// re-adding or re-pairing a host must not re-seed anything.
pub fn upsert_known_host(hosts: &mut Vec<KnownHost>, mut new: KnownHost) -> Option<&mut KnownHost> {
    let Some(at) = hosts.iter().position(|h| h.addr == new.addr && h.port == new.port) else {
        hosts.push(new);
        return hosts.last_mut();
    };
    let existing = &mut hosts[at];
    if !new.is_paired() {
        new.fp_hex.clone_from(&existing.fp_hex);
        new.paired = existing.paired;
    }
    if new.mac.is_empty() {
        new.mac.clone_from(&existing.mac);
    }
    if new.os.is_empty() {
        new.os.clone_from(&existing.os);
    }
    new.id.clone_from(&existing.id);
    new.preset_id.clone_from(&existing.preset_id);
    new.pinned_presets.clone_from(&existing.pinned_presets);
    new.game_presets.clone_from(&existing.game_presets);
    new.wol_auto = existing.wol_auto;
    new.exit_action = existing.exit_action;
    *existing = new;
    None
}

/// Follows a paired host to the address it now advertises from, matched by its pinned
/// fingerprint, so a DHCP move doesn't leave the record offline and a re-add doesn't save
/// it twice. `advertised` says whether some advert currently comes from an address: a record
/// whose own address still advertises is a second interface, not a move, and stays put.
///
/// The advert is unauthenticated, but the pin is what every connection checks, so a forged
/// one can only point the record at an address whose handshake then fails.
///
/// With no record at the advert's address, the record that pins `fp_hex` moves there. With
/// one there that pins the same certificate, the others pinning it are dropped as
/// duplicates. Returns whether anything changed.
pub fn follow_moved_host(
    hosts: &mut Vec<KnownHost>,
    fp_hex: &str,
    addr: &str,
    port: u16,
    advertised: impl Fn(&str, u16) -> bool,
) -> bool {
    if fp_hex.is_empty() {
        return false;
    }
    let stale =
        |h: &KnownHost| h.fp_hex == fp_hex && (h.addr.as_str(), h.port) != (addr, port) && !advertised(&h.addr, h.port);
    match hosts.iter().find(|h| (h.addr.as_str(), h.port) == (addr, port)) {
        Some(here) if here.fp_hex == fp_hex => {
            let before = hosts.len();
            hosts.retain(|h| !stale(h));
            hosts.len() != before
        }
        Some(_) => false,
        None => {
            let Some(moved) = hosts.iter_mut().find(|h| stale(h)) else {
                return false;
            };
            moved.addr = addr.to_string();
            moved.port = port;
            true
        }
    }
}

fn unique_profile_name(catalog: &[StreamPreset], wanted: &str) -> String {
    let taken = |name: &str| catalog.iter().any(|p| p.name == name);
    if !taken(wanted) {
        return wanted.to_string();
    }
    (2..)
        .map(|n| format!("{wanted} {n}"))
        .find(|name| !taken(name))
        .expect("an unbounded counter finds a free name")
}

const DESKTOP_PROFILE_NAME: &str = "Desktop";

/// Seeds a new host with a Desktop profile pinning absolute pointer mode (while the global
/// default stays capture). Unlike a resolver rule, this pin is visible in Settings and mutable,
/// so users see what's overriding the default and can adjust it.
///
/// Takes the record [`upsert_known_host`] reports as new, so existing installs keep their state.
/// Idempotent: a host with an existing Desktop binding is left alone.
pub fn seed_new_host_profiles(host: &mut KnownHost, profiles: &mut Vec<StreamPreset>) {
    if host.game_profile(DESKTOP_PIN_ID).is_some() {
        return;
    }
    let mut profile = StreamPreset::new(unique_profile_name(profiles, DESKTOP_PROFILE_NAME));
    profile.overrides.mouse_mode = Some(pf_client_core::trust::MouseMode::Desktop.as_name().to_string());
    let id = profile.id.clone();
    profiles.push(profile);
    host.bind_game_preset(DESKTOP_PIN_ID, Some(&id));
}

/// Codec preference selectable in Settings — a *preference*, not a demand. The host
/// resolves the session codec from the client's advertised set via its own precedence
/// ladder (HEVC > H.264), honouring the preference only when its encoder can
/// actually produce it — so "H264" on a host that can't encode H.264 still gets HEVC
/// rather than no session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecPref {
    /// No preference — the host's precedence ladder decides (HEVC in practice).
    #[default]
    Auto,
    /// The intra-only wavelet codec, decoded on the GPU. Wired LAN only; never picked by
    /// the host on its own, so choosing it here is what asks for it.
    #[serde(rename = "pyrowave")]
    PyroWave,
    H264,
    Hevc,
}

/// Which controller the host should present to the game, selectable in Settings.
///
/// This is the *virtual* pad the host builds, not what the user is holding — the host
/// translates. The host resolves the choice against what its platform can actually build (the
/// `PlayStation` and Switch backends need Linux UHID) and falls back on its own if not, so an
/// unbuildable pick degrades to a working session rather than none.
///
/// A deliberate subset of `punktfunk_core::config::GamepadPref`: the Steam Controller/Deck
/// backends exist for clients running *on* that hardware, which a console is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GamepadType {
    /// Mirror the pad in hand: a Joy-Con pair or a Pro Controller, resolved per pad so the
    /// stored preference stays "match my pad".
    #[default]
    Auto,
    /// The host's Xbox pad. The alias keeps a settings file written when this client also
    /// offered a separate Xbox 360 pick loadable: the two differed only in the virtual pad's
    /// name.
    #[serde(alias = "xbox360")]
    XboxOne,
    DualShock4,
    DualSense,
    /// `DualSense` plus the two back buttons and two Fn buttons.
    DualSenseEdge,
    SwitchPro,
    /// Two Joy-Cons as one pad: SL and SR on the paddles, a gyro in each half.
    JoyConPair,
}

impl GamepadType {
    /// The wire preference sent in the handshake, which becomes the session-default pad kind —
    /// and the name the shared settings document stores, and what the console shell picks its
    /// button-glyph legend by.
    pub fn to_core(self) -> punktfunk_core::config::GamepadPref {
        use punktfunk_core::config::GamepadPref as P;
        match self {
            Self::Auto => P::Auto,
            Self::XboxOne => P::XboxOne,
            Self::DualShock4 => P::DualShock4,
            Self::DualSense => P::DualSense,
            Self::DualSenseEdge => P::DualSenseEdge,
            Self::SwitchPro => P::SwitchPro,
            Self::JoyConPair => P::JoyConPair,
        }
    }

    /// The inverse of [`Self::to_core`]. `None` for a kind this client has no row for (a Steam
    /// Deck's pad, say), so the caller keeps its default rather than showing a control it
    /// cannot honour.
    pub fn from_core(pref: punktfunk_core::config::GamepadPref) -> Option<Self> {
        use punktfunk_core::config::GamepadPref as P;
        Some(match pref {
            P::Auto => Self::Auto,
            P::XboxOne => Self::XboxOne,
            P::DualShock4 => Self::DualShock4,
            P::DualSense => Self::DualSense,
            P::DualSenseEdge => Self::DualSenseEdge,
            P::SwitchPro => Self::SwitchPro,
            P::JoyConPair => Self::JoyConPair,
            _ => return None,
        })
    }

    /// The dial's Controller type step: Automatic, the two Nintendo kinds this console's pads
    /// are, then the shared order (`overlay_actions::next_pad_type`) over the kinds this client
    /// offers. The shared order names no Nintendo kind yet (`docs/improvements.md`).
    pub fn next_on_dial(self) -> Self {
        let mut pref = match self {
            Self::Auto => return Self::JoyConPair,
            Self::JoyConPair => return Self::SwitchPro,
            Self::SwitchPro => punktfunk_core::config::GamepadPref::Auto,
            other => other.to_core(),
        };
        loop {
            pref = pf_client_core::overlay_actions::next_pad_type(pref);
            if let Some(next) = Self::from_core(pref) {
                return next;
            }
        }
    }
}

/// Automatic bitrate's ceiling where `PyroWave` decodes, handed to `punktfunk_core::abr` through
/// `run` (`PUNKTFUNK_ABR_MAX_MBPS`). It binds every codec, so it sits above Automatic `PyroWave`'s
/// quality floor (0.5 bit/pixel), below which core clamps a `PyroWave` session to that floor.
pub const BITRATE_MAX_KBPS: u32 = 650_000;

/// Automatic bitrate's ceiling where only NVDEC decodes: the rate a pinned stream ran clean at,
/// handheld at stock clocks. Core finds the link's edge by crossing it, and its climbs to
/// 22-25 Mb/s preceded every 60-110 ms delay spike (2026-10-31). Under the ceiling it still cuts
/// for a dock or a weak Wi-Fi. A raised CPU clock is what would carry more.
pub const NVDEC_BITRATE_MAX_KBPS: u32 = 20_000;

/// Automatic bitrate's ceiling for a title whose video lane can or cannot decode `PyroWave`.
/// On the console the receive path bounds every codec, and the ceiling is read once at start,
/// before a codec is negotiated: a `PyroWave` lane there must not lift HEVC's ceiling. How core
/// weighs this ceiling against `PyroWave`'s quality floor is unchecked, so the console measures
/// `PyroWave` at a pinned rate, which the ceiling does not bind.
pub const fn bitrate_max_kbps(pyrowave: bool) -> u32 {
    if pyrowave && !cfg!(target_os = "horizon") {
        BITRATE_MAX_KBPS
    } else {
        NVDEC_BITRATE_MAX_KBPS
    }
}

/// 70% of measured goodput (headroom for FEC, loss).
/// Integer arithmetic order matters (`/ 10 * 7`, not `* 0.7`) — all clients must agree exactly.
pub fn recommended_bitrate_kbps(throughput_kbps: u32) -> u32 {
    throughput_kbps / 10 * 7
}

/// Everything this app persists, as one document — `settings.json`, written by
/// `services::store::StateWriter`.
///
/// One file rather than one per concern: separate files can disagree after a mid-write power
/// cut, which on a console is the normal way to turn it off. Only the credentials
/// (`client-{cert,key}.pem`) stay outside, so a settings document that fails to parse can fall
/// back to defaults without silently discarding the identity every host has pinned.
///
/// `PartialEq` is load-bearing: `services::store::StateWriter` skips writing an unchanged
/// snapshot.
///
/// `Default` is spelled out rather than derived: a fresh install starts from this console's document
/// ([`crate::core::settings::default_document`]), not the bare shared one, and `#[serde(default)]`
/// fills a missing field from it too — one source of defaults for both paths.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub settings: pf_client_core::trust::Settings,
    pub known_hosts: Vec<KnownHost>,
    /// The host the user last had active — so relaunching lands back on its library.
    /// `(host, port)`, not an index: `known_hosts` order isn't stable across a forget/re-add.
    pub selected_host: Option<(String, u16)>,
    /// The app version that last wrote this document (`core::VERSION`). `None` means it
    /// was written before versioning existed — the only signal a future migration gets about
    /// which shape it is reading. `store::load` stamps it on first sight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The settings-profile catalog: named bundles of sparse overrides, the model punktfunk's
    /// other clients keep in `client-profiles.json`. Here it rides the one document this client
    /// writes, for the reason everything else does — one file, one writer, no merge.
    ///
    /// A game's settings ARE a profile (the shared `game_presets`), which is what lets the
    /// shared shell list them and bind one to a title. A console with no desktop app beside it can still
    /// fill this: opening a game's settings and changing a row creates the profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<StreamPreset>,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            settings: crate::core::settings::default_document(),
            known_hosts: Vec::new(),
            selected_host: None,
            version: None,
            profiles: Vec::new(),
        }
    }
}

/// Cover-art paths for a title (host-relative, fetched via mTLS). Covers prefer
/// `portrait` then `header` then `hero`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Artwork {
    pub portrait: Option<String>,
    pub hero: Option<String>,
    pub header: Option<String>,
}

/// One title in the host's unified library. `id` is store-qualified (`steam:<appid>`,
/// `custom:<id>`) and doubles as the launch handle `session::connect`'s `launch`
/// parameter takes — the host resolves the actual launch spec itself from `id`.
#[derive(Clone, Debug, Deserialize)]
pub struct GameEntry {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub art: Artwork,
    /// Packaged brand mark token, such as `steam`, for art-less launcher cards.
    #[serde(default)]
    pub icon: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dial steps the Nintendo kinds, then the shared order over this client's rows.
    #[test]
    fn the_dial_steps_the_types_this_client_offers() {
        let mut seen = vec![GamepadType::Auto];
        let mut kind = GamepadType::Auto.next_on_dial();
        while kind != GamepadType::Auto {
            seen.push(kind);
            kind = kind.next_on_dial();
        }
        assert_eq!(
            seen,
            [
                GamepadType::Auto,
                GamepadType::JoyConPair,
                GamepadType::SwitchPro,
                GamepadType::XboxOne,
                GamepadType::DualSense,
                GamepadType::DualShock4
            ]
        );
    }

    /// The saved record is the shared one plus this console's fields, in one flat object (plan D8):
    /// what the shell reads is what is stored, and the pin round-trips through its hex.
    #[test]
    fn host_record_is_the_shared_record_flattened() {
        let mut h = KnownHost {
            shared: pf_client_core::trust::KnownHost {
                name: "desk".into(),
                addr: "10.0.0.2".into(),
                port: 47_989,
                ..Default::default()
            },
            wol_auto: true,
            ..Default::default()
        };
        h.set_fingerprint([0x5a; 32]);
        h.bind_game_preset("doom", Some("p1"));
        let json = serde_json::to_value(&h).unwrap();
        assert_eq!(json["addr"], "10.0.0.2");
        assert_eq!(json["fp_hex"], "5a".repeat(32));
        assert_eq!(json["wol_auto"], true);
        assert_eq!(json["game_presets"]["doom"], "p1");
        assert!(json.get("shared").is_none(), "flattened, not nested");
        let back: KnownHost = serde_json::from_value(json).unwrap();
        assert_eq!(back, h);
        assert!(back.is_paired());
        assert_eq!(back.fingerprint(), Some([0x5a; 32]));

        // Re-adding the host unpaired keeps the pin, the id and the bindings.
        let mut hosts = vec![h.clone()];
        upsert_known_host(
            &mut hosts,
            KnownHost {
                shared: pf_client_core::trust::KnownHost {
                    name: "desk".into(),
                    addr: "10.0.0.2".into(),
                    port: 47_989,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(hosts.len(), 1);
        assert!(hosts[0].is_paired() && hosts[0].wol_auto);
        assert_eq!(hosts[0].id, h.id);
        assert_eq!(hosts[0].game_profile("doom"), Some("p1"));
    }

    fn paired_at(addr: &str, fp: u8) -> KnownHost {
        let mut h = KnownHost {
            shared: pf_client_core::trust::KnownHost {
                name: "desk".into(),
                addr: addr.into(),
                port: 9777,
                ..Default::default()
            },
            ..Default::default()
        };
        h.set_fingerprint([fp; 32]);
        h
    }

    /// The 2026-11-02 settings: the router moved the host from .137 to .10, the record stayed
    /// on .137 and showed offline, and re-adding it saved the same pin a second time.
    #[test]
    fn a_paired_host_follows_its_advert_to_a_new_address() {
        let fp = "5a".repeat(32);
        let quiet = |_: &str, _: u16| false;

        let mut hosts = vec![paired_at("192.168.0.137", 0x5a)];
        assert!(follow_moved_host(&mut hosts, &fp, "192.168.0.10", 9777, quiet));
        assert_eq!(hosts[0].addr, "192.168.0.10");

        let mut hosts = vec![paired_at("192.168.0.137", 0x5a), paired_at("192.168.0.10", 0x5a)];
        assert!(follow_moved_host(&mut hosts, &fp, "192.168.0.10", 9777, quiet));
        assert_eq!(hosts.len(), 1, "the stale duplicate is dropped");
        assert_eq!(hosts[0].addr, "192.168.0.10");

        // A second interface that still advertises is not a move.
        let mut hosts = vec![paired_at("192.168.0.137", 0x5a)];
        let both = |a: &str, _: u16| a == "192.168.0.137";
        assert!(!follow_moved_host(&mut hosts, &fp, "192.168.0.10", 9777, both));
        assert_eq!(hosts[0].addr, "192.168.0.137");

        // Another certificate, or another host already at the address, moves nothing.
        let mut hosts = vec![paired_at("192.168.0.137", 0x11)];
        assert!(!follow_moved_host(&mut hosts, &fp, "192.168.0.10", 9777, quiet));
        let mut hosts = vec![paired_at("192.168.0.137", 0x5a), paired_at("192.168.0.10", 0x11)];
        assert!(!follow_moved_host(&mut hosts, &fp, "192.168.0.10", 9777, quiet));
        assert_eq!(hosts.len(), 2);
    }
}
