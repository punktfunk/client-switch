//! WP0's probe: one line per numbered check, `probe N <name> ok|fail <detail>`, on stdout
//! (nxlink, or the screen when launched from hbmenu) and in `probe.log` in the data folder.
//!
//! A check runs all its steps and lists each as `step=value` or `step=FAIL(why)`, so one run on
//! the console names every failure. `--panic` ends the run with a panic: the hook writes
//! `panic.txt`, and the next run prints it as `probe 1 panic-log`. Runs the same on a dev box.

#[cfg(target_os = "horizon")]
mod nx;
#[cfg(all(target_os = "horizon", feature = "nvdec"))]
mod nvdec;
#[cfg(all(target_os = "horizon", feature = "vulkan"))]
mod vk;

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::{Ipv4Addr, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Steps = Vec<(&'static str, Result<String, String>)>;

fn main() {
    #[cfg(target_os = "horizon")]
    pf_nx_sys::rt::link_guard();
    #[cfg(target_os = "horizon")]
    let sockets = nx::sockets_up();
    #[cfg(target_os = "horizon")]
    nx::stdio_up();

    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Out::new(&dir);
    out.line(&format!(
        "probe 0 start pf-nx-probe {} on {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    ));
    #[cfg(target_os = "horizon")]
    out.line(&format!("probe 0 socket-init {sockets}"));

    let panic_file = dir.join("panic.txt");
    if let Ok(text) = std::fs::read_to_string(&panic_file) {
        out.line(&format!("probe 1 panic-log ok {}", text.replace('\n', " | ")));
        let _ = std::fs::remove_file(&panic_file);
    }
    std::panic::set_hook(Box::new(move |info| {
        let text = info.to_string();
        let wrote = std::fs::write(&panic_file, &text).is_ok();
        println!("probe 1 panic {} {text}", if wrote { "ok" } else { "fail" });
    }));

    out.check(1, "std", std_steps(&dir));
    out.check(2, "sockets", socket_steps());
    #[cfg(feature = "quic")]
    {
        let steps = quic_steps(&dir, &mut out);
        out.check(3, "quic", steps);
    }
    #[cfg(all(target_os = "horizon", feature = "vulkan"))]
    out.check(4, "vulkan", vk::steps());
    #[cfg(all(target_os = "horizon", feature = "nvdec"))]
    out.check(6, "nvdec", nvdec::steps(&dir));

    out.line(&format!("probe end {} failed", out.failed));
    if std::env::args().any(|a| a == "--panic") {
        out.line("probe 1 panic start");
        panic!("the probe's --panic check");
    }
    #[cfg(target_os = "horizon")]
    nx::finish();
}

fn data_dir() -> PathBuf {
    if cfg!(target_os = "horizon") {
        PathBuf::from("sdmc:/switch/punktfunk")
    } else {
        std::env::temp_dir().join("pf-nx-probe")
    }
}

/// Where a line goes: stdout and `probe.log`, synced per line so a crash keeps what ran.
struct Out {
    file: Option<File>,
    failed: u32,
}

impl Out {
    fn new(dir: &Path) -> Self {
        Self {
            file: File::create(dir.join("probe.log")).ok(),
            failed: 0,
        }
    }

    fn line(&mut self, text: &str) {
        println!("{text}");
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{text}").and_then(|()| file.sync_data());
        }
        #[cfg(target_os = "horizon")]
        nx::screen_flush();
    }

    fn check(&mut self, n: u32, name: &str, steps: Steps) {
        let ok = steps.iter().all(|(_, r)| r.is_ok());
        self.failed += u32::from(!ok);
        let detail: Vec<String> = steps
            .into_iter()
            .map(|(step, r)| match r {
                Ok(v) => format!("{step}={v}"),
                Err(e) => format!("{step}=FAIL({e})"),
            })
            .collect();
        self.line(&format!(
            "probe {n} {name} {} {}",
            if ok { "ok" } else { "fail" },
            detail.join(" ")
        ));
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn std_steps(dir: &Path) -> Steps {
    vec![
        ("threads", threads()),
        ("hashmap", hashmap()),
        ("instant", instant()),
        ("systime", systime()),
        ("file", file(dir)),
        ("read_dir", read_dir(dir)),
    ]
}

thread_local! {
    static SLOT: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// Four std threads, each finding its own fresh `thread_local!` and leaving the main thread's.
fn threads() -> Result<String, String> {
    SLOT.with(|s| s.set(99));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            std::thread::spawn(move || {
                let before = SLOT.with(Cell::get);
                SLOT.with(|s| s.set(i));
                std::thread::sleep(Duration::from_millis(5));
                (before, SLOT.with(Cell::get))
            })
        })
        .collect();
    for (i, handle) in handles.into_iter().enumerate() {
        let (before, after) = handle.join().map_err(|_| format!("thread {i} panicked"))?;
        if before != usize::MAX || after != i {
            return Err(format!("thread {i} read {before} then {after}"));
        }
    }
    match SLOT.with(Cell::get) {
        99 => Ok("4".into()),
        other => Err(format!("main thread's slot became {other}")),
    }
}

fn hashmap() -> Result<String, String> {
    let map: HashMap<u32, u32> = (0..1000).map(|i| (i, i * 3)).collect();
    match (0..1000).all(|i| map.get(&i) == Some(&(i * 3))) {
        true => Ok("1000".into()),
        false => Err("a lookup missed".into()),
    }
}

fn instant() -> Result<String, String> {
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(10));
    let slept = start.elapsed();
    if slept < Duration::from_millis(10) {
        return Err(format!("a 10 ms sleep took {slept:?}"));
    }
    Ok(format!("{}us", slept.as_micros()))
}

/// Later than 2025-01-01: a clock that reads 1970 means the time service never came up.
fn systime() -> Result<String, String> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_err(err)?.as_secs();
    if secs < 1_735_689_600 {
        return Err(format!("unix {secs}"));
    }
    Ok(format!("unix{secs}"))
}

fn file(dir: &Path) -> Result<String, String> {
    let path = dir.join("probe-file.txt");
    let body = b"punktfunk probe\n".repeat(64);
    std::fs::write(&path, &body).map_err(err)?;
    let back = std::fs::read(&path).map_err(err)?;
    if back != body {
        return Err(format!("read {} of {} bytes back changed", back.len(), body.len()));
    }
    Ok(format!("{}B", back.len()))
}

fn read_dir(dir: &Path) -> Result<String, String> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .map_err(err)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let _ = std::fs::remove_file(dir.join("probe-file.txt"));
    match names.iter().any(|n| n == "probe-file.txt") {
        true => Ok(format!("{}entries", names.len())),
        false => Err(format!("probe-file.txt not among {names:?}")),
    }
}

fn socket_steps() -> Steps {
    vec![
        ("udp", udp()),
        ("msg", sendmsg_recvmsg()),
        ("rcvbuf", rcvbuf()),
        ("mcast", multicast()),
        ("gai", getaddrinfo()),
    ]
}

fn loopback() -> Result<UdpSocket, String> {
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(err)?;
    socket.set_read_timeout(Some(Duration::from_secs(1))).map_err(err)?;
    Ok(socket)
}

fn udp() -> Result<String, String> {
    let socket = loopback()?;
    let addr = socket.local_addr().map_err(err)?;
    socket.send_to(b"ping", addr).map_err(err)?;
    let mut buf = [0u8; 16];
    let (n, from) = socket.recv_from(&mut buf).map_err(err)?;
    match &buf[..n] {
        b"ping" if from == addr => Ok(format!("{addr}")),
        got => Err(format!("got {got:?} from {from}")),
    }
}

/// quinn-udp's path: `sendmsg`/`recvmsg` on a socket connected to itself.
fn sendmsg_recvmsg() -> Result<String, String> {
    let socket = loopback()?;
    socket.connect(socket.local_addr().map_err(err)?).map_err(err)?;
    let fd = socket.as_raw_fd();
    let mut payload = *b"pong";
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // SAFETY: all-zero is a valid `msghdr`: no name, no control, no iovecs.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // SAFETY: `msg` points at one live iovec over `payload`; the socket is connected.
    let sent = unsafe { libc::sendmsg(fd, &msg, 0) };
    if sent != 4 {
        return Err(format!("sendmsg {sent}: {}", std::io::Error::last_os_error()));
    }
    let mut buf = [0u8; 16];
    iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    msg.msg_iov = &mut iov;
    // SAFETY: as above, the iovec now over `buf`, which is writable for its length.
    let got = unsafe { libc::recvmsg(fd, &mut msg, 0) };
    match usize::try_from(got) {
        Ok(n) if &buf[..n] == b"pong" => Ok("4B".into()),
        _ => Err(format!("recvmsg {got}: {}", std::io::Error::last_os_error())),
    }
}

/// Core grows the receive buffer for 4K bursts. Asks for 4 MiB, then halves down to 64 KiB, and
/// reports the first size the stack takes with what `getsockopt` reads back.
fn rcvbuf() -> Result<String, String> {
    const WANT: libc::c_int = 4 << 20;
    const FLOOR: libc::c_int = 64 << 10;
    let socket = loopback()?;
    let fd = socket.as_raw_fd();
    let mut refused = Vec::new();
    let mut ask = WANT;
    while ask >= FLOOR {
        match set_rcvbuf(fd, ask) {
            Ok(got) if ask == WANT => return Ok(format!("{got}")),
            Ok(got) => return Err(format!("max={ask} granted={got} refused[{}]", refused.join(","))),
            Err(e) => refused.push(format!("{ask}:{e}")),
        }
        ask /= 2;
    }
    Err(format!("none down to {FLOOR}: {}", refused.join(",")))
}

/// Sets `SO_RCVBUF` to `size` and reads back what the stack granted.
fn set_rcvbuf(fd: libc::c_int, size: libc::c_int) -> Result<libc::c_int, std::io::Error> {
    let len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `size` is a readable int of `len` bytes.
    let set = unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, (&raw const size).cast(), len) };
    if set != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (mut got, mut got_len) = (0 as libc::c_int, len);
    // SAFETY: `got` is a writable int and `got_len` holds its size.
    let get = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&raw mut got).cast(),
            &mut got_len,
        )
    };
    if get != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(got)
}

