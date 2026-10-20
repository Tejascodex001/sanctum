//! The open session (identity + encrypted store + MLS + temp chats), and
//! the network node that talks to friends over Tor.

use crate::frb_generated::StreamSink;
use sanctum_core::identity::{pair_fingerprint, safety_number};
use sanctum_core::session::{Event, Paired, Session};
use sanctum_net::nearby::{Bluetooth, Nearby, Reach};
use sanctum_net::{NetEvent, Node, Transport};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

static SESSION: LazyLock<Mutex<Option<Arc<Mutex<Session>>>>> = LazyLock::new(Default::default);
/// The decoy profile: its own identity and database, shown after the decoy
/// PIN. The network keeps serving the real profile meanwhile.
static DECOY: LazyLock<Mutex<Option<Arc<Mutex<Session>>>>> = LazyLock::new(Default::default);
static USE_DECOY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NODE: LazyLock<Mutex<Option<Arc<Node>>>> = LazyLock::new(Default::default);
/// How friends are reached: directly when nearby, else over Tor once it's up.
static REACH: LazyLock<Arc<Reach>> = LazyLock::new(Default::default);
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
});
/// Everything the UI should hear about, from the session and the network.
static EVENTS: LazyLock<broadcast::Sender<AppEvent>> = LazyLock::new(|| broadcast::channel(512).0);
static STATUS: LazyLock<Mutex<NetInfo>> = LazyLock::new(|| {
    Mutex::new(NetInfo { status: NetStatus::Off, progress: 0, detail: String::new() })
});
/// How long Tor gets to come up before we call it failed and offer a retry.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// Opens the encrypted store at `dir` and restores MLS state. Call once at
/// startup, before anything else that touches chats.
pub fn session_open(
    identity_secret: Vec<u8>,
    dir: String,
    store_key: Vec<u8>,
) -> anyhow::Result<()> {
    let secret: [u8; 32] = identity_secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be 32 bytes"))?;
    let key: [u8; 32] = store_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("store key must be 32 bytes"))?;
    let session = Session::open(secret, PathBuf::from(&dir).join("sanctum.db"), &key)?;
    *SESSION.lock().unwrap() = Some(Arc::new(Mutex::new(session)));
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetStatus {
    Off,
    Connecting,
    Online,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppEventKind {
    /// A chat got a message or a delivery receipt.
    Chat,
    /// A temp chat appeared or changed.
    Temp,
    /// A temp chat was wiped by the friend, or burned away.
    TempGone,
    /// Network status changed; see `net`.
    Status,
    /// A friend request arrived.
    Request,
    /// A friend came online or went offline (`id`: their chat).
    Presence,
    /// A friend's message arrived (`id`: the chat). Also sent as `Chat`.
    Message,
    /// A friend's message arrived in a temp chat. Also sent as `Temp`.
    TempMessage,
    /// Today's moments changed.
    Moments,
}

/// Where the connection to Tor stands.
#[derive(Clone, Debug)]
pub struct NetInfo {
    pub status: NetStatus,
    /// 0–100 while connecting.
    pub progress: u8,
    /// What Tor is doing, or why it failed.
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct AppEvent {
    pub kind: AppEventKind,
    /// The chat or temp chat concerned.
    pub id: i64,
    pub net: NetInfo,
}

fn emit(kind: AppEventKind, id: i64) {
    let net = STATUS.lock().unwrap().clone();
    let _ = EVENTS.send(AppEvent { kind, id, net });
}

fn set_status(status: NetStatus, progress: u8, detail: impl Into<String>) {
    *STATUS.lock().unwrap() = NetInfo { status, progress, detail: detail.into() };
    emit(AppEventKind::Status, 0);
}

/// Streams [`AppEvent`]s to the UI for as long as the app runs.
pub fn app_events(sink: StreamSink<AppEvent>) -> anyhow::Result<()> {
    let mut rx = EVENTS.subscribe();
    RUNTIME.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if sink.add(event).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

pub fn net_status() -> NetInfo {
    STATUS.lock().unwrap().clone()
}

/// Connects to Tor and starts this phone's onion service, in the
/// background. Progress arrives as `Status` events. Call again after a
/// failure to retry.
pub fn net_start(
    state_dir: String,
    cache_dir: String,
    identity_secret: Vec<u8>,
) -> anyhow::Result<()> {
    let secret: [u8; 32] = identity_secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be 32 bytes"))?;
    if matches!(net_status().status, NetStatus::Connecting | NetStatus::Online) {
        return Ok(());
    }
    let session = real_session()?;
    // The node runs from the start, so nearby friends work without Tor.
    let node = ensure_node(session);
    set_status(NetStatus::Connecting, 0, "Starting Tor");
    RUNTIME.spawn(async move {
        // Run Tor in its own task so a panic inside it surfaces here as an
        // error instead of leaving the app "connecting" forever.
        let mut task = tokio::spawn(sanctum_net::tor::Tor::start(
            PathBuf::from(state_dir),
            PathBuf::from(cache_dir),
            secret,
            |frac, detail| {
                let percent = (frac.clamp(0.0, 1.0) * 100.0) as u8;
                if net_status().status == NetStatus::Connecting {
                    set_status(NetStatus::Connecting, percent, detail);
                }
            },
            {
                let node = node.clone();
                move |transport| {
                    *REACH.tor.write().unwrap() = Some(transport);
                    node
                }
            },
        ));
        let started = match tokio::time::timeout(START_TIMEOUT, &mut task).await {
            Ok(Ok(started)) => started,
            Ok(Err(join)) => Err(anyhow::anyhow!("Tor crashed: {}", panic_text(join))),
            Err(_) => {
                task.abort();
                Err(anyhow::anyhow!("Tor took too long to connect"))
            }
        };
        match started {
            Ok((tor, _)) => {
                *super::update::CONNECT.lock().unwrap() = Some(tor.connector());
                // Keep Tor running for the life of the app.
                std::mem::forget(tor);
                node.reconnect_now();
                set_status(NetStatus::Online, 100, "Connected");
            }
            Err(e) => set_status(NetStatus::Failed, 0, format!("{e:#}")),
        }
    });
    Ok(())
}

/// The network node: created once, used by Tor and nearby alike.
fn ensure_node(session: Arc<Mutex<Session>>) -> Arc<Node> {
    let mut slot = NODE.lock().unwrap();
    if let Some(node) = slot.clone() {
        return node;
    }
    let _guard = RUNTIME.enter();
    let reach: Arc<dyn Transport> = REACH.clone();
    let node = Node::new(session, reach);
    forward_events(&node);
    // Keeps a live link to every friend, reconnecting quickly.
    node.spawn_retry_loop(Duration::from_secs(3));
    *slot = Some(node.clone());
    node
}

/// Nearby on or off: friends on the same Wi-Fi or hotspot connect directly,
/// without internet. They see this phone's local network address.
pub fn nearby_set(enabled: bool) -> anyhow::Result<()> {
    let session = real_session()?;
    let node = ensure_node(session.clone());
    let mut slot = REACH.nearby.write().unwrap();
    if enabled && slot.is_none() {
        *slot = Some(RUNTIME.block_on(Nearby::start(node, session))?);
    } else if !enabled {
        if let Some(nearby) = slot.take() {
            nearby.stop();
        }
    }
    Ok(())
}

/// Where nearby friends' connections arrive (for the Bluetooth relay).
pub fn nearby_port() -> Option<u16> {
    REACH.nearby.read().unwrap().as_ref().map(|n| n.port())
}

/// Bluetooth is up: friends seen are reached through the relay on
/// `proxy_port` (see `BluetoothNearby.kt`).
pub fn bt_start(proxy_port: u16) {
    *REACH.bluetooth.write().unwrap() = Some(Arc::new(Bluetooth::new(proxy_port)));
}

pub fn bt_stop() {
    REACH.bluetooth.write().unwrap().take();
}

/// The rotating ID to advertise this minute.
pub fn bt_my_id() -> anyhow::Result<Vec<u8>> {
    with(|s| Ok(s.my_nearby_id(minute()).to_vec()))
}

/// A Sanctum phone advertising `id` was seen as device `handle`: if it's a
/// friend, connect.
pub fn bt_seen(id: Vec<u8>, handle: u64) -> anyhow::Result<()> {
    let real = real_session()?;
    let friend = real.lock().unwrap().nearby_friend(&id, minute())?;
    let Some(friend) = friend else {
        return Ok(());
    };
    let Some(bt) = REACH.bluetooth.read().unwrap().clone() else {
        return Ok(());
    };
    bt.saw(friend, handle);
    if let Some(node) = NODE.lock().unwrap().clone() {
        let _guard = RUNTIME.enter();
        node.dial_now(friend);
    }
    Ok(())
}

fn minute() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 60)
}

/// Friends' chats whose friend is nearby right now (Wi-Fi or Bluetooth).
pub fn nearby_chats() -> anyhow::Result<Vec<i64>> {
    let mut peers = REACH.nearby.read().unwrap().as_ref().map(|n| n.peers()).unwrap_or_default();
    peers.extend(REACH.bluetooth.read().unwrap().as_ref().map(|b| b.peers()).unwrap_or_default());
    with(|s| {
        let mut chats = Vec::new();
        for peer in &peers {
            if let Some(id) = s.chat_id_for(peer)? {
                chats.push(id);
            }
        }
        Ok(chats)
    })
}

fn panic_text(join: tokio::task::JoinError) -> String {
    match join.try_into_panic() {
        Ok(payload) => payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into()),
        Err(e) => e.to_string(),
    }
}

