use std::io;
use std::net::UdpSocket;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

/// Waker backed by a UDP socket connected to itself on loopback (punktfunk's Horizon leg).
///
/// libnx has no `pipe(2)` or `socketpair(2)`, but `poll(2)` on a socket works, so the
/// selector polls this socket for `POLLIN` and a wake is one datagram to it.
///
/// `PF_NX_LEAN_WAKER` in the environment saves socket calls, each one an IPC on the console: a
/// wake with one already on its way sends nothing, and the ack reads one datagram rather than
/// reading until the socket is empty.
#[derive(Debug)]
pub(crate) struct Waker {
    socket: UdpSocket,
    lean: bool,
    /// Lean only: a wake was sent and no ack has come.
    pending: AtomicBool,
}

impl Waker {
    pub(crate) fn new_unregistered() -> io::Result<Waker> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.connect(socket.local_addr()?)?;
        socket.set_nonblocking(true)?;
        Ok(Waker {
            socket,
            lean: std::env::var_os("PF_NX_LEAN_WAKER").is_some(),
            pending: AtomicBool::new(false),
        })
    }

    pub(crate) fn wake(&self) -> io::Result<()> {
        if self.lean && self.pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let sent = self.socket.send(&[1]);
        if sent.is_err() {
            self.pending.store(false, Ordering::Release);
        }
        match sent {
            Ok(_) => Ok(()),
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                // The receive buffer is full of wakes already: one more changes nothing.
                Ok(())
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => self.wake(),
            // After the console sleeps a send here fails, and tokio aborts the title on a failed
            // wake (Atmosphère report `01793482713_05446530aca7e000`). A dead waker wakes no one:
            // the driver runs again on its next event or timer.
            // TODO(verify): the error sleep leaves, and that the driver's poll survives it.
            Err(_) => Ok(()),
        }
    }

    pub(crate) fn ack_and_reset(&self) {
        let mut buf = [0; 64];
        if self.lean {
            // Cleared first: a wake from here on sends, and a datagram left behind only makes
            // the next poll return at once.
            self.pending.store(false, Ordering::Release);
            let _ = self.socket.recv(&mut buf);
            return;
        }
        while matches!(self.socket.recv(&mut buf), Ok(n) if n > 0) {}
    }

    pub(crate) fn fd(&self) -> Option<RawFd> {
        Some(self.socket.as_raw_fd())
    }

    pub(crate) fn woken(&self) -> bool {
        false
    }
}
