//! Muting, drafts, pinned messages, disappearing messages, view-once
//! photos, places, scheduled messages and forwarding. See the matching
//! modules in `sanctum_core`.

use super::session::{self, poke};
use super::store::MediaKindView;
use sanctum_core::media::Outgoing;
use sanctum_core::protocol::MediaKind;
use sanctum_core::session::Session;

fn reach(peers: Vec<[u8; 32]>) {
    for peer in peers {
        poke(Some(peer));
    }
}

/// Muted for good (`chat_set_muted`).
#[flutter_rust_bridge::frb(sync)]
pub fn muted_forever() -> i64 {
    i64::MAX
}

/// Silences a chat's notifications until `until` (Unix seconds;
/// `muted_forever()` for good, 0 to unmute).
pub fn chat_set_muted(chat_id: i64, until: i64) -> anyhow::Result<()> {
    let until = if until == i64::MAX { sanctum_core::chat_prefs::MUTED_FOREVER } else { until.max(0) as u64 };
    session::with(|s| Ok(s.set_muted(chat_id, until)?))
}

/// Keeps what was typed but not sent (empty clears it).
pub fn chat_set_draft(chat_id: i64, text: String) -> anyhow::Result<()> {
    session::with(|s| Ok(s.set_draft(chat_id, &text)?))
}

pub fn chat_draft(chat_id: i64) -> anyhow::Result<String> {
    session::with(|s| Ok(s.draft(chat_id)?))
}

/// Pins the message at `seq` for everyone in the chat (None takes it away).
pub fn message_pin(chat_id: i64, seq: Option<i64>) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.pin_message(chat_id, seq)?))?);
    Ok(())
}

/// Where the pinned message is in the chat, if one is pinned and here.
pub fn chat_pinned_message(chat_id: i64) -> anyhow::Result<Option<i64>> {
    session::with(|s| Ok(s.pinned_message(chat_id)?))
}

/// How long new messages in the chat last, in seconds (0: for good).
pub fn chat_set_disappear(chat_id: i64, seconds: u32) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.set_disappear(chat_id, seconds as u64)?))?);
    Ok(())
}

/// A timer in words: "1 hour", "1 week".
#[flutter_rust_bridge::frb(sync)]
pub fn disappear_words(seconds: u32) -> String {
    sanctum_core::disappear::timer_words(seconds as u64)
}

/// Sends a photo or video the friend can open once (friends' chats only).
#[allow(clippy::too_many_arguments)]
pub fn view_once_send(
    chat_id: i64,
    path: String,
    kind: MediaKindView,
    mime: String,
    name: String,
    width: u32,
    height: u32,
    duration_ms: u32,
) -> anyhow::Result<()> {
    let kind = match kind {
        MediaKindView::Image => MediaKind::Image,
        MediaKindView::Video => MediaKind::Video,
        _ => anyhow::bail!("Only photos and videos can be sent to open once."),
    };
    let out = Outgoing {
        path: std::path::Path::new(&path),
        kind,
        mime: &mime,
        name: &name,
        caption: "",
        thumb: Vec::new(),
        width,
        height,
        duration_ms,
        album: None,
    };
    reach(session::with(|s| Ok(s.send_view_once(chat_id, out)?))?);
    Ok(())
}

/// Opens a friend's view-once photo or video: its bytes, this once. None
/// if it was opened already or hasn't arrived.
pub fn view_once_open(chat_id: i64, seq: i64) -> anyhow::Result<Option<Vec<u8>>> {
    session::with(|s| Ok(s.open_view_once(chat_id, seq)?))
}

/// Sends where we are (or any place) in the chat.
pub fn location_send(chat_id: i64, lat: f64, lon: f64) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.send_location(chat_id, lat, lon)?))?);
    Ok(())
}

pub struct ScheduledView {
    pub id: u64,
    pub body: String,
    /// When it goes, in Unix seconds.
    pub at: i64,
}

/// Sends `body` in the chat at `at` (Unix seconds).
pub fn message_schedule(chat_id: i64, body: String, at: i64) -> anyhow::Result<()> {
    session::with(|s| Ok(s.schedule_message(chat_id, &body, at.max(0) as u64)?))?;
    Ok(())
}

/// Messages waiting to be sent in the chat, soonest first.
pub fn scheduled_list(chat_id: i64) -> anyhow::Result<Vec<ScheduledView>> {
    session::with(|s| {
        Ok(s.scheduled(chat_id)?
            .into_iter()
            .map(|m| ScheduledView { id: m.id, body: m.body, at: m.at as i64 })
            .collect())
    })
}

pub fn scheduled_cancel(id: u64) -> anyhow::Result<()> {
    session::with(|s| Ok(s.cancel_scheduled(id)?))
}

/// Sends a copy of the message at `seq` to each of the chats `to`.
pub fn message_forward(from: i64, seq: i64, to: Vec<i64>) -> anyhow::Result<()> {
    for chat in to {
        reach(session::with(|s| Ok(s.forward(from, seq, chat)?))?);
    }
    Ok(())
}

/// Whether a message can be forwarded (`MessageView::can_forward` says the
/// same for messages already listed).
pub(crate) fn can_forward(m: &sanctum_core::store::StoredMessage) -> bool {
    Session::can_forward(m)
}

/// A view-once photo's state, for `MessageView`.
pub enum ViewOnceView {
    /// An ordinary message.
    No,
    /// Not opened yet.
    Waiting,
    /// Opened; the file is gone from both phones.
    Opened,
}

pub(crate) fn view_once_view(state: u8) -> ViewOnceView {
    match state {
        sanctum_core::view_once::WAITING => ViewOnceView::Waiting,
        sanctum_core::view_once::OPENED => ViewOnceView::Opened,
        _ => ViewOnceView::No,
    }
}

/// A place someone shared.
pub struct PlaceView {
    pub lat: f64,
    pub lon: f64,
}
