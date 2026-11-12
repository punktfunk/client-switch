//! The stream: the picture on the display's stream layer, the dial and the stats over it, and
//! the pads on an input thread of their own.
//!
//! The main thread presents at the display's rate. The input thread polls the pads, their
//! motion and the touch screen at 250 Hz, sends their input straight to the host, applies the
//! host's rumble, and hands the main thread only what it must act on (the dial, and what the
//! shortcuts fire).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pf_client_core::overlay_actions::{OverlayConfig, RingPlatform, SlotId};
use pf_client_core::ring::{RingCommand, RingFacts, RingInput};
use pf_client_core::shortcuts::{resolve, Action, Client, Device};
use pf_console_ui::theme;
use punktfunk_core::client::{NativeClient, PunktfunkEndReason};
use punktfunk_core::hud::StatsVerbosity;
use punktfunk_core::input::PadMouseMode;
use punktfunk_core::quic::v2::hello::{RoleEcho, ScreenEcho, ScreenWish};

use super::connect::{self, ConnectOutcome};
use super::hud::StatsOverlay;
use super::motion::MotionRouter;
use super::pads::{key_event, resolve_kind, PadRouter, Routed, SLOTS};
use super::testrun::Scenario;
use super::touch::{Screen, TouchRouter};
use super::{App, StreamOutcome};
use crate::core::model::GamepadType;
use crate::core::settings::NxSettings;
use crate::platform::{Fit, PadFeedback, PadSample, Pads, SystemEvent, TouchPoint};
use crate::session::{Connected, InputSender};

/// Input thread cadence: 250 Hz, under the pad's own report rate.
const INPUT_PERIOD: Duration = Duration::from_millis(4);
/// Feedback events handled per input tick, so a flood cannot starve the pads.
const FEEDBACK_BUDGET: usize = 32;
/// Redials after a lost link, and how long each may take.
const RECONNECTS: u32 = 3;
/// How long the stream-start notice shows: the console's toast time.
const NOTICE_FOR: Duration = Duration::from_secs(4);

/// What the main thread tells the input thread.
enum ToInput {
    DialOpened,
    DialClosed,
    ReleaseAll,
    /// The output's new size, after a dock change.
    Output((u32, u32)),
    /// The dial's Controller type: every pad plugs in again as this kind.
    PadType(GamepadType),
}

/// The input thread and its channels. Dropping it without [`Self::stop`] leaks the pads.
struct InputThread {
    stop: Arc<AtomicBool>,
    to: Sender<ToInput>,
    from: Receiver<Routed>,
    handle: std::thread::JoinHandle<Box<dyn Pads>>,
}

impl InputThread {
    fn start(
        pads: Box<dyn Pads>,
        router: PadRouter,
        input: InputSender,
        kind: GamepadType,
        output: (u32, u32),
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (to, rx) = mpsc::channel();
        let (tx, from) = mpsc::channel();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("pf-input".into())
            .spawn(move || input_loop(pads, router, &input, kind, output, &thread_stop, &rx, &tx))
            .expect("spawn the input thread");
        Self { stop, to, from, handle }
    }

    /// Stops the thread, which lets go of everything, and takes the pads back.
    fn stop(self) -> Option<Box<dyn Pads>> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().ok()
    }
}

