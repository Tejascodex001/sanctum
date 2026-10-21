//! Nearby: friends on the same Wi-Fi (or one phone's hotspot) talk
//! directly, with no internet.
//!
//! Each phone broadcasts a small beacon every few seconds on the local
//! network. It names no one: for each friend it carries an 8-byte tag
//! that only that friend can compute (a keyed hash of the pair's
//! fingerprint and the current minute), so strangers on the network learn
//! nothing and can't follow a phone from one minute to the next. A friend
//! who recognizes a tag connects to the TCP port in the beacon, and from
//! there it's the usual link: the same signed hello, the same MLS.
//!
//! What a nearby friend does see is your local network address
//! (192.168.x.x), never your internet address; that's why it's opt-in.

use crate::engine::{BoxConn, Node, Transport};
use async_trait::async_trait;
use sanctum_core::identity::pair_fingerprint;
use sanctum_core::session::Session;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::time::timeout;

/// Where beacons are sent and heard.
pub const DISCOVERY_PORT: u16 = 47_823;
const BEACON_EVERY: Duration = Duration::from_secs(4);
/// A friend not heard from for this long isn't nearby any more.
const FRESH: Duration = Duration::from_secs(20);
const MAGIC: [u8; 4] = *b"SNC1";
/// Keeps a beacon inside one unfragmented UDP packet.
const TAGS_PER_BEACON: usize = 150;
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize, Deserialize)]
struct Beacon {
    magic: [u8; 4],
    /// Random per run: to ignore our own beacons.
    instance: u64,
    /// Our TCP port.
    port: u16,
    tags: Vec<[u8; 8]>,
}

/// The tag `me` and `friend` both compute for this minute.
fn tag(me: &[u8; 32], friend: &[u8; 32], minute: u64) -> [u8; 8] {
    let key: [u8; 32] = pair_fingerprint(me, friend);
    let mut input = b"sanctum nearby v1".to_vec();
    input.extend_from_slice(&minute.to_le_bytes());
    let hash = blake3::keyed_hash(&key, &input);
    hash.as_bytes()[..8].try_into().unwrap()
}

fn minute() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 60)
}

pub struct Nearby {
    /// Friends heard recently, and where they listen.
    book: Mutex<HashMap<[u8; 32], (SocketAddr, Instant)>>,
    instance: u64,
    /// Where friends' connections arrive (Wi-Fi, and Bluetooth links handed
    /// over by the Android side on localhost).
    port: u16,
    stop: watch::Sender<bool>,
}

impl Nearby {
    /// Starts listening, beaconing and hearing beacons. Connections friends
    /// make to us go to `node`.
    pub async fn start(node: Arc<Node>, session: Arc<Mutex<Session>>) -> anyhow::Result<Arc<Self>> {
        Self::start_with(node, session, DISCOVERY_PORT, None).await
    }

