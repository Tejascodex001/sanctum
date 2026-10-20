//! Runs connections between friends: handshake, then frames both ways,
//! driving the core [`Session`] engine. Transport-agnostic: Tor in the app,
//! plain TCP in tests.

use async_trait::async_trait;
use sanctum_core::protocol::{encode_frame, take_frame, Frame};
use sanctum_core::session::{Event, Session};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

pub trait Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Conn for T {}
pub type BoxConn = Box<dyn Conn>;

/// How to reach a friend.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn dial(&self, peer: [u8; 32]) -> anyhow::Result<BoxConn>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum NetEvent {
    Session(Event),
    /// Whether we can reach the network (Tor bootstrapped).
    Online(bool),
    PeerConnected([u8; 32]),
    PeerDisconnected([u8; 32]),
}

/// Handshakes must finish quickly; Tor circuits can be slow to build, so
/// this is generous.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// Links to friends stay open while the app runs, so messages arrive in
/// about a second instead of waiting for a new Tor circuit. A ping keeps
/// each one alive and shows when it has died.
const KEEPALIVE: Duration = Duration::from_secs(40);
/// Nothing heard for this long (two missed pings): the link is dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Reconnecting to a friend who can't be reached backs off from this...
const BACKOFF_MIN: Duration = Duration::from_secs(4);
/// ...up to this, or this while messages are waiting for them.
const BACKOFF_MAX: Duration = Duration::from_secs(120);
const BACKOFF_MAX_PENDING: Duration = Duration::from_secs(20);
/// While the app is closed, unreachable friends are retried less often to
/// save battery. A friend coming online dials us anyway.
const BACKOFF_MAX_BACKGROUND: Duration = Duration::from_secs(600);
const BACKOFF_MAX_PENDING_BACKGROUND: Duration = Duration::from_secs(120);
/// Every this many upkeep ticks, unacknowledged frames are resent.
const RESEND_EVERY: u64 = 10;
/// Tor circuits are expensive; don't build too many at once.
const MAX_DIALS: usize = 6;

enum Cmd {
    /// There's news to send.
    News,
    /// A better link to the same friend replaced this one.
    Close,
}

/// A live connection.
struct Link {
    id: u64,
    tx: mpsc::UnboundedSender<Cmd>,
    /// Dialed by the friend with the smaller key. When both phones dial at
    /// once, both keep this one, so they never close each other's.
    preferred: bool,
}


pub struct Node {
    session: Arc<Mutex<Session>>,
    transport: Arc<dyn Transport>,
    events: broadcast::Sender<NetEvent>,
    /// Live connections, each with a channel to ask it to send news.
    links: Mutex<HashMap<[u8; 32], Link>>,
    dialing: Mutex<HashSet<[u8; 32]>>,
    /// When to next try a friend we couldn't reach, and the current delay.
    backoff: Mutex<HashMap<[u8; 32], (Instant, Duration)>>,
    background: AtomicBool,
    ticks: Mutex<u64>,
    next_link: Mutex<u64>,
}

enum Role {
    Reached,
    Dialed([u8; 32]),
}

