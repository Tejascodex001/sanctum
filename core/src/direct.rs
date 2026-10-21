//! "Faster calls": a call's sealed packets sent straight between the two
//! phones over UDP, instead of through Tor. Opt-in on both phones.
//!
//! There are no servers. Each phone that has the setting on lists the
//! addresses it knows for itself (global IPv6 addresses, which most mobile
//! carriers hand out, and local-network IPv4 ones) with the port of a UDP
//! socket opened for this call, as `routes` inside the encrypted call
//! signaling. The phone called only lists its own if the caller listed
//! theirs, so nobody's address goes anywhere unless both want it.
//!
//! Once the call is answered, both phones send small probes to each
//! other's addresses at the same time ("hole punching": each probe going
//! out opens the phone's own firewall for answers from that address, which
//! is how typical IPv6 firewalls and a shared Wi-Fi let the two meet). A
//! probe is an ordinary sealed call packet, so only the friend in the call
//! can make one, and replays are refused. A probe is answered with an
//! acknowledgement; the first address whose acknowledgement comes back is
//! the direct path. Keepalive probes run every second; when nothing has
//! come over it for a few seconds the call goes back to the friends' own
//! link (Tor or nearby), and probing starts again.
//!
//! Packets are only ever sent to addresses the friend listed, never to
//! loopback, multicast, unspecified or link-local addresses.
//!
//! This module is the bookkeeping: what to probe when, and which path is
//! up. The network code ([`crate::call::MediaHub`] and `sanctum_net`)
//! sends and receives.

use crate::protocol::CallId;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

/// A route on the wire: this tag, the address's bytes, and the port.
const ROUTE_V4: u8 = 4;
const ROUTE_V6: u8 = 6;
/// At most this many routes are offered or taken.
pub const MAX_ROUTES: usize = 8;
/// UDP packets start with this, then the call id, the packet's number and
/// the sealed packet.
const DATAGRAM_TAG: [u8; 2] = *b"S1";
const DATAGRAM_HEAD: usize = 2 + 16 + 8;

/// Probes go out this often while no path is up...
pub const PROBE_EVERY: Duration = Duration::from_millis(250);
/// ...for this long after the call starts (or the path dies)...
pub const PROBE_FAST_FOR: Duration = Duration::from_secs(10);
/// ...and then this often, in case a path opens up later.
pub const PROBE_SLOW_EVERY: Duration = Duration::from_secs(2);
/// On a path that's up, a keepalive this often.
pub const KEEPALIVE_EVERY: Duration = Duration::from_secs(1);
/// Nothing over the direct path for this long: it's gone.
pub const DEAD_AFTER: Duration = Duration::from_secs(3);

/// Which addresses packets may go to.
pub type AddrFilter = fn(&SocketAddr) -> bool;

/// For phones: anywhere but this phone itself, multicast, unspecified or
/// link-local addresses (which need an interface to mean anything).
pub fn public_only(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }
    match canonical(*addr).ip() {
        IpAddr::V4(ip) => {
            !(ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() || ip.is_broadcast() || ip.is_link_local())
        }
        IpAddr::V6(ip) => {
            !(ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() || (ip.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// For tests on one machine: loopback allowed too.
pub fn loopback_too(addr: &SocketAddr) -> bool {
    addr.port() != 0 && !addr.ip().is_unspecified() && !addr.ip().is_multicast()
}

/// IPv4 addresses as such, even when a dual-stack socket reports them as
/// `::ffff:a.b.c.d`.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
            None => addr,
        },
        v4 => v4,
    }
}

pub fn encode_route(addr: SocketAddr) -> Vec<u8> {
    let addr = canonical(addr);
    let mut out = Vec::with_capacity(19);
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(ROUTE_V4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(ROUTE_V6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
    out
}

/// A route from the friend; None if it's of a kind this build doesn't know.
pub fn decode_route(bytes: &[u8]) -> Option<SocketAddr> {
    let (&tag, rest) = bytes.split_first()?;
    let (ip, port) = match tag {
        ROUTE_V4 if rest.len() == 6 => (IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(&rest[..4]).ok()?)), &rest[4..]),
        ROUTE_V6 if rest.len() == 18 => {
            (IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&rest[..16]).ok()?)), &rest[16..])
        }
        _ => return None,
    };
    Some(canonical(SocketAddr::new(ip, u16::from_be_bytes([port[0], port[1]]))))
}

