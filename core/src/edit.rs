//! Editing sent messages, in friends' chats, groups and notes.
//!
//! An edit ([`Content::Edit`]) names its message by the shared message id,
//! like a reaction, and carries the whole new text. Only the writer can
//! edit, only for [`EDIT_WINDOW`] after sending (notes: any time), and only
//! words: a message's text or a photo's, video's or file's caption. Polls,
//! voice notes, call records and notices stay as they are.
//!
//! Every phone keeps the newest words and when they were written
//! (`StoredMessage::edited_at`), so a late older edit can't undo a newer
//! one. In groups an edit can arrive before its message (they may travel
//! over different links); it waits on the side until the message comes.
//!
//! An edit changes how a message looks, so it refreshes the chat but never
//! notifies.

use crate::protocol::{Content, MediaKind, MessageId};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, StoredMessage};
use serde::{Deserialize, Serialize};

/// How long after sending a message can still be edited, in seconds.
pub const EDIT_WINDOW: u64 = 15 * 60;
/// Longest text, in characters, as for a caption.
const MAX_BODY: usize = 4000;
/// An edit waiting for a message that hasn't arrived (only the newest is
/// kept).
#[derive(Serialize, Deserialize)]
struct Waiting {
    from: [u8; 32],
    body: String,
    at: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pending_name(chat_id: i64, message: &MessageId) -> Vec<u8> {
    [b"edit:".as_slice(), &chat_id.to_le_bytes(), message].concat()
}

/// Whether a message's words can be changed at all (by its writer).
pub fn editable(m: &StoredMessage) -> bool {
    m.id.is_some()
        && m.poll.is_none()
        && m.call.is_none()
        && !m.notice
        && m.shared_by.is_none()
        && m.media.as_ref().is_none_or(|media| media.kind != MediaKind::Audio)
}

/// Whether we can change this message's words now: ours, words, and
/// (except in notes) sent less than [`EDIT_WINDOW`] ago.
pub fn can_edit_now(m: &StoredMessage, notes: bool) -> bool {
    m.from_me && editable(m) && (notes || now().saturating_sub(m.sent_at) <= EDIT_WINDOW)
}

/// The new words, tidied; None if they can't replace this message's (a
/// text message can't be emptied, a caption can).
fn tidy(m: &StoredMessage, body: &str) -> Option<String> {
    let body = clip(body.trim(), MAX_BODY);
    (m.media.is_some() || !body.is_empty()).then_some(body)
}

/// Puts the edit on the message if it's newer than the last. True if the
/// message changed (and must be saved).
fn apply(m: &mut StoredMessage, body: String, at: u64) -> bool {
    if at <= m.edited_at || !editable(m) {
        return false;
    }
    let Some(body) = tidy(m, &body) else {
        return false;
    };
    m.body = body;
    m.edited_at = at;
    true
}

impl Session {
    /// Changes the words of our message at `seq`. Returns who to reach.
    pub fn edit_message(&mut self, chat_id: i64, seq: i64, body: &str) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        let Some(mut msg) = self.store.message(chat_id, seq)? else {
            return Err(SessionError::Protocol);
        };
        if !msg.from_me || !editable(&msg) {
            return Err(SessionError::Protocol);
        }
        if !can_edit_now(&msg, meta.kind == ChatKind::Notes) {
            return Err(SessionError::GroupRule("Messages can only be changed in the first 15 minutes."));
        }
        let Some(body) = tidy(&msg, body) else {
            return Err(SessionError::GroupRule("A message can't be empty."));
        };
        if body == msg.body {
            return Ok(Vec::new());
        }
        let Some(id) = msg.id else {
            return Err(SessionError::Protocol);
        };
        let at = now_ms().max(msg.edited_at + 1);
        msg.body = body.clone();
        msg.edited_at = at;
        self.store.update_message(chat_id, seq, &msg)?;
        self.events.push(Event::Chat(chat_id));
        self.tell_chat(chat_id, Content::Edit { id, body, at })
    }

    /// A friend's or member's edit (they're known to be in the chat: it
    /// came inside the chat's MLS group, from them). Only their own
    /// messages.
    pub(crate) fn on_edit(&mut self, chat_id: i64, peer: &[u8; 32], id: MessageId, body: String, at: u64) -> Result<()> {
        match self.store.message_by_id(chat_id, &id)? {
            Some((seq, mut msg)) => {
                if !msg.from_me && msg.sender == Some(*peer) && apply(&mut msg, body, at) {
                    self.store.update_message(chat_id, seq, &msg)?;
                    self.events.push(Event::Chat(chat_id));
                }
            }
            None => {
                let name = pending_name(chat_id, &id);
                let newer = self.store.kv_get::<Waiting>(&name)?.is_none_or(|w| at > w.at);
                if newer {
                    let body = clip(&body, MAX_BODY);
                    self.store.kv_put(&name, &Waiting { from: *peer, body, at })?;
                }
            }
        }
        Ok(())
    }

    /// Puts an edit that came before its message on the message.
    pub(crate) fn take_waiting_edit(&mut self, chat_id: i64, msg: &mut StoredMessage) -> Result<()> {
        let Some(id) = msg.id else { return Ok(()) };
        let name = pending_name(chat_id, &id);
        if let Some(w) = self.store.kv_get::<Waiting>(&name)? {
            if !msg.from_me && msg.sender == Some(w.from) {
                apply(msg, w.body, w.at);
            }
            self.store.kv_delete(&name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Media, Poll};

    fn text(body: &str) -> StoredMessage {
        StoredMessage { body: body.into(), id: Some([1; 16]), ..Default::default() }
    }

    #[test]
    fn newer_edits_win() {
        let mut m = text("hi");
        assert!(apply(&mut m, "hello".into(), 5));
        assert!(!apply(&mut m, "older".into(), 4), "a late older edit loses");
        assert!(!apply(&mut m, "same time".into(), 5));
        assert_eq!((m.body.as_str(), m.edited_at), ("hello", 5));
    }

    #[test]
    fn only_words_can_change() {
        let mut m = text("hi");
        assert!(!apply(&mut m, "   ".into(), 1), "text can't be emptied");
        let mut photo = StoredMessage { media: Some(Media { kind: MediaKind::Image, ..Default::default() }), ..text("cat") };
        assert!(apply(&mut photo, "".into(), 1), "a caption can");
        let voice = StoredMessage { media: Some(Media { kind: MediaKind::Audio, ..Default::default() }), ..text("") };
        assert!(!editable(&voice));
        assert!(!editable(&StoredMessage { poll: Some(Poll::default()), ..text("?") }));
        assert!(!editable(&StoredMessage { notice: true, ..text("Sam joined") }));
        assert!(!editable(&StoredMessage { id: None, ..text("old") }));
    }
}
