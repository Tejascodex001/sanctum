//! Sharing where you are, and passing a message on to another chat.
//!
//! A location ([`Content::Location`]) is a point, sent once: never live
//! tracking. The app shows it as numbers and offers to open it in a maps
//! app on the phone; nothing is fetched from a map service.
//!
//! Forwarding sends a copy of a message's words, file or place as a new
//! message of ours. Polls, call records, notices and view-once photos
//! can't be forwarded.

use crate::media::Outgoing;
use crate::protocol::{Content, MessageId};
use crate::session::{now, Event, Result, Session, SessionError};
use crate::store::{Place, StoredMessage};

/// What a location says in the chat list, in search and in quotes.
pub const LOCATION_BODY: &str = "📍 Location";

fn valid(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

impl Session {
    /// Sends a place in the chat. Returns who to reach.
    pub fn send_location(&mut self, chat_id: i64, lat: f64, lon: f64) -> Result<Vec<[u8; 32]>> {
        if !valid(lat, lon) {
            return Err(SessionError::Protocol);
        }
        let meta = self.chat(chat_id)?;
        let id: MessageId = rand::random();
        let sent_at = now();
        let seq = self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: LOCATION_BODY.into(),
                sent_at,
                delivered: meta.kind == crate::store::ChatKind::Notes,
                id: Some(id),
                place: Some(Place { lat, lon }),
                ..Default::default()
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        let content = Content::Location { id, sent_at, lat, lon };
        self.send_to_members(&meta, id, &content, (chat_id, seq))
    }

    /// A friend's or member's place arrived.
    pub(crate) fn on_location(&mut self, chat_id: i64, peer: &[u8; 32], id: MessageId, sent_at: u64, lat: f64, lon: f64) -> Result<()> {
        if !valid(lat, lon) || self.store.message_by_id(chat_id, &id)?.is_some() {
            return Ok(());
        }
        self.add_received(
            chat_id,
            StoredMessage {
                body: LOCATION_BODY.into(),
                sent_at: sent_at.min(now()),
                delivered: true,
                id: Some(id),
                sender: Some(*peer),
                place: Some(Place { lat, lon }),
                ..Default::default()
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        self.events.push(Event::Message(chat_id));
        Ok(())
    }

    /// Whether the message at `seq` can be passed on.
    pub fn can_forward(m: &StoredMessage) -> bool {
        !m.notice && m.poll.is_none() && m.call.is_none() && m.view_once == 0 && (m.media.is_some() || m.place.is_some() || !m.body.is_empty())
    }

    /// Sends a copy of the message at `seq` in `from` to the chat `to`.
    /// Returns who to reach.
    pub fn forward(&mut self, from: i64, seq: i64, to: i64) -> Result<Vec<[u8; 32]>> {
        let m = self.store.message(from, seq)?.ok_or(SessionError::Protocol)?;
        if !Self::can_forward(&m) {
            return Err(SessionError::GroupRule("This message can't be forwarded."));
        }
        if let Some(place) = m.place {
            return self.send_location(to, place.lat, place.lon);
        }
        let Some(media) = m.media else {
            return self.send_to_chat(to, &m.body, None);
        };
        let Some(bytes) = self.open_file(&media)? else {
            return Err(SessionError::GroupRule("This file hasn't arrived yet."));
        };
        std::fs::create_dir_all(&self.media_dir).map_err(|e| SessionError::Io(e.to_string()))?;
        let path = self.media_dir.join(format!("forward-{:016x}", rand::random::<u64>()));
        std::fs::write(&path, &bytes).map_err(|e| SessionError::Io(e.to_string()))?;
        let sent = self.send_media(
            to,
            Outgoing {
                path: &path,
                kind: media.kind,
                mime: &media.mime,
                name: &media.name,
                caption: &m.body,
                thumb: media.thumb.clone(),
                width: media.width,
                height: media.height,
                duration_ms: media.duration_ms,
                album: None,
            },
            None,
        );
        let _ = std::fs::remove_file(&path);
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{befriend, connect, outgoing, photo, Phone};

    #[test]
    fn places_reach_friends_and_groups() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_location(ab, 12.9716, 77.5946).unwrap();
        assert!(alice.s.send_location(ab, 91.0, 0.0).is_err());
        assert!(alice.s.send_location(ab, f64::NAN, 0.0).is_err());
        connect(&mut alice, &mut bob);
        let m = bob.s.store.messages(ba, None).unwrap().pop().unwrap();
        assert_eq!((m.body.as_str(), m.place), (LOCATION_BODY, Some(Place { lat: 12.9716, lon: 77.5946 })));
        let (group, _) = alice.s.create_group("Us", &[ab]).unwrap();
        connect(&mut alice, &mut bob);
        alice.s.send_location(group, 1.0, 2.0).unwrap();
        connect(&mut alice, &mut bob);
        let bg = crate::session::tests::group_of(&bob, "Us").unwrap();
        let m = bob.s.store.messages(bg, None).unwrap().pop().unwrap();
        assert_eq!(m.place, Some(Place { lat: 1.0, lon: 2.0 }));
    }

    #[test]
    fn forwarding_copies_words_files_and_places() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        let notes = alice
            .s
            .store
            .create_chat(&crate::store::ChatMeta {
                kind: crate::store::ChatKind::Notes,
                title: "Notes".into(),
                sigil_key: vec![1],
                pinned: false,
                created_at: 0,
                group_id: Vec::new(),
                peers: Vec::new(),
                hidden: false,
                admin: None,
                admins: Vec::new(),
            })
            .unwrap();
        connect(&mut alice, &mut bob);
        bob.s.send_text(ba, "the wifi password is hunter2").unwrap();
        let (path, bytes) = photo(&bob, 70_000);
        bob.s.send_media(ba, outgoing(&path), None).unwrap();
        bob.s.send_location(ba, 3.0, 4.0).unwrap();
        connect(&mut bob, &mut alice);
        for seq in 0..3 {
            alice.s.forward(ab, seq, notes).unwrap();
        }
        let copied = alice.s.store.messages(notes, None).unwrap();
        assert_eq!(copied[0].body, "the wifi password is hunter2");
        assert!(copied.iter().all(|m| m.from_me));
        assert_eq!(copied[1].body, "sunset");
        assert_eq!(alice.s.media_bytes(notes, 1).unwrap(), Some(bytes));
        assert_eq!(copied[2].place, Some(Place { lat: 3.0, lon: 4.0 }));
        // Back to Bob, too.
        alice.s.forward(notes, 0, ab).unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(ba).last().unwrap().1, "the wifi password is hunter2");
    }
}