#[allow(clippy::too_many_arguments)]
fn input_loop(
    mut pads: Box<dyn Pads>,
    mut router: PadRouter,
    input: &InputSender,
    mut kind: GamepadType,
    mut output: (u32, u32),
    stop: &AtomicBool,
    rx: &Receiver<ToInput>,
    tx: &Sender<Routed>,
) -> Box<dyn Pads> {
    let mut motion = MotionRouter::default();
    let mut touch = TouchRouter::default();
    let mut batch: Vec<PadSample> = Vec::new();
    let mut fingers: Vec<TouchPoint> = Vec::new();
    let mut wire = Vec::new();
    let mut rich = Vec::new();
    let mut routed = Vec::new();
    let mut dial_open = false;
    // Pads whose motion the host's pad cannot take, logged once each.
    let mut no_motion = [false; SLOTS];
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        wire.clear();
        rich.clear();
        routed.clear();
        for msg in rx.try_iter() {
            match msg {
                ToInput::DialOpened => {
                    dial_open = true;
                    router.dial_opened(&mut wire);
                    touch.release_all(&mut wire);
                    motion.still_all(&mut rich);
                }
                ToInput::DialClosed => {
                    dial_open = false;
                    router.dial_closed();
                }
                ToInput::ReleaseAll => {
                    router.release_all(&mut wire);
                    touch.release_all(&mut wire);
                    motion.still_all(&mut rich);
                }
                ToInput::Output(size) => output = size,
                ToInput::PadType(next) => {
                    kind = next;
                    router.replug_all(kind, &mut wire);
                    motion.still_all(&mut rich);
                }
            }
        }
        batch.clear();
        pads.poll(&mut batch);
        router.step(&batch, Instant::now(), dial_open, kind, &mut wire, &mut routed);
        if !dial_open {
            let client = input.client();
            for pad in 0..SLOTS as u8 {
                let Some(sample) = batch.iter().find(|s| s.slot == pad) else {
                    motion.step(pad, None);
                    continue;
                };
                if !motion_reaches(kind, sample, client) {
                    if !std::mem::replace(&mut no_motion[usize::from(pad)], true) {
                        tracing::info!("pad {pad}: the host's pad takes no motion; the gyro stays home");
                    }
                    continue;
                }
                rich.extend(motion.step(pad, pads.motion(pad)));
            }
            pads.touches(&mut fingers);
            let mode = client.mode();
            let screen = Screen {
                output,
                frame: (mode.width, mode.height),
            };
            touch.step(&fingers, screen, &mut wire);
        }
        for ev in &wire {
            input.send(ev);
        }
        for r in rich.drain(..) {
            let _ = input.client().send_rich_input(r);
        }
        for r in routed.drain(..) {
            let _ = tx.send(r);
        }
        apply_feedback(pads.as_mut(), input);
        next += INPUT_PERIOD;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    wire.clear();
    router.release_all(&mut wire);
    touch.release_all(&mut wire);
    for ev in &wire {
        input.send(ev);
    }
    motion.still_all(&mut rich);
    for r in rich.drain(..) {
        let _ = input.client().send_rich_input(r);
    }
    input.release();
    pads.release_all();
    pads
}

/// Whether the host's pad for `sample` takes motion. A pad that declared no kind of its own
/// (`Auto`) is the session's pad.
fn motion_reaches(kind: GamepadType, sample: &PadSample, client: &punktfunk_core::client::NativeClient) -> bool {
    use punktfunk_core::config::{pad_motion_reaches, GamepadPref};
    let declared = match resolve_kind(kind, sample.kind).to_core() {
        GamepadPref::Auto => client.requested_gamepad,
        pref => pref,
    };
    pad_motion_reaches(declared, client.requested_gamepad, client.resolved_gamepad)
}

/// The host's rumble, onto the pads. The plane drains every tick: a bounded plane left unread
/// drops its newest events, including the zero that stops a motor.
fn apply_feedback(pads: &mut dyn Pads, input: &InputSender) {
    let client = input.client();
    for _ in 0..FEEDBACK_BUDGET {
        let Ok(cmd) = client.next_rumble_command(Duration::ZERO) else {
            break;
        };
        let Ok(slot) = u8::try_from(cmd.pad) else { continue };
        pads.feedback(
            slot,
            &PadFeedback::Rumble {
                low: cmd.low,
                high: cmd.high,
            },
        );
    }
}

/// What the main loop decided this frame.
enum Next {
    End(StreamOutcome),
    /// The link was lost: dial again.
    Redial,
}

