//! Runs connections between friends: handshake, then frames both ways,
//! driving the core [`Session`] engine. Transport-agnostic: Tor in the app,
//! plain TCP in tests.

use crate::direct::DirectSocket;
use async_trait::async_trait;
use sanctum_core::call::{MediaHub, Outgoing, Route, VideoSent};
use sanctum_core::protocol::{
    encode_frame, encode_frame_with_features, take_frame, take_frame_features, Frame, FEATURES,
};
use sanctum_core::session::{Event, Session};
use sanctum_core::video::{Backlog, FrameInfo};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
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

    /// Dials, and says whether the link is nearby (the local Wi-Fi or
    /// Bluetooth) rather than over Tor.
    async fn dial_ranked(&self, peer: [u8; 32]) -> anyhow::Result<(BoxConn, bool)> {
        Ok((self.dial(peer).await?, false))
    }

    /// Whether `peer` can be reached nearby right now.
    fn nearby(&self, _peer: &[u8; 32]) -> bool {
        false
    }
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
/// While a call goes on, its media's housekeeping (direct-path probes,
/// camera news, keyframe requests) runs this often.
const CALL_TICK: Duration = Duration::from_millis(50);
/// Answers bigger than this are written a frame at a time, between reads.
const WRITE_AT_ONCE: usize = 64 * 1024;
/// A friend linked over Tor who is also nearby is offered the nearby link
/// at most this often.
const UPGRADE_EVERY: Duration = Duration::from_secs(60);

enum Cmd {
    /// There's news to send.
    News,
    /// A better link to the same friend replaced this one.
    Close,
    /// Send this as is, now (a typing signal: never kept or resent).
    Send(Frame),
    /// A call's sound: like `Send`, counted in the link's [`Backlog`].
    Media(Frame),
}

/// A piece of a call's picture, and its share of the bytes counted in the
/// link's [`Backlog`]. Written only when no sound is waiting.
type VideoPiece = (Frame, usize);

/// A live connection.
struct Link {
    id: u64,
    tx: mpsc::UnboundedSender<Cmd>,
    /// Dialed by the friend with the smaller key. When both phones dial at
    /// once, both keep this one, so they never close each other's.
    preferred: bool,
    /// Over the local Wi-Fi or Bluetooth rather than Tor: faster, so it
    /// wins over a Tor link to the same friend.
    local: bool,
    /// Pictures queue here, behind everything on `tx`.
    video: mpsc::UnboundedSender<VideoPiece>,
    /// Call packets queued on this link and not yet written.
    backlog: Arc<Backlog>,
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
    /// The call's media, taken straight from links (no session lock).
    media: MediaHub,
    /// When each friend was last offered a nearby link instead of Tor.
    upgrades: Mutex<HashMap<[u8; 32], Instant>>,
    /// The call's UDP socket, with "Faster calls" on.
    direct: Mutex<Option<DirectSocket>>,
    /// The call's housekeeping is running.
    pumping: AtomicBool,
}

enum Role {
    /// `true`: on the nearby listener.
    Reached(bool),
    /// `true`: dialed nearby.
    Dialed([u8; 32], bool),
}

impl Node {
    pub fn new(session: Arc<Mutex<Session>>, transport: Arc<dyn Transport>) -> Arc<Self> {
        let media = session.lock().unwrap().media();
        Arc::new(Self {
            media,
            upgrades: Mutex::default(),
            direct: Mutex::default(),
            pumping: AtomicBool::new(false),
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
            let _ = node.run(conn, Role::Reached(false)).await;
        });
    }

    /// Serves a connection a nearby friend made to us (local Wi-Fi or
    /// Bluetooth).
    pub fn accept_nearby(self: &Arc<Self>, conn: BoxConn) {
        let node = Arc::clone(self);
        tokio::spawn(async move {
            let _ = node.run(conn, Role::Reached(true)).await;
        });
    }

    /// Whether the link to `peer` is up and nearby (not over Tor).
    pub fn is_nearby(&self, peer: &[u8; 32]) -> bool {
        self.links.lock().unwrap().get(peer).is_some_and(|l| l.local)
    }

    /// The friend is linked over Tor but also nearby: try the faster
    /// nearby link, which replaces the Tor one once up. At most once a
    /// minute per friend, unless `now` (a call is starting).
    pub fn prefer_nearby(self: &Arc<Self>, peer: [u8; 32], now: bool) {
        let linked_far = self.links.lock().unwrap().get(&peer).is_some_and(|l| !l.local);
        if !linked_far || !self.transport.nearby(&peer) {
            return;
        }
        let at = Instant::now();
        {
            let mut upgrades = self.upgrades.lock().unwrap();
            if !now && upgrades.get(&peer).is_some_and(|last| at.duration_since(*last) < UPGRADE_EVERY) {
                return;
            }
            upgrades.insert(peer, at);
        }
        self.dial(peer);
    }

