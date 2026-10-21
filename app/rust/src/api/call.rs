//! Voice calls with a friend (see `sanctum_core::call`). Only the real
//! profile calls and rings. The sound itself never passes through Dart: the
//! phone's audio code hands it to Rust directly (`call_jni.rs`).

use super::session::{self, node, poke, profile_is_decoy, with_real};
use sanctum_core::call::{AudioCaps, CallView, Outcome, Phase};
use sanctum_core::session::Session;

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
    pub phase: CallPhaseView,
    /// Ended calls: how it went.
    pub outcome: Option<CallOutcomeView>,
    /// When we started talking (Unix milliseconds).
    pub answered_at_ms: Option<i64>,
    /// The sound format this phone records into, and the one it plays.
    pub send_codec: String,
    pub recv_codec: String,
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
        phase,
        outcome,
        answered_at_ms: view.answered_at.map(|t| t as i64),
        send_codec: view.send_audio,
        recv_codec: view.recv_audio,
        nearby: node().is_some_and(|n| n.is_nearby(&view.peer)),
    }
}

fn caps(play: Vec<String>, record: Vec<String>) -> AudioCaps {
    AudioCaps { play, record }
}

/// Calls the friend in this chat. `play` and `record`: the sound formats
/// this phone can play and record into, best first ("opus", "amr-wb").
/// Progress arrives as `Call` events.
pub fn call_start(chat_id: i64, play: Vec<String>, record: Vec<String>) -> anyhow::Result<CallInfo> {
    anyhow::ensure!(!profile_is_decoy(), "calls aren't available here");
    let (peer, info) = with_real(|s| {
        let (peer, view) = s.call_start(chat_id, &caps(play, record))?;
        Ok((peer, info(view, s)))
    })?;
    poke(Some(peer));
    if let Some(node) = node() {
        let _guard = session::runtime().enter();
        // On the same Wi-Fi: talk over that, it's faster.
        node.prefer_nearby(peer, true);
    }
    Ok(info)
}

/// Picks up the call ringing now.
pub fn call_answer(play: Vec<String>, record: Vec<String>) -> anyhow::Result<()> {
    let peer = with_real(|s| Ok(s.call_answer(&caps(play, record))?))?;
    poke(peer);
    if let (Some(peer), Some(node)) = (peer, node()) {
        let _guard = session::runtime().enter();
        node.prefer_nearby(peer, true);
    }
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