    /// [`start`](Self::start) hearing on `discovery_port` and, if given,
    /// beaconing to `targets` instead of broadcasting (for tests).
    pub async fn start_with(
        node: Arc<Node>,
        session: Arc<Mutex<Session>>,
        discovery_port: u16,
        targets: Option<Vec<SocketAddr>>,
    ) -> anyhow::Result<Arc<Self>> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        let port = listener.local_addr()?.port();
        // Beacons are best effort: some networks or phones refuse the port,
        // and Bluetooth still works without them.
        let udp = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, discovery_port))
            .and_then(|u| {
                u.set_broadcast(true)?;
                u.set_nonblocking(true)?;
                UdpSocket::from_std(u)
            })
            .ok()
            .map(Arc::new);
        let (stop, _) = watch::channel(false);
        let nearby = Arc::new(Self {
            book: Mutex::default(),
            instance: rand::random(),
            port,
            stop,
        });

        // Friends connecting to us.
        let (n, mut stopped) = (node.clone(), nearby.stop.subscribe());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            let _ = stream.set_nodelay(true);
                            n.accept_nearby(Box::new(stream));
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    },
                    _ = stopped.changed() => break,
                }
            }
        });

        let Some(udp) = udp else {
            return Ok(nearby);
        };
        // Our beacon.
        let (me, s, sock, mut stopped) = (nearby.clone(), session.clone(), udp.clone(), nearby.stop.subscribe());
        tokio::spawn(async move {
            loop {
                let targets = targets.clone().unwrap_or_else(broadcast_addresses);
                for packet in me.beacons(&s, port) {
                    for target in &targets {
                        let _ = sock.send_to(&packet, target).await;
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(BEACON_EVERY) => {}
                    _ = stopped.changed() => break,
                }
            }
        });

        // Friends' beacons.
        let (me, mut stopped) = (nearby.clone(), nearby.stop.subscribe());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    got = udp.recv_from(&mut buf) => {
                        if let Ok((n, from)) = got {
                            me.heard(&buf[..n], from.ip(), &session, &node);
                        }
                    }
                    _ = stopped.changed() => break,
                }
            }
        });
        Ok(nearby)
    }

    /// Where friends' connections arrive.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn stop(&self) {
        let _ = self.stop.send(true);
        self.book.lock().unwrap().clear();
    }

    /// Where a nearby friend listens, if they're nearby.
    pub fn addr(&self, peer: &[u8; 32]) -> Option<SocketAddr> {
        let book = self.book.lock().unwrap();
        book.get(peer).filter(|(_, at)| at.elapsed() < FRESH).map(|(addr, _)| *addr)
    }

    /// Friends nearby right now.
    pub fn peers(&self) -> Vec<[u8; 32]> {
        let book = self.book.lock().unwrap();
        book.iter().filter(|(_, (_, at))| at.elapsed() < FRESH).map(|(p, _)| *p).collect()
    }

    fn beacons(&self, session: &Arc<Mutex<Session>>, port: u16) -> Vec<Vec<u8>> {
        let (me, friends) = {
            let s = session.lock().unwrap();
            (s.public_key(), s.friend_keys().unwrap_or_default())
        };
        let minute = minute();
        let tags: Vec<[u8; 8]> = friends.iter().map(|f| tag(&me, f, minute)).collect();
        tags.chunks(TAGS_PER_BEACON)
            .map(|chunk| {
                postcard::to_stdvec(&Beacon {
                    magic: MAGIC,
                    instance: self.instance,
                    port,
                    tags: chunk.to_vec(),
                })
                .expect("serializable")
            })
            .collect()
    }

    fn heard(&self, packet: &[u8], from: IpAddr, session: &Arc<Mutex<Session>>, node: &Arc<Node>) {
        let Ok(beacon) = postcard::from_bytes::<Beacon>(packet) else { return };
        if beacon.magic != MAGIC || beacon.instance == self.instance {
            return;
        }
        let (me, friends) = {
            let s = session.lock().unwrap();
            (s.public_key(), s.friend_keys().unwrap_or_default())
        };
        // This minute's tags, and the last minute's (clocks differ a little).
        let minute = minute();
        for friend in friends {
            let known = [tag(&me, &friend, minute), tag(&me, &friend, minute.saturating_sub(1))];
            if beacon.tags.iter().any(|t| known.contains(t)) {
                let addr = SocketAddr::new(from, beacon.port);
                let new = self.book.lock().unwrap().insert(friend, (addr, Instant::now())).is_none();
                if new || !node.connected().contains(&friend) {
                    node.dial_now(friend);
                } else {
                    // Linked over Tor: the nearby link is faster.
                    node.prefer_nearby(friend, false);
                }
            }
        }
    }
}