/// mDNS discovery joins 224.0.0.251.
fn multicast() -> Result<String, String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(err)?;
    socket
        .join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 251), &Ipv4Addr::UNSPECIFIED)
        .map_err(err)?;
    Ok("224.0.0.251".into())
}

fn getaddrinfo() -> Result<String, String> {
    let addrs: Vec<String> = ("localhost", 0)
        .to_socket_addrs()
        .map_err(err)?
        .map(|a| a.ip().to_string())
        .collect();
    match addrs.is_empty() {
        true => Err("no address for localhost".into()),
        false => Ok(addrs.join(",")),
    }
}

/// punktfunk-core's client endpoint on the shared socket, on a two-worker tokio runtime (Horizon
/// has no `available_parallelism`), with the probe's identity through the crypto provider; then,
/// when `probe-host.txt` names one, a real host: pair, connect, first video frame. Each step logs
/// before it starts, so a crash names the step it died in.
#[cfg(feature = "quic")]
fn quic_steps(dir: &Path, out: &mut Out) -> Steps {
    use punktfunk_core::quic::{endpoint, v2::registry::ALPN};
    punktfunk_core::tls::install_default_provider();
    out.line("probe 3 quic start runtime");
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return vec![("runtime", Err(err(e)))],
    };
    out.line("probe 3 quic start identity");
    let identity = probe_identity(dir);
    out.line("probe 3 quic start endpoint");
    let bound = runtime.block_on(async {
        let (cert, key) = identity.as_ref().map_err(Clone::clone)?;
        let (shared, _observed) = endpoint::client_shared(None, Some((cert, key)), &[ALPN], None);
        let (ep, _media) = shared.map_err(err)?;
        ep.local_addr().map(|a| a.to_string()).map_err(err)
    });
    drop(runtime);
    let mut steps = vec![
        ("runtime", Ok("2workers".into())),
        ("provider", Ok("aws-lc-rs".into())),
        (
            "identity",
            identity
                .as_ref()
                .map(|(cert, _)| format!("{}B", cert.len()))
                .map_err(Clone::clone),
        ),
        ("endpoint", bound),
    ];
    if let Ok(identity) = &identity {
        steps.extend(host_steps(dir, identity, out));
    }
    steps
}

