//! Disappearing messages in friends' chats and groups.
//!
//! Anyone in a chat can set its timer ([`Content::Disappear`]); the newest
//! choice wins on every phone, and a notice says who changed it. Messages
//! sent while it's on carry when they go (`StoredMessage::expires_at`,
//! counted from when they were sent), and every phone deletes them, with
//! their files, once that time comes. Messages from before stay.

use crate::protocol::Content;
use crate::session::{now, Event, Result, Session, SessionError};
use crate::store::{DisappearTimer, StoredMessage};

/// The shortest and longest timer a chat can have.
const SHORTEST: u64 = 60;
const LONGEST: u64 = 90 * 24 * 3600;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A timer in words: "1 hour", "1 week".
pub fn timer_words(seconds: u64) -> String {
    let (n, unit) = match seconds {
        s if s % (7 * 86400) == 0 => (s / (7 * 86400), "week"),
        s if s % 86400 == 0 => (s / 86400, "day"),
        s if s % 3600 == 0 => (s / 3600, "hour"),
        s => (s.div_ceil(60), "minute"),
    };
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

fn notice(who: &str, seconds: u64) -> String {
    match seconds {
        0 => format!("{who} turned off disappearing messages."),
        s => format!("{who} turned on disappearing messages. New messages go after {}.", timer_words(s)),
    }
}

fn tidy(seconds: u64) -> Option<u64> {
    match seconds {
        0 => Some(0),
        s if (SHORTEST..=LONGEST).contains(&s) => Some(s),
        _ => None,
    }
}

impl Session {
    /// Sets how long new messages in this chat last (0: for good). Returns
    /// who to reach.
    pub fn set_disappear(&mut self, chat_id: i64, seconds: u64) -> Result<Vec<[u8; 32]>> {
        self.chat(chat_id)?;
        let seconds = tidy(seconds).ok_or(SessionError::Protocol)?;
        let last = self.store.disappear_timer(chat_id)?;
        if last.seconds == seconds {
            return Ok(Vec::new());
        }
        let at = now_ms().max(last.at + 1);
        self.store.set_disappear_timer(chat_id, &DisappearTimer { seconds, at })?;
        self.add_timer_notice(chat_id, notice("You", seconds))?;
        self.tell_chat(chat_id, Content::Disappear { seconds, at })
    }

    /// How long new messages in this chat last, in seconds (0: for good).
    pub fn disappear_seconds(&self, chat_id: i64) -> Result<u64> {
        Ok(self.store.disappear_timer(chat_id)?.seconds)
    }

    /// `who` in the chat set its timer.
    pub(crate) fn on_disappear(&mut self, chat_id: i64, who: &str, seconds: u64, at: u64) -> Result<()> {
        let Some(seconds) = tidy(seconds) else {
            return Ok(());
        };
        let last = self.store.disappear_timer(chat_id)?;
        if at <= last.at {
            return Ok(());
        }
        self.store.set_disappear_timer(chat_id, &DisappearTimer { seconds, at })?;
        if last.seconds != seconds {
            self.add_timer_notice(chat_id, notice(who, seconds))?;
        }
        Ok(())
    }

    fn add_timer_notice(&mut self, chat_id: i64, body: String) -> Result<()> {
        let msg = StoredMessage { body, sent_at: now(), delivered: true, notice: true, ..Default::default() };
        self.store.add_message(chat_id, &msg)?;
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    /// Deletes the messages whose time has come, with their files. Cheap
    /// until one is due.
    pub(crate) fn burn_expired(&mut self) -> Result<()> {
        for (chat_id, seq, m) in self.store.take_expired(now())? {
            if let Some(media) = &m.media {
                self.delete_blobs(&[media.blob]);
            }
            self.store.delete_message(chat_id, seq)?;
            if !self.events.contains(&Event::Chat(chat_id)) {
                self.events.push(Event::Chat(chat_id));
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
    fn timers_read_well() {
        assert_eq!(timer_words(3600), "1 hour");
        assert_eq!(timer_words(86400), "1 day");
        assert_eq!(timer_words(7 * 86400), "1 week");
        assert_eq!(timer_words(8 * 3600), "8 hours");
        assert_eq!(timer_words(90), "2 minutes");
    }

    #[test]
    fn both_phones_follow_the_timer_and_burn_on_time() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "stays").unwrap();
        alice.s.set_disappear(ab, 3600).unwrap();
        alice.s.send_text(ab, "goes").unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.s.disappear_seconds(ba).unwrap(), 3600);
        let bodies = |p: &Phone, chat| p.texts(chat).into_iter().map(|t| t.1).collect::<Vec<_>>();
        assert_eq!(bodies(&bob, ba).len(), 3, "the notice and both messages");
        assert!(bodies(&bob, ba)[1].starts_with("Alice turned on"), "{:?}", bodies(&bob, ba));
        let msgs = bob.s.store.messages(ba, None).unwrap();
        assert_eq!(msgs[0].expires_at, 0);
        assert!(msgs[2].expires_at > now());
        // Bob answers: his reply goes too.
        bob.s.send_text(ba, "ok").unwrap();
        assert!(bob.s.store.messages(ba, None).unwrap()[3].expires_at > now());
        connect(&mut bob, &mut alice);
        // An hour later, on both phones.
        for (phone, chat) in [(&mut alice, ab), (&mut bob, ba)] {
            let due = phone.s.store.take_expired(now() + 3601).unwrap();
            assert_eq!(due.len(), 2);
            for (c, seq, _) in due {
                phone.s.store.delete_message(c, seq).unwrap();
            }
            assert_eq!(bodies(phone, chat)[0], "stays");
        }
        // Turning it off is shared too.
        bob.s.set_disappear(ba, 0).unwrap();
        connect(&mut bob, &mut alice);
        assert_eq!(alice.s.disappear_seconds(ab).unwrap(), 0);
        assert!(bodies(&alice, ab).last().unwrap().starts_with("Bob turned off"));
        assert!(alice.s.set_disappear(ab, 5).is_err(), "too short");
    }
}
