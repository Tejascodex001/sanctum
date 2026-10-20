//! Persistent chats, encrypted at rest. See `sanctum_core::store`.

use super::poll::PollView;
use super::session;
use sanctum_core::media::Outgoing;
use sanctum_core::protocol::{Album, MediaKind};
use sanctum_core::store::{ChatKind, ChatMeta, Store};
use std::time::{SystemTime, UNIX_EPOCH};

pub enum ChatKindView {
    Notes,
    Direct,
    Group,
}

pub struct StoredChat {
    pub id: i64,
    pub kind: ChatKindView,
    pub title: String,
    pub sigil_key: Vec<u8>,
    pub pinned: bool,
    pub created_at: i64,
    /// Friends' chats: their key is confirmed (scanned in person or
    /// safety number compared).
    pub verified: bool,
    /// Friends' chats: they haven't added us back yet (our friend request
    /// is still going out).
    pub waiting: bool,
    /// Left out of the list until revealed with biometrics.
    pub hidden: bool,
    /// Groups: how many others are in it (or invited).
    pub members: u32,
    /// Groups: false once we've left or been removed.
    pub active: bool,
}

pub struct MessageView {
    /// Position in the chat: what a reply points at.
    pub seq: i64,
    pub from_me: bool,
    pub body: String,
    pub sent_at: i64,
    /// Our messages: the friend's phone has it.
    pub delivered: bool,
    /// The message this one answers.
    pub reply: Option<ReplyView>,
    /// Who wrote it, in a group (None for ours, and in direct chats).
    pub sender_name: Option<String>,
    /// A photo, video, voice note or file.
    pub media: Option<MediaView>,
    /// A poll, date poll or plan, with everyone's answers.
    pub poll: Option<PollView>,
    /// The shared album a photo or video is in.
    pub album: Option<AlbumView>,
}

/// A shared album: every photo and video in the chat with its id.
pub struct AlbumView {
    /// 16 random bytes, made by whoever starts it.
    pub id: Vec<u8>,
    pub title: String,
}

pub enum MediaKindView {
    Image,
    Video,
    Audio,
    File,
}

pub struct MediaView {
    pub kind: MediaKindView,
    pub mime: String,
    pub name: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    /// A tiny JPEG preview (may be empty).
    pub thumb: Vec<u8>,
    /// The file has arrived (always, for our own).
    pub ready: bool,
}

fn media_kind(kind: MediaKindView) -> MediaKind {
    match kind {
        MediaKindView::Image => MediaKind::Image,
        MediaKindView::Video => MediaKind::Video,
        MediaKindView::Audio => MediaKind::Audio,
        MediaKindView::File => MediaKind::File,
    }
}

/// Sends a file (encrypted on this phone, then to the chat's other phones).
#[allow(clippy::too_many_arguments)]
pub fn media_send(
    chat_id: i64,
    path: String,
    kind: MediaKindView,
    mime: String,
    name: String,
    caption: String,
    thumb: Vec<u8>,
    width: u32,
    height: u32,
    duration_ms: u32,
    reply_to: Option<i64>,
    album: Option<AlbumView>,
) -> anyhow::Result<()> {
    let album = match album {
        Some(a) => Some(Album {
            id: a.id.try_into().map_err(|_| anyhow::anyhow!("bad album id"))?,
            title: a.title,
        }),
        None => None,
    };
    let out = Outgoing {
        path: std::path::Path::new(&path),
        kind: media_kind(kind),
        mime: &mime,
        name: &name,
        caption: &caption,
        thumb,
        width,
        height,
        duration_ms,
        album,
    };
    reach(session::with(|s| Ok(s.send_media(chat_id, out, reply_to)?))?);
    Ok(())
}

/// A message's file, decrypted (None until it has arrived).
pub fn media_bytes(chat_id: i64, seq: i64) -> anyhow::Result<Option<Vec<u8>>> {
    session::with(|s| Ok(s.media_bytes(chat_id, seq)?))
}

/// Decrypts a message's file to `dest`, to play or open it. False until it
/// has arrived.
pub fn media_export(chat_id: i64, seq: i64, dest: String) -> anyhow::Result<bool> {
    session::with(|s| Ok(s.media_export(chat_id, seq, std::path::Path::new(&dest))?))
}

pub struct ReplyView {
    /// "You", or the author's name.
    pub author: String,
    pub from_me: bool,
    pub snippet: String,
    /// Where the quoted message is in this chat, if it's still there.
    pub seq: Option<i64>,
}

/// A fresh 32-byte storage key; Dart keeps it in Keystore-backed storage.
pub fn store_new_key() -> Vec<u8> {
    rand::random::<[u8; 32]>().to_vec()
}

pub fn chat_list() -> anyhow::Result<Vec<StoredChat>> {
    with(|s| {
        Ok(s.chats()?
            .into_iter()
            .map(|(id, m)| StoredChat {
                id,
                kind: match m.kind {
                    ChatKind::Notes => ChatKindView::Notes,
                    ChatKind::Direct => ChatKindView::Direct,
                    ChatKind::Group => ChatKindView::Group,
                },
                title: m.title,
                sigil_key: m.sigil_key,
                pinned: m.pinned,
                created_at: m.created_at as i64,
                verified: m.peers.iter().all(|p| p.verified),
                waiting: m.kind == ChatKind::Direct && m.group_id.is_empty(),
                hidden: m.hidden,
                members: m.peers.len() as u32,
                active: m.kind != ChatKind::Group || !m.group_id.is_empty(),
            })
            .collect())
    })
}

