//! Emoji reactions to messages, in friends' chats and groups.
//!
//! A reaction ([`Content::React`]) names its message by the shared message
//! id, like a poll answer names its poll. Each person has at most one per
//! message: reacting again replaces it, and an empty emoji takes it back.
//! Every phone keeps each person's newest reaction with the message itself
//! (`StoredMessage::reactions`), so deleting the message deletes them too.
//! In groups a reaction can arrive before its message (they travel over
//! different links); it waits on the side until the message comes.
//!
//! A reaction changes how a message looks, so it refreshes the chat but
//! never notifies.

use crate::protocol::{Content, MessageId};
use crate::session::{Event, Result, Session, SessionError};
use crate::store::{Reaction, StoredMessage};

/// Longest reaction, in bytes: enough for any emoji, even a family or a
/// flag with modifiers, but not for a message.
pub const MAX_EMOJI_BYTES: usize = 32;
/// Reactions kept waiting for a message that hasn't arrived, per message.
const MAX_PENDING: usize = 200;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pending_name(chat_id: i64, message: &MessageId) -> Vec<u8> {
    [b"reacts:".as_slice(), &chat_id.to_le_bytes(), message].concat()
}

/// A reaction made safe to keep and show: trimmed, short, no control
/// characters or line breaks. Empty means "taken back"; None if it's not
/// a reaction at all.
pub fn tidy_emoji(emoji: &str) -> Option<String> {
    let emoji = emoji.trim();
    if emoji.len() > MAX_EMOJI_BYTES || emoji.chars().any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}') {
        return None;
    }
    Some(emoji.to_string())
}

/// Records `reaction` on the message, if it's newer than that person's
/// last. True if the message changed (and must be saved). A reaction taken
/// back is kept too, so an older one arriving late stays taken back.
fn apply(msg: &mut StoredMessage, reaction: Reaction) -> bool {
    match msg.reactions.iter_mut().find(|r| r.who == reaction.who) {
        Some(old) if old.at >= reaction.at => false,
        Some(old) => {
            *old = reaction;
            true
        }
        None => {
            msg.reactions.push(reaction);
            true
        }
    }
}

impl Session {
    /// Reacts to the message at `seq` with `emoji`, replacing our earlier
    /// reaction; an empty `emoji` takes it back. Returns who to reach.
    pub fn react(&mut self, chat_id: i64, seq: i64, emoji: &str) -> Result<Vec<[u8; 32]>> {
        let emoji = tidy_emoji(emoji).ok_or(SessionError::Protocol)?;
        let Some(mut msg) = self.store.message(chat_id, seq)? else {
            return Err(SessionError::Protocol);
        };
        // Messages from before shared ids can't be pointed at.
        let Some(id) = msg.id else {
            return Err(SessionError::Protocol);
        };
        let at = now_ms();
        let mine = Reaction {
            who: self.public_key(),
            emoji: emoji.clone(),
            at,
        };
        if apply(&mut msg, mine) {
            self.store.update_message(chat_id, seq, &msg)?;
            self.events.push(Event::Chat(chat_id));
        }
        self.tell_chat(chat_id, Content::React { id, emoji, at })
    }

    /// A friend's or member's reaction (they're known to be in the chat:
    /// it came inside the chat's MLS group, from them).
    pub(crate) fn on_react(&mut self, chat_id: i64, peer: &[u8; 32], id: MessageId, emoji: String, at: u64) -> Result<()> {
        let Some(emoji) = tidy_emoji(&emoji) else {
            return Ok(());
        };
        let reaction = Reaction { who: *peer, emoji, at };
        match self.store.message_by_id(chat_id, &id)? {
            Some((seq, mut msg)) => {
                if apply(&mut msg, reaction) {
                    self.store.update_message(chat_id, seq, &msg)?;
                    self.events.push(Event::Chat(chat_id));
                }
            }
            None => {
                let name = pending_name(chat_id, &id);
                let mut waiting = self.store.kv_get::<Vec<Reaction>>(&name)?.unwrap_or_default();
                waiting.retain(|r| r.who != reaction.who || r.at > reaction.at);
                if waiting.iter().all(|r| r.who != reaction.who) && waiting.len() < MAX_PENDING {
                    waiting.push(reaction);
                }
                self.store.kv_put(&name, &waiting)?;
            }
        }
        Ok(())
    }

    /// Stores a message that arrived, with any reactions (and edit) that
    /// came first.
    /// Returns its sequence number.
    pub(crate) fn add_received(&mut self, chat_id: i64, mut msg: StoredMessage) -> Result<i64> {
        if let Some(id) = msg.id {
            let name = pending_name(chat_id, &id);
            if let Some(waiting) = self.store.kv_get::<Vec<Reaction>>(&name)? {
                for reaction in waiting {
                    apply(&mut msg, reaction);
                }
                self.store.kv_delete(&name)?;
            }
        }
        self.take_waiting_edit(chat_id, &mut msg)?;
        Ok(self.store.add_message(chat_id, &msg)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_are_tidied() {
        assert_eq!(tidy_emoji("  👍 ").as_deref(), Some("👍"));
        assert_eq!(tidy_emoji("").as_deref(), Some(""), "taking it back");
        // A family with skin tones is long, but still one reaction.
        assert!(tidy_emoji("👨🏽‍👩🏽‍👧🏽").is_some());
        assert!(tidy_emoji(&"🔥".repeat(9)).is_none(), "too long");
        assert!(tidy_emoji("a\nb").is_none());
        assert!(tidy_emoji("\u{7}").is_none());
    }

    #[test]
    fn one_reaction_each_and_newer_wins() {
        let mut msg = StoredMessage::default();
        let r = |who: u8, emoji: &str, at| Reaction { who: [who; 32], emoji: emoji.into(), at };
        assert!(apply(&mut msg, r(1, "👍", 5)));
        assert!(apply(&mut msg, r(1, "❤️", 6)), "replaces");
        assert!(!apply(&mut msg, r(1, "😂", 4)), "older loses");
        assert!(apply(&mut msg, r(2, "❤️", 1)));
        assert_eq!(msg.reactions.len(), 2);
        assert!(apply(&mut msg, r(1, "", 7)), "taken back");
        assert!(!apply(&mut msg, r(1, "👍", 6)), "stays taken back");
        // Taken back before the reaction itself arrived: it stays gone.
        assert!(apply(&mut msg, r(3, "", 9)));
        assert!(!apply(&mut msg, r(3, "😮", 8)));
        assert_eq!(msg.reactions.iter().filter(|r| !r.emoji.is_empty()).count(), 1);
    }
}