    /// Seals a call's sound and sends it to the friend in the call, over
    /// the direct path if one is up, else the link that's up now. Dropped
    /// when there's no call, no way to them, or the link is falling
    /// behind. Returns whether it went.
    pub fn send_call_audio(&self, data: &[u8]) -> bool {
        let Some(out) = self.media.seal_audio(data) else {
            return false;
        };
        if let Some((to, datagram)) = out.datagram() {
            return self.send_direct(to, &datagram);
        }
        let links = self.links.lock().unwrap();
        let Some(link) = links.get(&out.peer) else {
            return false;
        };
        if !link.backlog.take_audio() {
            return false;
        }
        if link.tx.send(Cmd::Media(out.frame)).is_err() {
            link.backlog.audio_done();
            return false;
        }
        true
    }

    /// Seals an encoded video frame and sends it, in pieces, if our camera
    /// is on and the path has room: over the direct path if one is up,
    /// else behind the sound on the link. `want_key`: the encoder should
    /// make its next frame a keyframe.
    pub fn send_call_video(&self, data: &[u8], info: FrameInfo) -> VideoSent {
        let link = self
            .media
            .peer()
            .and_then(|peer| self.links.lock().unwrap().get(&peer).map(|l| (l.video.clone(), l.backlog.clone())));
        let mut admitted = 0;
        let sent = self.media.seal_video(data, info, |route, bytes| {
            let ok = match (route, &link) {
                (Route::Direct(_), _) => true,
                (Route::Link, Some((_, backlog))) => backlog.take_video(bytes),
                (Route::Link, None) => false,
            };
            if ok {
                admitted = bytes;
            }
            ok
        });
        let count = sent.packets.len();
        for (i, out) in sent.packets.iter().enumerate() {
            if let Some((to, datagram)) = out.datagram() {
                self.send_direct(to, &datagram);
                continue;
            }
            let Some((video, backlog)) = &link else { break };
            // Each piece carries its share of what was counted.
            let share = admitted / count + if i + 1 == count { admitted % count } else { 0 };
            if video.send((out.frame.clone(), share)).is_err() {
                backlog.video_done(share);
            }
        }
        sent
    }

    /// Sends what the call's media has queued (probes, answers, camera
    /// news, keyframe requests).
    pub fn flush_media(&self) {
        let out = self.media.take_out();
        if !out.is_empty() {
            self.send_out(out);
        }
    }

    fn send_out(&self, out: Vec<Outgoing>) {
        for o in out {
            match o.datagram() {
                Some((to, datagram)) => {
                    self.send_direct(to, &datagram);
                }
                None => {
                    self.send_live(&o.peer, o.frame);
                }
            }
        }
    }

    fn send_direct(&self, to: SocketAddr, datagram: &[u8]) -> bool {
        self.direct.lock().unwrap().as_ref().is_some_and(|d| d.send(to, datagram))
    }

    /// Opens this call's UDP socket for "Faster calls" on `bind` (port 0:
    /// any free one; an unspecified address: everywhere). Returns its port.
    pub fn direct_open(self: &Arc<Self>, bind: Option<SocketAddr>) -> std::io::Result<u16> {
        let node = Arc::downgrade(self);
        let answered = move || {
            if let Some(node) = node.upgrade() {
                node.flush_media();
            }
        };
        let socket = match bind {
            Some(bind) => DirectSocket::open(bind, self.media.clone(), answered)?,
            None => DirectSocket::open_any(self.media.clone(), answered)?,
        };
        let port = socket.port();
        *self.direct.lock().unwrap() = Some(socket);
        Ok(port)
    }

    /// Closes the call's UDP socket.
    pub fn direct_close(&self) {
        self.direct.lock().unwrap().take();
    }

    /// Whether the call's media goes straight to the friend's phone now.
    pub fn call_direct(&self) -> bool {
        self.media.info().is_some_and(|i| i.direct)
    }

