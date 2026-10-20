//! Temp chats: conversations whose messages never outlive 24 hours and
//! that either person can wipe at any moment, from both phones.
//!
//! They live in memory only: never written to the database, on either
//! phone. Closing the app ends them.

use crate::protocol::{MessageId, TempId};

pub const TEMP_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct TempMessage {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: u64,
    /// Our messages: the delivery id, and whether the friend's phone
    /// acknowledged it.
    pub id: Option<MessageId>,
    pub delivered: bool,
}

impl TempMessage {
    pub fn expires_at(&self) -> u64 {
        self.sent_at + TEMP_TTL_SECS
    }
}

#[derive(Debug)]
pub struct TempChat {
    /// This phone's id for the chat.
    pub id: u64,
    /// The id both phones share.
    pub remote: TempId,
    /// The friend in this chat; `None` for a temp chat with yourself.
    pub peer: Option<[u8; 32]>,
    pub title: String,
    pub messages: Vec<TempMessage>,
}

#[derive(Debug, Default)]
pub struct TempChats {
    chats: Vec<TempChat>,
    next_id: u64,
}

impl TempChats {
    /// Starts a new temp chat.
    pub fn create(&mut self, peer: Option<[u8; 32]>, title: String) -> &mut TempChat {
        self.insert(rand::random(), peer, title)
    }

    /// The chat a friend's message belongs to, created if this is the first
    /// we hear of it.
    pub fn for_remote(&mut self, peer: [u8; 32], remote: TempId, title: &str) -> &mut TempChat {
        match self
            .chats
            .iter()
            .position(|c| c.peer == Some(peer) && c.remote == remote)
        {
            Some(i) => &mut self.chats[i],
            None => self.insert(remote, Some(peer), title.to_string()),
        }
    }

    fn insert(&mut self, remote: TempId, peer: Option<[u8; 32]>, title: String) -> &mut TempChat {
        self.next_id += 1;
        self.chats.push(TempChat {
            id: self.next_id,
            remote,
            peer,
            title,
            messages: Vec::new(),
        });
        self.chats.last_mut().unwrap()
    }

    pub fn get(&self, id: u64) -> Option<&TempChat> {
        self.chats.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut TempChat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    pub fn all(&self) -> &[TempChat] {
        &self.chats
    }

    /// Drops expired messages, and chats whose last message just expired.
    /// Returns the ids of chats that burned away entirely.
    pub fn purge_expired(&mut self, now: u64) -> Vec<u64> {
        let mut burned = Vec::new();
        self.chats.retain_mut(|chat| {
            let had_messages = !chat.messages.is_empty();
            chat.messages.retain(|m| m.expires_at() > now);
            let keep = !had_messages || !chat.messages.is_empty();
            if !keep {
                burned.push(chat.id);
            }
            keep
        });
        burned
    }

    /// Removes a chat. Returns it so the caller can tell the friend.
    pub fn wipe(&mut self, id: u64) -> Option<TempChat> {
        let i = self.chats.iter().position(|c| c.id == id)?;
        Some(self.chats.remove(i))
    }

    /// Removes the chat a friend wiped. Returns this phone's id for it.
    pub fn wipe_remote(&mut self, peer: [u8; 32], remote: TempId) -> Option<u64> {
        let i = self
            .chats
            .iter()
            .position(|c| c.peer == Some(peer) && c.remote == remote)?;
        Some(self.chats.remove(i).id)
    }

    pub fn wipe_all(&mut self) -> Vec<TempChat> {
        std::mem::take(&mut self.chats)
    }

    /// Marks our message with delivery `id` as received by the friend.
    /// Returns the chat's id if found.
    pub fn mark_delivered(&mut self, id: &MessageId) -> Option<u64> {
        for chat in &mut self.chats {
            if let Some(m) = chat.messages.iter_mut().find(|m| m.id.as_ref() == Some(id)) {
                m.delivered = true;
                return Some(chat.id);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(sent_at: u64) -> TempMessage {
        TempMessage {
            from_me: true,
            body: "hi".into(),
            sent_at,
            id: None,
            delivered: false,
        }
    }

    #[test]
    fn messages_expire_after_24h() {
        let mut t = TempChats::default();
        let id = t.create(None, "Ember".into()).id;
        t.get_mut(id).unwrap().messages.extend([msg(0), msg(1000)]);
        assert!(t.purge_expired(TEMP_TTL_SECS - 1).is_empty());
        assert_eq!(t.get(id).unwrap().messages.len(), 2);
        t.purge_expired(TEMP_TTL_SECS);
        assert_eq!(t.get(id).unwrap().messages, vec![msg(1000)]);
        assert_eq!(t.purge_expired(TEMP_TTL_SECS + 1000), vec![id]);
        assert!(t.all().is_empty());
    }

    #[test]
    fn new_empty_chat_survives_purge() {
        let mut t = TempChats::default();
        let id = t.create(None, "x".into()).id;
        t.purge_expired(u64::MAX);
        assert!(t.get(id).is_some());
    }

    #[test]
    fn remote_chats_are_matched_by_friend_and_id() {
        let mut t = TempChats::default();
        let (bob, carol) = ([1; 32], [2; 32]);
        let a = t.for_remote(bob, [9; 16], "Bob").id;
        assert_eq!(t.for_remote(bob, [9; 16], "Bob").id, a);
        let b = t.for_remote(carol, [9; 16], "Carol").id;
        assert_ne!(a, b, "same id from a different friend is a different chat");
        assert_eq!(t.wipe_remote(carol, [9; 16]), Some(b));
        assert_eq!(t.wipe_remote(carol, [9; 16]), None);
        assert!(t.get(a).is_some());
    }

    #[test]
    fn wipe_one_and_all() {
        let mut t = TempChats::default();
        let a = t.create(None, "a".into()).id;
        let b = t.create(Some([1; 32]), "b".into()).id;
        assert_eq!(t.wipe(a).unwrap().id, a);
        assert!(t.get(b).is_some());
        assert_eq!(t.wipe_all().len(), 1);
        assert!(t.all().is_empty());
    }
}
