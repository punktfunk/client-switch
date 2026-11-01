use std::{
    io::{self, IoSliceMut},
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

use super::{IO_ERROR_LOG_INTERVAL, RecvMeta, Transmit, UdpSockRef, log_sendmsg_error};

/// Fallback UDP socket interface that stubs out all special functionality
///
/// Used when a better implementation is not available for a particular target, at the cost of
/// reduced performance compared to that enabled by some target-specific interfaces.
#[derive(Debug)]
pub struct UdpSocketState {
    last_send_error: Mutex<Instant>,
    /// Horizon (punktfunk's leg): whether `recvmmsg` works here, [`BATCH_UNKNOWN`] until its
    /// first answer.
    batched: AtomicU8,
}

const BATCH_UNKNOWN: u8 = 0;
const BATCH_ON: u8 = 1;
const BATCH_OFF: u8 = 2;

impl UdpSocketState {
    pub fn new(socket: UdpSockRef<'_>) -> io::Result<Self> {
        socket.0.set_nonblocking(true)?;
        let now = Instant::now();
        Ok(Self {
            last_send_error: Mutex::new(now.checked_sub(2 * IO_ERROR_LOG_INTERVAL).unwrap_or(now)),
            batched: AtomicU8::new(BATCH_UNKNOWN),
        })
    }

    /// Sends a [`Transmit`] on the given socket.
    ///
    /// This function will only ever return errors of kind [`io::ErrorKind::WouldBlock`].
    /// All other errors will be logged and converted to `Ok`.
    ///
    /// UDP transmission errors are considered non-fatal because higher-level protocols must
    /// employ retransmits and timeouts anyway in order to deal with UDP's unreliable nature.
    /// Thus, logging is most likely the only thing you can do with these errors.
    ///
    /// If you would like to handle these errors yourself, use [`UdpSocketState::try_send`]
    /// instead.
    pub fn send(&self, socket: UdpSockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
        match send(socket, transmit) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(e),
            Err(e) => {
                log_sendmsg_error(&self.last_send_error, e, transmit);

                Ok(())
            }
        }
    }

    /// Sends a [`Transmit`] on the given socket without any additional error handling.
    pub fn try_send(&self, socket: UdpSockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
        send(socket, transmit)
    }

    /// Horizon (punktfunk's leg): up to [`BATCH_SIZE`] datagrams in one `recvmmsg`, one bsd
    /// call for the lot. A first refusal other than an empty socket turns batching off for
    /// this socket and reads one datagram a call, as before.
    pub fn recv(
        &self,
        socket: UdpSockRef<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> io::Result<usize> {
        #[cfg(target_os = "horizon")]
        if self.batched.load(Ordering::Relaxed) != BATCH_OFF {
            match recv_batch(&socket, bufs, meta) {
                Ok(n) => {
                    if self.batched.swap(BATCH_ON, Ordering::Relaxed) == BATCH_UNKNOWN {
                        crate::log::info!("recvmmsg: up to {BATCH_SIZE} datagrams a call, {n} in the first");
                    }
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
                Err(e) if self.batched.load(Ordering::Relaxed) == BATCH_ON => return Err(e),
                Err(e) => {
                    crate::log::warn!("recvmmsg refused ({e}); one datagram a call from here");
                    self.batched.store(BATCH_OFF, Ordering::Relaxed);
                }
            }
        }
        recv_one(&socket, bufs, meta)
    }

    #[inline]
    pub fn max_gso_segments(&self) -> usize {
        1
    }

    #[inline]
    pub fn gro_segments(&self) -> usize {
        1
    }

    /// Resize the send buffer of `socket` to `bytes`
    #[inline]
    pub fn set_send_buffer_size(&self, socket: UdpSockRef<'_>, bytes: usize) -> io::Result<()> {
        socket.0.set_send_buffer_size(bytes)
    }

    /// Resize the receive buffer of `socket` to `bytes`
    #[inline]
    pub fn set_recv_buffer_size(&self, socket: UdpSockRef<'_>, bytes: usize) -> io::Result<()> {
        socket.0.set_recv_buffer_size(bytes)
    }

    /// Get the size of the `socket` send buffer
    #[inline]
    pub fn send_buffer_size(&self, socket: UdpSockRef<'_>) -> io::Result<usize> {
        socket.0.send_buffer_size()
    }

    /// Get the size of the `socket` receive buffer
    #[inline]
    pub fn recv_buffer_size(&self, socket: UdpSockRef<'_>) -> io::Result<usize> {
        socket.0.recv_buffer_size()
    }

    #[inline]
    pub fn may_fragment(&self) -> bool {
        true
    }
}

fn send(socket: UdpSockRef<'_>, transmit: &Transmit<'_>) -> io::Result<()> {
    socket
        .0
        .send_to(
            transmit.contents,
            &socket2::SockAddr::from(transmit.destination),
        )
        .map(|_| ())
}

/// One datagram into the first buffer, which quinn sizes for a whole datagram: socket2 has no
/// vectored receive on Horizon.
fn recv_one(
    socket: &UdpSockRef<'_>,
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
) -> io::Result<usize> {
    // Safety: `u8` and `MaybeUninit<u8>` share a layout, and `recv_from` writes only
    // initialised bytes into the buffer.
    let buf = unsafe { &mut *(&mut *bufs[0] as *mut [u8] as *mut [std::mem::MaybeUninit<u8>]) };
    let (len, addr) = socket.0.recv_from(buf)?;
    meta[0] = RecvMeta {
        len,
        stride: len,
        addr: addr.as_socket().unwrap(),
        ecn: None,
        dst_ip: None,
    };
    Ok(1)
}

/// Up to [`BATCH_SIZE`] datagrams, one per buffer, through libnx's `recvmmsg` (bsd's
/// RecvMMsg, firmware 7.0.0). `MSG_DONTWAIT`: an empty socket is `WouldBlock`, and the call
/// returns what is queued rather than waiting for a full batch.
#[cfg(target_os = "horizon")]
fn recv_batch(
    socket: &UdpSockRef<'_>,
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;
    let n = bufs.len().min(meta.len()).min(BATCH_SIZE);
    // Safety: all-zero is a valid value of these plain C structs.
    let mut names: [libc::sockaddr_storage; BATCH_SIZE] = unsafe { std::mem::zeroed() };
    let mut iovs: [libc::iovec; BATCH_SIZE] = unsafe { std::mem::zeroed() };
    let mut hdrs: [libc::mmsghdr; BATCH_SIZE] = unsafe { std::mem::zeroed() };
    for i in 0..n {
        iovs[i] = libc::iovec {
            iov_base: bufs[i].as_mut_ptr().cast(),
            iov_len: bufs[i].len(),
        };
        let h = &mut hdrs[i].msg_hdr;
        h.msg_name = (&mut names[i] as *mut libc::sockaddr_storage).cast();
        h.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        h.msg_iov = &mut iovs[i];
        h.msg_iovlen = 1;
    }
    // Safety: the first `n` headers point at `n` live buffers, iovecs and address slots, all
    // outliving the call; bsd writes within their stated lengths.
    let got = unsafe {
        libc::recvmmsg(
            socket.0.as_raw_fd(),
            hdrs.as_mut_ptr(),
            n as libc::c_uint,
            libc::MSG_DONTWAIT,
            std::ptr::null_mut(),
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    let got = got as usize;
    for i in 0..got {
        let len = hdrs[i].msg_len as usize;
        let addr = socket_addr(&names[i]).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "recvmmsg: an address of no known family")
        })?;
        meta[i] = RecvMeta {
            len,
            stride: len,
            addr,
            ecn: None,
            dst_ip: None,
        };
    }
    Ok(got)
}

/// The sender's address as `recvmmsg` wrote it.
#[cfg(target_os = "horizon")]
fn socket_addr(s: &libc::sockaddr_storage) -> Option<std::net::SocketAddr> {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    match libc::c_int::from(s.ss_family) {
        libc::AF_INET => {
            // Safety: an AF_INET address is a `sockaddr_in`, which the storage outsizes.
            let a = unsafe { &*(s as *const libc::sockaddr_storage).cast::<libc::sockaddr_in>() };
            Some(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)),
                u16::from_be(a.sin_port),
            )))
        }
        libc::AF_INET6 => {
            // Safety: an AF_INET6 address is a `sockaddr_in6`, which the storage outsizes.
            let a = unsafe { &*(s as *const libc::sockaddr_storage).cast::<libc::sockaddr_in6>() };
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(a.sin6_addr.s6_addr),
                u16::from_be(a.sin6_port),
                a.sin6_flowinfo,
                a.sin6_scope_id,
            )))
        }
        _ => None,
    }
}

/// Datagrams one [`UdpSocketState::recv`] takes: one `recvmmsg` batch on Horizon.
#[cfg(target_os = "horizon")]
pub(crate) const BATCH_SIZE: usize = 32;
#[cfg(not(target_os = "horizon"))]
pub(crate) const BATCH_SIZE: usize = 1;
