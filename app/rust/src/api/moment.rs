//! Daily moments: one photo a day for every friend, gone after 24 hours.
//! See `sanctum_core::moment`.

use super::session;
use sanctum_core::media::Outgoing;
use sanctum_core::protocol::MediaKind;

pub struct MomentView {
    pub id: Vec<u8>,
    pub mine: bool,
    /// "You", or the friend's name.
    pub name: String,
    /// Their chat's sigil (empty for ours).
    pub sigil_key: Vec<u8>,
    /// Unix seconds.
    pub sent_at: i64,
    pub expires_at: i64,
    pub caption: String,
    pub width: u32,
    pub height: u32,
    /// A tiny JPEG preview.
    pub thumb: Vec<u8>,
    /// The photo has arrived.
    pub ready: bool,
    pub seen: bool,
}

/// Today's moments: ours first, then friends' newest first.
pub fn moment_list() -> anyhow::Result<Vec<MomentView>> {
    session::with(|s| {
        let chats = s.store.chats()?;
        let list = s.moments()?;
        // A hidden friend's moment would give the chat away.
        let hidden = |f: &[u8; 32]| {
            chats
                .iter()
                .any(|(_, c)| c.hidden && c.peers.first().is_some_and(|p| &p.identity == f))
        };
        Ok(list
            .into_iter()
            .filter(|m| m.from.is_none_or(|f| !hidden(&f)))
            .map(|m| {
                let chat = m.from.and_then(|f| {
                    chats
                        .iter()
                        .find(|(_, c)| c.peers.first().is_some_and(|p| p.identity == f))
                        .map(|(_, c)| c.clone())
                });
                MomentView {
                    id: m.id.to_vec(),
                    mine: m.from.is_none(),
                    name: match (&m.from, &chat) {
                        (None, _) => "You".into(),
                        (Some(_), Some(c)) => c.title.clone(),
                        (Some(_), None) => "Friend".into(),
                    },
                    sigil_key: chat.map(|c| c.sigil_key).unwrap_or_default(),
                    sent_at: m.sent_at as i64,
                    expires_at: m.expires_at() as i64,
                    ready: s.media_ready(&m.media),
                    width: m.media.width,
                    height: m.media.height,
                    thumb: m.media.thumb,
                    caption: m.caption,
                    seen: m.seen,
                }
            })
            .collect())
    })
}

/// Shares a photo as today's moment, replacing any earlier one.
pub fn moment_share(
    path: String,
    mime: String,
    name: String,
    caption: String,
    thumb: Vec<u8>,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    let out = Outgoing {
        path: std::path::Path::new(&path),
        kind: MediaKind::Image,
        mime: &mime,
        name: &name,
        caption: &caption,
        thumb,
        width,
        height,
        duration_ms: 0,
        album: None,
    };
    for peer in session::with(|s| Ok(s.share_moment(out)?))? {
        session::poke(Some(peer));
    }
    Ok(())
}

/// Takes our moment down, here and on friends' phones.
pub fn moment_delete() -> anyhow::Result<()> {
    for peer in session::with(|s| Ok(s.delete_my_moment()?))? {
        session::poke(Some(peer));
    }
    Ok(())
}

pub fn moment_seen(id: Vec<u8>) -> anyhow::Result<()> {
    let id: [u8; 16] = id.try_into().map_err(|_| anyhow::anyhow!("bad id"))?;
    session::with(|s| Ok(s.moment_seen(&id)?))
}

/// A moment's photo, decrypted (None until it has arrived).
pub fn moment_bytes(id: Vec<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    let id: [u8; 16] = id.try_into().map_err(|_| anyhow::anyhow!("bad id"))?;
    session::with(|s| Ok(s.moment_bytes(&id)?))
}