fn forward_events(node: &Arc<Node>) {
    let mut rx = node.subscribe();
    RUNTIME.spawn(async move {
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match event {
                NetEvent::Session(e) => emit_session(e),
                NetEvent::PeerConnected(peer) | NetEvent::PeerDisconnected(peer) => {
                    if let Ok(Some(chat)) = with(|s| Ok(s.chat_id_for(&peer)?)) {
                        emit(AppEventKind::Presence, chat);
                    }
                }
                NetEvent::Online(_) => {}
            }
        }
    });
}

/// The app went to the background (or came back): pace reconnects for
/// battery. Coming back also reconnects everyone now.
pub fn net_set_background(background: bool) {
    if let Some(node) = NODE.lock().unwrap().clone() {
        node.set_background(background);
        if !background {
            let _guard = RUNTIME.enter();
            node.reconnect_now();
        }
    }
}

/// Panic wipe, after the app has destroyed the keys: cuts every link,
/// closes the store and deletes `paths` (the database, Tor's state and
/// cache). The app exits right after; temp chats die with the process.
pub fn wipe_everything(paths: Vec<String>) -> anyhow::Result<()> {
    if let Some(nearby) = REACH.nearby.write().unwrap().take() {
        nearby.stop();
    }
    if let Some(node) = NODE.lock().unwrap().take() {
        node.shutdown_links();
    }
    SESSION.lock().unwrap().take();
    DECOY.lock().unwrap().take();
    set_status(NetStatus::Off, 0, "Wiped");
    let mut failed = Vec::new();
    for path in paths {
        let path = PathBuf::from(path);
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else if path.exists() {
            std::fs::remove_file(&path)
        } else {
            Ok(())
        };
        if result.is_err() {
            failed.push(path.display().to_string());
        }
    }
    anyhow::ensure!(failed.is_empty(), "couldn't delete {}", failed.join(", "));
    Ok(())
}

