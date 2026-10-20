//! Runs connections between friends: handshake, then frames both ways,
//! driving the core [`Session`] engine. Transport-agnostic: Tor in the app,
//! plain TCP in tests.

use async_trait::async_trait;
use sanctum_core::protocol::{encode_frame, take_frame, Frame};
use sanctum_core::session::{Event, Session};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
/// Idle connections are closed; a new one is made when there is news.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// A live connection: its id, and a channel asking it to send news.
type Link = (u64, mpsc::UnboundedSender<()>);

pub struct Node {
    session: Arc<Mutex<Session>>,
    transport: Arc<dyn Transport>,
    events: broadcast::Sender<NetEvent>,
    /// Live connections, each with a channel to ask it to send news.
    links: Mutex<HashMap<[u8; 32], Link>>,
    dialing: Mutex<HashSet<[u8; 32]>>,
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
        if let Some((_, tx)) = self.links.lock().unwrap().get(&peer) {
            let _ = tx.send(());
            return;
        }
        self.dial(peer);
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

    /// Retries undelivered messages every `every`, for as long as the node
    /// lives.
    pub fn spawn_retry_loop(self: &Arc<Self>, every: Duration) {
        let node = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(node) = node.upgrade() else { break };
                node.session.lock().unwrap().purge();
                node.emit_session_events();
                node.poke_pending();
            }
        });
    }

    fn dial(self: &Arc<Self>, peer: [u8; 32]) {
        if !self.dialing.lock().unwrap().insert(peer) {
            return;
        }
        let node = Arc::clone(self);
        tokio::spawn(async move {
            let conn = timeout(HANDSHAKE_TIMEOUT, node.transport.dial(peer)).await;
            node.dialing.lock().unwrap().remove(&peer);
            if let Ok(Ok(conn)) = conn {
                let _ = node.run(conn, Role::Dialed(peer)).await;
            }
        });
    }

    fn emit_session_events(&self) {
        let events = self.session.lock().unwrap().take_events();
        for e in events {
            let _ = self.events.send(NetEvent::Session(e));
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

        let link = {
            let mut next = self.next_link.lock().unwrap();
            *next += 1;
            *next
        };
        let (tx, mut news) = mpsc::unbounded_channel();
        self.links.lock().unwrap().insert(peer, (link, tx));
        let _ = self.events.send(NetEvent::PeerConnected(peer));

        let result = self.serve(&mut conn, &mut buf, &peer, &mut news).await;

        let mut links = self.links.lock().unwrap();
        if links.get(&peer).is_some_and(|(id, _)| *id == link) {
            links.remove(&peer);
            drop(links);
            let _ = self.events.send(NetEvent::PeerDisconnected(peer));
        }
        result
    }

    async fn serve(
        &self,
        conn: &mut BoxConn,
        buf: &mut Vec<u8>,
        peer: &[u8; 32],
        news: &mut mpsc::UnboundedReceiver<()>,
    ) -> anyhow::Result<()> {
        let first = self.session.lock().unwrap().on_link_up(peer)?;
        write_frames(conn, &first).await?;
        self.emit_session_events();
        // Frames may have arrived together with the handshake.
        self.handle_buffered(conn, buf, peer).await?;
        let mut chunk = [0u8; 16 * 1024];
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
                Some(()) = news.recv() => {
                    let out = self.session.lock().unwrap().on_link_up(peer)?;
                    write_frames(conn, &out).await?;
                    self.emit_session_events();
                }
            }
        }
    }
}

impl Node {
    async fn handle_buffered(
        &self,
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
            .add_friend(&b_code)
            .unwrap()
            .chat_id;
        let ba = b
            .session
            .lock()
            .unwrap()
            .add_friend(&a_code)
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

        expect(&mut bob.events, |e| {
            *e == NetEvent::Session(Event::Chat(ba))
        })
        .await;
        let got = bob
            .session
            .lock()
            .unwrap()
            .store
            .messages(ba, None)
            .unwrap();
        assert_eq!(got[0].body, "hello over the wire");

        expect(&mut alice.events, |e| {
            *e == NetEvent::Session(Event::Chat(ab))
        })
        .await;
        // The ack may land just after the event; give it a moment.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            alice
                .session
                .lock()
                .unwrap()
                .store
                .messages(ab, None)
                .unwrap()[0]
                .delivered
        );
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