/// The limited broadcast address plus each network's own (a hotspot's
/// clients often only hear the latter).
fn broadcast_addresses() -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT))];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if let Some(broadcast) = v4.broadcast {
                if !v4.ip.is_loopback() {
                    out.push(SocketAddr::from((broadcast, DISCOVERY_PORT)));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Friends seen over Bluetooth, reached through the Android side: it
/// listens on a localhost port, takes an 8-byte handle naming the device,
/// opens a Bluetooth link to it and relays bytes both ways.
pub struct Bluetooth {
    proxy_port: u16,
    seen: Mutex<HashMap<[u8; 32], (u64, Instant)>>,
}

impl Bluetooth {
    pub fn new(proxy_port: u16) -> Self {
        Self {
            proxy_port,
            seen: Mutex::default(),
        }
    }

    /// `peer`'s phone was seen as device `handle`.
    pub fn saw(&self, peer: [u8; 32], handle: u64) -> bool {
        self.seen.lock().unwrap().insert(peer, (handle, Instant::now())).is_none()
    }

    pub fn handle(&self, peer: &[u8; 32]) -> Option<u64> {
        let seen = self.seen.lock().unwrap();
        seen.get(peer).filter(|(_, at)| at.elapsed() < FRESH).map(|(h, _)| *h)
    }

    pub fn peers(&self) -> Vec<[u8; 32]> {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|(_, (_, at))| at.elapsed() < FRESH).map(|(p, _)| *p).collect()
    }

    async fn dial(&self, handle: u64) -> anyhow::Result<BoxConn> {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.proxy_port)).await?;
        tokio::io::AsyncWriteExt::write_all(&mut stream, &handle.to_le_bytes()).await?;
        let _ = stream.set_nodelay(true);
        Ok(Box::new(stream))
    }
}

/// Reaches friends nearby directly when possible (Wi-Fi, then Bluetooth),
/// else over Tor once it's up. Any can be missing: no internet, nearby off.
#[derive(Default)]
pub struct Reach {
    pub nearby: RwLock<Option<Arc<Nearby>>>,
    pub bluetooth: RwLock<Option<Arc<Bluetooth>>>,
    pub tor: RwLock<Option<Arc<dyn Transport>>>,
}

#[async_trait]
impl Transport for Reach {
    async fn dial(&self, peer: [u8; 32]) -> anyhow::Result<BoxConn> {
        Ok(self.dial_ranked(peer).await?.0)
    }

    async fn dial_ranked(&self, peer: [u8; 32]) -> anyhow::Result<(BoxConn, bool)> {
        let nearby = self.nearby.read().unwrap().as_ref().and_then(|n| n.addr(&peer));
        if let Some(addr) = nearby {
            if let Ok(Ok(stream)) = timeout(DIAL_TIMEOUT, TcpStream::connect(addr)).await {
                let _ = stream.set_nodelay(true);
                return Ok((Box::new(stream), true));
            }
        }
        let bluetooth = self.bluetooth.read().unwrap().clone();
        if let Some(handle) = bluetooth.as_ref().and_then(|b| b.handle(&peer)) {
            // Bluetooth links take a few seconds to open.
            if let Ok(Ok(conn)) = timeout(Duration::from_secs(15), bluetooth.unwrap().dial(handle)).await {
                return Ok((conn, true));
            }
        }
        let tor = self.tor.read().unwrap().clone();
        match tor {
            Some(tor) => Ok((tor.dial(peer).await?, false)),
            None => anyhow::bail!("offline"),
        }
    }

    fn nearby(&self, peer: &[u8; 32]) -> bool {
        self.nearby.read().unwrap().as_ref().is_some_and(|n| n.addr(peer).is_some())
            || self.bluetooth.read().unwrap().as_ref().is_some_and(|b| b.handle(peer).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sanctum_core::identity::Identity;

    #[test]
    fn tags_match_only_for_the_pair_and_change_every_minute() {
        let (a, b, c) = (
            Identity::generate().public_key(),
            Identity::generate().public_key(),
            Identity::generate().public_key(),
        );
        assert_eq!(tag(&a, &b, 7), tag(&b, &a, 7), "both compute it");
        assert_ne!(tag(&a, &b, 7), tag(&a, &c, 7), "another friend can't");
        assert_ne!(tag(&a, &b, 7), tag(&a, &b, 8), "not linkable across minutes");
    }

    use crate::engine::Node;

    async fn phone(port: u16, other: u16) -> (Arc<Mutex<Session>>, Arc<Node>, Arc<Nearby>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::open(Identity::generate().secret(), dir.path().join("s.db"), &rand::random()).unwrap();
        let session = Arc::new(Mutex::new(s));
        // No Tor at all: only nearby.
        let reach = Arc::new(Reach::default());
        let node = Node::new(session.clone(), reach.clone());
        node.spawn_retry_loop(Duration::from_millis(100));
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, other));
        let nearby = Nearby::start_with(node.clone(), session.clone(), port, Some(vec![target])).await.unwrap();
        *reach.nearby.write().unwrap() = Some(nearby.clone());
        (session, node, nearby, dir)
    }

