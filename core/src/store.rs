//! Encrypted on-device storage for chats that persist (everything except
//! temp chats, which never touch disk).
//!
//! Each row's payload is sealed with XChaCha20-Poly1305 under a 32-byte key
//! that the app keeps in the Android Keystore. The row's identity (table and
//! ids) is bound in as associated data, so rows can't be swapped or replayed
//! between chats. SQLite only ever sees random ids, sequence numbers and
//! ciphertext; `secure_delete` zeroes deleted pages.

use crate::protocol::{Album, BlobId, Media, MessageId, Poll, Quote};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const NONCE_LEN: usize = 24;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("data could not be decrypted (wrong key or tampered)")]
    Decrypt,
    #[error("encoding: {0}")]
    Encoding(#[from] postcard::Error),
    #[error("no such chat")]
    NoSuchChat,
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatKind {
    Notes,
    Direct,
    Group,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMeta {
    pub kind: ChatKind,
    pub title: String,
    /// Key material the chat's sigil is derived from.
    pub sigil_key: Vec<u8>,
    pub pinned: bool,
    /// Unix seconds.
    pub created_at: u64,
    /// The MLS group carrying this chat; empty for notes.
    pub group_id: Vec<u8>,
    /// Everyone in the chat except us.
    pub peers: Vec<Peer>,
    /// Left out of the chat list until revealed with biometrics.
    pub hidden: bool,
    /// Groups: the member who manages membership (its creator). Only they
    /// commit changes, so members never disagree about the group's state.
    pub admin: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub identity: [u8; 32],
    pub name: String,
    /// Where to reach them over Tor, once known.
    pub onion: Option<String>,
    /// Their key is confirmed: their QR was scanned in person, or the
    /// safety number was compared (like Threema's green level).
    pub verified: bool,
}

/// The third on-disk format, before hidden chats and groups.
#[derive(Deserialize)]
struct ChatMetaV2 {
    kind: ChatKind,
    title: String,
    sigil_key: Vec<u8>,
    pinned: bool,
    created_at: u64,
    group_id: Vec<u8>,
    peers: Vec<Peer>,
}

impl From<ChatMetaV2> for ChatMeta {
    fn from(v: ChatMetaV2) -> Self {
        Self {
            kind: v.kind,
            title: v.title,
            sigil_key: v.sigil_key,
            pinned: v.pinned,
            created_at: v.created_at,
            group_id: v.group_id,
            peers: v.peers,
            hidden: false,
            admin: None,
        }
    }
}

/// The second on-disk format, before friends could be verified.
#[derive(Deserialize)]
struct ChatMetaV1 {
    kind: ChatKind,
    title: String,
    sigil_key: Vec<u8>,
    pinned: bool,
    created_at: u64,
    group_id: Vec<u8>,
    peers: Vec<PeerV1>,
}

#[derive(Deserialize)]
struct PeerV1 {
    identity: [u8; 32],
    name: String,
    onion: Option<String>,
}

impl From<ChatMetaV1> for ChatMeta {
    fn from(v: ChatMetaV1) -> Self {
        Self {
            kind: v.kind,
            title: v.title,
            sigil_key: v.sigil_key,
            pinned: v.pinned,
            created_at: v.created_at,
            group_id: v.group_id,
            peers: v
                .peers
                .into_iter()
                .map(|p| Peer {
                    identity: p.identity,
                    name: p.name,
                    onion: p.onion,
                    verified: false,
                })
                .collect(),
            hidden: false,
            admin: None,
        }
    }
}

/// The first on-disk format, before chats carried groups and peers.
#[derive(Deserialize)]
struct ChatMetaV0 {
    kind: ChatKind,
    title: String,
    sigil_key: Vec<u8>,
    pinned: bool,
    created_at: u64,
}

impl From<ChatMetaV0> for ChatMeta {
    fn from(v: ChatMetaV0) -> Self {
        Self {
            kind: v.kind,
            title: v.title,
            sigil_key: v.sigil_key,
            pinned: v.pinned,
            created_at: v.created_at,
            group_id: Vec::new(),
            peers: Vec::new(),
            hidden: false,
            admin: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoredMessage {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: u64,
    /// Our messages: the friend's phone confirmed it received this.
    pub delivered: bool,
    /// The same on every phone in the chat (the network message id), so
    /// replies can point at it. None for messages from before replies.
    pub id: Option<MessageId>,
    /// The message this one answers.
    pub reply: Option<Quote>,
    /// Who wrote it, for messages we received.
    pub sender: Option<[u8; 32]>,
    /// A photo, video, voice note or file (the file is in the media folder).
    pub media: Option<Media>,
    /// A poll, date poll or plan.
    pub poll: Option<Poll>,
    /// Polls: everyone's latest answer (ours under our own key).
    pub votes: Vec<Vote>,
    /// The shared album a photo or video is in.
    pub album: Option<Album>,
    /// Everyone's latest reaction (ours under our own key). One per
    /// person; an empty emoji is a reaction taken back, kept so an older
    /// one arriving late can't bring it back.
    pub reactions: Vec<Reaction>,
    /// A call's record in the chat ("Voice call · 4:12", "Missed call"),
    /// kept on this phone only. `from_me`: we called. `body` holds the
    /// words shown, so previews and search need nothing special.
    pub call: Option<CallLog>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Reaction {
    pub who: [u8; 32],
    /// Empty: taken back.
    pub emoji: String,
    /// When they reacted (milliseconds), so a late older reaction loses.
    pub at: u64,
}

/// How a call went, for its record in the chat.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CallLog {
    pub video: bool,
    /// `call::Outcome` as a number (an unknown one reads as "ended").
    pub outcome: u8,
    /// How long we talked, in seconds.
    pub seconds: u32,
}

/// The seventh message format, before calls.
#[derive(Deserialize)]
struct StoredMessageV6 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
    id: Option<MessageId>,
    reply: Option<Quote>,
    sender: Option<[u8; 32]>,
    media: Option<Media>,
    poll: Option<Poll>,
    votes: Vec<Vote>,
    album: Option<Album>,
    reactions: Vec<Reaction>,
}

/// The sixth message format, before reactions.
#[derive(Deserialize)]
struct StoredMessageV5 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
    id: Option<MessageId>,
    reply: Option<Quote>,
    sender: Option<[u8; 32]>,
    media: Option<Media>,
    poll: Option<Poll>,
    votes: Vec<Vote>,
    album: Option<Album>,
}

/// The fifth message format, before shared albums.
#[derive(Deserialize)]
struct StoredMessageV4 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
    id: Option<MessageId>,
    reply: Option<Quote>,
    sender: Option<[u8; 32]>,
    media: Option<Media>,
    poll: Option<Poll>,
    votes: Vec<Vote>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Vote {
    pub voter: [u8; 32],
    pub choices: Vec<u8>,
    /// When they answered (milliseconds), so a late older answer loses.
    pub at: u64,
}

/// The fourth message format, before polls.
#[derive(Deserialize)]
struct StoredMessageV3 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
    id: Option<MessageId>,
    reply: Option<Quote>,
    sender: Option<[u8; 32]>,
    media: Option<Media>,
}

/// The third message format, before attachments.
#[derive(Deserialize)]
struct StoredMessageV2 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
    id: Option<MessageId>,
    reply: Option<Quote>,
    sender: Option<[u8; 32]>,
}

/// The second message format, before replies.
#[derive(Deserialize)]
struct StoredMessageV1 {
    from_me: bool,
    body: String,
    sent_at: u64,
    delivered: bool,
}

/// The first message format, before delivery receipts.
#[derive(Deserialize)]
struct StoredMessageV0 {
    from_me: bool,
    body: String,
    sent_at: u64,
}

/// Something waiting to be delivered to a friend, kept sealed on disk so it
/// survives restarts until the friend's phone acknowledges it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub peer: [u8; 32],
    pub state: OutboxState,
    /// The stored message this delivers, to mark it delivered on ack.
    pub message: Option<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutboxState {
    /// Waiting for the chat's encrypted session to exist.
    Plain(Vec<u8>),
    /// Encrypted, sent until acknowledged. (Older builds; no plaintext.)
    Cipher(Vec<u8>),
    /// Encrypted, sent until acknowledged. The plaintext stays (sealed like
    /// everything here) so it can be re-encrypted if the pair's group has
    /// to be rebuilt before the friend could read it.
    Sealed { plain: Vec<u8>, cipher: Vec<u8> },
    /// For a group: encrypted once, the same bytes go to every member.
    Group { group: Vec<u8>, cipher: Vec<u8> },
    /// A whole frame (a group welcome), sent as is until acknowledged.
    Frame(Vec<u8>),
    /// An attachment's encrypted file, sent as chunks (see `media`).
    Blob { blob: BlobId, total: u32 },
}

pub struct Store {
    db: Connection,
    cipher: XChaCha20Poly1305,
    /// Keys the hashes that index MLS state, so lookup keys (which contain
    /// group ids) never appear on disk.
    index_key: [u8; 32],
}

impl Store {
    pub fn open(path: impl AsRef<Path>, key: &[u8; 32]) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA secure_delete = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS chats (
                 id INTEGER PRIMARY KEY,
                 sealed BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mls_kv (
                 k BLOB PRIMARY KEY,
                 sealed BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS kv (
                 k BLOB PRIMARY KEY,
                 sealed BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS outbox (
                 id BLOB PRIMARY KEY,
                 peer BLOB NOT NULL,
                 sealed BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS outbox_peer ON outbox(peer);
             CREATE TABLE IF NOT EXISTS seen (
                 k BLOB PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS messages (
                 chat_id INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
                 seq INTEGER NOT NULL,
                 sealed BLOB NOT NULL,
                 PRIMARY KEY (chat_id, seq)
             );
             -- The next sequence number per chat, so numbers aren't reused
             -- after messages are deleted (pending receipts point at them).
             CREATE TABLE IF NOT EXISTS chat_seq (
                 chat_id INTEGER PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
                 next INTEGER NOT NULL
             );",
        )?;
        let store = Self {
            db,
            cipher: XChaCha20Poly1305::new(key.into()),
            index_key: blake3::derive_key("sanctum store index v1", key),
        };
        // Fail fast on a wrong key rather than on first read.
        if let Some((id, sealed)) = store
            .db
            .query_row("SELECT id, sealed FROM chats LIMIT 1", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .optional()?
        {
            store.decode_chat(id, &sealed)?;
        }
        Ok(store)
    }

    pub fn create_chat(&self, meta: &ChatMeta) -> Result<i64> {
        // Random ids leak nothing about how many chats exist or their order.
        let id = loop {
            let id = (rand::random::<u64>() >> 11) as i64;
            if !self.chat_exists(id)? {
                break id;
            }
        };
        self.db.execute(
            "INSERT INTO chats (id, sealed) VALUES (?1, ?2)",
            params![id, self.seal(&chat_aad(id), meta)?],
        )?;
        Ok(id)
    }

    pub fn update_chat(&self, id: i64, meta: &ChatMeta) -> Result<()> {
        let n = self.db.execute(
            "UPDATE chats SET sealed = ?2 WHERE id = ?1",
            params![id, self.seal(&chat_aad(id), meta)?],
        )?;
        if n == 0 {
            return Err(StoreError::NoSuchChat);
        }
        Ok(())
    }

    pub fn chats(&self) -> Result<Vec<(i64, ChatMeta)>> {
        let mut stmt = self.db.prepare("SELECT id, sealed FROM chats")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        rows.map(|row| {
            let (id, sealed) = row?;
            Ok((id, self.decode_chat(id, &sealed)?))
        })
        .collect()
    }

    /// Deletes a chat and all its messages.
    pub fn delete_chat(&self, id: i64) -> Result<()> {
        self.db.execute("DELETE FROM chats WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn add_message(&self, chat_id: i64, msg: &StoredMessage) -> Result<i64> {
        if !self.chat_exists(chat_id)? {
            return Err(StoreError::NoSuchChat);
        }
        let seq: i64 = self.db.query_row(
            "SELECT MAX(COALESCE((SELECT MAX(seq) + 1 FROM messages WHERE chat_id = ?1), 0),
                        COALESCE((SELECT next FROM chat_seq WHERE chat_id = ?1), 0))",
            [chat_id],
            |r| r.get(0),
        )?;
        self.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, ?2, ?3)",
            params![chat_id, seq, self.seal(&message_aad(chat_id, seq), msg)?],
        )?;
        self.db.execute(
            "INSERT INTO chat_seq (chat_id, next) VALUES (?1, ?2)
             ON CONFLICT(chat_id) DO UPDATE SET next = excluded.next",
            params![chat_id, seq + 1],
        )?;
        Ok(seq)
    }

    /// Messages in order. `limit` keeps only the newest ones.
    pub fn messages(&self, chat_id: i64, limit: Option<u32>) -> Result<Vec<StoredMessage>> {
        Ok(self
            .messages_indexed(chat_id, limit)?
            .into_iter()
            .map(|(_, m)| m)
            .collect())
    }

    /// Messages in order with their sequence numbers.
    pub fn messages_indexed(
        &self,
        chat_id: i64,
        limit: Option<u32>,
    ) -> Result<Vec<(i64, StoredMessage)>> {
        let mut stmt = self.db.prepare(
            "SELECT seq, sealed FROM messages WHERE chat_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let limit = limit.map_or(-1, i64::from);
        let rows = stmt.query_map(params![chat_id, limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = rows
            .map(|row| {
                let (seq, sealed) = row?;
                Ok((seq, self.decode_message(chat_id, seq, &sealed)?))
            })
            .collect::<Result<Vec<_>>>()?;
        out.reverse();
        Ok(out)
    }

    /// Whether anything waiting to be delivered belongs to this message
    /// (a group message waits for each member separately).
    pub fn outbox_has_message(&self, chat_id: i64, seq: i64) -> Result<bool> {
        let mut stmt = self.db.prepare("SELECT id, sealed FROM outbox")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, sealed) = row?;
            let id: [u8; 16] = id.try_into().map_err(|_| StoreError::Decrypt)?;
            let item: OutboxItem = self.open_sealed(&outbox_aad(&id), &sealed)?;
            if item.message == Some((chat_id, seq)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A message by its shared id.
    pub fn message_by_id(&self, chat_id: i64, id: &MessageId) -> Result<Option<(i64, StoredMessage)>> {
        Ok(self
            .messages_indexed(chat_id, None)?
            .into_iter()
            .find(|(_, m)| m.id.as_ref() == Some(id)))
    }

    pub fn delete_message(&self, chat_id: i64, seq: i64) -> Result<()> {
        self.db.execute("DELETE FROM messages WHERE chat_id = ?1 AND seq = ?2", params![chat_id, seq])?;
        Ok(())
    }

    /// The attachments of a chat's messages (to delete their files).
    pub fn blobs_in(&self, chat_id: i64) -> Result<Vec<BlobId>> {
        Ok(self
            .messages_indexed(chat_id, None)?
            .into_iter()
            .filter_map(|(_, m)| m.media.map(|media| media.blob))
            .collect())
    }

    /// Every message in the chat; the chat itself stays.
    pub fn clear_messages(&self, chat_id: i64) -> Result<()> {
        self.db.execute("DELETE FROM messages WHERE chat_id = ?1", [chat_id])?;
        Ok(())
    }

    pub fn has_outbox_for(&self, peer: &[u8; 32]) -> Result<bool> {
        Ok(!self.outbox_for(peer)?.is_empty())
    }

    /// One message, by its sequence number.
    pub fn message(&self, chat_id: i64, seq: i64) -> Result<Option<StoredMessage>> {
        let sealed: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT sealed FROM messages WHERE chat_id = ?1 AND seq = ?2",
                params![chat_id, seq],
                |r| r.get(0),
            )
            .optional()?;
        sealed.map(|s| self.decode_message(chat_id, seq, &s)).transpose()
    }

    /// Replaces a message (a poll's answers changed).
    pub fn update_message(&self, chat_id: i64, seq: i64, msg: &StoredMessage) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET sealed = ?3 WHERE chat_id = ?1 AND seq = ?2",
            params![chat_id, seq, self.seal(&message_aad(chat_id, seq), msg)?],
        )?;
        Ok(())
    }

    pub fn set_delivered(&self, chat_id: i64, seq: i64) -> Result<()> {
        let sealed: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT sealed FROM messages WHERE chat_id = ?1 AND seq = ?2",
                params![chat_id, seq],
                |r| r.get(0),
            )
            .optional()?;
        let Some(sealed) = sealed else { return Ok(()) };
        let mut msg = self.decode_message(chat_id, seq, &sealed)?;
        msg.delivered = true;
        self.db.execute(
            "UPDATE messages SET sealed = ?3 WHERE chat_id = ?1 AND seq = ?2",
            params![chat_id, seq, self.seal(&message_aad(chat_id, seq), &msg)?],
        )?;
        Ok(())
    }

    pub fn outbox_put(&self, id: &[u8; 16], item: &OutboxItem) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO outbox (id, peer, sealed) VALUES (?1, ?2, ?3)",
            params![
                id.as_slice(),
                self.peer_index(&item.peer),
                self.seal(&outbox_aad(id), item)?
            ],
        )?;
        Ok(())
    }

    /// Everything waiting for `peer`, oldest first.
    pub fn outbox_for(&self, peer: &[u8; 32]) -> Result<Vec<([u8; 16], OutboxItem)>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, sealed FROM outbox WHERE peer = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map([self.peer_index(peer)], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        rows.map(|row| {
            let (id, sealed) = row?;
            let id: [u8; 16] = id.try_into().map_err(|_| StoreError::Decrypt)?;
            Ok((id, self.open_sealed(&outbox_aad(&id), &sealed)?))
        })
        .collect()
    }

    /// Friends with something waiting, so the app knows whom to reach.
    pub fn outbox_peers(&self) -> Result<Vec<[u8; 32]>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, sealed FROM outbox ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut peers = Vec::new();
        for row in rows {
            let (id, sealed) = row?;
            let id: [u8; 16] = id.try_into().map_err(|_| StoreError::Decrypt)?;
            let item: OutboxItem = self.open_sealed(&outbox_aad(&id), &sealed)?;
            if !peers.contains(&item.peer) {
                peers.push(item.peer);
            }
        }
        Ok(peers)
    }

    pub fn outbox_remove(&self, id: &[u8; 16]) -> Result<Option<OutboxItem>> {
        let sealed: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT sealed FROM outbox WHERE id = ?1",
                [id.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(sealed) = sealed else {
            return Ok(None);
        };
        let item = self.open_sealed(&outbox_aad(id), &sealed)?;
        self.db
            .execute("DELETE FROM outbox WHERE id = ?1", [id.as_slice()])?;
        Ok(Some(item))
    }

    /// Records a delivered message id. Returns false if it was seen before
    /// (a resend after a lost acknowledgement).
    pub fn mark_seen(&self, id: &[u8; 16]) -> Result<bool> {
        let n = self.db.execute(
            "INSERT OR IGNORE INTO seen (k) VALUES (?1)",
            [self.mls_index(&[b"seen:".as_slice(), id].concat())],
        )?;
        Ok(n == 1)
    }

    pub fn is_seen(&self, id: &[u8; 16]) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM seen WHERE k = ?1",
                [self.mls_index(&[b"seen:".as_slice(), id].concat())],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn peer_index(&self, peer: &[u8; 32]) -> Vec<u8> {
        self.mls_index(&[b"peer:".as_slice(), peer].concat())
    }

    fn decode_message(&self, chat_id: i64, seq: i64, sealed: &[u8]) -> Result<StoredMessage> {
        let plain = self.open_bytes(&message_aad(chat_id, seq), sealed)?;
        // Newest format first; older records end before the newer fields.
        if let Ok(m) = postcard::from_bytes::<StoredMessage>(&plain) {
            return Ok(m);
        }
        if let Ok(v6) = postcard::from_bytes::<StoredMessageV6>(&plain) {
            return Ok(StoredMessage {
                from_me: v6.from_me,
                body: v6.body,
                sent_at: v6.sent_at,
                delivered: v6.delivered,
                id: v6.id,
                reply: v6.reply,
                sender: v6.sender,
                media: v6.media,
                poll: v6.poll,
                votes: v6.votes,
                album: v6.album,
                reactions: v6.reactions,
                call: None,
            });
        }
        if let Ok(v5) = postcard::from_bytes::<StoredMessageV5>(&plain) {
            return Ok(StoredMessage {
                from_me: v5.from_me,
                body: v5.body,
                sent_at: v5.sent_at,
                delivered: v5.delivered,
                id: v5.id,
                reply: v5.reply,
                sender: v5.sender,
                media: v5.media,
                poll: v5.poll,
                votes: v5.votes,
                album: v5.album,
                ..Default::default()
            });
        }
        if let Ok(v4) = postcard::from_bytes::<StoredMessageV4>(&plain) {
            return Ok(StoredMessage {
                from_me: v4.from_me,
                body: v4.body,
                sent_at: v4.sent_at,
                delivered: v4.delivered,
                id: v4.id,
                reply: v4.reply,
                sender: v4.sender,
                media: v4.media,
                poll: v4.poll,
                votes: v4.votes,
                ..Default::default()
            });
        }
        if let Ok(v3) = postcard::from_bytes::<StoredMessageV3>(&plain) {
            return Ok(StoredMessage {
                from_me: v3.from_me,
                body: v3.body,
                sent_at: v3.sent_at,
                delivered: v3.delivered,
                id: v3.id,
                reply: v3.reply,
                sender: v3.sender,
                media: v3.media,
                ..Default::default()
            });
        }
        if let Ok(v2) = postcard::from_bytes::<StoredMessageV2>(&plain) {
            return Ok(StoredMessage {
                from_me: v2.from_me,
                body: v2.body,
                sent_at: v2.sent_at,
                delivered: v2.delivered,
                id: v2.id,
                reply: v2.reply,
                sender: v2.sender,
                ..Default::default()
            });
        }
        if let Ok(v1) = postcard::from_bytes::<StoredMessageV1>(&plain) {
            return Ok(StoredMessage {
                from_me: v1.from_me,
                body: v1.body,
                sent_at: v1.sent_at,
                delivered: v1.delivered,
                ..Default::default()
            });
        }
        let v0: StoredMessageV0 = postcard::from_bytes(&plain)?;
        Ok(StoredMessage {
            from_me: v0.from_me,
            body: v0.body,
            sent_at: v0.sent_at,
            ..Default::default()
        })
    }

    /// All persisted MLS state (group secrets, ratchets, key packages).
    /// A small sealed value under `name` (e.g. a friend's nearby key).
    pub fn kv_put<T: Serialize>(&self, name: &[u8], value: &T) -> Result<()> {
        let hashed = self.kv_index(name);
        self.db.execute(
            "INSERT INTO kv (k, sealed) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET sealed = excluded.sealed",
            params![hashed, self.seal(&kv_aad(&hashed), value)?],
        )?;
        Ok(())
    }

    pub fn kv_get<T: DeserializeOwned>(&self, name: &[u8]) -> Result<Option<T>> {
        let hashed = self.kv_index(name);
        let sealed: Option<Vec<u8>> = self
            .db
            .query_row("SELECT sealed FROM kv WHERE k = ?1", [&hashed], |r| r.get(0))
            .optional()?;
        sealed.map(|s| self.open_sealed(&kv_aad(&hashed), &s)).transpose()
    }

    pub fn kv_delete(&self, name: &[u8]) -> Result<()> {
        self.db.execute("DELETE FROM kv WHERE k = ?1", [self.kv_index(name)])?;
        Ok(())
    }

    fn kv_index(&self, name: &[u8]) -> Vec<u8> {
        blake3::keyed_hash(&self.index_key, &[b"kv:".as_slice(), name].concat()).as_bytes().to_vec()
    }

    pub fn mls_load(&self) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
        let mut stmt = self.db.prepare("SELECT k, sealed FROM mls_kv")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        rows.map(|row| {
            let (hashed, sealed) = row?;
            self.open_sealed::<(Vec<u8>, Vec<u8>)>(&mls_aad(&hashed), &sealed)
        })
        .collect()
    }

    /// Writes changed MLS entries and removes deleted ones, atomically.
    pub fn mls_apply(&self, upserts: &[(&[u8], &[u8])], deletes: &[&[u8]]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for (k, v) in upserts {
            let hashed = self.mls_index(k);
            tx.execute(
                "INSERT OR REPLACE INTO mls_kv (k, sealed) VALUES (?1, ?2)",
                params![hashed, self.seal(&mls_aad(&hashed), &(k, v))?],
            )?;
        }
        for k in deletes {
            tx.execute("DELETE FROM mls_kv WHERE k = ?1", [self.mls_index(k)])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn mls_index(&self, k: &[u8]) -> Vec<u8> {
        blake3::keyed_hash(&self.index_key, k).as_bytes().to_vec()
    }

    fn chat_exists(&self, id: i64) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM chats WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn seal<T: Serialize>(&self, aad: &[u8], value: &T) -> Result<Vec<u8>> {
        let nonce: [u8; NONCE_LEN] = rand::random();
        let plain = postcard::to_stdvec(value)?;
        let mut out = nonce.to_vec();
        out.extend(
            self.cipher
                .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plain, aad })
                .map_err(|_| StoreError::Decrypt)?,
        );
        Ok(out)
    }

    fn open_sealed<T: DeserializeOwned>(&self, aad: &[u8], sealed: &[u8]) -> Result<T> {
        Ok(postcard::from_bytes(&self.open_bytes(aad, sealed)?)?)
    }

    fn open_bytes(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < NONCE_LEN {
            return Err(StoreError::Decrypt);
        }
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        self.cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
            .map_err(|_| StoreError::Decrypt)
    }

    /// Reads a chat in the current format, upgrading older records.
    fn decode_chat(&self, id: i64, sealed: &[u8]) -> Result<ChatMeta> {
        let plain = self.open_bytes(&chat_aad(id), sealed)?;
        // Newest first: an older record ends before the newer fields, so it
        // fails to decode as the newer format.
        if let Ok(meta) = postcard::from_bytes::<ChatMeta>(&plain) {
            return Ok(meta);
        }
        if let Ok(v2) = postcard::from_bytes::<ChatMetaV2>(&plain) {
            return Ok(v2.into());
        }
        if let Ok(v1) = postcard::from_bytes::<ChatMetaV1>(&plain) {
            return Ok(v1.into());
        }
        Ok(postcard::from_bytes::<ChatMetaV0>(&plain)?.into())
    }
}

