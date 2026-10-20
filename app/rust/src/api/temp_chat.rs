//! Temp chats live only in this process's memory: never on disk, gone after
//! 24 hours, on wipe, or when the app is killed.

use sanctum_core::temp_chat::{TempChats, TempMessage};
use sanctum_core::transport::TransportKind;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP: LazyLock<Mutex<TempChats>> = LazyLock::new(Default::default);

pub struct TempMessageView {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: i64,
    pub expires_at: i64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn with<T>(f: impl FnOnce(&mut TempChats) -> T) -> T {
    let mut temp = TEMP.lock().unwrap_or_else(|e| e.into_inner());
    temp.purge_expired(now());
    f(&mut temp)
}

pub fn temp_open(id: i64) {
    with(|t| {
        t.open(id as u64, TransportKind::Internet);
    });
}

pub fn temp_send(id: i64, body: String) {
    with(|t| {
        t.open(id as u64, TransportKind::Internet)
            .messages
            .push(TempMessage {
                from_me: true,
                body,
                sent_at: now(),
            });
    });
}

pub fn temp_messages(id: i64) -> Vec<TempMessageView> {
    with(|t| {
        t.get(id as u64)
            .map(|c| {
                c.messages
                    .iter()
                    .map(|m| TempMessageView {
                        from_me: m.from_me,
                        body: m.body.clone(),
                        sent_at: m.sent_at as i64,
                        expires_at: m.expires_at() as i64,
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Live temp chat ids, after purging anything expired.
pub fn temp_chat_ids() -> Vec<i64> {
    with(|t| t.ids().into_iter().map(|id| id as i64).collect())
}

pub fn temp_wipe(id: i64) {
    with(|t| t.wipe(id as u64));
}

pub fn temp_wipe_all() {
    with(|t| t.wipe_all());
}