/// The probe's client identity, kept in the data folder so a pairing outlives the run.
#[cfg(feature = "quic")]
fn probe_identity(dir: &Path) -> Result<(String, String), String> {
    let (cert_path, key_path) = (dir.join("probe-cert.pem"), dir.join("probe-key.pem"));
    if let (Ok(cert), Ok(key)) = (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
        return Ok((cert, key));
    }
    let (cert, key) = punktfunk_core::quic::endpoint::generate_identity().map_err(err)?;
    std::fs::write(&cert_path, &cert).map_err(err)?;
    std::fs::write(&key_path, &key).map_err(err)?;
    Ok((cert, key))
}

/// The host check 3 dials, from `probe-host.txt` in the data folder: `host=` (an IPv4 address),
/// optional `port=` (9777) and `pin=` (the PIN on the host's Pairing page, needed once).
#[cfg(feature = "quic")]
struct HostConfig {
    host: String,
    port: u16,
    pin: Option<String>,
}

#[cfg(feature = "quic")]
fn host_config(dir: &Path) -> Result<HostConfig, String> {
    let text = std::fs::read_to_string(dir.join("probe-host.txt")).map_err(|e| format!("no probe-host.txt ({e})"))?;
    let (mut host, mut port, mut pin) = (None, 9777, None);
    for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
        let value = value.trim();
        match key.trim() {
            "host" => host = Some(value.to_owned()),
            "port" => port = value.parse().map_err(|_| format!("port {value} is not a port"))?,
            "pin" if !value.is_empty() => pin = Some(value.to_owned()),
            _ => {}
        }
    }
    Ok(HostConfig {
        host: host.ok_or("probe-host.txt has no host= line")?,
        port,
        pin,
    })
}

