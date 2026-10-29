//! Messages written now and sent later, by this phone.
//!
//! They wait here, sealed in the store, until their time; then they go
//! like any other message (the phone has to be on, with Sanctum running in
//! the background, at that time; otherwise they go as soon as it is).

use crate::session::{clip, now, Event, Result, Session, SessionError};
use serde::{Deserialize, Serialize};

const SCHEDULED: &[u8] = b"scheduled";
/// More than this waiting at once is refused.
const MAX_SCHEDULED: usize = 100;
/// The furthest ahead a message can be scheduled, in seconds.
const MAX_AHEAD: u64 = 366 * 24 * 3600;

/// A message waiting for its time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scheduled {
    pub id: u64,
    pub chat_id: i64,
    pub body: String,
    /// When it goes, in Unix seconds.
    pub at: u64,
}

impl Session {
    fn scheduled_all(&self) -> Result<Vec<Scheduled>> {
        Ok(self.store.kv_get(SCHEDULED)?.unwrap_or_default())
    }

    fn save_scheduled(&mut self, all: &[Scheduled]) -> Result<()> {
        self.store.kv_put(SCHEDULED, &all)?;
        self.scheduled_next = all.iter().map(|s| s.at).min().unwrap_or(u64::MAX);
        Ok(())
    }

    /// Sends `body` in the chat at `at` (Unix seconds). Returns its id.
    pub fn schedule_message(&mut self, chat_id: i64, body: &str, at: u64) -> Result<u64> {
        self.chat(chat_id)?;
        let body = clip(body.trim(), 4000);
        if body.is_empty() {
            return Err(SessionError::GroupRule("A message can't be empty."));
        }
        if at <= now() || at > now() + MAX_AHEAD {
            return Err(SessionError::GroupRule("Pick a time later today or in the coming year."));
        }
        let mut all = self.scheduled_all()?;
        if all.len() >= MAX_SCHEDULED {
            return Err(SessionError::GroupRule("Too many messages are waiting to be sent."));
        }
        let id = rand::random::<u64>() >> 11; // fits a double exactly
        all.push(Scheduled { id, chat_id, body, at });
        all.sort_by_key(|s| s.at);
        self.save_scheduled(&all)?;
        self.events.push(Event::Chat(chat_id));
        Ok(id)
    }

    /// Messages waiting in a chat, soonest first.
    pub fn scheduled(&self, chat_id: i64) -> Result<Vec<Scheduled>> {
        Ok(self.scheduled_all()?.into_iter().filter(|s| s.chat_id == chat_id).collect())
    }

    /// Forgets a waiting message.
    pub fn cancel_scheduled(&mut self, id: u64) -> Result<()> {
        let mut all = self.scheduled_all()?;
        if let Some(i) = all.iter().position(|s| s.id == id) {
            let gone = all.remove(i);
            self.save_scheduled(&all)?;
            self.events.push(Event::Chat(gone.chat_id));
        }
        Ok(())
    }

    /// Sends the messages whose time has come. Cheap until one is due.
    pub(crate) fn send_scheduled(&mut self) -> Result<()> {
        let now = now();
        if self.scheduled_next != 0 && self.scheduled_next > now {
            return Ok(());
        }
        let all = self.scheduled_all()?;
        let (due, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|s| s.at <= now);
        self.save_scheduled(&rest)?;
        for s in due {
            // A chat deleted since: nothing to send.
            if let Ok(reach) = self.send_to_chat(s.chat_id, &s.body, None) {
                self.pokes.extend(reach);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{befriend, connect, Phone};

    #[test]
    fn scheduled_messages_go_at_their_time() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        let soon = alice.s.schedule_message(ab, "happy birthday!", now() + 2).unwrap();
        let later = alice.s.schedule_message(ab, "never mind", now() + 3600).unwrap();
        assert!(alice.s.schedule_message(ab, "too late", now() - 1).is_err());
        assert_eq!(alice.s.scheduled(ab).unwrap().len(), 2);
        alice.s.purge();
        assert!(alice.texts(ab).is_empty(), "not yet");
        alice.s.cancel_scheduled(later).unwrap();
        // Its time comes (as if the clock moved on).
        let mut all = alice.s.scheduled_all().unwrap();
        all[0].at = now();
        assert_eq!(all[0].id, soon);
        alice.s.save_scheduled(&all).unwrap();
        alice.restart();
        alice.s.purge();
        assert!(alice.s.scheduled(ab).unwrap().is_empty());
        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(ba), [(false, "happy birthday!".into(), true)]);
    }
}