/// Runs one launch until it ends, redialling a lost link.
pub(super) fn run(app: &mut App, launch: ConnectOutcome) -> StreamOutcome {
    let ConnectOutcome {
        connected,
        target,
        settings,
        exit_plan,
    } = launch;
    let mut connected = match connected {
        Ok(c) => c,
        Err(e) => {
            let why = crate::core::errors::friendly(&e);
            tracing::warn!("connect failed: {e:#}");
            return end_test_run(app, false, &why).unwrap_or(StreamOutcome::ReturnToMenu(Some(why)));
        }
    };
    let mut redials = 0;
    loop {
        match stream(app, &connected, &settings, &target) {
            Next::End(outcome) => {
                super::teardown(connected);
                return match outcome {
                    StreamOutcome::Quit(_) => StreamOutcome::Quit(exit_plan),
                    other => other,
                };
            }
            Next::Redial if redials < RECONNECTS => {
                redials += 1;
                super::teardown(connected);
                tracing::info!("link lost — redial {redials} of {RECONNECTS}");
                match redial(app, &target, &settings) {
                    Some(c) => connected = c,
                    None => return StreamOutcome::ReturnToMenu(Some(pf_l10n::client::ended_lost().into())),
                }
            }
            Next::Redial => {
                let why = connected.end_message();
                super::teardown(connected);
                return StreamOutcome::ReturnToMenu(Some(why));
            }
        }
    }
}

/// Dials the same target again, drawing a plain frame while it waits.
fn redial(
    app: &mut App,
    target: &crate::core::model::ConnectTarget,
    settings: &crate::services::store::Settings,
) -> Option<Connected> {
    let native = super::menu::native_mode(app);
    let pending = connect::spawn(
        &app.media,
        app.identity.clone(),
        target.clone(),
        settings.clone(),
        native,
    )
    .ok()?;
    app.display.set_stream(false, Fit::Contain);
    while !pending.is_finished() {
        let _ = app.display.frame(&mut |_| {});
    }
    match pending.join() {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => {
            tracing::warn!("redial: {e:#}");
            None
        }
        Err(_) => None,
    }
}

