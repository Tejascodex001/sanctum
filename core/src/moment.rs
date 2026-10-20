//! Daily moments: one photo a day, shared with every friend, gone 24 hours
//! after it was taken.
//!
//! A moment travels like a photo in a chat ([`Content::Moment`] inside each
//! friend's MLS group, then the encrypted chunks), but isn't a message: it
//! lives in a sealed list of its own and is deleted, file and all, once
//! its day is over. Sharing another replaces the one before.

use crate::media::Outgoing;
use crate::protocol::{Content, Media, MediaKind, MessageId};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, OutboxItem, OutboxState};
use serde::{Deserialize, Serialize};

/// How long a moment is shown.
pub const LIFETIME: u64 = 24 * 60 * 60;
/// The "chat" a moment's file is expected for (no real chat has it).
pub(crate) const MOMENT_CHAT: i64 = -1;
const KV: &[u8] = b"moments";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Moment {
    pub id: MessageId,
    /// Who shared it (None: we did).
    pub from: Option<[u8; 32]>,
    /// Unix seconds.
    pub sent_at: u64,
    pub caption: String,
    pub media: Media,
    /// We've looked at it.
    pub seen: bool,
}

impl Moment {
    pub fn expires_at(&self) -> u64 {
        self.sent_at + LIFETIME
    }
}

impl Session {
    fn load_moments(&self) -> Result<Vec<Moment>> {
        Ok(self.store.kv_get::<Vec<Moment>>(KV)?.unwrap_or_default())
    }

    fn save_moments(&self, list: &[Moment]) -> Result<()> {
        self.store.kv_put(KV, &list)?;
        Ok(())
    }

    /// Deletes moments whose day is over (and those of people who are no
    /// longer friends), with their files.
    pub(crate) fn purge_moments(&mut self) -> Result<()> {
        let list = self.load_moments()?;
        if list.is_empty() {
            return Ok(());
        }
        let friends = self.friend_keys()?;
        let at = now();
        let (keep, gone): (Vec<Moment>, Vec<Moment>) = list
            .into_iter()
            .partition(|m| m.expires_at() > at && m.from.is_none_or(|f| friends.contains(&f)));
        if !gone.is_empty() {
            self.delete_blobs(&gone.iter().map(|m| m.media.blob).collect::<Vec<_>>());
            self.save_moments(&keep)?;
            self.events.push(Event::Moments);
        }
        Ok(())
    }

    /// Today's moments, ours first, then friends' newest first.
    pub fn moments(&mut self) -> Result<Vec<Moment>> {
        self.purge_moments()?;
        let mut list = self.load_moments()?;
        list.sort_by_key(|m| (m.from.is_some(), std::cmp::Reverse(m.sent_at)));
        Ok(list)
    }

    /// Shares a photo as our moment for the day, replacing any earlier one.
    /// Returns who to reach.
    pub fn share_moment(&mut self, out: Outgoing<'_>) -> Result<Vec<[u8; 32]>> {
        if out.kind != MediaKind::Image {
            return Err(SessionError::Protocol);
        }
        self.purge_moments()?;
        let (media, total) = self.seal_file(&out)?;
        let moment = Moment {
            id: rand::random(),
            from: None,
            sent_at: now(),
            caption: clip(out.caption.trim(), 200),
            media,
            seen: true,
        };
        let mut list = self.load_moments()?;
        // The old one's file goes, so it also stops being sent.
        let old: Vec<_> = list.iter().filter(|m| m.from.is_none()).map(|m| m.media.blob).collect();
        self.delete_blobs(&old);
        list.retain(|m| m.from.is_some());
        list.push(moment.clone());
        self.save_moments(&list)?;
        self.events.push(Event::Moments);

        let content = Content::Moment {
            id: moment.id,
            sent_at: moment.sent_at,
            caption: moment.caption,
            media: moment.media.clone(),
        };
        let friends = self.moment_audience()?;
        for peer in &friends {
            self.store.outbox_put(
                &rand::random(),
                &OutboxItem {
                    peer: *peer,
                    state: OutboxState::Plain(content.encode()),
                    message: None,
                },
            )?;
            self.store.outbox_put(
                &rand::random(),
                &OutboxItem {
                    peer: *peer,
                    state: OutboxState::Blob {
                        blob: moment.media.blob,
                        total,
                    },
                    message: None,
                },
            )?;
        }
        Ok(friends)
    }