fn chat_aad(id: i64) -> Vec<u8> {
    [b"chat:".as_slice(), &id.to_le_bytes()].concat()
}

fn kv_aad(hashed_key: &[u8]) -> Vec<u8> {
    [b"kv:".as_slice(), hashed_key].concat()
}

fn mls_aad(hashed_key: &[u8]) -> Vec<u8> {
    [b"mls:".as_slice(), hashed_key].concat()
}

fn outbox_aad(id: &[u8; 16]) -> Vec<u8> {
    [b"outbox:".as_slice(), id].concat()
}

fn message_aad(chat_id: i64, seq: i64) -> Vec<u8> {
    [
        b"msg:".as_slice(),
        &chat_id.to_le_bytes(),
        &seq.to_le_bytes(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: &str) -> ChatMeta {
        ChatMeta {
            kind: ChatKind::Notes,
            title: title.into(),
            sigil_key: vec![7; 32],
            pinned: false,
            created_at: 1,
            group_id: Vec::new(),
            peers: Vec::new(),
            hidden: false,
            admin: None,
        }
    }

    #[test]
    fn old_chat_records_upgrade() {
        #[derive(Serialize)]
        struct V0 {
            kind: ChatKind,
            title: String,
            sigil_key: Vec<u8>,
            pinned: bool,
            created_at: u64,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let key = [9u8; 32];
        {
            let s = Store::open(&path, &key).unwrap();
            let id = 42;
            let old = V0 {
                kind: ChatKind::Notes,
                title: "Notes to self".into(),
                sigil_key: vec![1; 32],
                pinned: true,
                created_at: 5,
            };
            s.db.execute(
                "INSERT INTO chats (id, sealed) VALUES (?1, ?2)",
                params![id, s.seal(&chat_aad(id), &old).unwrap()],
            )
            .unwrap();
        }
        let s = Store::open(&path, &key).unwrap();
        let (id, meta) = s.chats().unwrap().remove(0);
        assert_eq!(id, 42);
        assert_eq!(meta.title, "Notes to self");
        assert!(meta.pinned && meta.peers.is_empty() && meta.group_id.is_empty());
    }

    #[test]
    fn chats_from_before_hidden_chats_upgrade() {
        #[derive(Serialize)]
        struct V2 {
            kind: ChatKind,
            title: String,
            sigil_key: Vec<u8>,
            pinned: bool,
            created_at: u64,
            group_id: Vec<u8>,
            peers: Vec<Peer>,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let key = [3u8; 32];
        let peer = Peer { identity: [4; 32], name: "Bob".into(), onion: None, verified: true };
        {
            let s = Store::open(&path, &key).unwrap();
            let old = V2 {
                kind: ChatKind::Direct,
                title: "Bob".into(),
                sigil_key: vec![1; 32],
                pinned: false,
                created_at: 5,
                group_id: vec![8; 16],
                peers: vec![peer.clone()],
            };
            s.db.execute(
                "INSERT INTO chats (id, sealed) VALUES (?1, ?2)",
                params![7, s.seal(&chat_aad(7), &old).unwrap()],
            )
            .unwrap();
        }
        let s = Store::open(&path, &key).unwrap();
        let (_, meta) = s.chats().unwrap().remove(0);
        assert_eq!((&meta.peers, &meta.group_id), (&vec![peer], &vec![8; 16]));
        assert!(!meta.hidden && meta.admin.is_none());
        // And the new fields round-trip.
        let mut hidden = meta.clone();
        hidden.hidden = true;
        hidden.admin = Some([5; 32]);
        s.update_chat(7, &hidden).unwrap();
        assert_eq!(s.chats().unwrap().remove(0).1, hidden);
    }

    #[test]
    fn messages_from_before_calls_upgrade() {
        #[derive(Serialize)]
        struct V6 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
            id: Option<MessageId>,
            reply: Option<Quote>,
            sender: Option<[u8; 32]>,
            media: Option<Media>,
            poll: Option<Poll>,
            votes: Vec<Vote>,
            album: Option<Album>,
            reactions: Vec<Reaction>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[7; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V6 {
            from_me: true,
            body: "see you".into(),
            sent_at: 5,
            delivered: true,
            id: Some([2; 16]),
            reply: None,
            sender: None,
            media: None,
            poll: None,
            votes: Vec::new(),
            album: None,
            reactions: vec![Reaction { who: [4; 32], emoji: "👍".into(), at: 3 }],
        };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!((m.body.as_str(), m.id, m.delivered), ("see you", Some([2; 16]), true));
        assert_eq!(m.reactions[0].emoji, "👍");
        assert!(m.call.is_none());
        let call = StoredMessage {
            from_me: true,
            body: "Voice call · 4:12".into(),
            call: Some(CallLog { video: false, outcome: 0, seconds: 252 }),
            ..Default::default()
        };
        let seq = s.add_message(chat, &call).unwrap();
        assert_eq!(s.message(chat, seq).unwrap().unwrap(), call);
    }

    #[test]
    fn messages_from_before_reactions_upgrade() {
        #[derive(Serialize)]
        struct V5 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
            id: Option<MessageId>,
            reply: Option<Quote>,
            sender: Option<[u8; 32]>,
            media: Option<Media>,
            poll: Option<Poll>,
            votes: Vec<Vote>,
            album: Option<Album>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[7; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V5 {
            from_me: false,
            body: "beach!".into(),
            sent_at: 4,
            delivered: true,
            id: Some([1; 16]),
            reply: None,
            sender: Some([3; 32]),
            media: Some(Media { name: "b.jpg".into(), ..Default::default() }),
            poll: None,
            votes: Vec::new(),
            album: Some(Album { id: [9; 16], title: "Goa".into() }),
        };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!((m.body.as_str(), m.sender, m.id), ("beach!", Some([3; 32]), Some([1; 16])));
        assert_eq!(m.album.unwrap().title, "Goa");
        assert_eq!(m.media.unwrap().name, "b.jpg");
        assert!(m.reactions.is_empty());
        let liked = StoredMessage {
            body: "hi".into(),
            reactions: vec![Reaction { who: [2; 32], emoji: "🔥".into(), at: 9 }],
            ..Default::default()
        };
        let seq = s.add_message(chat, &liked).unwrap();
        assert_eq!(s.message(chat, seq).unwrap().unwrap(), liked);
    }

    #[test]
    fn messages_from_before_albums_upgrade() {
        #[derive(Serialize)]
        struct V4 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
            id: Option<MessageId>,
            reply: Option<Quote>,
            sender: Option<[u8; 32]>,
            media: Option<Media>,
            poll: Option<Poll>,
            votes: Vec<Vote>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[7; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V4 {
            from_me: false,
            body: "Lunch?".into(),
            sent_at: 4,
            delivered: true,
            id: Some([1; 16]),
            reply: None,
            sender: Some([3; 32]),
            media: None,
            poll: Some(Poll { question: "Lunch?".into(), options: vec!["Yes".into(), "No".into()], ..Default::default() }),
            votes: vec![Vote { voter: [2; 32], choices: vec![1], at: 5 }],
        };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!(m.poll.unwrap().question, "Lunch?");
        assert_eq!(m.votes[0].choices, vec![1]);
        assert!(m.album.is_none());
        let photo = StoredMessage {
            media: Some(Media { name: "a.jpg".into(), ..Default::default() }),
            album: Some(Album { id: [9; 16], title: "Goa".into() }),
            ..Default::default()
        };
        let seq = s.add_message(chat, &photo).unwrap();
        assert_eq!(s.message(chat, seq).unwrap().unwrap(), photo);
    }

    #[test]
    fn messages_from_before_polls_upgrade() {
        #[derive(Serialize)]
        struct V3 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
            id: Option<MessageId>,
            reply: Option<Quote>,
            sender: Option<[u8; 32]>,
            media: Option<Media>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[6; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V3 {
            from_me: true,
            body: "beach".into(),
            sent_at: 4,
            delivered: true,
            id: Some([1; 16]),
            reply: None,
            sender: None,
            media: Some(Media { name: "beach.jpg".into(), size: 7, ..Default::default() }),
        };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!((m.body.as_str(), m.id), ("beach", Some([1; 16])));
        assert_eq!(m.media.unwrap().name, "beach.jpg");
        assert!(m.poll.is_none() && m.votes.is_empty());
        // And a poll round-trips.
        let poll = StoredMessage {
            poll: Some(Poll { question: "Where?".into(), options: vec!["Beach".into()], ..Default::default() }),
            votes: vec![Vote { voter: [2; 32], choices: vec![0], at: 5 }],
            ..Default::default()
        };
        let seq = s.add_message(chat, &poll).unwrap();
        assert_eq!(s.message(chat, seq).unwrap().unwrap(), poll);
    }

    #[test]
    fn messages_from_before_attachments_upgrade() {
        #[derive(Serialize)]
        struct V2 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
            id: Option<MessageId>,
            reply: Option<Quote>,
            sender: Option<[u8; 32]>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[5; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V2 {
            from_me: false,
            body: "re: dinner".into(),
            sent_at: 9,
            delivered: true,
            id: Some([1; 16]),
            reply: Some(Quote { id: [2; 16], author: [3; 32], snippet: "dinner?".into() }),
            sender: Some([4; 32]),
        };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!((m.body.as_str(), m.id, m.sender), ("re: dinner", Some([1; 16]), Some([4; 32])));
        assert_eq!(m.reply.unwrap().snippet, "dinner?");
        assert!(m.media.is_none());
    }

    #[test]
    fn messages_from_before_replies_upgrade() {
        #[derive(Serialize)]
        struct V1 {
            from_me: bool,
            body: String,
            sent_at: u64,
            delivered: bool,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[2; 32]).unwrap();
        let chat = s.create_chat(&meta("Bob")).unwrap();
        let old = V1 { from_me: true, body: "old".into(), sent_at: 3, delivered: true };
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![chat, s.seal(&message_aad(chat, 0), &old).unwrap()],
        )
        .unwrap();
        let m = s.message(chat, 0).unwrap().unwrap();
        assert_eq!((m.body.as_str(), m.sent_at, m.delivered), ("old", 3, true));
        assert!(m.id.is_none() && m.reply.is_none() && m.sender.is_none());
    }

    fn msg(body: &str, sent_at: u64) -> StoredMessage {
        StoredMessage {
            from_me: true,
            body: body.into(),
            sent_at,
            ..Default::default()
        }
    }

    #[test]
    fn delivery_outbox_and_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let key = [6u8; 32];
        let (peer, other) = ([1u8; 32], [2u8; 32]);
        let chat = {
            let s = Store::open(&path, &key).unwrap();
            let chat = s.create_chat(&meta("Bob")).unwrap();
            let seq = s.add_message(chat, &msg("hi", 1)).unwrap();
            let item = OutboxItem {
                peer,
                state: OutboxState::Plain(b"hi".to_vec()),
                message: Some((chat, seq)),
            };
            s.outbox_put(&[9; 16], &item).unwrap();
            s.outbox_put(
                &[8; 16],
                &OutboxItem {
                    peer: other,
                    state: OutboxState::Cipher(vec![1]),
                    message: None,
                },
            )
            .unwrap();
            assert!(s.mark_seen(&[7; 16]).unwrap());
            assert!(
                !s.mark_seen(&[7; 16]).unwrap(),
                "second time is a duplicate"
            );
            chat
        };
        // Everything survives a restart.
        let s = Store::open(&path, &key).unwrap();
        assert_eq!(s.outbox_peers().unwrap(), vec![peer, other]);
        let pending = s.outbox_for(&peer).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!s.mark_seen(&[7; 16]).unwrap());
        let removed = s.outbox_remove(&pending[0].0).unwrap().unwrap();
        let (c, seq) = removed.message.unwrap();
        s.set_delivered(c, seq).unwrap();
        assert!(s.messages(chat, None).unwrap()[0].delivered);
        assert!(s.outbox_for(&peer).unwrap().is_empty());
        assert!(s.outbox_remove(&pending[0].0).unwrap().is_none());
    }

    #[test]
    fn friends_from_before_verification_upgrade_as_unverified() {
        #[derive(Serialize)]
        struct PeerV1 {
            identity: [u8; 32],
            name: String,
            onion: Option<String>,
        }
        #[derive(Serialize)]
        struct V1 {
            kind: ChatKind,
            title: String,
            sigil_key: Vec<u8>,
            pinned: bool,
            created_at: u64,
            group_id: Vec<u8>,
            peers: Vec<PeerV1>,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[4; 32]).unwrap();
        let old = V1 {
            kind: ChatKind::Direct,
            title: "Bob".into(),
            sigil_key: vec![2; 32],
            pinned: false,
            created_at: 3,
            group_id: vec![5, 6],
            peers: vec![PeerV1 {
                identity: [7; 32],
                name: "Bob".into(),
                onion: None,
            }],
        };
        s.db.execute(
            "INSERT INTO chats (id, sealed) VALUES (77, ?1)",
            params![s.seal(&chat_aad(77), &old).unwrap()],
        )
        .unwrap();
        let (_, meta) = s.chats().unwrap().remove(0);
        assert_eq!(meta.group_id, vec![5, 6]);
        assert_eq!(meta.peers[0].identity, [7; 32]);
        assert!(!meta.peers[0].verified);
    }

    #[test]
    fn old_messages_upgrade() {
        #[derive(Serialize)]
        struct V0 {
            from_me: bool,
            body: String,
            sent_at: u64,
        }
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[3; 32]).unwrap();
        let chat = s.create_chat(&meta("x")).unwrap();
        s.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, 0, ?2)",
            params![
                chat,
                s.seal(
                    &message_aad(chat, 0),
                    &V0 {
                        from_me: true,
                        body: "old".into(),
                        sent_at: 4
                    }
                )
                .unwrap()
            ],
        )
        .unwrap();
        let m = &s.messages(chat, None).unwrap()[0];
        assert_eq!((m.body.as_str(), m.delivered), ("old", false));
    }

    #[test]
    fn roundtrip_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let key = [1u8; 32];
        let id = {
            let s = Store::open(&path, &key).unwrap();
            let id = s.create_chat(&meta("Notes to self")).unwrap();
            s.add_message(id, &msg("one", 1)).unwrap();
            s.add_message(id, &msg("two", 2)).unwrap();
            let mut m = meta("Notes to self");
            m.pinned = true;
            s.update_chat(id, &m).unwrap();
            id
        };
        let s = Store::open(&path, &key).unwrap();
        let chats = s.chats().unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].0, id);
        assert!(chats[0].1.pinned);
        assert_eq!(
            s.messages(id, None).unwrap(),
            vec![msg("one", 1), msg("two", 2)]
        );
        assert_eq!(s.messages(id, Some(1)).unwrap(), vec![msg("two", 2)]);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        Store::open(&path, &[1; 32])
            .unwrap()
            .create_chat(&meta("x"))
            .unwrap();
        assert!(matches!(
            Store::open(&path, &[2; 32]),
            Err(StoreError::Decrypt)
        ));
    }

    #[test]
    fn nothing_readable_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        {
            let s = Store::open(&path, &[3; 32]).unwrap();
            let id = s.create_chat(&meta("SecretTitle")).unwrap();
            s.add_message(id, &msg("SecretBody", 1)).unwrap();
        }
        let mut bytes = std::fs::read(&path).unwrap();
        if let Ok(wal) = std::fs::read(dir.path().join("s.db-wal")) {
            bytes.extend(wal);
        }
        let hay = String::from_utf8_lossy(&bytes);
        assert!(!hay.contains("SecretTitle") && !hay.contains("SecretBody"));
    }

    #[test]
    fn swapped_rows_fail_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[4; 32]).unwrap();
        let id = s.create_chat(&meta("x")).unwrap();
        s.add_message(id, &msg("first", 1)).unwrap();
        s.add_message(id, &msg("second", 2)).unwrap();
        // Copy message 0's ciphertext over message 1.
        s.db.execute(
            "UPDATE messages SET sealed = (SELECT sealed FROM messages WHERE seq = 0) WHERE seq = 1",
            [],
        )
        .unwrap();
        assert!(matches!(s.messages(id, None), Err(StoreError::Decrypt)));
    }

    #[test]
    fn delete_removes_messages() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("s.db"), &[5; 32]).unwrap();
        let id = s.create_chat(&meta("x")).unwrap();
        s.add_message(id, &msg("a", 1)).unwrap();
        s.delete_chat(id).unwrap();
        assert!(s.chats().unwrap().is_empty());
        assert!(matches!(
            s.add_message(id, &msg("b", 2)),
            Err(StoreError::NoSuchChat)
        ));
        let n: i64 =
            s.db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n, 0);
    }
}