impl Node {
    pub fn new(session: Arc<Mutex<Session>>, transport: Arc<dyn Transport>) -> Arc<Self> {
        Arc::new(Self {
            session,
            transport,
            events: broadcast::channel(256).0,
            links: Mutex::default(),
            dialing: Mutex::default(),
            backoff: Mutex::default(),
            background: AtomicBool::new(false),
            ticks: Mutex::new(0),
            next_link: Mutex::new(0),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<NetEvent> {
        self.events.subscribe()
    }

    pub fn set_online(&self, online: bool) {
        let _ = self.events.send(NetEvent::Online(online));
    }

    pub fn connected(&self) -> Vec<[u8; 32]> {
        self.links.lock().unwrap().keys().copied().collect()
    }

    /// Serves a connection someone made to us.
    pub fn accept(self: &Arc<Self>, conn: BoxConn) {
        let node = Arc::clone(self);
        tokio::spawn(async move {
            let _ = node.run(conn, Role::Reached).await;
        });
    }

    /// Gets anything waiting for `peer` delivered: over the live connection
    /// if there is one, else by connecting.
    pub fn poke(self: &Arc<Self>, peer: [u8; 32]) {
        if let Some(link) = self.links.lock().unwrap().get(&peer) {
            let _ = link.tx.send(Cmd::News);
            return;
        }
        self.dial(peer);
    }

    /// Keeps a link open to every friend: reconnects dropped ones, and
    /// retries unreachable ones with backoff (sooner when messages wait).
    pub fn maintain(self: &Arc<Self>) {
        // Now and then, resend what live links haven't acknowledged (a
        // group message may have been unreadable until a change arrived).
        let tick = {
            let mut tick = self.ticks.lock().unwrap();
            *tick += 1;
            *tick
        };
        if tick % RESEND_EVERY == 0 {
            let pending = self.session.lock().unwrap().pending_peers().unwrap_or_default();
            for (peer, link) in self.links.lock().unwrap().iter() {
                if pending.contains(peer) {
                    let _ = link.tx.send(Cmd::News);
                }
            }
        }
        let (pending, friends) = {
            let s = self.session.lock().unwrap();
            (s.pending_peers().unwrap_or_default(), s.friend_keys().unwrap_or_default())
        };
        let now = Instant::now();
        for peer in pending.iter().chain(friends.iter().filter(|f| !pending.contains(f))) {
            if self.dialing.lock().unwrap().len() >= MAX_DIALS {
                break;
            }
            if self.links.lock().unwrap().contains_key(peer) {
                continue;
            }
            let due = self.backoff.lock().unwrap().get(peer).is_none_or(|(at, _)| *at <= now);
            if due {
                self.dial(*peer);
            }
        }
    }

    /// The app was closed (or opened): retry unreachable friends less (or
    /// more) often.
    pub fn set_background(&self, background: bool) {
        self.background.store(background, Ordering::Relaxed);
    }

    /// Connects to `peer` now unless already connected, skipping backoff
    /// (e.g. they just turned up nearby).
    pub fn dial_now(self: &Arc<Self>, peer: [u8; 32]) {
        if !self.links.lock().unwrap().contains_key(&peer) {
            self.dial(peer);
        }
    }

    /// Pull to refresh: try every friend not connected now, skipping backoff.
    pub fn reconnect_now(self: &Arc<Self>) {
        self.backoff.lock().unwrap().clear();
        self.maintain();
    }

    /// Until a link comes up, assume this attempt fails and schedule the next.
    fn back_off(&self, peer: &[u8; 32]) {
        let pending = self
            .session
            .lock()
            .unwrap()
            .pending_peers()
            .unwrap_or_default()
            .contains(peer);
        let cap = match (pending, self.background.load(Ordering::Relaxed)) {
            (true, false) => BACKOFF_MAX_PENDING,
            (false, false) => BACKOFF_MAX,
            (true, true) => BACKOFF_MAX_PENDING_BACKGROUND,
            (false, true) => BACKOFF_MAX_BACKGROUND,
        };
        let mut backoff = self.backoff.lock().unwrap();
        let delay = backoff.get(peer).map_or(BACKOFF_MIN, |(_, d)| (*d * 2).min(cap));
        backoff.insert(*peer, (Instant::now() + delay, delay));
    }

    /// Pokes every friend with something waiting.
    pub fn poke_pending(self: &Arc<Self>) {
        let peers = self
            .session
            .lock()
            .unwrap()
            .pending_peers()
            .unwrap_or_default();
        for peer in peers {
            self.poke(peer);
        }
    }

    /// Closes every link (the app going offline; also used in tests).
    pub fn shutdown_links(&self) {
        for link in self.links.lock().unwrap().values() {
            let _ = link.tx.send(Cmd::Close);
        }
    }

    /// Every `every`, for as long as the node lives: burns expired temp
    /// messages and keeps links to friends up ([`Node::maintain`]).
    pub fn spawn_retry_loop(self: &Arc<Self>, every: Duration) {
        let node = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(node) = node.upgrade() else { break };
                node.session.lock().unwrap().purge();
                node.emit_session_events();
                node.maintain();
            }
        });
    }

