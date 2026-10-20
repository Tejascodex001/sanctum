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
    /// Friends' chats: they haven't added us back yet (our friend request
    /// is still going out).
    pub waiting: bool,
    /// Left out of the list until revealed with biometrics.
    pub hidden: bool,
}

pub struct MessageView {
    /// Position in the chat: what a reply points at.
    pub seq: i64,
    pub from_me: bool,
    pub body: String,
    pub sent_at: i64,
    /// Our messages: the friend's phone has it.
    pub delivered: bool,
    /// The message this one answers.
    pub reply: Option<ReplyView>,
    /// Who wrote it, in a group (None for ours, and in direct chats).
    pub sender_name: Option<String>,
}

pub struct ReplyView {
    /// "You", or the author's name.
    pub author: String,
    pub from_me: bool,
    pub snippet: String,
    /// Where the quoted message is in this chat, if it's still there.
    pub seq: Option<i64>,
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
                waiting: m.kind == ChatKind::Direct && m.group_id.is_empty(),
                hidden: m.hidden,
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
            hidden: false,
            admin: None,
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

/// Renames a chat (for a friend: what you call them, on this phone only).
pub fn chat_rename(id: i64, name: String) -> anyhow::Result<()> {
    session::with(|s| Ok(s.rename_chat(id, &name)?))
}

/// Hides a chat from the list (or shows it again).
pub fn chat_set_hidden(id: i64, hidden: bool) -> anyhow::Result<()> {
    session::with(|s| Ok(s.set_hidden(id, hidden)?))
}

pub fn chat_delete(id: i64) -> anyhow::Result<()> {
    session::with(|s| Ok(s.delete_chat(id)?))
}

/// Saves a message and sends it to the friend (over Tor) if this is a
/// friend's chat.
pub fn message_send(chat_id: i64, body: String, reply_to: Option<i64>) -> anyhow::Result<()> {
    let peer = session::with(|s| Ok(s.send_message(chat_id, &body, reply_to)?))?;
    session::poke(peer);
    Ok(())
}

/// Messages in order; `limit` keeps only the newest.
pub fn message_list(chat_id: i64, limit: Option<u32>) -> anyhow::Result<Vec<MessageView>> {
    session::with(|s| {
        let me = s.public_key();
        let meta = s.chat(chat_id)?;
        let name_of = |key: &[u8; 32]| {
            meta.peers
                .iter()
                .find(|p| &p.identity == key)
                .map_or_else(|| "Someone".to_string(), |p| p.name.clone())
        };
        let messages = s.store.messages_indexed(chat_id, limit)?;
        let seq_of: std::collections::HashMap<[u8; 16], i64> = messages
            .iter()
            .filter_map(|(seq, m)| m.id.map(|id| (id, *seq)))
            .collect();
        let group = meta.kind == ChatKind::Group;
        Ok(messages
            .into_iter()
            .map(|(seq, m)| MessageView {
                seq,
                from_me: m.from_me,
                sent_at: m.sent_at as i64,
                delivered: m.delivered,
                reply: m.reply.map(|q| ReplyView {
                    author: if q.author == me { "You".into() } else { name_of(&q.author) },
                    from_me: q.author == me,
                    seq: seq_of.get(&q.id).copied().filter(|_| q.id != [0; 16]),
                    snippet: q.snippet,
                }),
                sender_name: m.sender.filter(|_| group && !m.from_me).map(|k| name_of(&k)),
                body: m.body,
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
