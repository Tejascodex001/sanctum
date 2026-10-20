//! Persistent chats, encrypted at rest. See `sanctum_core::store`.

use sanctum_core::store::{ChatKind, ChatMeta, Store, StoredMessage};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

static STORE: LazyLock<Mutex<Option<Store>>> = LazyLock::new(Default::default);

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
}

pub struct MessageView {
    pub from_me: bool,
    pub body: String,
    pub sent_at: i64,
}

/// A fresh 32-byte storage key; Dart keeps it in Keystore-backed storage.
pub fn store_new_key() -> Vec<u8> {
    rand::random::<[u8; 32]>().to_vec()
}

pub fn store_open(dir: String, key: Vec<u8>) -> anyhow::Result<()> {
    let key: [u8; 32] = key
        .try_into()
        .map_err(|_| anyhow::anyhow!("key must be 32 bytes"))?;
    let store = Store::open(std::path::Path::new(&dir).join("sanctum.db"), &key)?;
    *lock() = Some(store);
    Ok(())
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

pub fn message_send(chat_id: i64, body: String) -> anyhow::Result<()> {
    with(|s| {
        s.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body,
                sent_at: now(),
            },
        )?;
        Ok(())
    })
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
            })
            .collect())
    })
}

fn lock() -> std::sync::MutexGuard<'static, Option<Store>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<T>(f: impl FnOnce(&Store) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let guard = lock();
    let store = guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("store is not open"))?;
    f(store)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