/// The friend's routes this phone may send to: known kinds, allowed by
/// `filter`, each once, a few at most.
pub fn routes_from(routes: &[Vec<u8>], filter: AddrFilter) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for addr in routes.iter().filter_map(|r| decode_route(r)) {
        if filter(&addr) && !out.contains(&addr) && out.len() < MAX_ROUTES {
            out.push(addr);
        }
    }
    out
}

/// A call packet as a UDP datagram.
pub fn encode_datagram(call: &CallId, seq: u64, sealed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DATAGRAM_HEAD + sealed.len());
    out.extend_from_slice(&DATAGRAM_TAG);
    out.extend_from_slice(call);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(sealed);
    out
}

pub fn decode_datagram(bytes: &[u8]) -> Option<(CallId, u64, &[u8])> {
    if bytes.len() <= DATAGRAM_HEAD || bytes[..2] != DATAGRAM_TAG {
        return None;
    }
    let call: CallId = bytes[2..18].try_into().ok()?;
    let seq = u64::from_be_bytes(bytes[18..26].try_into().ok()?);
    Some((call, seq, &bytes[DATAGRAM_HEAD..]))
}

/// One call's direct path: where to probe, and which address answered.
#[derive(Debug)]
pub struct DirectPath {
    /// The friend's addresses, already filtered.
    candidates: Vec<SocketAddr>,
    /// The address that answered first, while it keeps answering.
    up: Option<SocketAddr>,
    /// When something last came from `up`.
    heard: Option<Instant>,
    /// Since when we've been looking for a path (start, or the last one
    /// died).
    looking_since: Instant,
    last_probe: Option<Instant>,
    next_probe: u32,
}

/// What [`DirectPath::on_packet`] makes of a packet that came over UDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Not from one of the friend's addresses: ignore it.
    Stranger,
    /// Fine (counts as the path being alive).
    Ok,
    /// The path to this address is up now.
    Up(SocketAddr),
}

impl DirectPath {
    pub fn new(candidates: Vec<SocketAddr>, now: Instant) -> Self {
        Self { candidates, up: None, heard: None, looking_since: now, last_probe: None, next_probe: 0 }
    }

    pub fn candidates(&self) -> &[SocketAddr] {
        &self.candidates
    }

    /// The direct path, if it's up.
    pub fn up(&self) -> Option<SocketAddr> {
        self.up
    }

    /// Whether `from` is one of the friend's addresses.
    pub fn knows(&self, from: &SocketAddr) -> bool {
        self.candidates.contains(&canonical(*from))
    }

    /// Probes due now: (where, probe number). Also notices a path that has
    /// gone quiet and starts looking again.
    pub fn due(&mut self, now: Instant) -> Vec<(SocketAddr, u32)> {
        if let (Some(_), Some(heard)) = (self.up, self.heard) {
            if now.saturating_duration_since(heard) > DEAD_AFTER {
                self.up = None;
                self.heard = None;
                self.looking_since = now;
                self.last_probe = None;
            }
        }
        let every = match self.up {
            Some(_) => KEEPALIVE_EVERY,
            None if now.saturating_duration_since(self.looking_since) < PROBE_FAST_FOR => PROBE_EVERY,
            None => PROBE_SLOW_EVERY,
        };
        if self.last_probe.is_some_and(|last| now.saturating_duration_since(last) < every) {
            return Vec::new();
        }
        self.last_probe = Some(now);
        let to: Vec<SocketAddr> = match self.up {
            Some(addr) => vec![addr],
            None => self.candidates.clone(),
        };
        to.into_iter()
            .map(|addr| {
                let n = self.next_probe;
                self.next_probe = self.next_probe.wrapping_add(1);
                (addr, n)
            })
            .collect()
    }

