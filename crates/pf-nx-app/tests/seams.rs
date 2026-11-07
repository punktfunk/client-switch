//! The seams' contract, run against the fakes: what every platform implementation must do,
//! and the whole title driven end to end on them.

use std::sync::Arc;

use pf_nx_app::platform::fake::{FakeAudio, FakeDisplay, FakeLifecycle, FakePads, FakeStorage, FakeVideo};
use pf_nx_app::platform::{AudioOut, Display, Pads, Platform, Refresh, SocketCalls, Storage};

/// A display presents what the UI drew and reads it back at its own size.
#[test]
fn a_display_reads_back_what_was_drawn() {
    let mut display = FakeDisplay::new(64, 36);
    display
        .frame(&mut |canvas| {
            canvas.clear(skia_safe::Color::from_rgb(255, 0, 0));
        })
        .unwrap();
    let frame = display.read_back().unwrap().expect("a read-back");
    assert_eq!((frame.width, frame.height), (64, 36));
    assert_eq!(frame.pixels.len(), 64 * 36 * 4);
    assert_eq!(frame.pixels[..4], [255u8, 0, 0, 255]);
}

/// A port takes exactly one grain per write.
#[test]
fn an_audio_port_takes_whole_grains() {
    let audio = FakeAudio::default();
    let mut port = audio.open(2, 48_000).unwrap();
    let grain = vec![0.0; port.grain_frames() * 2];
    port.write(&grain).unwrap();
    assert!(port.write(&grain[..10]).is_err());
}

/// A pad held down stays reported while nothing changes.
#[test]
fn a_held_pad_stays_held() {
    let mut pads = FakePads::default();
    let held = pf_nx_app::platform::PadSample {
        buttons: 1,
        ..Default::default()
    };
    pads.script.lock().unwrap().push_back(vec![held]);
    let mut out = Vec::new();
    pads.poll(&mut out);
    pads.poll(&mut out);
    assert_eq!(out, vec![held, held]);
}

#[test]
fn refresh_converts_to_hz_and_period() {
    assert_eq!(Refresh(119_880).hz(), 120);
    assert_eq!(Refresh(59_940).period_ns(), 16_683_350);
}

/// The title on fakes: a menu test run draws the console, captures it, writes the verdict and
/// leaves through the one exit.
#[test]
fn a_menu_test_run_leaves_a_bundle_and_exits() {
    let storage = FakeStorage::temp("menu-run");
    std::fs::write(storage.data_dir().join("test-run.json"), r#"{"scenario":"menu"}"#).unwrap();
    let dir = storage.data_dir().to_path_buf();
    let title = std::thread::spawn(move || {
        pf_nx_app::run(Platform {
            display: Box::new(FakeDisplay::new(640, 360)),
            video: Arc::new(FakeVideo::default()),
            audio: Arc::new(FakeAudio::default()),
            pads: Box::new(FakePads::default()),
            storage: Arc::new(storage),
            lifecycle: Box::new(FakeLifecycle::default()),
            log: None,
            device: pf_nx_app::platform::DeviceInfo {
                platform: "fake",
                ..Default::default()
            },
        })
    });
    let exit = title.join().expect_err("the title only leaves through Lifecycle::exit");
    assert_eq!(exit.downcast_ref::<&str>(), Some(&"title exited"));
    assert!(!dir.join("test-run.json").exists(), "the scenario is consumed");
    let verdict_root = dir.join("verdict");
    let bundle = std::fs::read_dir(&verdict_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let verdict: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("verdict.json")).unwrap()).unwrap();
    assert_eq!(verdict["ok"], true, "{verdict}");
    assert!(bundle.join("frame-menu.png").exists());
    assert!(bundle.join("environment.json").exists());
}

/// A window's rates come from what changed since the last one.
#[test]
fn a_socket_window_reads_as_rates() {
    let before = SocketCalls {
        polls: 100,
        batch_reads: 50,
        batch_datagrams: 200,
        ..SocketCalls::default()
    };
    let now = SocketCalls {
        polls: 300,
        batch_reads: 150,
        batch_datagrams: 600,
        batch_read_ns: 20_000_000,
        sends: 40,
        ..before
    };
    let line = now.window(&before, 10.0);
    assert!(line.starts_with("socket calls 10.0s: 34/s"), "{line}");
    assert!(line.contains("recvmmsg 10 (4.0 datagrams, 200 us each)"), "{line}");
    assert!(
        line.ends_with("50 us a datagram, 0% of the window in recvmmsg"),
        "{line}"
    );
}
