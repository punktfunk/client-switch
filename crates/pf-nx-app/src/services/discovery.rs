//! LAN discovery by mDNS legacy unicast (RFC 6762 §6.7): a PTR query from an ephemeral port,
//! which responders answer straight back to that port, so the title never binds UDP 5353 as a
//! full mDNS daemon must. The host's responder answers such queries.
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

/// mDNS service type punktfunk hosts advertise.
const SERVICE_TYPE: &str = "_punktfunk._udp.local.";

const MDNS_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);

#[derive(Clone, Debug)]
pub struct DiscoveredHost {
    pub name: String,
    pub addr: String,
    pub port: u16,
    /// Management API port from mDNS (None → `library::DEFAULT_MGMT_PORT`).
    pub mgmt_port: Option<u16>,
    /// Wake-on-LAN MACs from mDNS (learned while awake, persisted to `KnownHost`).
    pub mac: Vec<String>,
    /// Generic-to-specific OS identity chain, such as `linux/fedora/bazzite`.
    pub os: String,
    /// The advertised certificate fingerprint, lowercase hex; empty without one. An advert is
    /// unauthenticated: only the host's approval of this console makes it a pairing.
    pub fp_hex: String,
}

/// Trims an advertised OS chain to something bounded and predictable: lowercase, at most five
/// tokens of 32 characters, nothing outside [`paths::is_asset_char`]'s charset. Nothing
/// downstream trusts a host's strings, and this one is matched against packaged mark names.
fn sanitize_os(raw: &str) -> String {
    raw.to_lowercase()
        .split('/')
        .filter_map(|token| {
            let token: String = token
                .chars()
                .filter(|c| crate::services::paths::is_asset_char(*c))
                .take(32)
                .collect();
            // A token of dots alone names nothing (and was once a path segment).
            (!token.is_empty() && token.bytes().any(|b| b != b'.')).then_some(token)
        })
        .take(5)
        .collect::<Vec<_>>()
        .join("/")
}

/// The advert's TXT fields as the record keeps them: management port, wake MACs, OS chain.
/// Nothing here is trusted — a bad port reads as absent and the chain is bounded.
fn txt_fields(mgmt: Option<&str>, mac: Option<&str>, os: Option<&str>) -> (Option<u16>, Vec<String>, String) {
    (
        mgmt.and_then(|v| v.parse().ok()),
        mac.unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        sanitize_os(os.unwrap_or("")),
    )
}

/// The `fp` TXT value as canonical hex, or empty when it is not 32 bytes of hex.
fn advertised_fp(fp: Option<&str>) -> String {
    use crate::services::store::shared;
    fp.and_then(shared::parse_fp).map(shared::hex).unwrap_or_default()
}

/// The question every round asks: PTR for [`SERVICE_TYPE`], class IN with the unicast-response
/// bit, from a querier whose source port alone already makes the answer unicast.
fn query() -> Vec<u8> {
    // ID 0, standard query, one question.
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE_TYPE.trim_end_matches('.').split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 12, 0x80, 0x01]);
    q
}

/// The records of one or more answers, owner names lowercased (DNS compares them so).
#[derive(Default)]
struct Records {
    /// (owner, target) of each PTR.
    ptr: Vec<(String, String)>,
    /// Instance → (port, target host).
    srv: HashMap<String, (u16, String)>,
    /// Instance → TXT keys (lowercased) and values.
    txt: HashMap<String, HashMap<String, String>>,
    /// Host → IPv4 addresses.
    a: HashMap<String, Vec<Ipv4Addr>>,
}

fn be16(msg: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?])))
}

/// The name at `start` as dotted text with a trailing dot, and where the record goes on after
/// it. Follows compression pointers, a bounded number of them.
fn read_name(msg: &[u8], start: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut pos = start;
    let mut after = None;
    for _ in 0..128 {
        let len = usize::from(*msg.get(pos)?);
        match len & 0xc0 {
            0xc0 => {
                after.get_or_insert(pos + 2);
                pos = ((len & 0x3f) << 8) | usize::from(*msg.get(pos + 1)?);
            }
            0 if len == 0 => {
                name.push('.');
                return Some((name, after.unwrap_or(pos + 1)));
            }
            0 => {
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&String::from_utf8_lossy(msg.get(pos + 1..pos + 1 + len)?));
                pos += 1 + len;
            }
            _ => return None,
        }
    }
    None
}