    fn dial(self: &Arc<Self>, peer: [u8; 32]) {
        if !self.dialing.lock().unwrap().insert(peer) {
            return;
        }
        self.back_off(&peer);
        let node = Arc::clone(self);
        tokio::spawn(async move {
            let conn = timeout(HANDSHAKE_TIMEOUT, node.transport.dial(peer)).await;
            node.dialing.lock().unwrap().remove(&peer);
            if let Ok(Ok(conn)) = conn {
                let _ = node.run(conn, Role::Dialed(peer)).await;
            }
        });
    }

    fn emit_session_events(self: &Arc<Self>) {
        let (events, pokes) = {
            let mut s = self.session.lock().unwrap();
            (s.take_events(), s.take_pokes())
        };
        for e in events {
            let _ = self.events.send(NetEvent::Session(e));
        }
        // E.g. a group change others must apply, found handling a frame.
        for peer in pokes {
            self.poke(peer);
        }
    }

    async fn run(self: Arc<Self>, mut conn: BoxConn, role: Role) -> anyhow::Result<()> {
        let mut buf = Vec::new();
        let peer = timeout(HANDSHAKE_TIMEOUT, async {
            match role {
                Role::Reached => {
                    let (nonce, challenge) = self.session.lock().unwrap().challenge();
                    write_frames(&mut conn, &[challenge]).await?;
                    let hello = read_frame(&mut conn, &mut buf).await?;
                    Ok::<_, anyhow::Error>(
                        self.session.lock().unwrap().accept_hello(&nonce, &hello)?,
                    )
                }
                Role::Dialed(peer) => {
                    let challenge = read_frame(&mut conn, &mut buf).await?;
                    let hello = self.session.lock().unwrap().hello(&peer, &challenge)?;
                    write_frames(&mut conn, &[hello]).await?;
                    Ok(peer)
                }
            }
        })
        .await??;

        if !self.session.lock().unwrap().knows(&peer)? {
            // A stranger: hear their friend request, then hang up.
            let frame = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut conn, &mut buf)).await??;
            self.session.lock().unwrap().on_stranger_frame(&peer, frame)?;
            self.emit_session_events();
            return Ok(());
        }

        let link = {
            let mut next = self.next_link.lock().unwrap();
            *next += 1;
            *next
        };
        let me = self.session.lock().unwrap().public_key();
        let preferred = match role {
            Role::Dialed(_) => me < peer,
            Role::Reached => peer < me,
        };
        let (tx, mut news) = mpsc::unbounded_channel();
        {
            let mut links = self.links.lock().unwrap();
            if let Some(old) = links.get(&peer) {
                if old.preferred && !preferred {
                    // Keep the link both phones agree on.
                    return Ok(());
                }
                let _ = old.tx.send(Cmd::Close);
            }
            links.insert(peer, Link { id: link, tx, preferred });
        }
        self.backoff.lock().unwrap().remove(&peer);
        let _ = self.events.send(NetEvent::PeerConnected(peer));

        let result = self.serve(&mut conn, &mut buf, &peer, &mut news).await;

        let mut links = self.links.lock().unwrap();
        if links.get(&peer).is_some_and(|l| l.id == link) {
            links.remove(&peer);
            drop(links);
            let _ = self.events.send(NetEvent::PeerDisconnected(peer));
        }
        result
    }

    async fn serve(
        self: &Arc<Self>,
        conn: &mut BoxConn,
        buf: &mut Vec<u8>,
        peer: &[u8; 32],
        news: &mut mpsc::UnboundedReceiver<Cmd>,
    ) -> anyhow::Result<()> {
        let first = {
            let mut session = self.session.lock().unwrap();
            session.link_started(peer);
            session.on_link_up(peer)?
        };
        write_frames(conn, &first).await?;
        self.emit_session_events();
        // Frames may have arrived together with the handshake.
        self.handle_buffered(conn, buf, peer).await?;
        let mut chunk = [0u8; 16 * 1024];
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        loop {
            tokio::select! {
                read = timeout(IDLE_TIMEOUT, conn.read(&mut chunk)) => {
                    let n = read??;
                    if n == 0 {
                        return Ok(());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    self.handle_buffered(conn, buf, peer).await?;
                }
                cmd = news.recv() => match cmd {
                    Some(Cmd::News) => {
                        let out = self.session.lock().unwrap().on_link_up(peer)?;
                        write_frames(conn, &out).await?;
                        self.emit_session_events();
                    }
                    Some(Cmd::Close) | None => return Ok(()),
                },
                _ = ping.tick() => write_frames(conn, &[Frame::Ping]).await?,
            }
        }
    }
}

