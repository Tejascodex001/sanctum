//! Attachments: photos, videos, voice notes and files.
//!
//! Each attachment gets its own random key. The file is encrypted with it
//! in 64 KiB chunks (XChaCha20-Poly1305, the nonce being the blob id and
//! chunk number) and kept like that in the media folder, on both phones.
//! The key and a description ([`Media`]) travel inside the chat's MLS
//! message; the encrypted chunks follow over the same link
//! ([`Frame::BlobChunk`]), useless without the key. A receiver checks every
//! chunk as it arrives and shows the file once the last one is in.

use crate::protocol::{BlobId, Content, Frame, Media, MediaKind, MessageId};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, OutboxItem, OutboxState, StoredMessage};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CHUNK: usize = 64 * 1024;
pub const MAX_SIZE: u64 = 50 * 1024 * 1024;
/// Big files aren't resent on every flush: only on a new link, or after
/// this long without an acknowledgement.
const RESEND_AFTER: Duration = Duration::from_secs(300);

/// What a receiver expects of an attachment announced by a message.
#[derive(Serialize, Deserialize)]
struct Expected {
    from: [u8; 32],
    key: [u8; 32],
    total: u32,
    chat_id: i64,
}

/// What the app needs to send a file.
pub struct Outgoing<'a> {
    pub path: &'a Path,
    pub kind: MediaKind,
    pub mime: &'a str,
    pub name: &'a str,
    pub caption: &'a str,
    pub thumb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
}

fn nonce(blob: &BlobId, index: u32) -> XNonce {
    let mut n = [0u8; 24];
    n[..16].copy_from_slice(blob);
    n[16..20].copy_from_slice(&index.to_le_bytes());
    XNonce::from(n)
}

fn kv_name(blob: &BlobId) -> Vec<u8> {
    [b"blob:".as_slice(), blob].concat()
}

/// Chunks in a stored blob: `u32` length, then the sealed chunk.
fn read_chunks(path: &Path) -> std::io::Result<Vec<Vec<u8>>> {
    let data = fs::read(path)?;
    let mut chunks = Vec::new();
    let mut rest = data.as_slice();
    while rest.len() >= 4 {
        let n = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        rest = &rest[4..];
        if rest.len() < n {
            return Err(std::io::Error::other("blob cut off"));
        }
        chunks.push(rest[..n].to_vec());
        rest = &rest[n..];
    }
    Ok(chunks)
}

fn append_chunk(file: &mut fs::File, chunk: &[u8]) -> std::io::Result<()> {
    file.write_all(&(chunk.len() as u32).to_le_bytes())?;
    file.write_all(chunk)
}

