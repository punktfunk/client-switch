use std::io;
use std::net::UdpSocket;
use std::os::fd::{AsRawFd, RawFd};

/// Waker backed by a UDP socket connected to itself on loopback (punktfunk's Horizon leg).
///
/// libnx has no `pipe(2)` or `socketpair(2)`, but `poll(2)` on a socket works, so the
/// selector polls this socket for `POLLIN` and a wake is one datagram to it.
#[derive(Debug)]
pub(crate) struct Waker {
    socket: UdpSocket,
}

impl Waker {
    pub(crate) fn new_unregistered() -> io::Result<Waker> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.connect(socket.local_addr()?)?;
        socket.set_nonblocking(true)?;
        Ok(Waker { socket })
    }

    pub(crate) fn wake(&self) -> io::Result<()> {
        match self.socket.send(&[1]) {
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
        while matches!(self.socket.recv(&mut buf), Ok(n) if n > 0) {}
    }

    pub(crate) fn fd(&self) -> Option<RawFd> {
        Some(self.socket.as_raw_fd())
    }

    pub(crate) fn woken(&self) -> bool {
        false
    }
}