pub fn chat_create(kind: ChatKindView, title: String, sigil_key: Vec<u8>) -> anyhow::Result<i64> {
    let kind = match kind {
        ChatKindView::Notes => ChatKind::Notes,
        ChatKindView::Direct => ChatKind::Direct,
        ChatKindView::Group => ChatKind::Group,
    };
    with(|s| {
        Ok(s.create_chat(&ChatMeta {
            kind,
            title,
            sigil_key,
            pinned: false,
            created_at: now(),
            group_id: Vec::new(),
            peers: Vec::new(),
            hidden: false,
            admin: None,
        })?)
    })
}

pub fn chat_set_pinned(id: i64, pinned: bool) -> anyhow::Result<()> {
    with(|s| {
        let (_, mut meta) = s
            .chats()?
            .into_iter()
            .find(|(c, _)| *c == id)
            .ok_or_else(|| anyhow::anyhow!("no such chat"))?;
        meta.pinned = pinned;
        Ok(s.update_chat(id, &meta)?)
    })
}

/// Renames a chat (for a friend: what you call them, on this phone only).
pub fn chat_rename(id: i64, name: String) -> anyhow::Result<()> {
    session::with(|s| Ok(s.rename_chat(id, &name)?))
}

fn reach(peers: Vec<[u8; 32]>) {
    for peer in peers {
        session::poke(Some(peer));
    }
}

/// Deletes a message here, or (our own) on every phone in the chat.
pub fn message_delete(chat_id: i64, seq: i64, for_everyone: bool) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.delete_message(chat_id, seq, for_everyone)?))?);
    Ok(())
}

/// Clears a chat's messages here, or on both phones (friends' chats).
pub fn chat_clear(chat_id: i64, for_everyone: bool) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.clear_chat(chat_id, for_everyone)?))?);
    Ok(())
}

/// Removes a friend from this phone, or from both (their phone deletes its
/// chat with us too).
pub fn friend_remove(chat_id: i64, both_phones: bool) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.remove_friend(chat_id, both_phones)?))?);
    Ok(())
}

/// Hides a chat from the list (or shows it again).
pub fn chat_set_hidden(id: i64, hidden: bool) -> anyhow::Result<()> {
    session::with(|s| Ok(s.set_hidden(id, hidden)?))
}

pub fn chat_delete(id: i64) -> anyhow::Result<()> {
    session::with(|s| Ok(s.delete_chat(id)?))
}

/// Saves a message and sends it to the friend (over Tor) if this is a
/// friend's chat.
pub fn message_send(chat_id: i64, body: String, reply_to: Option<i64>) -> anyhow::Result<()> {
    let peers = session::with(|s| Ok(s.send_to_chat(chat_id, &body, reply_to)?))?;
    for peer in peers {
        session::poke(Some(peer));
    }
    Ok(())
}

/// Messages in order; `limit` keeps only the newest.
pub fn message_list(chat_id: i64, limit: Option<u32>) -> anyhow::Result<Vec<MessageView>> {
    session::with(|s| {
        let me = s.public_key();
        let meta = s.chat(chat_id)?;
        let name_of = |key: &[u8; 32]| {
            meta.peers
                .iter()
                .find(|p| &p.identity == key)
                .map_or_else(|| "Someone".to_string(), |p| p.name.clone())
        };
        let messages = s.store.messages_indexed(chat_id, limit)?;
        let seq_of: std::collections::HashMap<[u8; 16], i64> = messages
            .iter()
            .filter_map(|(seq, m)| m.id.map(|id| (id, *seq)))
            .collect();
        let group = meta.kind == ChatKind::Group;
        Ok(messages
            .into_iter()
            .map(|(seq, m)| MessageView {
                seq,
                from_me: m.from_me,
                sent_at: m.sent_at as i64,
                delivered: m.delivered,
                reply: m.reply.map(|q| ReplyView {
                    author: if q.author == me { "You".into() } else { name_of(&q.author) },
                    from_me: q.author == me,
                    seq: seq_of.get(&q.id).copied().filter(|_| q.id != [0; 16]),
                    snippet: q.snippet,
                }),
                sender_name: m.sender.filter(|_| group && !m.from_me).map(|k| name_of(&k)),
                media: m.media.map(|media| MediaView {
                    ready: s.media_ready(&media),
                    kind: match media.kind {
                        MediaKind::Image => MediaKindView::Image,
                        MediaKind::Video => MediaKindView::Video,
                        MediaKind::Audio => MediaKindView::Audio,
                        MediaKind::File => MediaKindView::File,
                    },
                    mime: media.mime,
                    name: media.name,
                    size: media.size,
                    width: media.width,
                    height: media.height,
                    duration_ms: media.duration_ms,
                    thumb: media.thumb,
                }),
                poll: m.poll.map(|p| super::poll::view(p, &m.votes, &me, |k| name_of(k))),
                album: m.album.map(|a| AlbumView {
                    id: a.id.to_vec(),
                    title: a.title,
                }),
                body: m.body,
            })
            .collect())
    })
}

fn with<T>(f: impl FnOnce(&Store) -> anyhow::Result<T>) -> anyhow::Result<T> {
    session::with(|s| f(&s.store))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
