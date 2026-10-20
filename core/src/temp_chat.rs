//! Temp chats: a separate section of the app whose messages never outlive
//! 24 hours, and which the user can wipe at any moment.
//!
//! Temp chats are held in memory only and never written to the database.

use crate::transport::TransportKind;

pub const TEMP_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct TempMessage {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: u64,
}

impl TempMessage {
    pub fn expires_at(&self) -> u64 {
        self.sent_at + TEMP_TTL_SECS
    }
}

#[derive(Debug, Default)]
pub struct TempChats {
    chats: Vec<TempChat>,
}

#[derive(Debug)]
pub struct TempChat {
    pub id: u64,
    pub transport: TransportKind,
    pub messages: Vec<TempMessage>,
}

impl TempChats {
    pub fn open(&mut self, id: u64, transport: TransportKind) -> &mut TempChat {
        let idx = match self.chats.iter().position(|c| c.id == id) {
            Some(i) => i,
            None => {
                self.chats.push(TempChat { id, transport, messages: Vec::new() });
                self.chats.len() - 1
            }
        };
        &mut self.chats[idx]
    }

    pub fn get(&self, id: u64) -> Option<&TempChat> {
        self.chats.iter().find(|c| c.id == id)
    }

    /// Drops expired messages and chats left empty. Run on a timer and on
    /// every app resume.
    pub fn purge_expired(&mut self, now: u64) {
        for chat in &mut self.chats {
            chat.messages.retain(|m| m.expires_at() > now);
        }
        self.chats.retain(|c| !c.messages.is_empty());
    }

    pub fn wipe(&mut self, id: u64) {
        self.chats.retain(|c| c.id != id);
    }

    pub fn wipe_all(&mut self) {
        self.chats.clear();
    }

    pub fn len(&self) -> usize {
        self.chats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chats.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(sent_at: u64) -> TempMessage {
        TempMessage { from_me: true, body: "hi".into(), sent_at }
    }

    #[test]
    fn messages_expire_after_24h() {
        let mut t = TempChats::default();
        t.open(1, TransportKind::Bluetooth).messages.extend([msg(0), msg(1000)]);
        t.purge_expired(TEMP_TTL_SECS - 1);
        assert_eq!(t.get(1).unwrap().messages.len(), 2);
        t.purge_expired(TEMP_TTL_SECS);
        assert_eq!(t.get(1).unwrap().messages, vec![msg(1000)]);
        t.purge_expired(TEMP_TTL_SECS + 1000);
        assert!(t.is_empty());
    }

    #[test]
    fn wipe_one_and_all() {
        let mut t = TempChats::default();
        t.open(1, TransportKind::Internet).messages.push(msg(0));
        t.open(2, TransportKind::WifiDirect).messages.push(msg(0));
        t.wipe(1);
        assert!(t.get(1).is_none() && t.get(2).is_some());
        t.wipe_all();
        assert!(t.is_empty());
    }
}