/// Pull to refresh: reconnects to every friend not connected, now.
pub fn net_refresh() {
    if let Some(node) = NODE.lock().unwrap().clone() {
        let _guard = RUNTIME.enter();
        node.reconnect_now();
    }
}

/// Friends' chats whose friend is connected right now.
pub fn online_chats() -> anyhow::Result<Vec<i64>> {
    let Some(node) = NODE.lock().unwrap().clone() else {
        return Ok(Vec::new());
    };
    let peers = node.connected();
    with(|s| {
        let mut chats = Vec::new();
        for peer in &peers {
            if let Some(id) = s.chat_id_for(peer)? {
                chats.push(id);
            }
        }
        Ok(chats)
    })
}

fn emit_session(e: Event) {
    match e {
        Event::Chat(id) => emit(AppEventKind::Chat, id),
        Event::Temp(id) => emit(AppEventKind::Temp, id as i64),
        Event::TempGone(id) => emit(AppEventKind::TempGone, id as i64),
        Event::Request => emit(AppEventKind::Request, 0),
        Event::Message(id) => emit(AppEventKind::Message, id),
        Event::TempMessage(id) => emit(AppEventKind::TempMessage, id as i64),
        Event::Moments => emit(AppEventKind::Moments, 0),
    }
}

/// Asks the network to deliver anything waiting for `peer` now.
pub(crate) fn poke(peer: Option<[u8; 32]>) {
    let Some(peer) = peer else { return };
    if let Some(node) = NODE.lock().unwrap().clone() {
        let _guard = RUNTIME.enter();
        node.poke(peer);
    }
}