    #[tokio::test]
    async fn friends_find_each_other_and_talk_without_internet() {
        let (pa, pb) = (47_901, 47_902);
        let (alice, alice_node, alice_nearby, _a) = phone(pa, pb).await;
        let (bob, _bob_node, bob_nearby, _b) = phone(pb, pa).await;
        let code = bob.lock().unwrap().my_code("Bob");
        let ab = alice.lock().unwrap().add_friend(&code, true).unwrap().chat_id;
        let code = alice.lock().unwrap().my_code("Alice");
        let ba = bob.lock().unwrap().add_friend(&code, true).unwrap().chat_id;
        let (a_key, b_key) = (alice.lock().unwrap().public_key(), bob.lock().unwrap().public_key());

        // Beacons find them; a link comes up with no Tor at all.
        timeout(Duration::from_secs(15), async {
            while !(alice_nearby.peers() == [b_key] && bob_nearby.peers() == [a_key] && alice_node.connected() == [b_key]) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("never found each other");

        // A temp chat, the point of nearby.
        let t = alice.lock().unwrap().temp_new(Some(ab)).unwrap();
        alice.lock().unwrap().temp_send(t, "no internet here").unwrap();
        alice.lock().unwrap().send_text(ab, "and a normal one").unwrap();
        alice_node.poke(b_key);
        timeout(Duration::from_secs(10), async {
            loop {
                {
                    let s = bob.lock().unwrap();
                    let temp_ok = s.temp.all().iter().any(|c| c.messages.iter().any(|m| m.body == "no internet here"));
                    let chat_ok = s.store.messages(ba, None).unwrap().iter().any(|m| m.body == "and a normal one");
                    if temp_ok && chat_ok {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("messages never arrived");
        alice_nearby.stop();
        bob_nearby.stop();
    }

    #[tokio::test]
    async fn bluetooth_routes_go_through_the_local_relay() {
        // Stands in for the Android side: reads the handle, then echoes.
        let relay = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = relay.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = relay.accept().await.unwrap();
            let mut handle = [0u8; 8];
            s.read_exact(&mut handle).await.unwrap();
            s.write_all(&handle).await.unwrap();
        });
        let reach = Reach::default();
        let bt = Arc::new(Bluetooth::new(port));
        let peer = [7u8; 32];
        assert!(bt.saw(peer, 42));
        assert!(!bt.saw(peer, 42), "already known");
        *reach.bluetooth.write().unwrap() = Some(bt.clone());
        let mut conn = reach.dial(peer).await.unwrap();
        let mut back = [0u8; 8];
        tokio::io::AsyncReadExt::read_exact(&mut conn, &mut back).await.unwrap();
        assert_eq!(u64::from_le_bytes(back), 42);
        assert_eq!(bt.peers(), [peer]);
        // Unknown peers aren't routed there.
        assert!(reach.dial([8u8; 32]).await.is_err());
    }

    #[test]
    fn beacons_name_no_one_and_fit_a_packet() {
        let beacon = Beacon {
            magic: MAGIC,
            instance: 1,
            port: 9,
            tags: vec![[0xAB; 8]; TAGS_PER_BEACON],
        };
        let bytes = postcard::to_stdvec(&beacon).unwrap();
        assert!(bytes.len() < 1400, "{} bytes", bytes.len());
    }
}