/// TXT strings as `key=value` (a bare key reads as empty), keys lowercased.
fn txt_strings(mut rdata: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    while let Some((&len, rest)) = rdata.split_first() {
        let Some(s) = rest.get(..usize::from(len)) else { break };
        let s = String::from_utf8_lossy(s);
        let (k, v) = s.split_once('=').unwrap_or((&s, ""));
        if !k.is_empty() {
            out.entry(k.to_ascii_lowercase()).or_insert_with(|| v.to_string());
        }
        rdata = &rest[usize::from(len)..];
    }
    out
}

impl Records {
    /// Folds in one mDNS response; anything else (a query, a truncated packet) adds nothing.
    fn add(&mut self, msg: &[u8]) -> Option<()> {
        if be16(msg, 2)? & 0x8000 == 0 {
            return None;
        }
        let (questions, records) = (be16(msg, 4)?, be16(msg, 6)? + be16(msg, 8)? + be16(msg, 10)?);
        let mut pos = 12;
        for _ in 0..questions {
            pos = read_name(msg, pos)?.1 + 4;
        }
        for _ in 0..records {
            let (owner, p) = read_name(msg, pos)?;
            let (rtype, len) = (be16(msg, p)?, be16(msg, p + 8)?);
            let at = p + 10;
            let rdata = msg.get(at..at + len)?;
            let owner = owner.to_ascii_lowercase();
            match rtype {
                1 if len == 4 => {
                    let ip = Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]);
                    let ips = self.a.entry(owner).or_default();
                    if !ips.contains(&ip) {
                        ips.push(ip);
                    }
                }
                12 => {
                    if let Some((target, _)) = read_name(msg, at) {
                        let ptr = (owner, target);
                        if !self.ptr.contains(&ptr) {
                            self.ptr.push(ptr);
                        }
                    }
                }
                16 => {
                    self.txt.insert(owner, txt_strings(rdata));
                }
                33 if len >= 6 => {
                    if let Some((target, _)) = read_name(msg, at + 6) {
                        let port = u16::from_be_bytes([rdata[4], rdata[5]]);
                        self.srv.insert(owner, (port, target.to_ascii_lowercase()));
                    }
                }
                _ => {}
            }
            pos = at + len;
        }
        Some(())
    }

    /// Every advertised instance the records resolve to an address and a port, as
    /// `(instance, host)`.
    fn hosts(&self) -> Vec<(String, DiscoveredHost)> {
        let service = SERVICE_TYPE.to_ascii_lowercase();
        let mut out = Vec::new();
        for (owner, instance) in &self.ptr {
            if *owner != service {
                continue;
            }
            let key = instance.to_ascii_lowercase();
            let Some((port, target)) = self.srv.get(&key) else {
                continue;
            };
            let candidates = self.a.get(target).map(Vec::as_slice).unwrap_or_default();
            let empty = HashMap::new();
            let txt = self.txt.get(&key).unwrap_or(&empty);
            let get = |k: &str| txt.get(k).map(String::as_str);
            // Advisory TXT: the host names which address its advert is for. The ranking is
            // `punktfunk_core::discovery`'s, shared with the desktop and Android clients.
            let declared = get("addr").and_then(|v| v.parse().ok());
            let Some(addr) = punktfunk_core::discovery::pick_host_addr(candidates, declared) else {
                continue;
            };
            let (mgmt_port, mac, os) = txt_fields(get("mgmt"), get("mac"), get("os"));
            out.push((
                key,
                DiscoveredHost {
                    name: instance.split('.').next().unwrap_or("?").to_string(),
                    addr: addr.to_string(),
                    port: *port,
                    mgmt_port,
                    mac,
                    os,
                    fp_hex: advertised_fp(get("fp")),
                },
            ));
        }
        out
    }
}

