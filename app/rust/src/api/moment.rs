//! Daily moments: one photo a day for every friend, gone after 24 hours.
//! See `sanctum_core::moment`.

use super::session;
use sanctum_core::media::Outgoing;
use sanctum_core::protocol::MediaKind;
use sanctum_core::store::ChatKind;

pub struct MomentView {
    pub id: Vec<u8>,
    pub mine: bool,
    /// "You", or the friend's name.
    pub name: String,
    /// Their chat's sigil (empty for ours).
    pub sigil_key: Vec<u8>,
    /// Their chat with us (None for ours).
    pub chat_id: Option<i64>,
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
    /// How long each look lasts, in seconds (0: no limit).
    pub seconds: u8,
    /// Each friend can open it once.
    pub once: bool,
    /// A view-once moment we've opened: nothing left to show.
    pub spent: bool,
    /// Ours: who looked, first look first (empty when we don't share views).
    pub viewers: Vec<MomentViewer>,
}

pub struct MomentViewer {
    pub name: String,
    /// Their chat's sigil.
    pub sigil_key: Vec<u8>,
    /// Unix seconds.
    pub at: i64,
}

/// Today's moments: ours first, then friends' newest first.
pub fn moment_list() -> anyhow::Result<Vec<MomentView>> {
    session::with(|s| {
        let chats = s.store.chats()?;
        let list = s.moments()?;
        let views = s.moment_views_shared()?;
        let friend = |f: &[u8; 32]| {
            chats
                .iter()
                .find(|(_, c)| c.kind == ChatKind::Direct && c.peers.first().is_some_and(|p| &p.identity == f))
                .map(|(_, c)| c)
        };
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
                let found = m.from.and_then(|f| {
                    chats
                        .iter()
                        .find(|(_, c)| c.kind == ChatKind::Direct && c.peers.first().is_some_and(|p| p.identity == f))
                });
                let chat_id = found.map(|(id, _)| *id);
                let chat = found.map(|(_, c)| c.clone());
                MomentView {
                    id: m.id.to_vec(),
                    mine: m.from.is_none(),
                    name: match (&m.from, &chat) {
                        (None, _) => "You".into(),
                        (Some(_), Some(c)) => c.title.clone(),
                        (Some(_), None) => "Friend".into(),
                    },
                    sigil_key: chat.map(|c| c.sigil_key).unwrap_or_default(),
                    chat_id,
                    sent_at: m.sent_at as i64,
                    expires_at: m.expires_at() as i64,
                    ready: s.media_ready(&m.media),
                    width: m.media.width,
                    height: m.media.height,
                    thumb: m.media.thumb,
                    caption: m.caption,
                    seen: m.seen,
                    seconds: m.seconds,
                    once: m.once,
                    spent: m.spent,
                    viewers: if views {
                        m.viewers
                            .iter()
                            .filter_map(|(who, at)| {
                                friend(who).map(|c| MomentViewer {
                                    name: c.title.clone(),
                                    sigil_key: c.sigil_key.clone(),
                                    at: *at as i64,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect())
    })
}

/// Shares a photo as today's moment, replacing any earlier one. `seconds`
/// (0: no limit) is how long each look lasts; `once`: each friend can open
/// it one time.
#[allow(clippy::too_many_arguments)]
pub fn moment_share(
    path: String,
    mime: String,
    name: String,
    caption: String,
    thumb: Vec<u8>,
    width: u32,
    height: u32,
    seconds: u8,
    once: bool,
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
    for peer in session::with(|s| Ok(s.share_moment(out, seconds, once)?))? {
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

/// We looked at a moment (tells its owner, if we share views).
pub fn moment_seen(id: Vec<u8>) -> anyhow::Result<()> {
    let id: [u8; 16] = id.try_into().map_err(|_| anyhow::anyhow!("bad id"))?;
    for peer in session::with(|s| Ok(s.moment_seen(&id)?))? {
        session::poke(Some(peer));
    }
    Ok(())
}

/// We've had our one look at a view-once moment: its photo is deleted.
pub fn moment_spent(id: Vec<u8>) -> anyhow::Result<()> {
    let id: [u8; 16] = id.try_into().map_err(|_| anyhow::anyhow!("bad id"))?;
    session::with(|s| Ok(s.moment_spent(&id)?))
}

/// Whether we tell friends when we've seen their moments, and see who saw
/// ours (both or neither).
pub fn moment_views_shared() -> anyhow::Result<bool> {
    session::with(|s| Ok(s.moment_views_shared()?))
}

pub fn set_moment_views_shared(shared: bool) -> anyhow::Result<()> {
    session::with(|s| Ok(s.set_moment_views_shared(shared)?))
}

/// A moment's photo, decrypted (None until it has arrived).
pub fn moment_bytes(id: Vec<u8>) -> anyhow::Result<Option<Vec<u8>>> {
    let id: [u8; 16] = id.try_into().map_err(|_| anyhow::anyhow!("bad id"))?;
    session::with(|s| Ok(s.moment_bytes(&id)?))
}