/// One session, until it ends or its link is lost.
fn stream(
    app: &mut App,
    connected: &Connected,
    settings: &crate::services::store::Settings,
    target: &crate::core::model::ConnectTarget,
) -> Next {
    let Some(pads) = app.pads.take() else {
        tracing::error!("stream: the pads are still held by another loop");
        return Next::End(StreamOutcome::ReturnToMenu(None));
    };
    // The dial's Controller type steps it for this stream; the next connect reads the setting.
    let mut pad_type = settings.gamepad_type();
    let mut plugged = pad_type;
    let shortcuts = resolve(&settings.shortcuts, Client::Switch, false, "");
    let hint = |action| {
        shortcuts
            .hint_for(&action, Device::Pad, Client::Switch)
            .unwrap_or_default()
    };
    tracing::info!(
        dial = %hint(Action::Ring),
        end = %hint(Action::Slot(SlotId::EndStream)),
        "pad shortcuts"
    );
    let router = PadRouter::new(
        &shortcuts,
        &OverlayConfig::parse(&settings.overlay_actions, RingPlatform::Desktop),
    );
    let input = InputThread::start(pads, router, connected.input(), pad_type, app.display.size());
    let fonts = match pf_console_ui::theme::build_fonts_with(app.lifecycle.fallback_fonts()) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("overlay fonts: {e:#}");
            None
        }
    };
    let ui = theme::Frame::new(
        theme::Ink::of(pf_console_ui::palette::palette(&settings.ui_palette)),
        settings.reduce_motion,
        true,
        0.0,
    );
    let mut ring = pf_console_ui::Ring::new();
    let mode = connected.client().mode();
    let (output_w, output_h) = app.display.size();
    // The output size the overlay and the stream mode were last fitted to.
    let mut fitted = (output_w, output_h);
    let (mut width, mut height) = app.display.ui_size();
    let mut facts = RingFacts {
        overlay_actions: settings.overlay_actions.clone(),
        // The screen passes touches straight through; the trackpad model is not here yet.
        touch_mode: "touch".into(),
        host_accepts_touch: true,
        stats_tier: settings.stats_verbosity().label().into(),
        mode: (mode.width, mode.height, mode.refresh_hz),
        native_mode: (output_w, output_h, app.display.refresh().hz()),
        addr: target.host.clone(),
        fp_hex: fp_hex(&target.fingerprint),
        pad_type: settings.gamepad_type().to_core(),
        ..RingFacts::default()
    };
    ring.set_facts(&facts);
    // A test run records everything; otherwise the player's stats setting.
    let initial_tier = if app.test_run.is_some() {
        StatsVerbosity::Detailed
    } else {
        settings.stats_verbosity()
    };
    let mut hud = StatsOverlay::new(initial_tier, settings.advanced_stats);
    let client = connected.client();
    let mut notice = start_line(
        client.screen() == Some(&ScreenEcho::Shared),
        ScreenWish::parse(&settings.host_screen).as_ref(),
        client.role(),
    );
    if let Some(line) = &notice {
        tracing::info!("stream: start notice: {line}");
    }
    connected.set_hud_enabled(true);
    let decoder = app.media.video.lane_name(connected.client().codec);
    let mut scale = overlay_scale(height);
    app.display.set_stream(true, Fit::Contain);

    let started = Instant::now();
    let mut last_elapsed = Duration::ZERO;
    let mut dial_was_open = false;
    let mut perf = StreamPerf::default();
    let mut sockets = None;
    let mut last_frame = Instant::now();
    let mut shown_holding = false;

    let next = loop {
        let events = app.system_events();
        if events.contains(&SystemEvent::FocusLost) {
            let _ = input.to.send(ToInput::ReleaseAll);
        }
        if events.contains(&SystemEvent::Suspend) {
            tracing::info!("sleep: ending the stream");
            break Next::End(StreamOutcome::ReturnToMenu(None));
        }
        // The display resizes inside the frame after a dock change, so the event itself comes a
        // frame early: the size is followed instead (the 2026-10-31 dock log asked for the
        // previous mode at each change).
        if app.display.size() != fitted {
            fitted = app.display.size();
            (width, height) = app.display.ui_size();
            scale = overlay_scale(height);
            let _ = input.to.send(ToInput::Output(fitted));
            follow_output(app, connected, settings);
        }
        let fired = take_routed(&input, &mut ring, width, height);
        // What the dial shows follows what its slots changed.
        let tier = hud.tier.label();
        let kind = pad_type.to_core();
        if follow_pad_mouse(connected.client(), &mut facts) || facts.stats_tier != tier || facts.pad_type != kind {
            facts.stats_tier = tier.into();
            facts.pad_type = kind;
            ring.set_facts(&facts);
        }
        ring.tick();
        let mut keyboard = false;
        if let Some(n) = take_ring_commands(
            &mut ring,
            fired,
            &mut hud,
            connected.client(),
            &mut pad_type,
            &mut keyboard,
        ) {
            break n;
        }
        for cmd in ring.take_cmds() {
            tracing::info!("dial: no host action {cmd:?} on this console");
        }
        let open = ring.open();
        if open != dial_was_open {
            dial_was_open = open;
            // The host re-plugs the pad once, for the type the dial closed on, not at every step.
            if !open && pad_type != plugged {
                plugged = pad_type;
                tracing::info!("dial: controller type {:?} for this stream", pad_type);
                let _ = input.to.send(ToInput::PadType(pad_type));
            }
            let _ = input
                .to
                .send(if open { ToInput::DialOpened } else { ToInput::DialClosed });
        }
        // The dial closed itself before asking: its own close went out just above.
        if keyboard {
            type_text(app, connected.client(), &input);
        }
        if connected.is_session_ended() {
            tracing::info!("session ended: {:?}", connected.end_reason());
            break if connected.end_reason() == PunktfunkEndReason::Lost {
                Next::Redial
            } else {
                Next::End(StreamOutcome::ReturnToMenu(Some(connected.end_message())))
            };
        }

        let due = hud.due();
        if let Some(core) = hud.refresh(connected, decoder) {
            tracing::info!("stats: {} | {core}{}", perf.take_window(), app.clocks_suffix());
            if let Some(line) = app.socket_window(&mut sockets) {
                tracing::info!("{line}");
            }
        }
        // The frame count steps with the overlay's window; the keyframe hold shows when it flips.
        let holding = connected.stats.holding.load(Ordering::Relaxed);
        if due || holding != shown_holding {
            shown_holding = holding;
            let mut present = pf_l10n::console::console_overlay_present_frames(perf.frames, perf.missed);
            if holding {
                present.push_str(" · ");
                present.push_str(pf_l10n::console::console_overlay_holding_keyframe());
            }
            hud.set_local(vec![present]);
        }
        let dt = last_frame.elapsed().as_secs_f64();
        last_frame = Instant::now();
        if started.elapsed() >= NOTICE_FOR {
            notice = None;
        }
        // Stats off, no notice and the dial out of sight draw nothing: the layer is skipped.
        app.display
            .set_ui_layer(hud.tier != StatsVerbosity::Off || notice.is_some() || ring.damage() != 0);
        let result = app.display.frame(&mut |canvas| {
            if let Some(fonts) = &fonts {
                hud.draw(canvas, fonts, scale);
                if let Some(text) = &notice {
                    super::hud::draw_notice(canvas, fonts, text, (width, height), scale);
                }
                ring.render(canvas, &ui, width, height, scale, fonts, dt);
            }
        });
        match result {
            Ok(report) => perf.note(&report),
            Err(e) => {
                tracing::error!("present: {e:#}");
                break Next::End(StreamOutcome::ReturnToMenu(Some(
                    pf_l10n::client::ended_unexpected().into(),
                )));
            }
        }

        let elapsed = started.elapsed();
        if let Some(n) = test_run_step(app, last_elapsed, elapsed, &perf, connected) {
            break n;
        }
        last_elapsed = elapsed;
    };

    app.display.set_stream(false, Fit::Contain);
    connected.set_hud_enabled(false);
    app.pads = input.stop();
    if app.pads.is_none() {
        tracing::error!("the input thread panicked; pads are gone until relaunch");
    }
    next
}