/// The first re-query; each later one waits twice as long, up to [`QUERY_MAX`].
const QUERY_FIRST: Duration = Duration::from_secs(1);
const QUERY_MAX: Duration = Duration::from_secs(16);

/// A running browse for punktfunk hosts, drained from the menu tick. No thread of its own:
/// each [`poll`](Self::poll) reads what has arrived and re-asks when due. What keeps discovery
/// off the network during a stream is `console::model::Service` dropping this when the menu
/// loop exits.
pub struct Discovery {
    socket: UdpSocket,
    records: Records,
    /// Instances already handed out since the last query.
    reported: HashSet<String>,
    next_query: Instant,
    interval: Duration,
}

impl Discovery {
    /// `None` if the socket won't open or the first query won't send: discovery is then
    /// simply absent.
    pub fn start() -> Option<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|s| s.set_nonblocking(true).map(|()| s))
            .inspect_err(|e| tracing::error!("mdns: no query socket: {e}"))
            .ok()?;
        let mut d = Self {
            socket,
            records: Records::default(),
            reported: HashSet::new(),
            next_query: Instant::now(),
            interval: QUERY_FIRST,
        };
        d.ask()
            .inspect_err(|e| tracing::error!("mdns: query for {SERVICE_TYPE} failed: {e}"))
            .ok()?;
        tracing::debug!("mdns: asking for {SERVICE_TYPE} from {:?}", d.socket.local_addr());
        Some(d)
    }

    /// Sends the question again and starts a fresh set of answers, so a host that moved or
    /// left is not reported from old records.
    fn ask(&mut self) -> std::io::Result<()> {
        self.socket.send_to(&query(), MDNS_GROUP)?;
        self.records = Records::default();
        self.reported.clear();
        self.next_query = Instant::now() + self.interval;
        self.interval = (self.interval * 2).min(QUERY_MAX);
        Ok(())
    }

    /// Hosts resolved since the last call, re-asking when due. Empty on almost every tick.
    pub fn poll(&mut self) -> Vec<DiscoveredHost> {
        if Instant::now() >= self.next_query {
            if let Err(e) = self.ask() {
                tracing::debug!("mdns: re-query failed: {e}");
            }
        }
        let mut buf = [0u8; 9000];
        let mut heard = false;
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((n, _)) => heard |= self.records.add(&buf[..n]).is_some(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    tracing::debug!("mdns: read failed: {e}");
                    break;
                }
            }
        }
        if !heard {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (key, host) in self.records.hosts() {
            if self.reported.insert(key) {
                tracing::info!("mdns: resolved {} at {}:{}", host.name, host.addr, host.port);
                found.push(host);
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host's TXT strings are input: the OS chain is lowercased, bounded and stripped to
    /// the mark charset, MACs split on commas with blanks dropped, a bad port reads as absent.
    #[test]
    fn txt_fields_are_bounded_and_never_trusted() {
        let (mgmt, mac, os) = txt_fields(
            Some("47990"),
            Some("aa:bb:cc:dd:ee:ff, , 11:22:33:44:55:66"),
            Some("Linux/Fedora/Bazzite"),
        );
        assert_eq!(mgmt, Some(47990));
        assert_eq!(mac, vec!["aa:bb:cc:dd:ee:ff", "11:22:33:44:55:66"]);
        assert_eq!(os, "linux/fedora/bazzite");
        let (mgmt, mac, os) = txt_fields(Some("port"), None, Some("a/b/c/d/e/f/g"));
        assert_eq!(mgmt, None);
        assert!(mac.is_empty());
        assert_eq!(os, "a/b/c/d/e", "five tokens at most");
        assert_eq!(sanitize_os("../Win dows!//"), "windows");
        assert_eq!(sanitize_os(&"x".repeat(40)), "x".repeat(32));
    }

    #[test]
    fn advertised_fp_is_canonical_or_empty() {
        let fp = "0C619FF7FCFBD520CC58909DEBF62500A79835877013728AC40C3EC1554A7D3B";
        assert_eq!(advertised_fp(Some(fp)), fp.to_lowercase());
        assert_eq!(advertised_fp(Some("0c61")), "");
        assert_eq!(advertised_fp(Some(&"zz".repeat(32))), "");
        assert_eq!(advertised_fp(None), "");
    }

    /// A legacy-unicast query with our service name: ID 0, one question, PTR, IN plus the
    /// unicast-response bit.
    #[test]
    fn the_query_asks_for_a_unicast_ptr() {
        let mut want = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        want.extend_from_slice(b"\x0a_punktfunk\x04_udp\x05local\x00");
        want.extend_from_slice(&[0, 12, 0x80, 0x01]);
        assert_eq!(query(), want);
    }

    fn record(out: &mut Vec<u8>, owner: &[u8], rtype: u16, rdata: &[u8]) {
        out.extend_from_slice(owner);
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&[0, 1, 0, 0, 0, 10]);
        out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        out.extend_from_slice(rdata);
    }

    /// A legacy unicast answer as mdns-sd sends one: the question echoed, the PTR, then SRV,
    /// TXT and A as additionals, names compressed against earlier ones.
    fn answer() -> Vec<u8> {
        let mut m = vec![0, 0, 0x84, 0, 0, 1, 0, 1, 0, 0, 0, 3];
        // Question at 12: "_punktfunk._udp.local." PTR.
        m.extend_from_slice(b"\x0a_punktfunk\x04_udp\x05local\x00");
        m.extend_from_slice(&[0, 12, 0, 1]);
        // PTR → "PC._punktfunk._udp.local." (instance label, then a pointer to offset 12).
        let instance_at = m.len() + 2 + 10;
        record(&mut m, &[0xc0, 12], 12, b"\x02PC\xc0\x0c");
        // SRV for the instance: port 9777, target "pc.local." ("pc", then "local" at 28).
        let mut srv = vec![0, 0, 0, 0, 0x26, 0x31];
        srv.extend_from_slice(b"\x02pc\xc0\x1c");
        let instance = [0xc0, instance_at as u8];
        let target_at = m.len() + 2 + 10 + 6;
        record(&mut m, &instance, 33, &srv);
        record(
            &mut m,
            &instance,
            16,
            b"\x0amgmt=47990\x08os=Linux\x10addr=192.168.0.9\x03odd",
        );
        record(&mut m, &[0xc0, target_at as u8], 1, &[192, 168, 0, 9]);
        m
    }

    #[test]
    fn a_legacy_answer_resolves_to_a_host() {
        let mut r = Records::default();
        assert!(r.add(&answer()).is_some());
        let hosts = r.hosts();
        assert_eq!(hosts.len(), 1);
        let (key, h) = &hosts[0];
        assert_eq!(key, "pc._punktfunk._udp.local.");
        assert_eq!(h.name, "PC");
        assert_eq!(h.port, 9777);
        assert_eq!(h.addr, "192.168.0.9");
        assert_eq!(h.mgmt_port, Some(47990));
        assert_eq!(h.os, "linux");
    }

    #[test]
    fn queries_and_broken_packets_add_nothing() {
        let mut r = Records::default();
        assert!(r.add(&query()).is_none(), "a query is not an answer");
        let a = answer();
        for cut in [3, 12, 20, a.len() - 1] {
            let _ = r.add(&a[..cut]);
        }
        assert!(r.hosts().is_empty());
        // A pointer that points at itself ends, it does not spin.
        assert!(read_name(&[0xc0, 0], 0).is_none());
    }

    #[test]
    fn an_answer_split_across_packets_still_resolves() {
        let a = answer();
        let mut first = a.clone();
        // Same packet with only the question and PTR: SRV/TXT/A come in the next one.
        first[11] = 0;
        let mut r = Records::default();
        r.add(&first);
        assert!(r.hosts().is_empty());
        r.add(&a);
        assert_eq!(r.hosts().len(), 1);
    }
}