impl Node {
    async fn handle_buffered(
        self: &Arc<Self>,
        conn: &mut BoxConn,
        buf: &mut Vec<u8>,
        peer: &[u8; 32],
    ) -> anyhow::Result<()> {
        while let Some(frame) = take_frame(buf)? {
            let out = self.session.lock().unwrap().on_frame(peer, frame)?;
            write_frames(conn, &out).await?;
            self.emit_session_events();
        }
        Ok(())
    }
}

async fn write_frames(conn: &mut BoxConn, frames: &[Frame]) -> anyhow::Result<()> {
    if frames.is_empty() {
        return Ok(());
    }
    let bytes: Vec<u8> = frames.iter().flat_map(encode_frame).collect();
    conn.write_all(&bytes).await?;
    conn.flush().await?;
    Ok(())
}

async fn read_frame(conn: &mut BoxConn, buf: &mut Vec<u8>) -> anyhow::Result<Frame> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(frame) = take_frame(buf)? {
            return Ok(frame);
        }
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("closed during handshake");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sanctum_core::identity::Identity;
    use sanctum_core::store::ChatKind;
    use std::net::SocketAddr;
    use tempfile::TempDir;
    use tokio::net::{TcpListener, TcpStream};

    /// Loopback TCP standing in for Tor: each phone listens on a port.
    struct Tcp {
        book: Arc<Mutex<HashMap<[u8; 32], SocketAddr>>>,
    }

    #[async_trait]
    impl Transport for Tcp {
        async fn dial(&self, peer: [u8; 32]) -> anyhow::Result<BoxConn> {
            let addr = *self
                .book
                .lock()
                .unwrap()
                .get(&peer)
                .ok_or_else(|| anyhow::anyhow!("offline"))?;
            Ok(Box::new(TcpStream::connect(addr).await?))
        }
    }

    struct Phone {
        node: Arc<Node>,
        session: Arc<Mutex<Session>>,
        pk: [u8; 32],
        events: broadcast::Receiver<NetEvent>,
        _dir: TempDir,
    }

    async fn phone(book: &Arc<Mutex<HashMap<[u8; 32], SocketAddr>>>) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::open(
            Identity::generate().secret(),
            dir.path().join("s.db"),
            &rand::random(),
        )
        .unwrap();
        let pk = s.public_key();
        let session = Arc::new(Mutex::new(s));
        let node = Node::new(session.clone(), Arc::new(Tcp { book: book.clone() }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        book.lock()
            .unwrap()
            .insert(pk, listener.local_addr().unwrap());
        let n = node.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                n.accept(Box::new(stream));
            }
        });
        let events = node.subscribe();
        Phone {
            node,
            session,
            pk,
            events,
            _dir: dir,
        }
    }

    async fn befriend(a: &Phone, b: &Phone) -> (i64, i64) {
        let a_code = a.session.lock().unwrap().my_code("Alice");
        let b_code = b.session.lock().unwrap().my_code("Bob");
        let ab = a
            .session
            .lock()
            .unwrap()
            .add_friend(&b_code, true)
            .unwrap()
            .chat_id;
        let ba = b
            .session
            .lock()
            .unwrap()
            .add_friend(&a_code, true)
            .unwrap()
            .chat_id;
        (ab, ba)
    }

    /// Waits until `cond` holds.
    async fn until(cond: impl Fn() -> bool) {
        timeout(Duration::from_secs(10), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("condition never held");
    }

    /// Waits for an event matching `want`.
    async fn expect(events: &mut broadcast::Receiver<NetEvent>, want: impl Fn(&NetEvent) -> bool) {
        timeout(Duration::from_secs(10), async {
            loop {
                if want(&events.recv().await.unwrap()) {
                    return;
                }
            }
        })
        .await
        .expect("event did not arrive");
    }

    #[tokio::test]
    async fn message_travels_over_a_real_connection_and_is_acknowledged() {
        let book = Arc::default();
        let (mut alice, mut bob) = (phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;

        let peer = alice
            .session
            .lock()
            .unwrap()
            .send_text(ab, "hello over the wire")
            .unwrap()
            .unwrap();
        alice.node.poke(peer);

        let session = bob.session.clone();
        until(move || {
            let got = session.lock().unwrap().store.messages(ba, None).unwrap();
            got.first().is_some_and(|m| m.body == "hello over the wire")
        })
        .await;
        let session = alice.session.clone();
        until(move || session.lock().unwrap().store.messages(ab, None).unwrap()[0].delivered).await;
        let _ = (&mut alice.events, &mut bob.events);
    }

    #[tokio::test]
    async fn both_dialing_at_once_settle_on_one_link() {
        let book = Arc::default();
        let (mut alice, bob) = (phone(&book).await, phone(&book).await);
        befriend(&alice, &bob).await;
        // Nothing to send, yet both connect, each dialing the other.
        alice.node.spawn_retry_loop(Duration::from_millis(50));
        bob.node.spawn_retry_loop(Duration::from_millis(50));
        expect(&mut alice.events, |e| *e == NetEvent::PeerConnected(bob.pk)).await;
        let (a, b) = (alice.node.clone(), bob.node.clone());
        until(move || a.connected() == [bob.pk] && b.connected() == [alice.pk]).await;
        // Count connects from here: a stable link means none.
        let mut events = alice.node.subscribe();
        tokio::time::sleep(Duration::from_millis(800)).await;
        while let Ok(e) = events.try_recv() {
            assert!(!matches!(e, NetEvent::PeerDisconnected(_)), "links keep flapping");
        }
        assert_eq!(alice.node.connected(), [bob.pk]);
    }

    #[tokio::test]
    async fn dropped_links_reconnect_on_their_own() {
        let book = Arc::default();
        let (mut alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        alice.node.spawn_retry_loop(Duration::from_millis(50));
        let b = bob.node.clone();
        until(move || b.connected() == [alice.pk]).await;

        // Bob drops off the network.
        let addr = book.lock().unwrap().remove(&bob.pk).unwrap();
        bob.node.shutdown_links();
        expect(&mut alice.events, |e| *e == NetEvent::PeerDisconnected(bob.pk)).await;
        alice.session.lock().unwrap().send_text(ab, "back?").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(alice.node.connected().is_empty());

        // He's back: Alice reconnects without being told and delivers.
        book.lock().unwrap().insert(bob.pk, addr);
        let session = bob.session.clone();
        until(move || {
            session.lock().unwrap().store.messages(ba, None).unwrap().iter().any(|m| m.body == "back?")
        })
        .await;
    }

    #[tokio::test]
    async fn a_group_works_over_real_connections() {
        let book = Arc::default();
        let (alice, bob, carol) = (phone(&book).await, phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        let (ac, _) = befriend(&alice, &carol).await;
        for p in [&alice, &bob, &carol] {
            p.node.spawn_retry_loop(Duration::from_millis(50));
        }
        let (group, invitees) = alice.session.lock().unwrap().create_group("Trip", &[ab, ac]).unwrap();
        for p in invitees {
            alice.node.poke(p);
        }
        // Both join, and learn about each other, on their own.
        let group_on = |p: &Phone| {
            p.session
                .lock()
                .unwrap()
                .store
                .chats()
                .unwrap()
                .into_iter()
                .find(|(_, m)| m.kind == ChatKind::Group)
                .map(|(id, m)| (id, m.peers.len()))
        };
        let (b, c) = (bob.session.clone(), carol.session.clone());
        until(move || {
            let joined = |s: &Arc<Mutex<Session>>| {
                s.lock().unwrap().store.chats().unwrap().iter().any(|(_, m)| m.kind == ChatKind::Group && m.peers.len() == 2)
            };
            joined(&b) && joined(&c)
        })
        .await;
        let (cg, _) = group_on(&carol).unwrap();
        // Carol writes; Bob, who isn't her friend, gets it from her directly.
        let reach = carol.session.lock().unwrap().send_to_chat(cg, "hi all", None).unwrap();
        for p in reach {
            carol.node.poke(p);
        }
        let (bg, _) = group_on(&bob).unwrap();
        let session = bob.session.clone();
        until(move || {
            session.lock().unwrap().store.messages(bg, None).unwrap().iter().any(|m| m.body == "hi all")
        })
        .await;
        let session = alice.session.clone();
        until(move || {
            session.lock().unwrap().store.messages(group, None).unwrap().iter().any(|m| m.body == "hi all")
        })
        .await;
        // Delivered to everyone.
        let session = carol.session.clone();
        until(move || session.lock().unwrap().store.messages(cg, None).unwrap()[0].delivered).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_photo_crosses_a_real_connection() {
        let book = Arc::default();
        let (alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        alice.node.spawn_retry_loop(Duration::from_millis(50));
        let bytes: Vec<u8> = (0..400_000u32).map(|i| (i % 253) as u8).collect();
        let path = alice._dir.path().join("p.jpg");
        std::fs::write(&path, &bytes).unwrap();
        let out = sanctum_core::media::Outgoing {
            path: &path,
            kind: sanctum_core::protocol::MediaKind::Image,
            mime: "image/jpeg",
            name: "p.jpg",
            caption: "",
            thumb: Vec::new(),
            width: 1,
            height: 1,
            duration_ms: 0,
            album: None,
        };
        let reach = alice.session.lock().unwrap().send_media(ab, out, None).unwrap();
        for p in reach {
            alice.node.poke(p);
        }
        let session = bob.session.clone();
        until(move || matches!(session.lock().unwrap().media_bytes(ba, 0), Ok(Some(_)))).await;
        assert_eq!(bob.session.lock().unwrap().media_bytes(ba, 0).unwrap().unwrap(), bytes);
        let session = alice.session.clone();
        until(move || session.lock().unwrap().store.messages(ab, None).unwrap()[0].delivered).await;
    }

    #[tokio::test]
    async fn one_code_is_enough_a_friend_request_does_the_rest() {
        let book = Arc::default();
        let (alice, mut bob) = (phone(&book).await, phone(&book).await);
        alice.session.lock().unwrap().set_my_name("Alice");
        let b_code = bob.session.lock().unwrap().my_code("Bob");
        let ab = alice
            .session
            .lock()
            .unwrap()
            .add_friend(&b_code, false)
            .unwrap()
            .chat_id;
        alice.session.lock().unwrap().send_text(ab, "add me?").unwrap();
        alice.node.spawn_retry_loop(Duration::from_millis(200));
        bob.node.spawn_retry_loop(Duration::from_millis(200));
        alice.node.poke_pending();

        expect(&mut bob.events, |e| *e == NetEvent::Session(Event::Request)).await;
        let ba = {
            let mut s = bob.session.lock().unwrap();
            assert_eq!(s.requests()[0].name, "Alice");
            s.accept_request(&alice.pk).unwrap().chat_id
        };
        bob.node.poke_pending();
        let session = bob.session.clone();
        until(move || {
            session
                .lock()
                .unwrap()
                .store
                .messages(ba, None)
                .unwrap()
                .iter()
                .any(|m| m.body == "add me?")
        })
        .await;
    }

    #[tokio::test]
    async fn news_flows_over_an_open_connection_both_ways() {
        let book = Arc::default();
        let (mut alice, mut bob) = (phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        let p = alice
            .session
            .lock()
            .unwrap()
            .send_text(ab, "first")
            .unwrap()
            .unwrap();
        alice.node.poke(p);
        expect(&mut bob.events, |e| {
            *e == NetEvent::Session(Event::Chat(ba))
        })
        .await;

        // Bob answers over the same connection (no new dial needed).
        let p = bob
            .session
            .lock()
            .unwrap()
            .send_text(ba, "reply")
            .unwrap()
            .unwrap();
        assert!(bob.node.connected().contains(&p));
        bob.node.poke(p);
        // Chat(ab) also fires for acks of our own message, so wait for the
        // reply itself.
        let session = alice.session.clone();
        until(move || {
            session
                .lock()
                .unwrap()
                .store
                .messages(ab, None)
                .unwrap()
                .iter()
                .any(|m| m.body == "reply")
        })
        .await;
        let _ = &mut alice.events;
    }

    #[tokio::test]
    async fn temp_chat_and_wipe_over_the_wire() {
        let book = Arc::default();
        let (mut alice, mut bob) = (phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        let t = alice.session.lock().unwrap().temp_new(Some(ab)).unwrap();
        let p = alice
            .session
            .lock()
            .unwrap()
            .temp_send(t, "burn after reading")
            .unwrap()
            .unwrap();
        alice.node.poke(p);
        expect(&mut bob.events, |e| {
            matches!(e, NetEvent::Session(Event::Temp(_)))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let bt = {
            let s = bob.session.lock().unwrap();
            let chat = &s.temp.all()[0];
            assert_eq!(
                chat.messages.last().map(|m| m.body.as_str()),
                Some("burn after reading")
            );
            chat.id
        };
        let p = bob.session.lock().unwrap().temp_wipe(bt).unwrap();
        bob.node.poke(p);
        expect(&mut alice.events, |e| {
            *e == NetEvent::Session(Event::TempGone(t))
        })
        .await;
        assert!(alice.session.lock().unwrap().temp.get(t).is_none());
    }

    #[tokio::test]
    async fn offline_friend_gets_messages_when_back() {
        let book: Arc<Mutex<HashMap<[u8; 32], SocketAddr>>> = Arc::default();
        let (alice, mut bob) = (phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        // Bob is unreachable.
        let bob_addr = book.lock().unwrap().remove(&bob.pk).unwrap();
        let p = alice
            .session
            .lock()
            .unwrap()
            .send_text(ab, "while you were away")
            .unwrap()
            .unwrap();
        alice.node.poke(p);
        alice.node.spawn_retry_loop(Duration::from_millis(200));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !alice
                .session
                .lock()
                .unwrap()
                .store
                .messages(ab, None)
                .unwrap()[0]
                .delivered
        );
        // Bob comes back; the retry loop delivers.
        book.lock().unwrap().insert(bob.pk, bob_addr);
        expect(&mut bob.events, |e| {
            *e == NetEvent::Session(Event::Chat(ba))
        })
        .await;
    }

    #[tokio::test]
    async fn strangers_are_cut_off() {
        let book = Arc::default();
        let (alice, mut bob) = (phone(&book).await, phone(&book).await);
        // Not friends: bob refuses alice's hello.
        alice.node.poke(bob.pk);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(bob.node.connected().is_empty());
        assert!(bob.events.try_recv().is_err());
    }
}
