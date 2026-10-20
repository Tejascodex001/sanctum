//! Temp chats: in memory only (never on disk, on either phone), gone 24
//! hours after each message, on wipe, or when the app is closed. With a
//! friend they travel over Tor and appear on both phones.

use super::session::{poke, with};

pub struct TempChatView {
    pub id: i64,
    pub title: String,
    /// The friend's chat id, or none for a temp chat with yourself.
    pub friend_chat_id: Option<i64>,
    /// Both phones derive the same sigil from this.
    pub sigil_key: Vec<u8>,
    pub last_body: Option<String>,
    pub last_at: Option<i64>,
    /// When the oldest message burns, if any.
    pub next_burn: Option<i64>,
}

pub struct TempMessageView {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: i64,
    pub expires_at: i64,
    pub delivered: bool,
}

/// Starts a temp chat with a friend (their chat id) or with yourself.
pub fn temp_new(friend_chat_id: Option<i64>) -> anyhow::Result<i64> {
    let (id, peer) = with(|s| {
        let id = s.temp_new(friend_chat_id)?;
        Ok((id, s.temp.get(id).and_then(|c| c.peer)))
    })?;
    poke(peer);
    Ok(id as i64)
}

pub fn temp_list() -> anyhow::Result<Vec<TempChatView>> {
    with(|s| {
        s.purge();
        let friends = s.friends()?;
        Ok(s.temp
            .all()
            .iter()
            .map(|c| TempChatView {
                id: c.id as i64,
                title: c.title.clone(),
                friend_chat_id: c.peer.and_then(|p| {
                    friends
                        .iter()
                        .find(|(_, f)| f.identity == p)
                        .map(|(id, _)| *id)
                }),
                sigil_key: blake3::derive_key("sanctum temp sigil v1", &c.remote).to_vec(),
                last_body: c.messages.last().map(|m| m.body.clone()),
                last_at: c.messages.last().map(|m| m.sent_at as i64),
                next_burn: c.messages.first().map(|m| m.expires_at() as i64),
            })
            .collect())
    })
}

pub fn temp_send(id: i64, body: String) -> anyhow::Result<()> {
    let peer = with(|s| Ok(s.temp_send(id as u64, &body)?))?;
    poke(peer);
    Ok(())
}

pub fn temp_messages(id: i64) -> anyhow::Result<Vec<TempMessageView>> {
    with(|s| {
        s.purge();
        Ok(s.temp
            .get(id as u64)
            .map(|c| {
                c.messages
                    .iter()
                    .map(|m| TempMessageView {
                        from_me: m.from_me,
                        body: m.body.clone(),
                        sent_at: m.sent_at as i64,
                        expires_at: m.expires_at() as i64,
                        delivered: m.delivered,
                    })
                    .collect()
            })
            .unwrap_or_default())
    })
}

/// Wipes the temp chat here and on the friend's phone.
pub fn temp_wipe(id: i64) -> anyhow::Result<()> {
    let peer = with(|s| Ok(s.temp_wipe(id as u64)))?;
    poke(peer);
    Ok(())
}

pub fn temp_wipe_all() -> anyhow::Result<()> {
    let peers = with(|s| Ok(s.temp_wipe_all()))?;
    for peer in peers {
        poke(Some(peer));
    }
    Ok(())
}
