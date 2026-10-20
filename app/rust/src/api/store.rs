//! Persistent chats, encrypted at rest. See `sanctum_core::store`.

use super::session;
use sanctum_core::store::{ChatKind, ChatMeta, Store};
use std::time::{SystemTime, UNIX_EPOCH};

pub enum ChatKindView {
    Notes,
    Direct,
    Group,
}

pub struct StoredChat {
    pub id: i64,
    pub kind: ChatKindView,
    pub title: String,
    pub sigil_key: Vec<u8>,
    pub pinned: bool,
    pub created_at: i64,
    /// Friends' chats: their key is confirmed (scanned in person or
    /// safety number compared).
    pub verified: bool,
}

pub struct MessageView {
    pub from_me: bool,
    pub body: String,
    pub sent_at: i64,
    /// Our messages: the friend's phone has it.
    pub delivered: bool,
}

/// A fresh 32-byte storage key; Dart keeps it in Keystore-backed storage.
pub fn store_new_key() -> Vec<u8> {
    rand::random::<[u8; 32]>().to_vec()
}

pub fn chat_list() -> anyhow::Result<Vec<StoredChat>> {
    with(|s| {
        Ok(s.chats()?
            .into_iter()
            .map(|(id, m)| StoredChat {
                id,
                kind: match m.kind {
                    ChatKind::Notes => ChatKindView::Notes,
                    ChatKind::Direct => ChatKindView::Direct,
                    ChatKind::Group => ChatKindView::Group,
                },
                title: m.title,
                sigil_key: m.sigil_key,
                pinned: m.pinned,
                created_at: m.created_at as i64,
                verified: m.peers.iter().all(|p| p.verified),
            })
            .collect())
    })
}

pub fn chat_create(kind: ChatKindView, title: String, sigil_key: Vec<u8>) -> anyhow::Result<i64> {
    let kind = match kind {
        ChatKindView::Notes => ChatKind::Notes,
        ChatKindView::Direct => ChatKind::Direct,
        ChatKindView::Group => ChatKind::Group,
    };
    with(|s| {
        Ok(s.create_chat(&ChatMeta {
            kind,
            title,
            sigil_key,
            pinned: false,
            created_at: now(),
            group_id: Vec::new(),
            peers: Vec::new(),
        })?)
    })
}

pub fn chat_set_pinned(id: i64, pinned: bool) -> anyhow::Result<()> {
    with(|s| {
        let (_, mut meta) = s
            .chats()?
            .into_iter()
            .find(|(c, _)| *c == id)
            .ok_or_else(|| anyhow::anyhow!("no such chat"))?;
        meta.pinned = pinned;
        Ok(s.update_chat(id, &meta)?)
    })
}

pub fn chat_delete(id: i64) -> anyhow::Result<()> {
    with(|s| Ok(s.delete_chat(id)?))
}

/// Saves a message and sends it to the friend (over Tor) if this is a
/// friend's chat.
pub fn message_send(chat_id: i64, body: String) -> anyhow::Result<()> {
    let peer = session::with(|s| Ok(s.send_text(chat_id, &body)?))?;
    session::poke(peer);
    Ok(())
}

/// Messages in order; `limit` keeps only the newest.
pub fn message_list(chat_id: i64, limit: Option<u32>) -> anyhow::Result<Vec<MessageView>> {
    with(|s| {
        Ok(s.messages(chat_id, limit)?
            .into_iter()
            .map(|m| MessageView {
                from_me: m.from_me,
                body: m.body,
                sent_at: m.sent_at as i64,
                delivered: m.delivered,
            })
            .collect())
    })
}

fn with<T>(f: impl FnOnce(&Store) -> anyhow::Result<T>) -> anyhow::Result<T> {
    session::with(|s| f(&s.store))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