pub struct PairedView {
    pub chat_id: i64,
    pub friend: String,
    /// The friend's identity key: their own sigil comes from it.
    pub friend_key: Vec<u8>,
    /// Added by scanning their QR in person.
    pub verified: bool,
    /// The shared chat sigil, made from both keys.
    pub sigil_key: Vec<u8>,
    /// Read aloud on a call to confirm a remote pairing.
    pub safety_number: String,
}

impl From<Paired> for PairedView {
    fn from(p: Paired) -> Self {
        Self {
            chat_id: p.chat_id,
            friend: p.friend,
            friend_key: p.friend_key.to_vec(),
            verified: p.verified,
            sigil_key: p.sigil_key,
            safety_number: p.safety_number,
        }
    }
}

/// Your friend code, to show as a QR or share.
pub fn my_code(my_name: String) -> anyhow::Result<String> {
    with(|s| Ok(s.my_code(&my_name)))
}

/// Adds the friend whose code was scanned (`scanned`: in person, so
/// verified), pasted or typed (verify later by comparing safety numbers).
pub fn friend_add(code: String, scanned: bool) -> anyhow::Result<PairedView> {
    let paired = with(|s| Ok(s.add_friend(&code, scanned)?))?;
    // Our friend request goes out now, in case they haven't added us.
    poke(Some(paired.friend_key));
    Ok(paired.into())
}

/// Marks a friend verified (or not) after comparing safety numbers.
pub fn friend_set_verified(chat_id: i64, verified: bool) -> anyhow::Result<()> {
    with(|s| Ok(s.set_verified(chat_id, verified)?))
}

pub fn friend_safety_number(chat_id: i64) -> anyhow::Result<String> {
    with(|s| Ok(s.safety_number(chat_id)?))
}

/// The name your friend requests carry.
pub fn profile_set_name(name: String) -> anyhow::Result<()> {
    with(|s| {
        s.set_my_name(&name);
        Ok(())
    })
}

/// Someone who added your code and asks you to add them back.
pub struct RequestView {
    /// Their identity key: their own sigil comes from it.
    pub key: Vec<u8>,
    pub name: String,
    /// Your chat's sigil, made from both keys.
    pub sigil_key: Vec<u8>,
    pub safety_number: String,
    pub at: i64,
}

pub fn request_list() -> anyhow::Result<Vec<RequestView>> {
    with(|s| {
        let me = s.public_key();
        Ok(s.requests()
            .into_iter()
            .map(|r| {
                let sigil_key = pair_fingerprint(&me, &r.identity).to_vec();
                RequestView {
                    key: r.identity.to_vec(),
                    name: r.name,
                    safety_number: safety_number(&sigil_key),
                    sigil_key,
                    at: r.at as i64,
                }
            })
            .collect())
    })
}