    /// Takes our moment down, here and on friends' phones.
    pub fn delete_my_moment(&mut self) -> Result<Vec<[u8; 32]>> {
        let mut list = self.load_moments()?;
        let Some(mine) = list.iter().find(|m| m.from.is_none()).cloned() else {
            return Ok(Vec::new());
        };
        self.delete_blobs(&[mine.media.blob]);
        list.retain(|m| m.from.is_some());
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        let friends = self.moment_audience()?;
        let content = Content::MomentGone { id: mine.id };
        for peer in &friends {
            self.store.outbox_put(
                &rand::random(),
                &OutboxItem {
                    peer: *peer,
                    state: OutboxState::Plain(content.encode()),
                    message: None,
                },
            )?;
        }
        Ok(friends)
    }

    pub fn moment_seen(&mut self, id: &MessageId) -> Result<()> {
        let mut list = self.load_moments()?;
        if let Some(m) = list.iter_mut().find(|m| &m.id == id && !m.seen) {
            m.seen = true;
            self.save_moments(&list)?;
            self.events.push(Event::Moments);
        }
        Ok(())
    }

    /// A moment's photo, decrypted (None until it has arrived).
    pub fn moment_bytes(&self, id: &MessageId) -> Result<Option<Vec<u8>>> {
        match self.load_moments()?.into_iter().find(|m| &m.id == id) {
            Some(m) => self.open_file(&m.media),
            None => Ok(None),
        }
    }

    /// Friends we have a chat with (moments go to friends only, never to
    /// group members who aren't).
    fn moment_audience(&self) -> Result<Vec<[u8; 32]>> {
        Ok(self
            .store
            .chats()?
            .into_iter()
            .filter(|(_, m)| m.kind == ChatKind::Direct)
            .filter_map(|(_, m)| m.peers.first().map(|p| p.identity))
            .collect())
    }

    /// A friend's moment arrived: it replaces their earlier one.
    pub(crate) fn on_moment(
        &mut self,
        peer: &[u8; 32],
        id: MessageId,
        sent_at: u64,
        caption: String,
        media: Media,
    ) -> Result<()> {
        let sent_at = sent_at.min(now());
        if sent_at + LIFETIME <= now() || media.kind != MediaKind::Image {
            return Ok(());
        }
        if media.size > crate::media::max_size(media.kind) {
            return Ok(());
        }
        let mut list = self.load_moments()?;
        if list.iter().any(|m| m.id == id) {
            return Ok(());
        }
        // A late copy of an older moment doesn't replace a newer one.
        if list.iter().any(|m| m.from == Some(*peer) && m.sent_at > sent_at) {
            return Ok(());
        }
        let old: Vec<_> = list.iter().filter(|m| m.from == Some(*peer)).map(|m| m.media.blob).collect();
        self.delete_blobs(&old);
        list.retain(|m| m.from != Some(*peer));
        self.expect_blob(MOMENT_CHAT, peer, &media)?;
        list.push(Moment {
            id,
            from: Some(*peer),
            sent_at,
            caption: clip(&caption, 200),
            media,
            seen: false,
        });
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        Ok(())
    }

    /// A friend took their moment down.
    pub(crate) fn on_moment_gone(&mut self, peer: &[u8; 32], id: MessageId) -> Result<()> {
        let mut list = self.load_moments()?;
        let Some(m) = list.iter().find(|m| m.id == id && m.from == Some(*peer)).cloned() else {
            return Ok(());
        };
        self.delete_blobs(&[m.media.blob]);
        list.retain(|x| x.id != id);
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        Ok(())
    }
}