    /// An authenticated packet arrived over UDP from `from`; `ack` if it
    /// answered one of our probes.
    pub fn on_packet(&mut self, from: SocketAddr, ack: bool, now: Instant) -> Heard {
        let from = canonical(from);
        if !self.candidates.contains(&from) {
            return Heard::Stranger;
        }
        match self.up {
            Some(addr) if addr == from => {
                self.heard = Some(now);
                Heard::Ok
            }
            None if ack => {
                self.up = Some(from);
                self.heard = Some(now);
                Heard::Up(from)
            }
            _ => Heard::Ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn routes_round_trip_and_unknown_kinds_are_skipped() {
        for a in ["192.168.1.20:5000", "[2401:4900:1c2a::17]:443"] {
            assert_eq!(decode_route(&encode_route(addr(a))), Some(addr(a)));
        }
        // A dual-stack socket's view of an IPv4 address.
        assert_eq!(decode_route(&encode_route(addr("[::ffff:10.0.0.2]:9"))), Some(addr("10.0.0.2:9")));
        assert_eq!(decode_route(&[9, 1, 2]), None);
        assert_eq!(decode_route(&[ROUTE_V4, 1, 2, 3]), None);
        assert_eq!(decode_route(&[]), None);
    }

    #[test]
    fn only_addresses_elsewhere_are_taken() {
        let routes: Vec<Vec<u8>> = [
            "127.0.0.1:5000",
            "[::1]:5000",
            "0.0.0.0:5000",
            "224.0.0.1:5000",
            "[ff02::1]:5000",
            "[fe80::1]:5000",
            "169.254.3.3:5000",
            "255.255.255.255:5000",
            "192.168.1.20:0",
            "192.168.1.20:5000",
            "192.168.1.20:5000",
            "[2401:4900:1c2a::17]:5000",
        ]
        .iter()
        .map(|a| encode_route(addr(a)))
        .collect();
        assert_eq!(
            routes_from(&routes, public_only),
            [addr("192.168.1.20:5000"), addr("[2401:4900:1c2a::17]:5000")]
        );
        assert!(routes_from(&routes, loopback_too).contains(&addr("127.0.0.1:5000")));
        let many: Vec<Vec<u8>> = (1..40).map(|i| encode_route(addr(&format!("10.0.0.{i}:7")))).collect();
        assert_eq!(routes_from(&many, public_only).len(), MAX_ROUTES);
    }

    #[test]
    fn datagrams_round_trip() {
        let d = encode_datagram(&[3; 16], 77, b"sealed");
        assert_eq!(decode_datagram(&d), Some(([3; 16], 77, b"sealed".as_slice())));
        assert_eq!(decode_datagram(&d[..20]), None);
        assert_eq!(decode_datagram(b"nope, not a call packet at all"), None);
    }

    #[test]
    fn probes_every_address_until_one_answers_then_keeps_it_alive() {
        let t0 = Instant::now();
        let (a, b) = (addr("192.168.1.20:5000"), addr("[2401:4900::17]:5000"));
        let mut path = DirectPath::new(vec![a, b], t0);
        assert_eq!(path.due(t0).len(), 2);
        assert!(path.due(t0 + Duration::from_millis(100)).is_empty(), "not yet");
        assert_eq!(path.due(t0 + PROBE_EVERY).len(), 2);
        // A stranger, even answering, changes nothing.
        assert_eq!(path.on_packet(addr("10.9.9.9:5000"), true, t0), Heard::Stranger);
        // A probe from them isn't proof they hear us; an answer is.
        assert_eq!(path.on_packet(b, false, t0), Heard::Ok);
        assert_eq!(path.up(), None);
        let t1 = t0 + Duration::from_millis(600);
        assert_eq!(path.on_packet(b, true, t1), Heard::Up(b));
        assert_eq!(path.on_packet(a, true, t1), Heard::Ok, "the first one stays");
        assert_eq!(path.up(), Some(b));
        // Up: only keepalives, to that address, once a second.
        let probes = path.due(t1 + KEEPALIVE_EVERY);
        assert_eq!(probes.iter().map(|p| p.0).collect::<Vec<_>>(), [b]);
        assert!(path.due(t1 + KEEPALIVE_EVERY + Duration::from_millis(500)).is_empty());
    }

    #[test]
    fn a_quiet_path_dies_and_probing_starts_again() {
        let t0 = Instant::now();
        let a = addr("192.168.1.20:5000");
        let mut path = DirectPath::new(vec![a], t0);
        path.on_packet(a, true, t0);
        path.due(t0);
        path.on_packet(a, false, t0 + Duration::from_secs(2));
        path.due(t0 + Duration::from_secs(4));
        assert_eq!(path.up(), Some(a), "heard 2 s ago");
        let late = t0 + Duration::from_secs(2) + DEAD_AFTER + Duration::from_millis(1);
        assert_eq!(path.due(late).len(), 1);
        assert_eq!(path.up(), None);
        // Looking again, quickly at first.
        assert_eq!(path.due(late + PROBE_EVERY).len(), 1);
        // Later, slowly.
        let later = late + PROBE_FAST_FOR;
        assert_eq!(path.due(later).len(), 1);
        assert!(path.due(later + PROBE_EVERY).is_empty());
        assert_eq!(path.due(later + PROBE_SLOW_EVERY).len(), 1);
    }
}
