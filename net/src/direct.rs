//! "Faster calls": the UDP side of a direct path between two phones in a
//! call (the bookkeeping is in `sanctum_core::direct`).
//!
//! A socket is opened for one call, only when the user turned the setting
//! on. Its addresses (see [`local_routes`]) go to the friend inside the
//! encrypted call signaling; what arrives on it is handed to the call's
//! media, which takes only sealed packets from the friend's own addresses.

use sanctum_core::call::MediaHub;
use sanctum_core::direct::{decode_datagram, public_only};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// Big enough for any call packet.
const MAX_DATAGRAM: usize = 2048;

/// A call's UDP socket and the task reading from it.
pub struct DirectSocket {
    socket: Arc<UdpSocket>,
    reader: JoinHandle<()>,
}

impl DirectSocket {
    /// Binds `bind` (port 0: any) and hands what arrives to `media`;
    /// `answered` runs after each packet taken (to send any answers).
    /// Needs a Tokio runtime.
    pub fn open(
        bind: SocketAddr,
        media: MediaHub,
        answered: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let std = std::net::UdpSocket::bind(bind)?;
        std.set_nonblocking(true)?;
        let socket = Arc::new(UdpSocket::from_std(std)?);
        let reading = socket.clone();
        let reader = tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_DATAGRAM];
            loop {
                let Ok((n, from)) = reading.recv_from(&mut buf).await else {
                    // E.g. an ICMP "unreachable" from an earlier probe: carry on.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                };
                let Some((call, seq, sealed)) = decode_datagram(&buf[..n]) else { continue };
                if media.receive_direct(from, &call, seq, sealed) {
                    answered();
                }
            }
        });
        Ok(Self { socket, reader })
    }

    /// Opens on any port, IPv6 and IPv4 at once where the phone allows,
    /// else IPv4 only.
    pub fn open_any(media: MediaHub, answered: impl Fn() + Send + Sync + Clone + 'static) -> std::io::Result<Self> {
        Self::open(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0), media.clone(), answered.clone())
            .or_else(|_| Self::open(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), media, answered))
    }

    pub fn port(&self) -> u16 {
        self.socket.local_addr().map_or(0, |a| a.port())
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.socket.local_addr().ok()
    }

    /// Sends now, or drops it if the socket is busy (late sound is
    /// useless). Returns whether it went.
    pub fn send(&self, to: SocketAddr, datagram: &[u8]) -> bool {
        let Ok(local) = self.socket.local_addr() else { return false };
        // A dual-stack IPv6 socket reaches IPv4 addresses in their mapped form.
        let to = match (local, to) {
            (SocketAddr::V6(_), SocketAddr::V4(v4)) => SocketAddr::new(IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port()),
            (SocketAddr::V4(_), SocketAddr::V6(v6)) => match v6.ip().to_ipv4_mapped() {
                Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
                None => return false,
            },
            _ => to,
        };
        self.socket.try_send_to(datagram, to).is_ok()
    }
}

impl Drop for DirectSocket {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Where the friend may reach this phone on `port`: its global IPv6
/// addresses and its local-network IPv4 ones. Never loopback, link-local
/// or carrier-internal addresses.
pub fn local_routes(port: u16) -> Vec<SocketAddr> {
    let ips = if_addrs::get_if_addrs().unwrap_or_default().into_iter().map(|i| i.ip()).collect();
    usable(ips, port)
}

fn usable(ips: Vec<IpAddr>, port: u16) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    for ip in ips {
        let ok = match ip {
            // Global unicast (2000::/3), as mobile carriers hand out.
            IpAddr::V6(v6) => v6.segments()[0] & 0xe000 == 0x2000,
            // The local network: 10/8, 172.16/12, 192.168/16.
            IpAddr::V4(v4) => v4.is_private(),
        };
        let addr = SocketAddr::new(ip, port);
        if ok && public_only(&addr) && !out.contains(&addr) {
            out.push(addr);
        }
    }
    // IPv6 first: it usually works from anywhere.
    out.sort_by_key(|a| a.is_ipv4());
    out.truncate(sanctum_core::direct::MAX_ROUTES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_a_friend_could_reach() {
        let ips: Vec<IpAddr> = [
            "127.0.0.1",
            "::1",
            "fe80::1",
            "169.254.1.1",
            "100.72.1.2",
            "8.8.8.8",
            "192.168.1.5",
            "10.0.0.7",
            "2401:4900:1c2a::17",
            "fd00::5",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        let got: Vec<String> = usable(ips, 4000).iter().map(|a| a.to_string()).collect();
        assert_eq!(got, ["[2401:4900:1c2a::17]:4000", "192.168.1.5:4000", "10.0.0.7:4000"]);
    }
}