/// Adds them; encryption is set up as soon as their phone is reached.
pub fn request_accept(key: Vec<u8>) -> anyhow::Result<PairedView> {
    let key = to_key(key)?;
    let paired = with(|s| Ok(s.accept_request(&key)?))?;
    poke(Some(key));
    Ok(paired.into())
}

pub fn request_decline(key: Vec<u8>) -> anyhow::Result<()> {
    let key = to_key(key)?;
    with(|s| {
        s.decline_request(&key);
        Ok(())
    })
}

fn to_key(key: Vec<u8>) -> anyhow::Result<[u8; 32]> {
    key.try_into().map_err(|_| anyhow::anyhow!("keys are 32 bytes"))
}

pub struct FriendView {
    pub chat_id: i64,
    /// Their identity key.
    pub key: Vec<u8>,
    pub name: String,
    pub sigil_key: Vec<u8>,
    pub hidden: bool,
}

/// Friends, for choosing whom to start a temp chat with.
pub fn friend_list() -> anyhow::Result<Vec<FriendView>> {
    with(|s| {
        let friends = s.friends()?;
        friends
            .into_iter()
            .map(|(chat_id, peer)| {
                let meta = s.chat(chat_id)?;
                Ok(FriendView {
                    chat_id,
                    key: peer.identity.to_vec(),
                    name: peer.name,
                    sigil_key: meta.sigil_key,
                    hidden: meta.hidden,
                })
            })
            .collect()
    })
}

pub(crate) fn runtime() -> &'static Runtime {
    &RUNTIME
}

/// Opens the decoy profile's own database (`decoy.db` in `dir`).
pub fn decoy_open(identity_secret: Vec<u8>, dir: String, store_key: Vec<u8>) -> anyhow::Result<()> {
    let secret: [u8; 32] = identity_secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be 32 bytes"))?;
    let key: [u8; 32] = store_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("store key must be 32 bytes"))?;
    let decoy = Session::open(secret, PathBuf::from(&dir).join("decoy.db"), &key)?;
    *DECOY.lock().unwrap() = Some(Arc::new(Mutex::new(decoy)));
    Ok(())
}

/// Shows the decoy profile (or the real one again). Only what the app
/// shows changes; the network always serves the real profile.
pub fn profile_use(decoy: bool) {
    let decoy = decoy && DECOY.lock().unwrap().is_some();
    USE_DECOY.store(decoy, std::sync::atomic::Ordering::Relaxed);
}

pub fn profile_is_decoy() -> bool {
    USE_DECOY.load(std::sync::atomic::Ordering::Relaxed)
}

/// A slow (Argon2id) hash of an app PIN, for storing and checking it.
pub fn pin_hash(pin: String, salt: Vec<u8>) -> Vec<u8> {
    sanctum_core::pin::pin_hash(&pin, &salt).to_vec()
}

/// The real profile, whatever the app shows: for the network.
pub(crate) fn real_session() -> anyhow::Result<Arc<Mutex<Session>>> {
    SESSION
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("session is not open"))
}

pub(crate) fn session() -> anyhow::Result<Arc<Mutex<Session>>> {
    if profile_is_decoy() {
        if let Some(decoy) = DECOY.lock().unwrap().clone() {
            return Ok(decoy);
        }
    }
    SESSION
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("session is not open"))
}

pub(crate) fn with<T>(f: impl FnOnce(&mut Session) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let session = session()?;
    let mut guard = session.lock().unwrap();
    let out = f(&mut guard);
    // Pass events on, never drop them: some may be the network's (a message
    // that arrived a moment ago and wasn't announced yet).
    let events = guard.take_events();
    let pokes = guard.take_pokes();
    drop(guard);
    for e in events {
        emit_session(e);
    }
    for peer in pokes {
        poke(Some(peer));
    }
    out
}