/// The one line a session owes about its screen at start: that it is another device's, else
/// why it is not the one the preset asked for. One, because one notice shows at a time.
fn start_line(shared: bool, wish: Option<&ScreenWish>, echo: Option<RoleEcho>) -> Option<String> {
    if shared {
        return Some(pf_l10n::client::notice_shared_screen().into());
    }
    pf_client_core::host_text::screen_wish_line(wish, echo?.reason)
}

/// The overlay's size for a UI canvas `height` pixels tall.
fn overlay_scale(height: u32) -> f32 {
    (height as f32 / 800.0).clamp(0.75, 3.0)
}

/// Docked or undocked: a session on "Native" asks the host for the display's new mode, without a
/// redial. A refused ask streams on at the old size, scaled by the presenter.
fn follow_output(app: &App, connected: &Connected, settings: &crate::services::store::Settings) {
    if !connect::follows_output(settings) {
        return;
    }
    let want = connect::stream_mode(settings, super::menu::native_mode(app));
    let client = connected.client();
    if want == client.mode() {
        return;
    }
    tracing::info!(
        "operation mode: asking for {}x{}@{}",
        want.width,
        want.height,
        want.refresh_hz
    );
    if let Err(e) = client.request_mode(want) {
        tracing::warn!("operation mode: the mode request did not go out: {e:#}");
    }
}

