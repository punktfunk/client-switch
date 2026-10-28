//! The BSD socket driver std's networking runs on, and nxlink's stdio. Externs from
//! `runtime/devices/socket.h` and `runtime/nxlink.h`, libnx 4.12.0.

/// `SocketInitConfig`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketInitConfig {
    pub tcp_tx_buf_size: u32,
    pub tcp_rx_buf_size: u32,
    pub tcp_tx_buf_max_size: u32,
    pub tcp_rx_buf_max_size: u32,
    pub udp_tx_buf_size: u32,
    pub udp_rx_buf_size: u32,
    pub sb_efficiency: u32,
    pub num_bsd_sessions: u32,
    /// `BsdServiceType`.
    pub bsd_service_type: u32,
}

/// `BsdServiceType_User`: bsd:u, libnx's default.
pub const BSD_SERVICE_TYPE_USER: u32 = 1;

/// The tunable half of [`SocketInitConfig`], in bytes. A `*_max` of 0 fixes that TCP buffer at
/// its initial size; `sb_efficiency` is buffers per socket, 1–8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub udp_rx_buf: u32,
    pub udp_tx_buf: u32,
    pub tcp_rx_buf: u32,
    pub tcp_tx_buf: u32,
    pub tcp_rx_max: u32,
    pub tcp_tx_max: u32,
    pub sb_efficiency: u32,
}

impl Default for Config {
    /// libnx's `g_defaultSocketInitConfig` (socket.c, 4.12.0).
    fn default() -> Self {
        Self {
            udp_rx_buf: 0xA500,
            udp_tx_buf: 0x2400,
            tcp_rx_buf: 0x10000,
            tcp_tx_buf: 0x8000,
            tcp_rx_max: 0x40000,
            tcp_tx_max: 0x40000,
            sb_efficiency: 4,
        }
    }
}

impl From<&Config> for SocketInitConfig {
    /// Three bsd:u sessions, as libnx's default.
    fn from(c: &Config) -> Self {
        Self {
            tcp_tx_buf_size: c.tcp_tx_buf,
            tcp_rx_buf_size: c.tcp_rx_buf,
            tcp_tx_buf_max_size: c.tcp_tx_max,
            tcp_rx_buf_max_size: c.tcp_rx_max,
            udp_tx_buf_size: c.udp_tx_buf,
            udp_rx_buf_size: c.udp_rx_buf,
            sb_efficiency: c.sb_efficiency,
            num_bsd_sessions: 3,
            bsd_service_type: BSD_SERVICE_TYPE_USER,
        }
    }
}

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        pub fn socketInitialize(config: *const super::SocketInitConfig) -> u32;
        pub fn socketExit();
        /// `nxlinkStdio` is `static inline` over this, with both flags set.
        pub fn nxlinkConnectToHost(redir_stdout: bool, redir_stderr: bool) -> i32;
    }
}

/// Starts the socket driver; the buffers are lent to the bsd service until [`exit`].
#[cfg(target_os = "horizon")]
pub fn init(config: &Config) -> anyhow::Result<()> {
    let raw = SocketInitConfig::from(config);
    // SAFETY: `raw` is a complete SocketInitConfig that libnx copies.
    let rc = unsafe { ffi::socketInitialize(&raw) };
    if rc != 0 {
        return Err(crate::nx_error("initialize sockets", rc));
    }
    Ok(())
}

#[cfg(target_os = "horizon")]
pub fn exit() {
    // SAFETY: no arguments; a no-op when the driver is not running.
    unsafe { ffi::socketExit() };
}

/// Sends stdout and stderr to the nxlink host; the socket fd, or `None` with no host.
#[cfg(target_os = "horizon")]
pub fn nxlink_stdio() -> Option<i32> {
    // SAFETY: plain flags; needs the socket driver, which `init` started.
    let fd = unsafe { ffi::nxlinkConnectToHost(true, true) };
    (fd >= 0).then_some(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_libnx_default() {
        let raw = SocketInitConfig::from(&Config::default());
        assert_eq!(raw.udp_rx_buf_size, 0xA500);
        assert_eq!(raw.tcp_tx_buf_max_size, 0x40000);
        assert_eq!((raw.num_bsd_sessions, raw.bsd_service_type), (3, BSD_SERVICE_TYPE_USER));
    }
}
