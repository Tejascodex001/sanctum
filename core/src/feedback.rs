//! Feedback from people using the app, delivered to the developer's own
//! Sanctum through Tor, with no server in between.
//!
//! The developer's phone keeps a separate feedback key (never its own
//! identity) and, while receiving is on, serves an onion service for it.
//! Each build carries that key's address. Sending is anonymous: the sender
//! connects through Tor, hands over one [`Feedback`] and hangs up, without
//! a hello, so nothing says which phone or person it came from. It isn't a
//! Sanctum link at all, so it never meets [`crate::protocol::Frame`].
//!
//! Wire format: [`MAGIC`], the length (u32, big-endian), then the postcard
//! of [`Feedback`]; the receiver answers [`RECEIVED`]. Waiting feedback is
//! kept (sealed) until it's through, so it survives restarts.

use crate::identity::Identity;
use crate::session::{clip, now, Result, Session};
use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 4] = b"SFB1";
/// The receiver's answer: it's stored.
pub const RECEIVED: &[u8; 2] = b"OK";
/// Largest feedback accepted on the wire, length prefix included.
pub const MAX_WIRE: usize = 16 * 1024;
pub const MAX_TEXT: usize = 4000;
/// Phone and Android version, each.
pub const MAX_DETAIL: usize = 80;
/// Sent from one phone in a day, at most.
pub const MAX_PER_DAY: usize = 5;
/// Kept in the inbox; older ones go first.
pub const MAX_INBOX: usize = 500;
/// Feedback that couldn't be delivered for this long is dropped.
const GIVE_UP_AFTER: u64 = 30 * 24 * 3600;
const DAY: u64 = 24 * 3600;

const KEY: &[u8] = b"feedback:key";
const RECEIVING: &[u8] = b"feedback:receiving";
const INBOX: &[u8] = b"feedback:inbox";
const OUTBOX: &[u8] = b"feedback:outbox";
const SENT: &[u8] = b"feedback:sent";

/// What someone sends. Stored formats: never reorder or remove fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feedback {
    /// The phone's maker and model, if they chose to include it.
    pub phone: String,
    /// Android's version, if they chose to include it.
    pub android: String,
    /// Sanctum's build number (0 if left out).
    pub build: i64,
    /// 1 to 5 stars, 0 for none.
    pub rating: u8,
    pub text: String,
}

impl Feedback {
    /// Within the limits, trimmed; `None` if there's nothing to send.
    pub fn cleaned(&self) -> Option<Self> {
        let text = clip(self.text.trim(), MAX_TEXT);
        let rating = self.rating.min(5);
        (!text.is_empty() || rating > 0).then(|| Self {
            phone: clip(self.phone.trim(), MAX_DETAIL),
            android: clip(self.android.trim(), MAX_DETAIL),
            build: self.build.max(0),
            rating,
            text,
        })
    }
}

/// Feedback in the developer's inbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Received {
    /// When it arrived (seconds since 1970).
    pub at: u64,
    pub feedback: Feedback,
}

/// Feedback waiting to be delivered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub id: [u8; 16],
    /// The feedback key it goes to.
    pub to: [u8; 32],
    pub at: u64,
    pub feedback: Feedback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Nothing written and no stars.
    Empty,
    /// [`MAX_PER_DAY`] sent already.
    TooMany,
}