/// The input thread's hand-offs: the dial's presses, and the quick actions the shortcuts fired.
fn take_routed(input: &InputThread, ring: &mut pf_console_ui::Ring, width: u32, height: u32) -> Vec<RingCommand> {
    let mut fired = Vec::new();
    for routed in input.from.try_iter() {
        match routed {
            Routed::ToggleDial => ring.input(RingInput::Toggle {
                x: width as f32 / 2.0,
                y: height as f32 / 2.0,
            }),
            Routed::Dial(ev) => {
                ring.menu(ev);
            }
            Routed::Command(cmd) => {
                tracing::info!(?cmd, "shortcut fired");
                fired.push(cmd);
            }
        }
    }
    fired
}

/// Runs what the shortcuts `fired`, then what the dial sent.
fn take_ring_commands(
    ring: &mut pf_console_ui::Ring,
    fired: Vec<RingCommand>,
    hud: &mut StatsOverlay,
    client: &NativeClient,
    pad_type: &mut GamepadType,
    keyboard: &mut bool,
) -> Option<Next> {
    for cmd in fired.into_iter().chain(std::iter::from_fn(|| ring.take_command())) {
        match cmd {
            RingCommand::EndStream | RingCommand::DisconnectLinger => {
                return Some(Next::End(StreamOutcome::ReturnToMenu(None)));
            }
            RingCommand::CycleStats => hud.cycle(),
            RingCommand::CyclePadMouse => toggle_pad_mouse(client),
            // Only the slot steps here; the pad is re-plugged when the dial closes.
            RingCommand::CyclePadType => *pad_type = pad_type.next_on_dial(),
            // Touch is the screen's one model so far: nothing to step to.
            RingCommand::CycleTouchMode => {}
            // Shown once the dial's close has gone out to the input thread.
            RingCommand::Keyboard => *keyboard = true,
            RingCommand::Shortcut(keys) => send_chord(client, &keys),
            other => tracing::info!("dial: {other:?} does nothing on this console"),
        }
    }
    None
}

/// A dial shortcut, typed on the host: every key down in order, then up in reverse, as the
/// desktop client does. A name this build does not know fires nothing.
fn send_chord(client: &NativeClient, keys: &[String]) {
    use punktfunk_core::input::{key_vk, InputKind};
    let Some(vks) = keys.iter().map(|k| key_vk(k)).collect::<Option<Vec<u8>>>() else {
        tracing::info!("dial: shortcut {keys:?} names a key this build does not know");
        return;
    };
    for &vk in &vks {
        let _ = client.send_input(&key_event(InputKind::KeyDown, vk));
    }
    for &vk in vks.iter().rev() {
        let _ = client.send_input(&key_event(InputKind::KeyUp, vk));
    }
    tracing::info!("dial: shortcut {} sent", keys.join("+"));
}

/// The dial's Keyboard: the system keyboard over the stream, then its text typed on the host as
/// committed characters. The main thread waits on the keyboard, so no frame is presented
/// meanwhile; the stream and the audio go on.
fn type_text(app: &mut App, client: &NativeClient, input: &InputThread) {
    use punktfunk_core::input::{InputEvent, InputKind};
    use punktfunk_core::quic::{GRANT_KEYBOARD, HOST_CAP_TEXT_INPUT};
    if client.host_caps() & HOST_CAP_TEXT_INPUT == 0 {
        tracing::info!("keyboard: this host cannot type text");
        return;
    }
    if client.access_grants() & GRANT_KEYBOARD == 0 {
        tracing::info!("keyboard: this host does not grant the keyboard");
        return;
    }
    // The pads stay off the wire while the keyboard has them, as under the dial.
    let _ = input.to.send(ToInput::DialOpened);
    let text = app.lifecycle.keyboard();
    let _ = input.to.send(ToInput::DialClosed);
    // What the pads did meanwhile was typing, not the dial's.
    while input.from.try_recv().is_ok() {}
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return;
    };
    for c in text.chars() {
        let _ = client.send_input(&InputEvent {
            kind: InputKind::TextInput,
            _pad: [0; 3],
            code: u32::from(c),
            x: 0,
            y: 0,
            flags: 0,
        });
    }
    tracing::info!("keyboard: typed {} characters on the host", text.chars().count());
}

