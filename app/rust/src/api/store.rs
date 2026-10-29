//! Persistent chats, encrypted at rest. See `sanctum_core::store`.

use super::call::CallOutcomeView;
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
    /// Reactions, one entry per emoji, most used first.
    pub reactions: Vec<ReactionView>,
    /// A call's record ("Voice call · 4:12", "Missed call"; the words are
    /// in `body`). `from_me`: we called.
    pub call: Option<CallLogView>,
    /// A line about the group itself ("Sam joined and was shown the last
    /// week"), shown in the middle of the chat rather than as a bubble.
    pub notice: bool,
    /// Earlier group messages the leader showed a newcomer: who passed them on.
    pub shared_by: Option<String>,
    /// Its writer changed the words after sending.
    pub edited: bool,
    /// We can change its words now (ours, and recent enough).
    pub can_edit: bool,
}

pub struct CallLogView {
    pub outcome: CallOutcomeView,
    /// A video call.
    pub video: bool,
    /// How long we talked.
    pub seconds: u32,
}

fn call_log_view(log: sanctum_core::store::CallLog) -> CallLogView {
    use sanctum_core::call::Outcome;
    let outcome = match Outcome::from_wire(log.outcome) {
        Outcome::Talked => CallOutcomeView::Talked,
        Outcome::Missed => CallOutcomeView::Missed,
        Outcome::Declined => CallOutcomeView::Declined,
        Outcome::NoAnswer => CallOutcomeView::NoAnswer,
        Outcome::Unreachable => CallOutcomeView::Unreachable,
        Outcome::Busy => CallOutcomeView::Busy,
        Outcome::Failed => CallOutcomeView::Failed,
        Outcome::Dropped => CallOutcomeView::Dropped,
        Outcome::Cancelled => CallOutcomeView::Cancelled,
    };
    CallLogView { outcome, video: log.video, seconds: log.seconds }
}

/// Everyone who reacted to a message with one emoji.
pub struct ReactionView {
    pub emoji: String,
    pub count: u32,
    /// We reacted with this one (at most one entry is ours).
    pub mine: bool,
    /// Who reacted with it ("You" first).
    pub names: Vec<String>,
}

/// Reactions grouped by emoji: most used first, ties by who was first.
fn reaction_views(
    reactions: &[sanctum_core::store::Reaction],
    me: &[u8; 32],
    name_of: impl Fn(&[u8; 32]) -> String,
) -> Vec<ReactionView> {
    let mut groups: Vec<(&str, u64, Vec<&sanctum_core::store::Reaction>)> = Vec::new();
    for r in reactions.iter().filter(|r| !r.emoji.is_empty()) {
        match groups.iter_mut().find(|(emoji, _, _)| *emoji == r.emoji) {
            Some((_, first, who)) => {
                *first = (*first).min(r.at);
                who.push(r);
            }
            None => groups.push((&r.emoji, r.at, vec![r])),
        }
    }
    groups.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(a.1.cmp(&b.1)));
    groups
        .into_iter()
        .map(|(emoji, _, who)| {
            let mine = who.iter().any(|r| &r.who == me);
            let mut names: Vec<String> = who.iter().filter(|r| &r.who != me).map(|r| name_of(&r.who)).collect();
            names.sort();
            if mine {
                names.insert(0, "You".into());
            }
            ReactionView {
                emoji: emoji.to_string(),
                count: who.len() as u32,
                mine,
                names,
            }
        })
        .collect()
}

/// Reacts to the message at `seq` with `emoji`, replacing our earlier
/// reaction; an empty `emoji` takes it back. Shows on every phone in the
/// chat; nobody is notified.
pub fn message_react(chat_id: i64, seq: i64, emoji: String) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.react(chat_id, seq, &emoji)?))?);
    Ok(())
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
            admins: Vec::new(),
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

/// Changes the words of our message at `seq` (its text or caption), on
/// every phone in the chat. Nobody is notified.
pub fn message_edit(chat_id: i64, seq: i64, body: String) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.edit_message(chat_id, seq, &body)?))?);
    Ok(())
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
        let notes = meta.kind == ChatKind::Notes;
        Ok(messages
            .into_iter()
            .map(|(seq, m)| MessageView {
                seq,
                edited: m.edited_at > 0,
                can_edit: sanctum_core::edit::can_edit_now(&m, notes),
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
                reactions: reaction_views(&m.reactions, &me, |k| name_of(k)),
                album: m.album.map(|a| AlbumView {
                    id: a.id.to_vec(),
                    title: a.title,
                }),
                call: m.call.map(call_log_view),
                notice: m.notice,
                shared_by: m.shared_by.map(|k| name_of(&k)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use sanctum_core::store::Reaction;

    #[test]
    fn reactions_are_grouped_by_emoji() {
        let r = |who: u8, emoji: &str, at| Reaction { who: [who; 32], emoji: emoji.into(), at };
        let reactions = [r(1, "👍", 5), r(2, "❤️", 1), r(3, "👍", 7), r(4, "", 2), r(9, "❤️", 3), r(5, "😂", 0)];
        let views = reaction_views(&reactions, &[9; 32], |k| format!("P{}", k[0]));
        let summary: Vec<(&str, u32, bool, Vec<String>)> =
            views.iter().map(|v| (v.emoji.as_str(), v.count, v.mine, v.names.clone())).collect();
        assert_eq!(
            summary,
            [
                ("❤️", 2, true, vec!["You".to_string(), "P2".into()]),
                ("👍", 2, false, vec!["P1".into(), "P3".into()]),
                ("😂", 1, false, vec!["P5".into()]),
            ]
        );
    }
}
