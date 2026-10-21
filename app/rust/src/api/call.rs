//! Voice and video calls with a friend (see `sanctum_core::call`). Only the
//! real profile calls and rings. Sound and pictures never pass through
//! Dart: the phone's audio and camera code hands them to Rust directly
//! (`call_jni.rs`).

use super::session::{self, node, poke, profile_is_decoy, with_real};
use sanctum_core::call::{Camera, CallCaps, CallView, Outcome, Phase};
use sanctum_core::direct::encode_route;
use sanctum_core::session::Session;
use sanctum_net::direct::local_routes;

/// "Faster calls", kept sealed on the phone.
const DIRECT: &[u8] = b"calls:direct";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhaseView {
    /// We're calling; their phone hasn't got the call yet.
    Connecting,
    /// Ringing: on their phone (we called) or on this one.
    Ringing,
    /// Talking.
    Active,
    /// Over: see `outcome`.
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcomeView {
    Talked,
    Missed,
    Declined,
    NoAnswer,
    Unreachable,
    Busy,
    Failed,
    Dropped,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct CallInfo {
    /// The call's id, in hex.
    pub id: String,
    /// The friend's chat.
    pub chat_id: i64,
    pub name: String,
    /// The pair's sigil.
    pub sigil_key: Vec<u8>,
    /// The chat is hidden: show no name (as with notifications).
    pub hidden: bool,
    /// We called.
    pub outgoing: bool,
    /// A video call (the caller's camera was on from the start).
    pub video: bool,
    pub phase: CallPhaseView,
    /// Ended calls: how it went.
    pub outcome: Option<CallOutcomeView>,
    /// When we started talking (Unix milliseconds).
    pub answered_at_ms: Option<i64>,
    /// The sound format this phone records into, and the one it plays.
    pub send_codec: String,
    pub recv_codec: String,
    /// The same for pictures; empty when that way can't carry video.
    pub send_video_codec: String,
    pub recv_video_codec: String,
    /// The friend is linked nearby (same Wi-Fi or Bluetooth) right now,
    /// rather than over Tor.
    pub nearby: bool,
}

pub(crate) fn info(view: CallView, s: &Session) -> CallInfo {
    let meta = s.chat(view.chat).ok();
    let (phase, outcome) = match view.phase {
        Phase::Connecting => (CallPhaseView::Connecting, None),
        Phase::Ringing => (CallPhaseView::Ringing, None),
        Phase::Active => (CallPhaseView::Active, None),
        Phase::Ended(o) => (
            CallPhaseView::Ended,
            Some(match o {
                Outcome::Talked => CallOutcomeView::Talked,
                Outcome::Missed => CallOutcomeView::Missed,
                Outcome::Declined => CallOutcomeView::Declined,
                Outcome::NoAnswer => CallOutcomeView::NoAnswer,
                Outcome::Unreachable => CallOutcomeView::Unreachable,
                Outcome::Busy => CallOutcomeView::Busy,
                Outcome::Failed => CallOutcomeView::Failed,
                Outcome::Dropped => CallOutcomeView::Dropped,
                Outcome::Cancelled => CallOutcomeView::Cancelled,
            }),
        ),
    };
    CallInfo {
        id: view.id.iter().map(|b| format!("{b:02x}")).collect(),
        chat_id: view.chat,
        sigil_key: meta.as_ref().map(|m| m.sigil_key.clone()).unwrap_or_default(),
        hidden: meta.as_ref().is_some_and(|m| m.hidden),
        name: view.name,
        outgoing: view.outgoing,
        video: view.video,
        phase,
        outcome,
        answered_at_ms: view.answered_at.map(|t| t as i64),
        send_codec: view.send_audio,
        recv_codec: view.recv_audio,
        send_video_codec: view.send_video,
        recv_video_codec: view.recv_video,
        nearby: node().is_some_and(|n| n.is_nearby(&view.peer)),
    }
}

fn caps(play: Vec<String>, record: Vec<String>, video_play: Vec<String>, video_record: Vec<String>) -> CallCaps {
    CallCaps { play, record, video_play, video_record }
}

/// With "Faster calls" on: opens the call's UDP socket and lists the
/// addresses the friend may reach it on. Empty (and nothing opened) when
/// the setting is off or the phone has no usable address.
fn offer_routes() -> Vec<Vec<u8>> {
    if !calls_direct().unwrap_or(false) {
        return Vec::new();
    }
    let Some(node) = node() else { return Vec::new() };
    let _guard = session::runtime().enter();
    let Ok(port) = node.direct_open(None) else { return Vec::new() };
    let routes: Vec<Vec<u8>> = local_routes(port).into_iter().map(encode_route).collect();
    if routes.is_empty() {
        node.direct_close();
    }
    routes
}

/// The call is on its way: runs its housekeeping, and on the same Wi-Fi
/// talks over that (it's faster).
fn started(peer: Option<[u8; 32]>) {
    let Some(node) = node() else { return };
    let _guard = session::runtime().enter();
    node.call_started();
    if let Some(peer) = peer {
        node.prefer_nearby(peer, true);
    }
}

/// Calls the friend in this chat, with the camera on if `video`. `play`
/// and `record`: the sound formats this phone can play and record into,
/// best first ("opus", "amr-wb"); `video_play` and `video_record` the same
/// for pictures ("h264", "vp8"). Progress arrives as `Call` events.
pub fn call_start(
    chat_id: i64,
    play: Vec<String>,
    record: Vec<String>,
    video_play: Vec<String>,
    video_record: Vec<String>,
    video: bool,
) -> anyhow::Result<CallInfo> {
    anyhow::ensure!(!profile_is_decoy(), "calls aren't available here");
    let routes = offer_routes();
    let offered = !routes.is_empty();
    let started_call = with_real(|s| {
        let (peer, view) = s.call_start(chat_id, &caps(play, record, video_play, video_record), video, routes)?;
        Ok((peer, info(view, s)))
    });
    let (peer, info) = match started_call {
        Ok(started) => started,
        Err(e) => {
            if offered {
                if let Some(node) = node() {
                    node.direct_close();
                }
            }
            return Err(e);
        }
    };
    poke(Some(peer));
    started(Some(peer));
    Ok(info)
}

/// Picks up the call ringing now, with the camera on if `video`.
pub fn call_answer(
    play: Vec<String>,
    record: Vec<String>,
    video_play: Vec<String>,
    video_record: Vec<String>,
    video: bool,
) -> anyhow::Result<()> {
    // Only offer our addresses if the caller offered theirs.
    let offered_to_us = with_real(|s| Ok(s.call_now().is_some_and(|c| c.peer_routes && !c.outgoing)))?;
    let routes = if offered_to_us { offer_routes() } else { Vec::new() };
    let peer = with_real(|s| Ok(s.call_answer(&caps(play, record, video_play, video_record), video, routes)?))?;
    poke(peer);
    started(peer);
    Ok(())
}

/// Hangs up, declines the call ringing now, or stops calling.
pub fn call_end() -> anyhow::Result<()> {
    poke(with_real(|s| Ok(s.call_end()?))?);
    Ok(())
}

/// The call ringing, connecting or going on now, if any (never while the
/// decoy profile shows).
pub fn call_now() -> anyhow::Result<Option<CallInfo>> {
    if profile_is_decoy() {
        return Ok(None);
    }
    with_real(|s| Ok(s.call_now().map(|v| info(v, s))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraView {
    Off,
    /// On, sending pictures.
    Live,
    /// On, but the app is in the background: no picture until it's back.
    Paused,
}

impl From<Camera> for CameraView {
    fn from(c: Camera) -> Self {
        match c {
            Camera::Off => Self::Off,
            Camera::On => Self::Live,
            Camera::Paused => Self::Paused,
        }
    }
}

/// How the call's sound and pictures travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPathView {
    /// Through Tor: nobody learns who talks to whom.
    Private,
    /// Over the same Wi-Fi or Bluetooth.
    Nearby,
    /// Straight between the phones ("Faster calls").
    Direct,
}

/// How a call going on is getting along, for the call screen.
#[derive(Clone, Debug)]
pub struct CallMediaInfo {
    pub path: CallPathView,
    pub camera: CameraView,
    pub peer_camera: CameraView,
    /// This phone can send pictures in this call (a format in common).
    pub can_send_video: bool,
    pub can_receive_video: bool,
    /// The path is fast enough for sharper pictures (not through Tor).
    pub sharp: bool,
}

/// How the call going on now is getting along; None unless talking.
pub fn call_media() -> Option<CallMediaInfo> {
    if profile_is_decoy() {
        return None;
    }
    let media = session::media()?;
    let i = media.info()?;
    let nearby = media.peer().is_some_and(|p| node().is_some_and(|n| n.is_nearby(&p)));
    let path = match (i.direct, nearby) {
        (true, _) => CallPathView::Direct,
        (false, true) => CallPathView::Nearby,
        (false, false) => CallPathView::Private,
    };
    Some(CallMediaInfo {
        path,
        camera: i.camera.into(),
        peer_camera: i.peer_camera.into(),
        can_send_video: i.can_send_video,
        can_receive_video: i.can_receive_video,
        sharp: path != CallPathView::Private,
    })
}

/// Turns this phone's camera on or off in the call, or pauses it (the app
/// went to the background). The friend is told at once. False if
/// pictures can't be sent in this call.
pub fn call_camera(state: CameraView) -> bool {
    let camera = match state {
        CameraView::Off => Camera::Off,
        CameraView::Live => Camera::On,
        CameraView::Paused => Camera::Paused,
    };
    let Some(media) = session::media() else { return false };
    let ok = media.set_camera(camera);
    if let Some(node) = node() {
        // Tell the friend now rather than at the next housekeeping.
        node.flush_media();
    }
    ok
}

/// "Faster calls": calls connect straight between the phones when both
/// have it on. Off by default.
pub fn calls_direct() -> anyhow::Result<bool> {
    Ok(with_real(|s| Ok(s.store.kv_get::<bool>(DIRECT)?))?.unwrap_or(false))
}

pub fn calls_set_direct(enabled: bool) -> anyhow::Result<()> {
    with_real(|s| Ok(s.store.kv_put(DIRECT, &enabled)?))
}
