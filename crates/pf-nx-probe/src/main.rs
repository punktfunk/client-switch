//! WP0's probe: one line per numbered check, `probe N <name> ok|fail <detail>`, on stdout
//! (nxlink, or the screen when launched from hbmenu) and in `probe.log` in the data folder.
//!
//! A check runs all its steps and lists each as `step=value` or `step=FAIL(why)`, so one run on
//! the console names every failure. `--panic` ends the run with a panic: the hook writes
//! `panic.txt`, and the next run prints it as `probe 1 panic-log`. Runs the same on a dev box.

#[cfg(target_os = "horizon")]
mod nx;
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
    out.check(3, "quic", quic_steps());
    #[cfg(all(target_os = "horizon", feature = "vulkan"))]
    out.check(4, "vulkan", vk::steps());

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

/// Core grows the receive buffer for 4K bursts; this reads back what the stack grants.
fn rcvbuf() -> Result<String, String> {
    const WANT: libc::c_int = 4 << 20;
    let socket = loopback()?;
    let fd = socket.as_raw_fd();
    let (want, len) = (WANT, std::mem::size_of::<libc::c_int>() as libc::socklen_t);
    // SAFETY: `want` is a readable int of `len` bytes.
    let set = unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, (&raw const want).cast(), len) };
    let set_err = std::io::Error::last_os_error();
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
    match (set, get) {
        (0, 0) if got >= WANT => Ok(format!("{got}")),
        (0, 0) => Err(format!("granted {got}")),
        _ => Err(format!("set {set} ({set_err}), get {get}, granted {got}")),
    }
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
/// has no `available_parallelism`), with a fresh identity through the crypto provider.
#[cfg(feature = "quic")]
fn quic_steps() -> Steps {
    use punktfunk_core::quic::{endpoint, v2::registry::ALPN};
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return vec![("runtime", Err(err(e)))],
    };
    let identity = endpoint::generate_identity().map_err(err);
    let bound = runtime.block_on(async {
        let (cert, key) = identity.as_ref().map_err(Clone::clone)?;
        let (shared, _observed) = endpoint::client_shared(None, Some((cert, key)), &[ALPN], None);
        let (ep, _media) = shared.map_err(err)?;
        ep.local_addr().map(|a| a.to_string()).map_err(err)
    });
    vec![
        ("runtime", Ok("2workers".into())),
        ("provider", Ok("aws-lc-rs".into())),
        ("identity", identity.map(|(cert, _)| format!("{}B", cert.len()))),
        ("endpoint", bound),
    ]
}
