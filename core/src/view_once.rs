//! Photos and videos a friend can open only once (friends' chats only).
//!
//! One goes like any attachment, but as [`Content::ViewOnce`]: no preview,
//! no caption, and neither phone will save, forward or export it. When
//! the friend opens it, their copy of the file is deleted and their phone
//! says so ([`Content::Opened`]); then the sender's copy goes too. Both
//! keep a line saying it was opened. Nothing stops a screenshot or another
//! camera.

use crate::media::{max_size, Outgoing};
use crate::protocol::{Content, Media, MessageId};
use crate::session::{now, Event, Result, Session};
use crate::store::{ChatKind, StoredMessage};
use std::time::Instant;

/// Not opened yet; opened (`StoredMessage::view_once`).
pub const WAITING: u8 = 1;
pub const OPENED: u8 = 2;

impl Session {
    /// Sends a photo or video the friend can open once. Returns who to reach.
    pub fn send_view_once(&mut self, chat_id: i64, out: Outgoing<'_>) -> Result<Vec<[u8; 32]>> {
        self.send_media_as(chat_id, out, None, true)
    }

    /// Opens a friend's view-once photo or video: its bytes, this once.
    /// None if it isn't one, was opened already or hasn't arrived.
    pub fn open_view_once(&mut self, chat_id: i64, seq: i64) -> Result<Option<Vec<u8>>> {
        let Some(mut msg) = self.store.message(chat_id, seq)? else {
            return Ok(None);
        };
        let (Some(media), Some(id)) = (msg.media.clone(), msg.id) else {
            return Ok(None);
        };
        if msg.from_me || msg.view_once != WAITING || !self.media_ready(&media) {
            return Ok(None);
        }
        let bytes = self.open_file(&media)?;
        self.delete_blobs(&[media.blob]);
        msg.view_once = OPENED;
        self.store.update_message(chat_id, seq, &msg)?;
        self.events.push(Event::Chat(chat_id));
        let reach = self.tell_chat(chat_id, Content::Opened { id })?;
        self.pokes.extend(reach);
        Ok(bytes)
    }

    /// A friend's view-once photo or video arrived.
    pub(crate) fn on_view_once(&mut self, chat_id: i64, peer: &[u8; 32], id: MessageId, sent_at: u64, mut media: Media) -> Result<()> {
        if media.size > max_size(media.kind) || self.chat(chat_id)?.kind != ChatKind::Direct {
            return Ok(());
        }
        media.thumb.clear();
        self.expect_blob(chat_id, peer, &media)?;
        self.sync.expected_at.insert(media.blob, Instant::now());
        self.add_received(
            chat_id,
            StoredMessage {
                sent_at: sent_at.min(now()),
                delivered: true,
                id: Some(id),
                sender: Some(*peer),
                media: Some(media),
                view_once: WAITING,
                ..Default::default()
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        self.events.push(Event::Message(chat_id));
        Ok(())
    }

    /// The friend opened our view-once photo: our copy goes too.
    pub(crate) fn on_opened(&mut self, chat_id: i64, id: MessageId) -> Result<()> {
        if let Some((seq, mut msg)) = self.store.message_by_id(chat_id, &id)? {
            if msg.from_me && msg.view_once == WAITING {
                if let Some(media) = &msg.media {
                    self.delete_blobs(&[media.blob]);
                }
                msg.view_once = OPENED;
                self.store.update_message(chat_id, seq, &msg)?;
                self.events.push(Event::Chat(chat_id));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{befriend, connect, outgoing, photo, Phone};

    #[test]
    fn a_view_once_photo_opens_once_then_goes_from_both_phones() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (path, bytes) = photo(&alice, 100_000);
        alice.s.send_view_once(ab, outgoing(&path)).unwrap();
        connect(&mut alice, &mut bob);
        let mine = alice.s.store.messages(ab, None).unwrap().pop().unwrap();
        assert_eq!((mine.view_once, mine.body.as_str()), (WAITING, ""), "no caption");
        assert_eq!(alice.s.media_bytes(ab, 0).unwrap(), None, "the sender can't open it either");
        let theirs = bob.s.store.messages(ba, None).unwrap().pop().unwrap();
        assert!(theirs.media.as_ref().unwrap().thumb.is_empty(), "no preview");
        assert_eq!(bob.s.media_bytes(ba, 0).unwrap(), None, "can't be saved");
        assert_eq!(bob.s.open_view_once(ba, 0).unwrap(), Some(bytes));
        assert_eq!(bob.s.open_view_once(ba, 0).unwrap(), None, "only once");
        assert!(!bob.s.media_ready(theirs.media.as_ref().unwrap()));
        connect(&mut bob, &mut alice);
        let mine = alice.s.store.messages(ab, None).unwrap().pop().unwrap();
        assert_eq!(mine.view_once, OPENED);
        assert!(!alice.s.media_ready(mine.media.as_ref().unwrap()));
        assert!(alice.s.open_view_once(ab, 0).unwrap().is_none());
    }

    #[test]
    fn view_once_is_for_friends_chats_only() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (group, _) = alice.s.create_group("Us", &[ab]).unwrap();
        let (path, _) = photo(&alice, 1000);
        assert!(alice.s.send_view_once(group, outgoing(&path)).is_err());
        let file = Outgoing { kind: crate::protocol::MediaKind::File, ..outgoing(&path) };
        assert!(alice.s.send_view_once(ab, file).is_err());
    }
}