    /// A call began (ringing, or calling out): runs its housekeeping until
    /// it ends, then closes the UDP socket.
    pub fn call_started(self: &Arc<Self>) {
        if self.pumping.swap(true, Ordering::AcqRel) {
            return;
        }
        let node = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(CALL_TICK);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                let Some(node) = node.upgrade() else { break };
                if !node.media.active() {
                    node.direct_close();
                    node.pumping.store(false, Ordering::Release);
                    // A new call may have started just now.
                    if node.media.active() && !node.pumping.swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    break;
                }
                let fast = node.media.peer().is_some_and(|p| node.is_nearby(&p));
                node.media.tick(Instant::now(), fast);
                node.flush_media();
            }
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

    /// We're writing in this chat: tells whoever in it is connected right
    /// now. Nobody is dialed and nothing is kept for anyone else. At most
    /// one signal per chat every few seconds (see `sanctum_core::typing`).
    /// Returns how many got it.
    pub fn typing(&self, chat_id: i64) -> anyhow::Result<usize> {
        let Some((frame, to)) = self.session.lock().unwrap().typing(chat_id)? else {
            return Ok(0);
        };
        Ok(to.iter().filter(|peer| self.send_live(peer, frame.clone())).count())
    }

    /// Sends `frame` to `peer` if a link to them is up; otherwise drops it.
    fn send_live(&self, peer: &[u8; 32], frame: Frame) -> bool {
        self.links.lock().unwrap().get(peer).is_some_and(|link| link.tx.send(Cmd::Send(frame)).is_ok())
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
            let linked = self.connected();
            let (pending, swap) = {
                let mut s = self.session.lock().unwrap();
                // Group members linked to us also swap what's new in the
                // groups we share (see `sanctum_core::group_sync`).
                (s.pending_peers().unwrap_or_default(), s.sync_due(&linked).unwrap_or_default())
            };
            for (peer, link) in self.links.lock().unwrap().iter() {
                if pending.contains(peer) || swap.contains(peer) {
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
            let conn = timeout(HANDSHAKE_TIMEOUT, node.transport.dial_ranked(peer)).await;
            node.dialing.lock().unwrap().remove(&peer);
            if let Ok(Ok((conn, local))) = conn {
                let _ = node.run(conn, Role::Dialed(peer, local)).await;
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
        // Each side's first frame also says what its build can do; older
        // builds don't say, and ignore what they're told.
        let (peer, features) = timeout(HANDSHAKE_TIMEOUT, async {
            match role {
                Role::Reached(_) => {
                    let (nonce, challenge) = self.session.lock().unwrap().challenge();
                    write_handshake(&mut conn, &challenge).await?;
                    let (hello, features) = read_handshake(&mut conn, &mut buf).await?;
                    let peer = self.session.lock().unwrap().accept_hello(&nonce, &hello)?;
                    Ok::<_, anyhow::Error>((peer, features))
                }
                Role::Dialed(peer, _) => {
                    let (challenge, features) = read_handshake(&mut conn, &mut buf).await?;
                    let hello = self.session.lock().unwrap().hello(&peer, &challenge)?;
                    write_handshake(&mut conn, &hello).await?;
                    Ok((peer, features))
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
        let (preferred, local) = match role {
            Role::Dialed(_, local) => (me < peer, local),
            Role::Reached(local) => (peer < me, local),
        };
        let (tx, mut news) = mpsc::unbounded_channel();
        let (video, mut pictures) = mpsc::unbounded_channel();
        let backlog = Arc::new(Backlog::default());
        {
            let mut links = self.links.lock().unwrap();
            if let Some(old) = links.get(&peer) {
                // A nearby link beats one over Tor; between two of a kind,
                // keep the link both phones agree on.
                let keep_old = if old.local != local { old.local } else { old.preferred && !preferred };
                if keep_old {
                    return Ok(());
                }
                let _ = old.tx.send(Cmd::Close);
            }
            links.insert(peer, Link { id: link, tx, video, preferred, local, backlog: backlog.clone() });
        }
        self.backoff.lock().unwrap().remove(&peer);
        let _ = self.events.send(NetEvent::PeerConnected(peer));

        let result = self.serve(&mut conn, &mut buf, &peer, features, &mut news, &mut pictures, &backlog).await;

        let mut links = self.links.lock().unwrap();
        if links.get(&peer).is_some_and(|l| l.id == link) {
            links.remove(&peer);
            drop(links);
            let _ = self.events.send(NetEvent::PeerDisconnected(peer));
        }
        result
    }

    #[allow(clippy::too_many_arguments)] // one link's state
    async fn serve(
        self: &Arc<Self>,
        conn: &mut BoxConn,
        buf: &mut Vec<u8>,
        peer: &[u8; 32],
        features: u32,
        news: &mut mpsc::UnboundedReceiver<Cmd>,
        pictures: &mut mpsc::UnboundedReceiver<VideoPiece>,
        backlog: &Backlog,
    ) -> anyhow::Result<()> {
        let first = {
            let mut session = self.session.lock().unwrap();
            session.link_started(peer);
            session.link_features(peer, features)?;
            session.on_link_up(peer)?
        };
        self.send_paced(conn, peer, first).await?;
        self.emit_session_events();
        // Frames may have arrived together with the handshake.
        self.handle_buffered(conn, buf, peer).await?;
        let mut chunk = [0u8; 16 * 1024];
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        loop {
            // Pictures go only when nothing else waits to be sent.
            let idle = news.is_empty();
            tokio::select! {
                // No fixed order between sending and reading: always
                // writing first could leave both phones writing, each
                // waiting for the other to read (a busy sender is poked
                // again before its last write is done).
                cmd = news.recv() => match cmd {
                    Some(Cmd::News) => {
                        let out = self.session.lock().unwrap().on_link_up(peer)?;
                        self.send_paced(conn, peer, out).await?;
                        self.emit_session_events();
                    }
                    Some(Cmd::Send(frame)) => write_frames(conn, &[frame]).await?,
                    Some(Cmd::Media(frame)) => {
                        let written = write_frames(conn, &[frame]).await;
                        backlog.audio_done();
                        written?;
                    }
                    Some(Cmd::Close) | None => return Ok(()),
                },
                read = timeout(IDLE_TIMEOUT, conn.read(&mut chunk)) => {
                    let n = read??;
                    if n == 0 {
                        return Ok(());
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    self.handle_buffered(conn, buf, peer).await?;
                }
                Some((frame, bytes)) = pictures.recv(), if idle => {
                    let written = write_frames(conn, &[frame]).await;
                    backlog.video_done(bytes);
                    written?;
                }
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
            // A call's sound goes straight to the media, without waiting for
            // the session (which may be busy encrypting or saving).
            if let Frame::CallMedia { call, seq, sealed } = &frame {
                if self.media.receive(peer, call, *seq, sealed) {
                    // E.g. a keyframe request to answer.
                    self.flush_media();
                }
                continue;
            }
            let out = self.session.lock().unwrap().on_frame(peer, frame)?;
            self.send_paced(conn, peer, out).await?;
            self.emit_session_events();
        }
        Ok(())
    }

    /// Writes `frames` now, up to [`WRITE_AT_ONCE`] bytes; the rest queue on
    /// the link and go one at a time between reads, so a big answer (a
    /// file a member asked for, say) never keeps this side from reading
    /// while the other side is writing too.
    async fn send_paced(&self, conn: &mut BoxConn, peer: &[u8; 32], frames: Vec<Frame>) -> anyhow::Result<()> {
        let mut size = 0;
        let split = frames
            .iter()
            .position(|f| {
                size += encoded_len(f);
                size > WRITE_AT_ONCE
            })
            .map_or(frames.len(), |i| i.max(1));
        let mut frames = frames;
        let later = frames.split_off(split);
        write_frames(conn, &frames).await?;
        if !later.is_empty() {
            let tx = self.links.lock().unwrap().get(peer).map(|l| l.tx.clone());
            match tx {
                Some(tx) => {
                    for frame in later {
                        let _ = tx.send(Cmd::Send(frame));
                    }
                }
                None => write_frames(conn, &later).await?,
            }
        }
        Ok(())
    }
}

fn encoded_len(frame: &Frame) -> usize {
    match frame {
        Frame::BlobChunk { bytes, .. } | Frame::GroupMls { bytes, .. } => bytes.len() + 64,
        _ => 64,
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

async fn write_handshake(conn: &mut BoxConn, frame: &Frame) -> anyhow::Result<()> {
    conn.write_all(&encode_frame_with_features(frame, FEATURES)).await?;
    conn.flush().await?;
    Ok(())
}

async fn read_handshake(conn: &mut BoxConn, buf: &mut Vec<u8>) -> anyhow::Result<(Frame, u32)> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(got) = take_frame_features(buf)? {
            return Ok(got);
        }
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("closed during handshake");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
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

    /// The group chat on this phone, once it has `members` people.
    fn group_chat(p: &Phone, members: usize) -> Option<i64> {
        p.session
            .lock()
            .unwrap()
            .store
            .chats()
            .unwrap()
            .into_iter()
            .find(|(_, m)| m.kind == ChatKind::Group && m.peers.len() + 1 == members)
            .map(|(id, _)| id)
    }

    /// A group of everyone in `phones`, the first its admin (friends with
    /// all). Returns each one's chat.
    async fn net_group(phones: &[&Phone]) -> Vec<i64> {
        let admin = phones[0];
        let mut friends = Vec::new();
        for p in &phones[1..] {
            friends.push(befriend(admin, p).await.0);
        }
        admin.node.spawn_retry_loop(Duration::from_millis(50));
        let (chat, invitees) = admin.session.lock().unwrap().create_group("Crew", &friends).unwrap();
        for p in invitees {
            admin.node.poke(p);
        }
        let n = phones.len();
        let mut chats = vec![chat];
        for p in &phones[1..] {
            let s = p.session.clone();
            until(move || {
                let p = s.lock().unwrap();
                p.store.chats().unwrap().iter().any(|(_, m)| m.kind == ChatKind::Group && m.peers.len() + 1 == n)
            })
            .await;
            chats.push(group_chat(p, n).unwrap());
        }
        chats
    }

    fn offline(book: &Arc<Mutex<HashMap<[u8; 32], SocketAddr>>>, p: &Phone) -> SocketAddr {
        let addr = book.lock().unwrap().remove(&p.pk).unwrap();
        p.node.shutdown_links();
        addr
    }

    fn has_text(p: &Phone, chat: i64, body: &'static str) -> impl Fn() -> bool {
        let s = p.session.clone();
        move || s.lock().unwrap().store.messages(chat, None).unwrap().iter().any(|m| m.body == body)
    }

    #[tokio::test]
    async fn a_member_passes_a_message_on_while_its_sender_is_offline() {
        let book = Arc::default();
        let (alice, bob, carol) = (phone(&book).await, phone(&book).await, phone(&book).await);
        let chats = net_group(&[&alice, &bob, &carol]).await;
        // Carol goes offline; Bob writes, reaches Alice, then goes offline.
        let carol_addr = offline(&book, &carol);
        let reach = bob.session.lock().unwrap().send_to_chat(chats[1], "running late", None).unwrap();
        for p in reach {
            bob.node.poke(p);
        }
        until(has_text(&alice, chats[0], "running late")).await;
        let bob_addr = offline(&book, &bob);
        // Carol is back: Alice reaches her and passes Bob's message on.
        book.lock().unwrap().insert(carol.pk, carol_addr);
        alice.node.reconnect_now();
        until(has_text(&carol, chats[2], "running late")).await;
        let from = carol.session.lock().unwrap().store.messages(chats[2], None).unwrap().pop().unwrap().sender;
        assert_eq!(from, Some(bob.pk));
        assert!(!bob.session.lock().unwrap().store.messages(chats[1], None).unwrap()[0].delivered);
        // Bob is back: Carol tells him she has it, and it's delivered.
        book.lock().unwrap().insert(bob.pk, bob_addr);
        bob.node.poke_pending();
        let (s, bg) = (bob.session.clone(), chats[1]);
        until(move || s.lock().unwrap().store.messages(bg, None).unwrap()[0].delivered).await;
        let count = carol.session.lock().unwrap().store.messages(chats[2], None).unwrap().len();
        assert_eq!(count, 1, "no duplicate");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_photo_is_fetched_from_another_member() {
        let book = Arc::default();
        let mut p = Vec::new();
        for _ in 0..5 {
            p.push(phone(&book).await);
        }
        let all: Vec<&Phone> = p.iter().collect();
        let chats = net_group(&all).await;
        // Eve is offline when Alice shares a photo with the others.
        let eve_addr = offline(&book, &p[4]);
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let path = p[0]._dir.path().join("p.jpg");
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
        let reach = p[0].session.lock().unwrap().send_media(chats[0], out, None).unwrap();
        for peer in reach {
            p[0].node.poke(peer);
        }
        let got = |i: usize| {
            let (s, chat) = (p[i].session.clone(), chats[i]);
            move || {
                let s = s.lock().unwrap();
                let seq = s.store.messages_indexed(chat, None).unwrap().last().map(|m| m.0);
                seq.is_some_and(|seq| matches!(s.media_bytes(chat, seq), Ok(Some(_))))
            }
        };
        for i in 1..4 {
            until(got(i)).await;
        }
        // Alice goes offline; Eve, Bob's friend, comes back and gets both
        // the message and the photo from Bob.
        offline(&book, &p[0]);
        befriend(&p[1], &p[4]).await;
        book.lock().unwrap().insert(p[4].pk, eve_addr);
        p[1].node.spawn_retry_loop(Duration::from_millis(50));
        until(got(4)).await;
        let s = p[4].session.lock().unwrap();
        let seq = s.store.messages_indexed(chats[4], None).unwrap().last().unwrap().0;
        assert_eq!(s.media_bytes(chats[4], seq).unwrap().unwrap(), bytes);
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
    async fn typing_goes_only_over_a_live_link() {
        let book = Arc::default();
        let (alice, mut bob, carol) = (phone(&book).await, phone(&book).await, phone(&book).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        let (ac, _) = befriend(&alice, &carol).await;
        // Alice is connected to both, with encryption set up.
        for chat in [ab, ac] {
            let p = alice.session.lock().unwrap().send_text(chat, "hi").unwrap().unwrap();
            alice.node.poke(p);
            let session = alice.session.clone();
            until(move || session.lock().unwrap().store.messages(chat, None).unwrap()[0].delivered).await;
        }
        // Then Carol drops off the network.
        book.lock().unwrap().remove(&carol.pk);
        carol.node.shutdown_links();
        let a = alice.node.clone();
        until(move || !a.connected().contains(&carol.pk)).await;

        assert_eq!(alice.node.typing(ab).unwrap(), 1);
        expect(&mut bob.events, |e| {
            *e == NetEvent::Session(Event::Typing { chat: ba, who: alice.pk, name: "Alice".into() })
        })
        .await;
        assert_eq!(bob.session.lock().unwrap().store.messages(ba, None).unwrap().len(), 1, "nothing stored");
        // Again right away: dropped on Alice's side.
        assert_eq!(alice.node.typing(ab).unwrap(), 0);

        // Carol isn't connected: she's not dialed, and nothing waits for her.
        assert_eq!(alice.node.typing(ac).unwrap(), 0);
        assert!(!alice.node.dialing.lock().unwrap().contains(&carol.pk));
        {
            let s = alice.session.lock().unwrap();
            assert!(s.store.outbox_for(&carol.pk).unwrap().is_empty());
            assert!(!s.pending_peers().unwrap().contains(&carol.pk));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!alice.node.connected().contains(&carol.pk));
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

    fn call_caps() -> sanctum_core::call::CallCaps {
        sanctum_core::call::CallCaps {
            play: vec!["opus".into()],
            record: vec!["opus".into()],
            video_play: vec!["h264".into()],
            video_record: vec!["h264".into()],
        }
    }

    /// Alice calls Bob over the wire and he answers.
    async fn call(alice: &Phone, bob: &Phone, ab: i64) {
        call_with(alice, bob, ab, false, vec![], vec![]).await;
    }

    /// Alice calls Bob (a video call with `video`), each offering these
    /// direct routes, and he answers.
    async fn call_with(alice: &Phone, bob: &Phone, ab: i64, video: bool, a: Vec<Vec<u8>>, b: Vec<Vec<u8>>) {
        // First contact sets up encryption.
        let p = alice.session.lock().unwrap().send_text(ab, "hi").unwrap().unwrap();
        alice.node.poke(p);
        let session = alice.session.clone();
        until(move || session.lock().unwrap().store.messages(ab, None).unwrap()[0].delivered).await;
        let (peer, _) = alice.session.lock().unwrap().call_start(ab, &call_caps(), video, a).unwrap();
        alice.node.poke(peer);
        alice.node.call_started();
        let session = bob.session.clone();
        until(move || session.lock().unwrap().call_now().is_some()).await;
        let to = bob.session.lock().unwrap().call_answer(&call_caps(), video, b).unwrap().unwrap();
        bob.node.poke(to);
        bob.node.call_started();
        let session = alice.session.clone();
        until(move || {
            session.lock().unwrap().call_now().is_some_and(|c| c.phase == sanctum_core::call::Phase::Active)
        })
        .await;
    }

    #[tokio::test]
    async fn call_sound_crosses_a_live_link_and_is_dropped_without_one() {
        let book = Arc::default();
        let (alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        call(&alice, &bob, ab).await;

        for i in 0..6u8 {
            assert!(alice.node.send_call_audio(&[i; 80]));
            assert!(bob.node.send_call_audio(&[100 + i; 80]));
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let (a, b) = (alice.session.lock().unwrap().media(), bob.session.lock().unwrap().media());
        let heard = |hub: &sanctum_core::call::MediaHub| {
            let hub = hub.clone();
            move || matches!(hub.pull_audio(), sanctum_core::call::Pulled::Packet(_))
        };
        until(heard(&b)).await;
        until(heard(&a)).await;

        // Bob drops off: nothing is queued or kept for him.
        book.lock().unwrap().remove(&bob.pk);
        bob.node.shutdown_links();
        let node = alice.node.clone();
        until(move || node.connected().is_empty()).await;
        let queued = alice.session.lock().unwrap().store.outbox_for(&bob.pk).unwrap().len();
        assert!(!alice.node.send_call_audio(&[9; 80]));
        assert_eq!(alice.session.lock().unwrap().store.outbox_for(&bob.pk).unwrap().len(), queued);
    }

    fn loopback_route(port: u16) -> Vec<u8> {
        sanctum_core::direct::encode_route(SocketAddr::from(([127, 0, 0, 1], port)))
    }

    /// Phones on one machine: direct packets may go to loopback.
    fn allow_loopback(p: &Phone) {
        p.session.lock().unwrap().media().set_filter(sanctum_core::direct::loopback_too);
    }

    fn hears(p: &Phone) -> impl Fn() -> bool {
        let hub = p.session.lock().unwrap().media();
        move || matches!(hub.pull_audio(), sanctum_core::call::Pulled::Packet(_))
    }

    /// Keeps sending sound from `from` until `to` hears it.
    async fn sound_reaches(from: &Phone, to: &Phone) {
        let heard = hears(to);
        timeout(Duration::from_secs(10), async {
            loop {
                from.node.send_call_audio(&[7; 80]);
                tokio::time::sleep(Duration::from_millis(40)).await;
                if heard() {
                    return;
                }
            }
        })
        .await
        .expect("never heard");
    }

    #[tokio::test]
    async fn calls_go_direct_when_both_opt_in_and_fall_back_when_it_dies() {
        let book = Arc::default();
        let (alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        allow_loopback(&alice);
        allow_loopback(&bob);
        let loopback = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        let a = alice.node.direct_open(loopback).unwrap();
        let b = bob.node.direct_open(loopback).unwrap();
        call_with(&alice, &bob, ab, false, vec![loopback_route(a)], vec![loopback_route(b)]).await;
        // Both phones probe at once and meet.
        let (na, nb) = (alice.node.clone(), bob.node.clone());
        until(move || na.call_direct() && nb.call_direct()).await;

        // With the link gone, sound still flows: it goes straight across.
        alice.node.shutdown_links();
        let node = alice.node.clone();
        until(move || node.connected().is_empty()).await;
        sound_reaches(&alice, &bob).await;
        sound_reaches(&bob, &alice).await;

        // The link comes back; then Bob's side of the direct path goes
        // away. Alice notices within a few seconds and goes back to the
        // link, and the call carries on.
        alice.node.poke(bob.pk);
        let node = alice.node.clone();
        until(move || node.connected() == [bob.pk]).await;
        bob.node.direct_close();
        let node = alice.node.clone();
        until(move || !node.call_direct()).await;
        while hears(&bob)() {}
        sound_reaches(&alice, &bob).await;
    }

    #[tokio::test]
    async fn nothing_goes_direct_unless_both_opt_in() {
        let book = Arc::default();
        let (alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        allow_loopback(&alice);
        allow_loopback(&bob);
        // Alice has "Faster calls" on (her route is a socket we watch); Bob
        // has it off.
        let spy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let route = loopback_route(spy.local_addr().unwrap().port());
        call_with(&alice, &bob, ab, false, vec![route], vec![]).await;
        sound_reaches(&bob, &alice).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let mut buf = [0u8; 64];
        assert!(timeout(Duration::from_millis(200), spy.recv_from(&mut buf)).await.is_err(), "nothing sent direct");
        assert!(!alice.node.call_direct() && !bob.node.call_direct());
    }

    #[tokio::test]
    async fn pictures_cross_a_link_behind_the_sound() {
        let book = Arc::default();
        let (alice, bob) = (phone(&book).await, phone(&book).await);
        let (ab, _) = befriend(&alice, &bob).await;
        call_with(&alice, &bob, ab, true, vec![], vec![]).await;
        let info = |key| FrameInfo { key, rotation: 270, width: 320, height: 240 };
        let keyframe: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
        let sent = alice.node.send_call_video(&keyframe, info(true));
        assert_eq!(sent.packets.len(), 9);
        for n in 0..3u8 {
            assert!(!alice.node.send_call_video(&[n; 1500], info(false)).packets.is_empty());
            assert!(alice.node.send_call_audio(&[n; 80]));
        }
        let hub = bob.session.lock().unwrap().media();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let got = shown.clone();
        until(move || {
            if let Some(f) = hub.pull_video() {
                got.lock().unwrap().push(f);
            }
            got.lock().unwrap().len() == 4
        })
        .await;
        let shown = shown.lock().unwrap();
        assert_eq!(shown[0].data, keyframe);
        assert_eq!(shown[0].info, info(true));
        assert_eq!(shown.iter().map(|f| f.number).collect::<Vec<_>>(), [0, 1, 2, 3]);
    }

    /// Loopback standing in for both Tor and the local network: each phone
    /// listens twice, and "nearby" dials reach the nearby listener.
    struct TwoWays {
        far: Arc<Mutex<HashMap<[u8; 32], SocketAddr>>>,
        near: Arc<Mutex<HashMap<[u8; 32], SocketAddr>>>,
        nearby_on: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Transport for TwoWays {
        async fn dial(&self, peer: [u8; 32]) -> anyhow::Result<BoxConn> {
            Ok(self.dial_ranked(peer).await?.0)
        }

        async fn dial_ranked(&self, peer: [u8; 32]) -> anyhow::Result<(BoxConn, bool)> {
            let near = self.near.lock().unwrap().get(&peer).copied();
            if let (true, Some(addr)) = (self.nearby_on.load(Ordering::Relaxed), near) {
                return Ok((Box::new(TcpStream::connect(addr).await?), true));
            }
            let addr = *self.far.lock().unwrap().get(&peer).ok_or_else(|| anyhow::anyhow!("offline"))?;
            Ok((Box::new(TcpStream::connect(addr).await?), false))
        }

        fn nearby(&self, peer: &[u8; 32]) -> bool {
            self.nearby_on.load(Ordering::Relaxed) && self.near.lock().unwrap().contains_key(peer)
        }
    }

    async fn two_way_phone(ways: &TwoWays) -> Phone {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::open(Identity::generate().secret(), dir.path().join("s.db"), &rand::random()).unwrap();
        let pk = s.public_key();
        let session = Arc::new(Mutex::new(s));
        let transport =
            TwoWays { far: ways.far.clone(), near: ways.near.clone(), nearby_on: ways.nearby_on.clone() };
        let node = Node::new(session.clone(), Arc::new(transport));
        for nearby in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let book = if nearby { &ways.near } else { &ways.far };
            book.lock().unwrap().insert(pk, listener.local_addr().unwrap());
            let n = node.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    if nearby {
                        n.accept_nearby(Box::new(stream));
                    } else {
                        n.accept(Box::new(stream));
                    }
                }
            });
        }
        let events = node.subscribe();
        Phone { node, session, pk, events, _dir: dir }
    }

    #[tokio::test]
    async fn a_nearby_link_takes_over_from_tor() {
        let ways = TwoWays { far: Arc::default(), near: Arc::default(), nearby_on: Arc::default() };
        let (alice, bob) = (two_way_phone(&ways).await, two_way_phone(&ways).await);
        let (ab, ba) = befriend(&alice, &bob).await;
        let p = alice.session.lock().unwrap().send_text(ab, "over tor").unwrap().unwrap();
        alice.node.poke(p);
        let session = alice.session.clone();
        until(move || session.lock().unwrap().store.messages(ab, None).unwrap()[0].delivered).await;
        assert!(!alice.node.is_nearby(&bob.pk) && !bob.node.is_nearby(&alice.pk));

        // They turn out to be on the same Wi-Fi: either side moves over,
        // and both keep the nearby link.
        ways.nearby_on.store(true, Ordering::Relaxed);
        bob.node.prefer_nearby(alice.pk, true);
        let (a, b) = (alice.node.clone(), bob.node.clone());
        until(move || a.is_nearby(&bob.pk) && b.is_nearby(&alice.pk)).await;
        // A Tor link arriving now doesn't replace it.
        ways.nearby_on.store(false, Ordering::Relaxed);
        alice.node.dial(bob.pk);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(alice.node.is_nearby(&bob.pk) && bob.node.is_nearby(&alice.pk));
        // And messages still flow.
        let p = alice.session.lock().unwrap().send_text(ab, "nearby now").unwrap().unwrap();
        alice.node.poke(p);
        let session = bob.session.clone();
        until(move || session.lock().unwrap().store.messages(ba, None).unwrap().iter().any(|m| m.body == "nearby now"))
            .await;
    }
}
