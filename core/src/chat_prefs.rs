//! Per-chat choices: muting, the unsent draft, and the pinned message.
//!
//! Muting and drafts stay on this phone. A pin ([`Content::Pin`]) is
//! shared: anyone in the chat can pin a message or take the pin away, and
//! the newest choice wins on every phone. It names the message by its
//! shared id, so a pin can come before its message does.

use crate::protocol::{Content, MessageId};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use serde::{Deserialize, Serialize};

/// Muted until forever.
pub const MUTED_FOREVER: u64 = u64::MAX;
/// Longest draft kept, in characters.
const MAX_DRAFT: usize = 4000;

#[derive(Serialize, Deserialize, Default)]
struct Pinned {
    id: Option<MessageId>,
    /// When it was chosen, in milliseconds.
    at: u64,
}

fn name(what: &[u8], chat_id: i64) -> Vec<u8> {
    [what, &chat_id.to_le_bytes()].concat()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Session {
    /// Silences a chat's notifications until `until` (Unix seconds;
    /// [`MUTED_FOREVER`] for good, 0 to unmute). Messages still arrive and
    /// count as unread.
    pub fn set_muted(&mut self, chat_id: i64, until: u64) -> Result<()> {
        self.chat(chat_id)?;
        let key = name(b"mute:", chat_id);
        if until <= now() {
            self.store.kv_delete(&key)?;
        } else {
            self.store.kv_put(&key, &until)?;
        }
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    /// Until when a chat is muted (Unix seconds), or 0 if it isn't.
    pub fn muted_until(&self, chat_id: i64) -> Result<u64> {
        let until: u64 = self.store.kv_get(&name(b"mute:", chat_id))?.unwrap_or(0);
        Ok(if until > now() { until } else { 0 })
    }

    /// Keeps what was typed but not sent (empty clears it).
    pub fn set_draft(&mut self, chat_id: i64, text: &str) -> Result<()> {
        let key = name(b"draft:", chat_id);
        let had = self.draft(chat_id)?;
        let text = clip(text, MAX_DRAFT);
        if text.trim().is_empty() {
            self.store.kv_delete(&key)?;
        } else {
            self.store.kv_put(&key, &text)?;
        }
        if had.trim().is_empty() != text.trim().is_empty() {
            self.events.push(Event::Chat(chat_id));
        }
        Ok(())
    }

    /// What was typed but not sent, if anything.
    pub fn draft(&self, chat_id: i64) -> Result<String> {
        Ok(self.store.kv_get(&name(b"draft:", chat_id))?.unwrap_or_default())
    }

    /// Pins the message at `seq` for everyone in the chat (None takes the
    /// pin away). Returns who to reach.
    pub fn pin_message(&mut self, chat_id: i64, seq: Option<i64>) -> Result<Vec<[u8; 32]>> {
        self.chat(chat_id)?;
        let id = match seq {
            Some(seq) => {
                let m = self.store.message(chat_id, seq)?.ok_or(SessionError::Protocol)?;
                if m.notice || m.id.is_none() {
                    return Err(SessionError::Protocol);
                }
                m.id
            }
            None => None,
        };
        let key = name(b"pinned-msg:", chat_id);
        let last = self.store.kv_get::<Pinned>(&key)?.unwrap_or_default();
        let at = now_ms().max(last.at + 1);
        self.store.kv_put(&key, &Pinned { id, at })?;
        self.events.push(Event::Chat(chat_id));
        self.tell_chat(chat_id, Content::Pin { id, at })
    }

    /// The pinned message's place in the chat, if it's here.
    pub fn pinned_message(&self, chat_id: i64) -> Result<Option<i64>> {
        let Some(Pinned { id: Some(id), .. }) = self.store.kv_get(&name(b"pinned-msg:", chat_id))? else {
            return Ok(None);
        };
        Ok(self.store.message_by_id(chat_id, &id)?.map(|(seq, _)| seq))
    }

    /// Someone in the chat pinned a message (or took the pin away).
    pub(crate) fn on_pin(&mut self, chat_id: i64, id: Option<MessageId>, at: u64) -> Result<()> {
        let key = name(b"pinned-msg:", chat_id);
        let last = self.store.kv_get::<Pinned>(&key)?.unwrap_or_default();
        if at > last.at {
            self.store.kv_put(&key, &Pinned { id, at })?;
            self.events.push(Event::Chat(chat_id));
        }
        Ok(())
    }

    /// Forgets a deleted chat's choices.
    pub(crate) fn forget_chat_prefs(&mut self, chat_id: i64) -> Result<()> {
        for what in [b"mute:".as_slice(), b"draft:", b"pinned-msg:", b"disappear:"] {
            self.store.kv_delete(&name(what, chat_id))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::session::tests::{befriend, connect, Phone};
    use crate::session::now;

    #[test]
    fn pins_reach_the_friend_and_the_newest_wins() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "the address is 12 Elm St").unwrap();
        alice.s.send_text(ab, "see you").unwrap();
        connect(&mut alice, &mut bob);
        alice.s.pin_message(ab, Some(0)).unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.s.pinned_message(ba).unwrap(), Some(0));
        bob.s.pin_message(ba, Some(1)).unwrap();
        connect(&mut bob, &mut alice);
        assert_eq!(alice.s.pinned_message(ab).unwrap(), Some(1));
        alice.s.pin_message(ab, None).unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.s.pinned_message(ba).unwrap(), None);
        // An older choice arriving late changes nothing.
        bob.s.on_pin(ba, Some([9; 16]), 1).unwrap();
        assert_eq!(bob.s.pinned_message(ba).unwrap(), None);
    }

    #[test]
    fn mutes_and_drafts_stay_here() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        assert_eq!(alice.s.muted_until(ab).unwrap(), 0);
        alice.s.set_muted(ab, now() + 3600).unwrap();
        assert!(alice.s.muted_until(ab).unwrap() > now());
        alice.s.set_muted(ab, 0).unwrap();
        assert_eq!(alice.s.muted_until(ab).unwrap(), 0);
        alice.s.set_draft(ab, "half a thought").unwrap();
        alice.restart();
        assert_eq!(alice.s.draft(ab).unwrap(), "half a thought");
        alice.s.set_draft(ab, "  ").unwrap();
        assert_eq!(alice.s.draft(ab).unwrap(), "");
    }
}