/// Copies the controller-mouse facts the dial shows from the session; whether they changed.
fn follow_pad_mouse(client: &NativeClient, facts: &mut RingFacts) -> bool {
    let target = client.live_pads();
    let now = (
        client.access_grants() & punktfunk_core::quic::GRANT_POINTER != 0,
        target,
        client.pad_mouse_mode(target),
    );
    if (facts.pointer_granted, facts.pad_mouse_target, facts.pad_mouse) == now {
        return false;
    }
    (facts.pointer_granted, facts.pad_mouse_target, facts.pad_mouse) = now;
    true
}

/// Controller mouse on every pad, or off. The dial's touchpad step is skipped: no Switch pad
/// has a touchpad to drive the pointer with.
fn toggle_pad_mouse(client: &NativeClient) {
    let pads = client.live_pads();
    let on = client.pad_mouse_mode(pads) != PadMouseMode::Full;
    match client.set_pad_mouse(if on { pads } else { 0 }) {
        Ok(()) => tracing::info!("dial: controller mouse {}", if on { "on" } else { "off" }),
        Err(e) => tracing::warn!("dial: controller mouse: {e}"),
    }
}

/// What the presenter reported over the session, for a test run's `perf.json`, and over the
/// current window, for the `stats:` log line.
#[derive(Default)]
struct StreamPerf {
    frames: u64,
    missed: u64,
    pictures: u64,
    replaced: u64,
    overflow: u64,
    decode_to_present_us: Vec<u32>,
    window: PerfWindow,
}

#[derive(Default)]
struct PerfWindow {
    since: Option<Instant>,
    frames: u32,
    missed: u32,
    pictures: u32,
    replaced: u32,
    overflow: u32,
    decode_to_present_us: Vec<u32>,
    /// The composition's GPU time per frame, where the presenter can time it.
    gpu_us: Vec<u32>,
}

impl StreamPerf {
    fn note(&mut self, report: &crate::platform::FrameReport) {
        self.frames += 1;
        self.missed += u64::from(report.missed);
        self.pictures += u64::from(report.new_picture);
        self.replaced += u64::from(report.replaced);
        self.overflow += u64::from(report.overflow);
        let w = &mut self.window;
        w.since.get_or_insert_with(Instant::now);
        w.frames += 1;
        w.missed += u32::from(report.missed);
        w.pictures += u32::from(report.new_picture);
        w.replaced += report.replaced;
        w.overflow += report.overflow;
        if let Some(us) = report.decode_to_present_us {
            if self.decode_to_present_us.len() < 1 << 16 {
                self.decode_to_present_us.push(us);
            }
            w.decode_to_present_us.push(us);
        }
        if let Some(us) = report.gpu_us {
            w.gpu_us.push(us);
        }
    }

    /// The presenter's figures since the last call, and a new window.
    fn take_window(&mut self) -> String {
        let mut w = std::mem::take(&mut self.window);
        let secs = w.since.map_or(0.0, |t| t.elapsed().as_secs_f64());
        w.decode_to_present_us.sort_unstable();
        w.gpu_us.sort_unstable();
        let ms = |us: Option<&u32>| us.map_or("-".to_string(), |&us| format!("{:.1}", f64::from(us) / 1000.0));
        let fine = |us: Option<&u32>| us.map_or("-".to_string(), |&us| format!("{:.2}", f64::from(us) / 1000.0));
        let d = &w.decode_to_present_us;
        let g = &w.gpu_us;
        format!(
            "present {secs:.1}s: {} frames, {} missed vblanks, {} pictures, {} unseen ({} replaced, {} queue full), decode to present p50 {} p95 {} max {} ms, compose on the gpu p50 {} p95 {} max {} ms",
            w.frames,
            w.missed,
            w.pictures,
            w.replaced + w.overflow,
            w.replaced,
            w.overflow,
            ms(d.get(d.len() / 2)),
            ms(d.get(d.len() * 95 / 100)),
            ms(d.last()),
            fine(g.get(g.len() / 2)),
            fine(g.get(g.len() * 95 / 100)),
            fine(g.last()),
        )
    }