impl Session {
    fn blob_path(&self, blob: &BlobId) -> PathBuf {
        self.media_dir.join(blob.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    /// Encrypts `out.path` into the media folder, saves the message and
    /// queues it (and the file) for the chat's other phones. Returns who
    /// to reach.
    pub fn send_media(&mut self, chat_id: i64, out: Outgoing<'_>, reply_to: Option<i64>) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        let plain = fs::read(out.path).map_err(io)?;
        if plain.len() as u64 > MAX_SIZE {
            return Err(SessionError::TooLarge);
        }
        let blob: BlobId = rand::random();
        let key: [u8; 32] = rand::random();
        let cipher = XChaCha20Poly1305::new((&key).into());
        fs::create_dir_all(&self.media_dir).map_err(io)?;
        let mut file = fs::File::create(self.blob_path(&blob)).map_err(io)?;
        let mut total = 0u32;
        for (i, piece) in plain.chunks(CHUNK).enumerate() {
            let sealed = cipher.encrypt(&nonce(&blob, i as u32), piece).map_err(|_| SessionError::Protocol)?;
            append_chunk(&mut file, &sealed).map_err(io)?;
            total += 1;
        }
        file.sync_all().map_err(io)?;
        let media = Media {
            blob,
            key,
            kind: out.kind,
            mime: clip(out.mime, 100),
            name: clip(out.name, 200),
            size: plain.len() as u64,
            width: out.width,
            height: out.height,
            duration_ms: out.duration_ms,
            thumb: out.thumb,
        };
        let id: MessageId = rand::random();
        let sent_at = now();
        let quote = match reply_to {
            Some(seq) => self.quote(&meta, chat_id, seq)?,
            None => None,
        };
        let seq = self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: out.caption.into(),
                sent_at,
                delivered: meta.kind == ChatKind::Notes,
                id: Some(id),
                reply: quote.clone(),
                sender: None,
                media: Some(media.clone()),
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        let content = Content::Media {
            id,
            body: out.caption.into(),
            sent_at,
            quote,
            media,
        };
        let recipients = match meta.kind {
            ChatKind::Notes => return Ok(Vec::new()),
            ChatKind::Group => {
                if meta.group_id.is_empty() {
                    return Err(SessionError::NotAFriend);
                }
                self.broadcast_message(&meta, &content, (chat_id, seq))?
            }
            ChatKind::Direct => {
                let Some(peer) = meta.peers.first().map(|p| p.identity) else {
                    return Ok(Vec::new());
                };
                self.store.outbox_put(
                    &id,
                    &OutboxItem {
                        peer,
                        state: OutboxState::Plain(content.encode()),
                        message: Some((chat_id, seq)),
                    },
                )?;
                vec![peer]
            }
        };
        // The file itself, to each of them (after the message, in order).
        for peer in &recipients {
            self.store.outbox_put(
                &rand::random(),
                &OutboxItem {
                    peer: *peer,
                    state: OutboxState::Blob { blob, total },
                    // Delivered (✓✓) once the file is there too.
                    message: Some((chat_id, seq)),
                },
            )?;
        }
        Ok(recipients)
    }

    /// The decrypted attachment of a message, if it has arrived.
    pub fn media_bytes(&self, chat_id: i64, seq: i64) -> Result<Option<Vec<u8>>> {
        let Some(media) = self.store.message(chat_id, seq)?.and_then(|m| m.media) else {
            return Ok(None);
        };
        let path = self.blob_path(&media.blob);
        if !path.exists() {
            return Ok(None);
        }
        let cipher = XChaCha20Poly1305::new((&media.key).into());
        let mut out = Vec::with_capacity(media.size as usize);
        for (i, chunk) in read_chunks(&path).map_err(io)?.iter().enumerate() {
            let plain = cipher
                .decrypt(&nonce(&media.blob, i as u32), chunk.as_slice())
                .map_err(|_| SessionError::Protocol)?;
            out.extend_from_slice(&plain);
        }
        Ok(Some(out))
    }

    /// Decrypts a message's attachment to `dest` (to play or open it).
    pub fn media_export(&self, chat_id: i64, seq: i64, dest: &Path) -> Result<bool> {
        let Some(bytes) = self.media_bytes(chat_id, seq)? else {
            return Ok(false);
        };
        if let Some(dir) = dest.parent() {
            fs::create_dir_all(dir).map_err(io)?;
        }
        fs::write(dest, bytes).map_err(io)?;
        Ok(true)
    }

    /// Whether a message's attachment has arrived.
    pub fn media_ready(&self, media: &Media) -> bool {
        self.blob_path(&media.blob).exists()
    }

    /// A message announced an attachment from `from`: accept its chunks.
    pub(crate) fn expect_blob(&mut self, chat_id: i64, from: &[u8; 32], media: &Media) -> Result<()> {
        let total = media.size.div_ceil(CHUNK as u64).max(1) as u32;
        self.store.kv_put(
            &kv_name(&media.blob),
            &Expected {
                from: *from,
                key: media.key,
                total,
                chat_id,
            },
        )?;
        Ok(())
    }

    pub(crate) fn delete_blobs(&self, blobs: &[BlobId]) {
        for blob in blobs {
            let _ = fs::remove_file(self.blob_path(blob));
            let _ = fs::remove_file(self.blob_path(blob).with_extension("part"));
            let _ = self.store.kv_delete(&kv_name(blob));
        }
    }

    /// A new link to `peer`: files not yet acknowledged go again.
    pub fn link_started(&mut self, peer: &[u8; 32]) {
        self.blobs_sent.retain(|(p, _), _| p != peer);
    }

    /// The chunk frames of an attachment waiting for `peer`, unless sent
    /// recently on this link.
    pub(crate) fn blob_frames(&mut self, peer: &[u8; 32], id: MessageId, blob: &BlobId) -> Result<Vec<Frame>> {
        if self.blobs_sent.get(&(*peer, id)).is_some_and(|at| at.elapsed() < RESEND_AFTER) {
            return Ok(Vec::new());
        }
        let Ok(chunks) = read_chunks(&self.blob_path(blob)) else {
            // The file was deleted (with its message): nothing to send.
            self.store.outbox_remove(&id)?;
            return Ok(Vec::new());
        };
        self.blobs_sent.insert((*peer, id), Instant::now());
        let total = chunks.len() as u32;
        Ok(chunks
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| Frame::BlobChunk {
                id,
                blob: *blob,
                index: index as u32,
                total,
                bytes,
            })
            .collect())
    }

    pub(crate) fn on_blob_chunk(
        &mut self,
        peer: &[u8; 32],
        id: MessageId,
        blob: BlobId,
        index: u32,
        total: u32,
        bytes: Vec<u8>,
    ) -> Result<Vec<Frame>> {
        let last = index + 1 == total;
        let done = self.blob_path(&blob);
        if done.exists() {
            // A resend after our acknowledgement got lost.
            return Ok(if last { vec![Frame::Ack { id }] } else { Vec::new() });
        }
        // Only files a message announced, from whoever sent that message.
        let Some(expected) = self.store.kv_get::<Expected>(&kv_name(&blob))? else {
            return Ok(Vec::new());
        };
        if expected.from != *peer || expected.total != total || index >= total {
            return Ok(Vec::new());
        }
        let cipher = XChaCha20Poly1305::new((&expected.key).into());
        if cipher.decrypt(&nonce(&blob, index), bytes.as_slice()).is_err() {
            return Err(SessionError::Protocol);
        }
        let part = done.with_extension("part");
        fs::create_dir_all(&self.media_dir).map_err(io)?;
        // Chunks come in order over a link; a resend starts over.
        let have = if index == 0 { 0 } else { read_chunks(&part).map(|c| c.len() as u32).unwrap_or(0) };
        if index != have {
            return Ok(Vec::new());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(index != 0)
            .write(true)
            .truncate(index == 0)
            .open(&part)
            .map_err(io)?;
        append_chunk(&mut file, &bytes).map_err(io)?;
        if !last {
            return Ok(Vec::new());
        }
        file.sync_all().map_err(io)?;
        fs::rename(&part, &done).map_err(io)?;
        self.store.kv_delete(&kv_name(&blob))?;
        self.events.push(Event::Chat(expected.chat_id));
        Ok(vec![Frame::Ack { id }])
    }

    /// Stores a received media message and expects its file.
    #[allow(clippy::too_many_arguments)] // the message's fields, as received
    pub(crate) fn receive_media(
        &mut self,
        chat_id: i64,
        peer: &[u8; 32],
        id: MessageId,
        body: String,
        sent_at: u64,
        quote: Option<crate::protocol::Quote>,
        media: Media,
    ) -> Result<()> {
        if media.size > MAX_SIZE {
            return Ok(());
        }
        self.expect_blob(chat_id, peer, &media)?;
        self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: false,
                body: clip(&body, 4000),
                sent_at: sent_at.min(now()),
                delivered: true,
                id: Some(id),
                reply: quote,
                sender: Some(*peer),
                media: Some(media),
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        self.events.push(Event::Message(chat_id));
        Ok(())
    }
}

fn io(e: std::io::Error) -> SessionError {
    SessionError::Io(e.to_string())
}
