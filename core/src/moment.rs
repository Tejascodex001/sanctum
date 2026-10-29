//! Daily moments: one photo a day, shared with every friend, gone 24 hours
//! after it was taken.
//!
//! A moment travels like a photo in a chat ([`Content::Moment`] inside each
//! friend's MLS group, then the encrypted chunks), but isn't a message: it
//! lives in a sealed list of its own and is deleted, file and all, once
//! its day is over. Sharing another replaces the one before.
//!
//! The owner can set rules ([`Content::MomentRules`], sent right after the
//! moment): each look lasts a few seconds, and/or each friend can open it
//! once (the photo is deleted from their phone afterwards). Friends who
//! share views tell the owner when they've looked ([`Content::MomentSeen`]);
//! it's fair both ways: turning that off also hides who saw yours.

use crate::media::Outgoing;
use crate::protocol::{Content, Media, MediaKind, MessageId};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, OutboxItem, OutboxState};
use serde::{Deserialize, Serialize};

/// How long a moment is shown.
pub const LIFETIME: u64 = 24 * 60 * 60;
/// The "chat" a moment's file is expected for (no real chat has it).
pub(crate) const MOMENT_CHAT: i64 = -1;
const KV: &[u8] = b"moments-v2";
/// Where moments were kept before rules and views (read once, then moved).
const KV_V1: &[u8] = b"moments";
/// Set: we don't tell friends when we've seen their moments.
const KV_VIEWS_OFF: &[u8] = b"moment-views-off";
/// Longest look a rule can ask for, in seconds.
pub const MAX_SECONDS: u8 = 30;

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
    /// How long each look lasts, in seconds (0: as long as they like).
    pub seconds: u8,
    /// Each friend can open it once.
    pub once: bool,
    /// Ours: friends who looked, and when (Unix seconds), first look first.
    pub viewers: Vec<([u8; 32], u64)>,
    /// A friend's view-once moment we've opened: its photo is gone.
    pub spent: bool,
}

/// How moments were kept before rules and views.
#[derive(Deserialize)]
struct MomentV1 {
    id: MessageId,
    from: Option<[u8; 32]>,
    sent_at: u64,
    caption: String,
    media: Media,
    seen: bool,
}

impl Moment {
    pub fn expires_at(&self) -> u64 {
        self.sent_at + LIFETIME
    }
}

impl Session {
    fn load_moments(&self) -> Result<Vec<Moment>> {
        if let Some(list) = self.store.kv_get::<Vec<Moment>>(KV)? {
            return Ok(list);
        }
        let Some(old) = self.store.kv_get::<Vec<MomentV1>>(KV_V1)? else {
            return Ok(Vec::new());
        };
        let list: Vec<Moment> = old
            .into_iter()
            .map(|m| Moment {
                id: m.id,
                from: m.from,
                sent_at: m.sent_at,
                caption: m.caption,
                media: m.media,
                seen: m.seen,
                seconds: 0,
                once: false,
                viewers: Vec::new(),
                spent: false,
            })
            .collect();
        self.save_moments(&list)?;
        self.store.kv_delete(KV_V1)?;
        Ok(list)
    }

    #[cfg(test)]
    pub(crate) fn load_moments_for_test(&self) -> Vec<Moment> {
        self.load_moments().unwrap()
    }

    /// Whether we tell friends when we've seen their moments (and see who
    /// saw ours). On unless turned off.
    pub fn moment_views_shared(&self) -> Result<bool> {
        Ok(self.store.kv_get::<bool>(KV_VIEWS_OFF)?.is_none())
    }

