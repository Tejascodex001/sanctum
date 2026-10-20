//! Encrypted on-device storage for chats that persist (everything except
//! temp chats, which never touch disk).
//!
//! Each row's payload is sealed with XChaCha20-Poly1305 under a 32-byte key
//! that the app keeps in the Android Keystore. The row's identity (table and
//! ids) is bound in as associated data, so rows can't be swapped or replayed
//! between chats. SQLite only ever sees random ids, sequence numbers and
//! ciphertext; `secure_delete` zeroes deleted pages.

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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub identity: [u8; 32],
    pub name: String,
    /// Where to reach them over Tor, once known.
    pub onion: Option<String>,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub from_me: bool,
    pub body: String,
    /// Unix seconds.
    pub sent_at: u64,
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
             CREATE TABLE IF NOT EXISTS messages (
                 chat_id INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
                 seq INTEGER NOT NULL,
                 sealed BLOB NOT NULL,
                 PRIMARY KEY (chat_id, seq)
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
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE chat_id = ?1",
            [chat_id],
            |r| r.get(0),
        )?;
        self.db.execute(
            "INSERT INTO messages (chat_id, seq, sealed) VALUES (?1, ?2, ?3)",
            params![chat_id, seq, self.seal(&message_aad(chat_id, seq), msg)?],
        )?;
        Ok(seq)
    }

    /// Messages in order. `limit` keeps only the newest ones.
    pub fn messages(&self, chat_id: i64, limit: Option<u32>) -> Result<Vec<StoredMessage>> {
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
                self.open_sealed(&message_aad(chat_id, seq), &sealed)
            })
            .collect::<Result<Vec<_>>>()?;
        out.reverse();
        Ok(out)
    }

    /// All persisted MLS state (group secrets, ratchets, key packages).
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
        match postcard::from_bytes::<ChatMeta>(&plain) {
            Ok(meta) => Ok(meta),
            Err(_) => Ok(postcard::from_bytes::<ChatMetaV0>(&plain)?.into()),
        }
    }
}

fn chat_aad(id: i64) -> Vec<u8> {
    [b"chat:".as_slice(), &id.to_le_bytes()].concat()
}

fn mls_aad(hashed_key: &[u8]) -> Vec<u8> {
    [b"mls:".as_slice(), hashed_key].concat()
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

    fn msg(body: &str, sent_at: u64) -> StoredMessage {
        StoredMessage {
            from_me: true,
            body: body.into(),
            sent_at,
        }
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