    fn json(&self, connected: &Connected) -> serde_json::Value {
        let mut sorted = self.decode_to_present_us.clone();
        sorted.sort_unstable();
        let pct = |p: usize| sorted.get(sorted.len() * p / 100).copied();
        serde_json::json!({
            "frames": self.frames,
            "missed_vblanks": self.missed,
            "new_pictures": self.pictures,
            "unseen_pictures": { "replaced": self.replaced, "queue_full": self.overflow },
            "decode_to_present_us": { "p50": pct(50), "p95": pct(95), "p99": pct(99), "n": sorted.len() },
            "frames_dropped": connected.client().frames_dropped(),
            "rtt_us": connected.client().rtt_us(),
        })
    }
}

/// The stream scenario: captures on schedule, then the verdict and the exit.
fn test_run_step(
    app: &mut App,
    before: Duration,
    after: Duration,
    perf: &StreamPerf,
    connected: &Connected,
) -> Option<Next> {
    let run = app.test_run.as_ref().filter(|r| r.scenario == Scenario::Stream)?;
    if let Some(second) = run.capture_due(before, after) {
        match app.display.read_back() {
            Ok(Some(frame)) => {
                if let Err(e) = run.capture(&format!("{second}s"), frame) {
                    tracing::warn!("test run: {e:#}");
                }
            }
            Ok(None) => tracing::info!("test run: no read-back at {second}s — this output has no 8-bit pixels"),
            Err(e) => tracing::warn!("test run: read-back at {second}s: {e:#}"),
        }
    }
    if after < Duration::from_secs(run.seconds) {
        return None;
    }
    let run = app.test_run.take()?;
    if let Err(e) = run.write_json("perf.json", &perf.json(connected)) {
        tracing::warn!("test run: perf.json: {e:#}");
    }
    let ok = perf.pictures > 0;
    run.finish(app, ok, &format!("{} pictures over {}s", perf.pictures, run.seconds));
    Some(Next::End(StreamOutcome::Quit(None)))
}

/// A failed connect in a stream scenario ends the run with its reason.
fn end_test_run(app: &mut App, ok: bool, detail: &str) -> Option<StreamOutcome> {
    let run = app.test_run.take().filter(|r| r.scenario == Scenario::Stream)?;
    run.finish(app, ok, detail);
    Some(StreamOutcome::Quit(None))
}

fn fp_hex(fp: &[u8; 32]) -> String {
    use std::fmt::Write;
    fp.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use punktfunk_core::quic::v2::hello::{Role, RoleReason};

    /// A shared screen wins the one slot; a lost wish says why; a wish as asked says nothing.
    #[test]
    fn a_shared_screen_wins_the_start_line() {
        let wish = Some(ScreenWish::Second);
        let lost = Some(RoleEcho {
            role: Some(Role::Main),
            reason: RoleReason::Unavailable,
        });
        let shared = pf_l10n::client::notice_shared_screen();
        assert_eq!(start_line(true, wish.as_ref(), lost).as_deref(), Some(shared));
        let why = start_line(false, wish.as_ref(), lost);
        assert!(why.is_some());
        assert_eq!(
            why,
            pf_client_core::host_text::screen_wish_line(wish.as_ref(), RoleReason::Unavailable)
        );
        let kept = Some(RoleEcho {
            role: Some(Role::Second),
            reason: RoleReason::AsAsked,
        });
        assert_eq!(start_line(false, wish.as_ref(), kept), None);
        assert_eq!(start_line(false, wish.as_ref(), None), None);
    }
}