/// Pairs once (the host's fingerprint is kept in `probe-host.fp`), connects at 720p60 offering
/// H.264 and HEVC, and waits for the first video frame: a frame out of `next_frame` was received,
/// reassembled and decrypted.
#[cfg(feature = "quic")]
fn host_steps(dir: &Path, identity: &(String, String), out: &mut Out) -> Steps {
    use punktfunk_core::client::{ConnectParams, NativeClient};
    use punktfunk_core::config::Mode;
    use punktfunk_core::{quic, PunktfunkError};

    let config = match host_config(dir) {
        Ok(config) => config,
        Err(e) => return vec![("host", Err(e))],
    };
    let mut steps: Steps = vec![("host", Ok(format!("{}:{}", config.host, config.port)))];
    let fp_path = dir.join("probe-host.fp");

    out.line("probe 3 quic start pair");
    let paired = match (read_fingerprint(&fp_path), &config.pin) {
        (Some(fp), _) => Ok((fp, "saved")),
        (None, None) => Err("not paired yet and probe-host.txt has no pin= line".to_owned()),
        (None, Some(pin)) => NativeClient::pair(
            &config.host,
            config.port,
            (&identity.0, &identity.1),
            pin,
            "Switch probe",
            Duration::from_secs(60),
        )
        .map_err(|e| e.to_string())
        .and_then(|fp| {
            std::fs::write(&fp_path, hex(&fp)).map_err(err)?;
            Ok((fp, "new"))
        }),
    };
    let fingerprint = match paired {
        Ok((fp, how)) => {
            steps.push(("pair", Ok(how.into())));
            fp
        }
        Err(e) => {
            steps.push(("pair", Err(e)));
            return steps;
        }
    };

    out.line("probe 3 quic start connect");
    let mode = Mode {
        width: 1280,
        height: 720,
        refresh_hz: 60,
    };
    let client = match NativeClient::connect(ConnectParams {
        bitrate_kbps: 10_000,
        video_caps: 0,
        video_codecs: quic::CODEC_H264 | quic::CODEC_HEVC,
        pin: Some(fingerprint),
        identity: Some(identity.clone()),
        ..ConnectParams::new(&config.host, config.port, mode, Duration::from_secs(20))
    }) {
        Ok(client) => client,
        Err(e) => {
            steps.push(("connect", Err(e.to_string())));
            return steps;
        }
    };
    steps.push(("connect", Ok(format!("codec={}", client.codec))));

    out.line("probe 3 quic start frame");
    // One keyframe request at 5 s: the run at 3a07655 waited 3 s for the first frame, the one at
    // f0b53c0 saw none in 15 s.
    let start = Instant::now();
    let mut keyframe = None;
    let frame = loop {
        match client.next_frame(Duration::from_millis(250)) {
            Ok(_) => {
                let kf = keyframe.map_or(String::new(), |r| format!(" keyframe={r}"));
                break Ok(format!("{}ms{kf}", start.elapsed().as_millis()));
            }
            Err(PunktfunkError::NoFrame) if start.elapsed() < Duration::from_secs(15) => {
                if keyframe.is_none() && start.elapsed() >= Duration::from_secs(5) {
                    keyframe = Some(match client.request_keyframe() {
                        Ok(()) => "asked@5s".to_owned(),
                        Err(e) => format!("FAIL({e})"),
                    });
                }
            }
            Err(e) => {
                let facts = session_facts(&client, keyframe.as_deref());
                break Err(format!("{e} after {}ms {facts}", start.elapsed().as_millis()));
            }
        }
    };
    steps.push(("frame", frame));
    client.disconnect_quit();
    steps
}

/// What the session knows when no frame came: whether it ended, the round trip, the data
/// socket's buffer and drops, and the link the host paces at.
#[cfg(feature = "quic")]
fn session_facts(client: &punktfunk_core::client::NativeClient, keyframe: Option<&str>) -> String {
    format!(
        "keyframe={} ended={} reason={:?} rtt={}us rcvbuf={}KiB os_drops={:?} dropped={} fec={} \
         link=\"{}\"",
        keyframe.unwrap_or("no"),
        client.is_session_ended(),
        client.end_reason(),
        client.rtt_us(),
        client.recv_buffer_kb(),
        client.socket_drops(),
        client.frames_dropped(),
        client.fec_recovered_shards(),
        client.link_line(),
    )
}

#[cfg(feature = "quic")]
fn read_fingerprint(path: &Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    let mut fp = [0u8; 32];
    if text.len() != 64 {
        return None;
    }
    for (i, byte) in fp.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(fp)
}

#[cfg(feature = "quic")]
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