    pub fn set_moment_views_shared(&mut self, on: bool) -> Result<()> {
        if on {
            self.store.kv_delete(KV_VIEWS_OFF)?;
        } else {
            self.store.kv_put(KV_VIEWS_OFF, &true)?;
        }
        self.events.push(Event::Moments);
        Ok(())
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
    /// `seconds` (0: no limit) and `once` say how friends may look at it.
    /// Returns who to reach.
    pub fn share_moment(&mut self, out: Outgoing<'_>, seconds: u8, once: bool) -> Result<Vec<[u8; 32]>> {
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
            seconds: seconds.min(MAX_SECONDS),
            once,
            viewers: Vec::new(),
            spent: false,
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
        let rules = (moment.seconds > 0 || moment.once).then_some(Content::MomentRules {
            id: moment.id,
            seconds: moment.seconds,
            once: moment.once,
        });
        let friends = self.moment_audience()?;
        for peer in &friends {
            // In this order: the outbox keeps it, so the rules never come
            // before their moment.
            for c in std::iter::once(&content).chain(&rules) {
                self.store.outbox_put(
                    &rand::random(),
                    &OutboxItem {
                        peer: *peer,
                        state: OutboxState::Plain(c.encode()),
                        message: None,
                    },
                )?;
            }
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

    /// We looked at a moment. Tells its owner, if we share views. Returns
    /// who to reach.
    pub fn moment_seen(&mut self, id: &MessageId) -> Result<Vec<[u8; 32]>> {
        let mut list = self.load_moments()?;
        let Some(m) = list.iter_mut().find(|m| &m.id == id && !m.seen) else {
            return Ok(Vec::new());
        };
        m.seen = true;
        let owner = m.from;
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        match owner {
            Some(peer) if self.moment_views_shared()? => {
                self.store.outbox_put(
                    &rand::random(),
                    &OutboxItem {
                        peer,
                        state: OutboxState::Plain(Content::MomentSeen { id: *id }.encode()),
                        message: None,
                    },
                )?;
                Ok(vec![peer])
            }
            _ => Ok(Vec::new()),
        }
    }

    /// We've had our one look at a friend's view-once moment: its photo
    /// goes (the entry stays, so a late copy can't bring it back).
    pub fn moment_spent(&mut self, id: &MessageId) -> Result<()> {
        let mut list = self.load_moments()?;
        let Some(m) = list.iter_mut().find(|m| &m.id == id && m.once && m.from.is_some() && !m.spent) else {
            return Ok(());
        };
        m.spent = true;
        m.seen = true;
        let blob = m.media.blob;
        self.save_moments(&list)?;
        self.delete_blobs(&[blob]);
        self.events.push(Event::Moments);
        Ok(())
    }

    /// A moment's photo, decrypted (None until it has arrived).
    pub fn moment_bytes(&self, id: &MessageId) -> Result<Option<Vec<u8>>> {
        match self.load_moments()?.into_iter().find(|m| &m.id == id) {
            Some(m) if !m.spent => self.open_file(&m.media),
            _ => Ok(None),
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
            seconds: 0,
            once: false,
            viewers: Vec::new(),
            spent: false,
        });
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        Ok(())
    }

    /// How a friend's moment may be looked at (it came just before).
    pub(crate) fn on_moment_rules(&mut self, peer: &[u8; 32], id: MessageId, seconds: u8, once: bool) -> Result<()> {
        let mut list = self.load_moments()?;
        let Some(m) = list.iter_mut().find(|m| m.id == id && m.from == Some(*peer)) else {
            return Ok(());
        };
        m.seconds = seconds.min(MAX_SECONDS);
        m.once = once;
        self.save_moments(&list)?;
        self.events.push(Event::Moments);
        Ok(())
    }

    /// A friend looked at our moment.
    pub(crate) fn on_moment_seen(&mut self, peer: &[u8; 32], id: MessageId) -> Result<()> {
        let mut list = self.load_moments()?;
        let Some(m) = list.iter_mut().find(|m| m.id == id && m.from.is_none()) else {
            return Ok(());
        };
        if m.viewers.iter().any(|(who, _)| who == peer) {
            return Ok(());
        }
        m.viewers.push((*peer, now()));
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