/// The bytes to send for `feedback`.
pub fn encode(feedback: &Feedback) -> Vec<u8> {
    let body = postcard::to_allocvec(feedback).expect("feedback serializes");
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// What came in can't be feedback: wrong start, too big, unreadable or
/// empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotFeedback;

/// Feedback read from the start of `buf`: `Ok(None)` while more bytes are
/// needed.
pub fn decode(buf: &[u8]) -> std::result::Result<Option<Feedback>, NotFeedback> {
    let head = buf.len().min(MAGIC.len());
    if buf[..head] != MAGIC[..head] {
        return Err(NotFeedback);
    }
    let Some(len) = buf.get(4..8) else { return Ok(None) };
    let len = u32::from_be_bytes(len.try_into().unwrap()) as usize;
    if len + 8 > MAX_WIRE {
        return Err(NotFeedback);
    }
    let Some(body) = buf.get(8..8 + len) else { return Ok(None) };
    let feedback: Feedback = postcard::from_bytes(body).map_err(|_| NotFeedback)?;
    feedback.cleaned().map(Some).ok_or(NotFeedback)
}

impl Session {
    /// Queues feedback for the developer (the feedback key `to`). Sent
    /// through Tor as soon as possible, retried until it's through.
    pub fn feedback_queue(&mut self, to: [u8; 32], feedback: &Feedback) -> Result<std::result::Result<(), Refused>> {
        let Some(feedback) = feedback.cleaned() else { return Ok(Err(Refused::Empty)) };
        let at = now();
        let mut sent: Vec<u64> = self.store.kv_get(SENT)?.unwrap_or_default();
        sent.retain(|t| at.saturating_sub(*t) < DAY);
        if sent.len() >= MAX_PER_DAY {
            return Ok(Err(Refused::TooMany));
        }
        sent.push(at);
        self.store.kv_put(SENT, &sent)?;
        let mut queue = self.feedback_waiting()?;
        queue.push(Queued { id: rand::random(), to, at, feedback });
        self.store.kv_put(OUTBOX, &queue)?;
        Ok(Ok(()))
    }

    /// Feedback still to deliver, oldest first. Gives up on what has
    /// waited [`GIVE_UP_AFTER`].
    pub fn feedback_waiting(&mut self) -> Result<Vec<Queued>> {
        let mut queue: Vec<Queued> = self.store.kv_get(OUTBOX)?.unwrap_or_default();
        let before = queue.len();
        let at = now();
        queue.retain(|q| at.saturating_sub(q.at) < GIVE_UP_AFTER);
        if queue.len() != before {
            self.store.kv_put(OUTBOX, &queue)?;
        }
        Ok(queue)
    }

    /// The developer's phone has it.
    pub fn feedback_delivered(&mut self, id: &[u8; 16]) -> Result<()> {
        let mut queue: Vec<Queued> = self.store.kv_get(OUTBOX)?.unwrap_or_default();
        queue.retain(|q| q.id != *id);
        self.store.kv_put(OUTBOX, &queue).map_err(Into::into)
    }

    /// This phone's feedback key (made the first time it's asked for). Its
    /// public half is the address builds send feedback to.
    pub fn feedback_identity(&mut self) -> Result<Identity> {
        if let Some(secret) = self.store.kv_get::<[u8; 32]>(KEY)? {
            return Ok(Identity::from_secret(secret));
        }
        let identity = Identity::generate();
        self.store.kv_put(KEY, &identity.secret())?;
        Ok(identity)
    }

    /// Whether this phone takes feedback in (the developer's switch).
    pub fn feedback_receiving(&self) -> Result<bool> {
        Ok(self.store.kv_get(RECEIVING)?.unwrap_or(false))
    }

    pub fn set_feedback_receiving(&mut self, on: bool) -> Result<()> {
        self.store.kv_put(RECEIVING, &on).map_err(Into::into)
    }

    /// Feedback arrived. Kept only while receiving is on; `false` if not.
    pub fn feedback_arrived(&mut self, feedback: &Feedback) -> Result<bool> {
        let Some(feedback) = feedback.cleaned() else { return Ok(false) };
        if !self.feedback_receiving()? {
            return Ok(false);
        }
        let mut inbox = self.feedback_inbox()?;
        inbox.insert(0, Received { at: now(), feedback });
        inbox.truncate(MAX_INBOX);
        self.store.kv_put(INBOX, &inbox)?;
        Ok(true)
    }

    /// Feedback received, newest first.
    pub fn feedback_inbox(&self) -> Result<Vec<Received>> {
        Ok(self.store.kv_get(INBOX)?.unwrap_or_default())
    }

    /// Deletes one from the inbox (by when it arrived and what it says).
    pub fn feedback_delete(&mut self, item: &Received) -> Result<()> {
        let mut inbox = self.feedback_inbox()?;
        if let Some(i) = inbox.iter().position(|r| r == item) {
            inbox.remove(i);
            self.store.kv_put(INBOX, &inbox)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{onion_address, parse_onion_address};
    use crate::session::tests::Phone;

    fn sample() -> Feedback {
        Feedback {
            phone: "Samsung Galaxy A54".into(),
            android: "14".into(),
            build: 65,
            rating: 4,
            text: "Love the sigils. Calls drop sometimes.".into(),
        }
    }

    #[test]
    fn wire_round_trip_in_any_pieces() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut]), Ok(None), "cut at {cut}");
        }
        assert_eq!(decode(&bytes), Ok(Some(sample())));
    }

    #[test]
    fn junk_and_oversize_are_refused() {
        assert_eq!(decode(b"GET / HTTP/1.1"), Err(NotFeedback));
        let mut big = MAGIC.to_vec();
        big.extend_from_slice(&(MAX_WIRE as u32).to_be_bytes());
        assert_eq!(decode(&big), Err(NotFeedback));
        let mut bad = MAGIC.to_vec();
        bad.extend_from_slice(&3u32.to_be_bytes());
        bad.extend_from_slice(&[0xff, 0xff, 0xff]);
        assert_eq!(decode(&bad), Err(NotFeedback));
        // Empty feedback isn't worth keeping.
        let empty = Feedback { rating: 0, text: "  ".into(), ..sample() };
        assert_eq!(decode(&encode(&empty)), Err(NotFeedback));
    }

    #[test]
    fn cleaning_clips_and_caps() {
        let long = Feedback { text: "x".repeat(MAX_TEXT + 50), rating: 9, phone: "p".repeat(200), ..sample() };
        let clean = long.cleaned().unwrap();
        assert_eq!(clean.text.chars().count(), MAX_TEXT);
        assert_eq!(clean.phone.chars().count(), MAX_DETAIL);
        assert_eq!(clean.rating, 5);
        assert!(Feedback { text: String::new(), rating: 3, ..sample() }.cleaned().is_some());
    }

    #[test]
    fn queued_feedback_waits_until_delivered_even_across_restarts() {
        let mut p = Phone::new();
        let to = [7; 32];
        assert_eq!(p.s.feedback_queue(to, &sample()).unwrap(), Ok(()));
        p.restart();
        let waiting = p.s.feedback_waiting().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!((waiting[0].to, &waiting[0].feedback), (to, &sample()));
        p.s.feedback_delivered(&waiting[0].id).unwrap();
        assert!(p.s.feedback_waiting().unwrap().is_empty());
    }

    #[test]
    fn a_phone_sends_only_a_few_a_day() {
        let mut p = Phone::new();
        for _ in 0..MAX_PER_DAY {
            assert_eq!(p.s.feedback_queue([1; 32], &sample()).unwrap(), Ok(()));
        }
        assert_eq!(p.s.feedback_queue([1; 32], &sample()).unwrap(), Err(Refused::TooMany));
        let empty = Feedback { text: String::new(), rating: 0, ..sample() };
        assert_eq!(Phone::new().s.feedback_queue([1; 32], &empty).unwrap(), Err(Refused::Empty));
    }

    #[test]
    fn inbox_takes_feedback_only_while_receiving() {
        let mut dev = Phone::new();
        assert!(!dev.s.feedback_arrived(&sample()).unwrap());
        dev.s.set_feedback_receiving(true).unwrap();
        assert!(dev.s.feedback_arrived(&sample()).unwrap());
        let second = Feedback { text: "Second".into(), ..sample() };
        assert!(dev.s.feedback_arrived(&second).unwrap());
        dev.restart();
        let inbox = dev.s.feedback_inbox().unwrap();
        assert_eq!(inbox.iter().map(|r| r.feedback.text.as_str()).collect::<Vec<_>>(), ["Second", &sample().text]);
        dev.s.feedback_delete(&inbox[0]).unwrap();
        assert_eq!(dev.s.feedback_inbox().unwrap().len(), 1);
    }

    #[test]
    fn the_feedback_key_is_kept_and_isnt_the_identity() {
        let mut dev = Phone::new();
        let key = dev.s.feedback_identity().unwrap().public_key();
        assert_ne!(key, dev.pk());
        dev.restart();
        assert_eq!(dev.s.feedback_identity().unwrap().public_key(), key);
        assert_eq!(parse_onion_address(&onion_address(&key)), Some(key));
    }

    #[test]
    fn onion_addresses_parse_back_and_typos_are_caught() {
        let key = Identity::generate().public_key();
        let address = onion_address(&key);
        assert_eq!(parse_onion_address(&address.to_uppercase()), Some(key));
        assert_eq!(parse_onion_address(address.trim_end_matches(".onion")), Some(key));
        let mut typo = address.clone().into_bytes();
        typo[10] = if typo[10] == b'a' { b'b' } else { b'a' };
        assert_eq!(parse_onion_address(std::str::from_utf8(&typo).unwrap()), None);
        assert_eq!(parse_onion_address("not an address"), None);
    }
}
