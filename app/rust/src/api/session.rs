//! The open session (identity + encrypted store + MLS + temp chats), and
//! the network node that talks to friends over Tor.

use crate::frb_generated::StreamSink;
use sanctum_core::session::{Event, Paired, Session};
use sanctum_net::{NetEvent, Node};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

static SESSION: LazyLock<Mutex<Option<Arc<Mutex<Session>>>>> = LazyLock::new(Default::default);
static NODE: LazyLock<Mutex<Option<Arc<Node>>>> = LazyLock::new(Default::default);
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
});
/// Everything the UI should hear about, from the session and the network.
static EVENTS: LazyLock<broadcast::Sender<AppEvent>> = LazyLock::new(|| broadcast::channel(512).0);
static STATUS: LazyLock<Mutex<NetStatus>> = LazyLock::new(|| Mutex::new(NetStatus::Off));

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
    /// Network status changed; see `status`.
    Status,
}

#[derive(Clone, Debug)]
pub struct AppEvent {
    pub kind: AppEventKind,
    /// The chat or temp chat concerned.
    pub id: i64,
    pub status: NetStatus,
}

fn emit(kind: AppEventKind, id: i64) {
    let status = *STATUS.lock().unwrap();
    let _ = EVENTS.send(AppEvent { kind, id, status });
}

fn set_status(status: NetStatus) {
    *STATUS.lock().unwrap() = status;
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

pub fn net_status() -> NetStatus {
    *STATUS.lock().unwrap()
}

/// Connects to Tor and starts this phone's onion service, in the
/// background. Progress arrives as `Status` events.
pub fn net_start(
    state_dir: String,
    cache_dir: String,
    identity_secret: Vec<u8>,
) -> anyhow::Result<()> {
    let secret: [u8; 32] = identity_secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be 32 bytes"))?;
    if matches!(net_status(), NetStatus::Connecting | NetStatus::Online) {
        return Ok(());
    }
    let session = session()?;
    set_status(NetStatus::Connecting);
    RUNTIME.spawn(async move {
        let started = sanctum_net::tor::Tor::start(
            &PathBuf::from(state_dir),
            &PathBuf::from(cache_dir),
            secret,
            |transport| Node::new(session, transport),
        )
        .await;
        match started {
            Ok((tor, node)) => {
                // Keep Tor running for the life of the app.
                std::mem::forget(tor);
                forward_events(&node);
                node.spawn_retry_loop(Duration::from_secs(30));
                node.poke_pending();
                *NODE.lock().unwrap() = Some(node);
                set_status(NetStatus::Online);
            }
            Err(_) => set_status(NetStatus::Failed),
        }
    });
    Ok(())
}

fn forward_events(node: &Arc<Node>) {
    let mut rx = node.subscribe();
    RUNTIME.spawn(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                NetEvent::Session(e) => emit_session(e),
                NetEvent::Online(_)
                | NetEvent::PeerConnected(_)
                | NetEvent::PeerDisconnected(_) => {}
            }
        }
    });
}

fn emit_session(e: Event) {
    match e {
        Event::Chat(id) => emit(AppEventKind::Chat, id),
        Event::Temp(id) => emit(AppEventKind::Temp, id as i64),
        Event::TempGone(id) => emit(AppEventKind::TempGone, id as i64),
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
    Ok(paired.into())
}

/// Marks a friend verified (or not) after comparing safety numbers.
pub fn friend_set_verified(chat_id: i64, verified: bool) -> anyhow::Result<()> {
    with(|s| Ok(s.set_verified(chat_id, verified)?))
}

pub fn friend_safety_number(chat_id: i64) -> anyhow::Result<String> {
    with(|s| Ok(s.safety_number(chat_id)?))
}

pub struct FriendView {
    pub chat_id: i64,
    pub name: String,
    pub sigil_key: Vec<u8>,
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
                    name: peer.name,
                    sigil_key: meta.sigil_key,
                })
            })
            .collect()
    })
}

pub(crate) fn session() -> anyhow::Result<Arc<Mutex<Session>>> {
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
    // UI-initiated changes are refreshed by the UI itself.
    guard.take_events();
    out
}
