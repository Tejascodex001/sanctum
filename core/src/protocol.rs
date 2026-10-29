//! What two friends' phones say to each other over a connection.
//!
//! A connection is a byte stream (a Tor onion connection in the app; an
//! in-memory pipe in tests). It starts with a handshake: the side that was
//! reached sends a random [`Frame::Challenge`], and the side that connected
//! answers with [`Frame::Hello`], signing the challenge and the reached
//! side's key with its identity key. The reached side is already
//! authenticated by Tor itself: its onion address is its identity key.
//! Both handshake frames also carry what the sender's build can do
//! ([`FEATURES`]), tucked after the frame where older builds don't look.
//!
//! After that, frames flow both ways. Everything a person writes travels
//! inside MLS messages ([`Frame::Mls`]) as [`Content`]; Sanctum frames only
//! carry routing, acknowledgements, the MLS setup, the passing "typing"
//! signal (which holds no text), calls' sealed sound
//! ([`Frame::CallMedia`]) and group members' lists of what they have
//! ([`Frame::GroupHave`]: hashes and ids, never content).

use serde::{Deserialize, Serialize};

/// Largest frame accepted; bigger ones end the connection.
pub const MAX_FRAME: usize = 256 * 1024;

/// A delivery's id, for acknowledgement and de-duplication.
pub type MessageId = [u8; 16];

/// A temp chat's id, shared by both phones.
pub type TempId = [u8; 16];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    Challenge {
        nonce: [u8; 32],
    },
    Hello {
        from: [u8; 32],
        signature: Vec<u8>,
    },
    /// Setting up the pair's MLS group. The friend with the smaller key
    /// asks for a key package and sends back the welcome.
    KeyPackageRequest,
    KeyPackage(Vec<u8>),
    Welcome(Vec<u8>),
    /// An MLS message for the pair's group, carrying [`Content`].
    Mls {
        id: MessageId,
        bytes: Vec<u8>,
    },
    Ack {
        id: MessageId,
    },
    /// "Add me back": sent first to a friend we haven't set up encryption
    /// with yet. A phone that doesn't know us shows it as a friend request;
    /// the hello before it already proved who it's from.
    FriendRequest {
        name: String,
    },
    /// Keeps a link alive.
    Ping,
    /// Groups. The admin asks a friend it's adding for a key package...
    GroupKeyPackageRequest {
        group: Vec<u8>,
    },
    /// ...gets one...
    GroupKeyPackage {
        group: Vec<u8>,
        key_package: Vec<u8>,
    },
    /// ...and sends the welcome that lets them join, with the group's name
    /// and members. Acknowledged like a message.
    GroupWelcome {
        id: MessageId,
        welcome: Vec<u8>,
        info: GroupInfo,
    },
    /// An MLS message (a commit, or [`Content`]) for a group, sent to each
    /// member separately.
    GroupMls {
        group: Vec<u8>,
        id: MessageId,
        bytes: Vec<u8>,
    },
    /// A piece of an attachment, already encrypted with the attachment's
    /// own key (which travels inside MLS, in [`Media`]). The last piece is
    /// acknowledged with `id`.
    BlobChunk {
        id: MessageId,
        blob: BlobId,
        index: u32,
        total: u32,
        bytes: Vec<u8>,
    },
    /// "I'm typing" in a chat. Sent only over a link that is up right now,
    /// never stored, queued or resent, and it carries no text. `group`
    /// names a group chat (its MLS group id, as [`Frame::GroupMls`] does);
    /// empty means the pair's own chat. It stays out of MLS on purpose: a
    /// dropped signal must not leave a gap in the sender's MLS counter.
    Typing {
        group: Vec<u8>,
    },
    /// A piece of a call's sound (later also video), sealed with the call's
    /// own key (see `call`). Like [`Frame::Typing`], it goes only over a
    /// link that is up right now and is never stored, queued or resent; the
    /// receiver takes it only from the friend in the call. `seq` counts
    /// this direction's packets: it makes each packet's nonce and catches
    /// replays.
    CallMedia {
        call: CallId,
        seq: u64,
        sealed: Vec<u8>,
    },
    /// Members help deliver (see `group_sync`): "these are the recent
    /// messages and files of this group that I have". Sent only to a
    /// current member of the group whose phone understands it (see
    /// [`FEATURES`]). An entry with `owed` is one we haven't delivered to
    /// them yet: they confirm it with [`Frame::Ack`] if they already have
    /// it (or can't use it), and ask for it otherwise.
    GroupHave {
        group: Vec<u8>,
        ids: Vec<HaveEntry>,
        blobs: Vec<HaveBlob>,
    },
    /// The answer to [`Frame::GroupHave`]: "send me these". The messages
    /// come back as ordinary [`Frame::GroupMls`]. Also sent with no ids, to
    /// say an offer was heard.
    GroupWant {
        group: Vec<u8>,
        ids: Vec<SyncId>,
    },
    /// "Send me this group's file, from piece `from` on." It comes back as
    /// [`Frame::BlobChunk`]s, checked against the message that announced
    /// the file.
    BlobWant {
        group: Vec<u8>,
        blob: BlobId,
        from: u32,
    },
}

/// A group message as members know it: a hash of its sealed bytes, the
/// same on every phone (see `group_sync::sync_id`).
pub type SyncId = [u8; 16];

/// One message in a [`Frame::GroupHave`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaveEntry {
    pub id: SyncId,
    /// The group's MLS epoch it was sent in: a phone that joined later, or
    /// has moved on too far, doesn't ask for what it can't read.
    pub epoch: u64,
    /// Our outbox id, if we still owe it to the phone we tell.
    pub owed: Option<MessageId>,
}

/// One file in a [`Frame::GroupHave`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaveBlob {
    pub blob: BlobId,
    /// Our outbox id, if we still owe it to the phone we tell.
    pub owed: Option<MessageId>,
    /// "Fetch it from me now." The original sender says no once a few
    /// members have it, so the rest fetch it from them.
    pub ask: bool,
}

/// What this build can do, told to the other phone with the handshake
/// (see [`encode_frame_with_features`]).
pub const FEATURES: u32 = FEATURE_GROUP_SYNC;
/// Understands [`Frame::GroupHave`], [`Frame::GroupWant`] and
/// [`Frame::BlobWant`], and passes group messages and files on.
pub const FEATURE_GROUP_SYNC: u32 = 1;
const FEATURES_TAG: &[u8] = b"SNF1";

/// A call's id, made by the caller.
pub type CallId = [u8; 16];

/// "Let's talk": everything the friend's phone needs to ring and, if
/// answered, to talk (see `call`). Room is left for video and direct
/// connections, which later builds can fill in without changing the wire.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CallOffer {
    pub call: CallId,
    /// Seals every media packet of this call, both ways. Fresh per call.
    pub key: [u8; 32],
    /// When the call was started, in milliseconds. Arriving much later, it
    /// doesn't ring: it's a missed call.
    pub sent_at: u64,
    /// A video call. Always false for now.
    pub video: bool,
    /// Sound formats this phone can play, best first ("opus", "amr-wb").
    pub audio_in: Vec<String>,
    /// Sound formats this phone can record into.
    pub audio_out: Vec<String>,
    /// The same for video (empty for now).
    pub video_in: Vec<String>,
    pub video_out: Vec<String>,
    /// Other ways to reach this phone for the media, for a later direct
    /// mode. Empty: only over the friends' own link.
    pub routes: Vec<Vec<u8>>,
}

/// The friend picked up, and chose what each side sends.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CallAnswer {
    pub call: CallId,
    pub video: bool,
    /// The sound format the caller records into, and the one we do.
    pub caller_audio: String,
    pub callee_audio: String,
    /// The same for video (empty for now).
    pub caller_video: String,
    pub callee_video: String,
    /// See [`CallOffer::routes`].
    pub routes: Vec<Vec<u8>>,
}

pub type BlobId = [u8; 16];

/// An attachment, as its message describes it. The file itself follows as
/// [`Frame::BlobChunk`]s.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Media {
    pub blob: BlobId,
    /// Decrypts the chunks.
    pub key: [u8; 32],
    pub kind: MediaKind,
    pub mime: String,
    pub name: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Videos and voice notes.
    pub duration_ms: u32,
    /// A tiny preview (JPEG, a few KB), shown until the file arrives.
    pub thumb: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    #[default]
    File,
}

/// What a group is called and who's in it, as its admin last said.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GroupInfo {
    pub title: String,
    pub sigil_key: Vec<u8>,
    pub admin: [u8; 32],
    /// Everyone, the admin included, with the names the admin knows them by.
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GroupMember {
    pub identity: [u8; 32],
    pub name: String,
}

/// What is inside an MLS application message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Text {
        body: String,
        sent_at: u64,
    },
    /// A temp chat was started; the friend's phone shows it too.
    TempOpen {
        temp: TempId,
    },
    /// A temp message: kept in memory only, gone 24 hours after `sent_at`.
    TempText {
        temp: TempId,
        body: String,
        sent_at: u64,
    },
    /// The temp chat was wiped; wipe it on this phone too.
    TempWipe {
        temp: TempId,
    },
    /// A message answering an earlier one, which it quotes.
    Reply {
        body: String,
        sent_at: u64,
        quote: Quote,
    },
    /// A message in a group. Its id is shared by every member's copy (each
    /// copy travels in its own frame).
    GroupText {
        id: MessageId,
        body: String,
        sent_at: u64,
        quote: Option<Quote>,
    },
    /// From the admin: the group's name and members changed.
    GroupInfo(GroupInfo),
    /// The sender left the group.
    GroupLeave,
    /// Delete for everyone: the sender's message with this id.
    DeleteMessage { id: MessageId },
    /// Clear for everyone: every message in the pair's chat.
    ClearChat,
    /// The sender removed us as a friend, on both phones: delete the chat.
    Unfriend,
    /// The sender's nearby key: lets us recognize their phone's rotating
    /// Bluetooth ID (see `nearby`).
    NearbyKey([u8; 32]),
    /// A photo, video, voice note or file, with an optional caption. Used in
    /// friends' chats and groups alike.
    Media {
        id: MessageId,
        body: String,
        sent_at: u64,
        quote: Option<Quote>,
        media: Media,
    },
    /// A poll, a date poll or a plan, in a friends' chat or a group.
    Poll {
        id: MessageId,
        sent_at: u64,
        poll: Poll,
    },
    /// The sender's answer to a poll, replacing their earlier one. No
    /// choices: they took their vote back. `at` (milliseconds) orders
    /// answers that arrive out of order.
    Vote {
        poll: MessageId,
        choices: Vec<u8>,
        at: u64,
    },
    /// A photo or video added to one of the chat's shared albums.
    AlbumMedia {
        id: MessageId,
        body: String,
        sent_at: u64,
        media: Media,
        album: Album,
    },
    /// The sender's photo of the day (see `moment`); the file follows.
    Moment {
        id: MessageId,
        sent_at: u64,
        caption: String,
        media: Media,
    },
    /// The sender took their moment down.
    MomentGone { id: MessageId },
    /// The sender's reaction to the message with this id, replacing their
    /// earlier one. An empty `emoji` takes it back. `at` (milliseconds)
    /// orders reactions that arrive out of order, as for [`Content::Vote`].
    React {
        id: MessageId,
        emoji: String,
        at: u64,
    },
    /// The sender is calling us (friends' 1:1 chats only).
    CallInvite(CallOffer),
    /// The sender picked up our call.
    CallAnswer(CallAnswer),
    /// The call is over, or never started: `reason` says why (see
    /// `call::EndReason`; an unknown value counts as a plain hang-up).
    CallEnd { call: CallId, reason: u8 },
    /// From the group's leader: who leads it and who its admins are. Only
    /// the leader's word counts; `at` (milliseconds) lets a late copy of an
    /// older list be told from the newest.
    GroupRoles { leader: [u8; 32], admins: Vec<[u8; 32]>, at: u64 },
    /// From an admin to the group: "please do this". Only the leader acts
    /// on it (the leader alone changes who is in the group), and only if
    /// the sender is an admin.
    GroupRequest(GroupRequest),
    /// Sent by the leader to someone just added, over their own chat (not
    /// to the group): recent messages from before they joined.
    HistoryBatch { group: Vec<u8>, items: Vec<HistoryItem> },
    /// To the whole group: the leader passed recent messages on to this
    /// member, so nobody is surprised.
    HistoryShared { with: [u8; 32] },
    /// The sender changed the words of their message with this id (its
    /// text, or a photo's caption). `at` (milliseconds) orders edits that
    /// arrive out of order, as for [`Content::React`].
    Edit { id: MessageId, body: String, at: u64 },
    /// Sent right after [`Content::Moment`]: how friends may look at it.
    /// `seconds`: how long each look lasts (0: as long as they like).
    /// `once`: each friend can open it one time. Older builds ignore it and
    /// show the moment as before.
    MomentRules { id: MessageId, seconds: u8, once: bool },
    /// To a moment's owner: the sender looked at it.
    MomentSeen { id: MessageId },
}

/// What an admin asks of the leader (see [`Content::GroupRequest`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GroupRequest {
    /// Add this friend of the leader's, optionally passing on recent
    /// messages.
    Add { who: [u8; 32], history: bool },
    Remove { who: [u8; 32] },
    Rename { title: String },
    /// Take admin rights from this admin (the leader's can't be taken).
    Demote { who: [u8; 32] },
}

/// One earlier text message of a group, passed on to a new member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: MessageId,
    pub sender: [u8; 32],
    pub sent_at: u64,
    pub body: String,
}

/// A shared album: the photos and videos in a chat that carry its id.
/// Whoever adds the first one creates it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Album {
    pub id: MessageId,
    pub title: String,
}

/// A question with choices. A date poll's choices are times (`dates`); a
/// plan's are "Going", "Maybe" and "Can't go".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Poll {
    pub question: String,
    /// The choices' labels (for a date poll the app shows `dates` instead).
    pub options: Vec<String>,
    /// Date polls: each choice's time, in Unix seconds.
    pub dates: Vec<u64>,
    /// More than one choice allowed.
    pub multi: bool,
    pub plan: Option<Plan>,
}

/// Something to do together: when and where.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Unix seconds.
    pub when: u64,
    pub place: String,
}

/// The message a reply answers: enough to show it even if it's gone.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Quote {
    /// The quoted message's id (zero for messages from before replies).
    pub id: MessageId,
    /// Who wrote it.
    pub author: [u8; 32],
    /// Its start, up to 140 characters.
    pub snippet: String,
}

impl Content {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("serializable")
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// What the connecting side signs in its [`Frame::Hello`]: domain label,
/// the reached side's key and the challenge, so a signature can't be
/// replayed to anyone else or reused later.
pub fn hello_message(reached: &[u8; 32], nonce: &[u8; 32]) -> Vec<u8> {
    [b"sanctum hello v1".as_slice(), reached, nonce].concat()
}

/// A frame on the wire: 4-byte big-endian length, then postcard bytes.
impl Frame {
    /// Unframed bytes, for keeping a frame in the outbox.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("serializable")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    frame_with(postcard::to_stdvec(frame).expect("serializable"))
}

fn frame_with(body: Vec<u8>) -> Vec<u8> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

/// A handshake frame ([`Frame::Challenge`] or [`Frame::Hello`]) followed,
/// inside the same frame, by what this build can do. Older builds read
/// the frame and ignore the bytes after it (postcard stops where the
/// frame ends), so they keep working; newer ones learn the features with
/// [`take_frame_features`].
pub fn encode_frame_with_features(frame: &Frame, features: u32) -> Vec<u8> {
    let mut body = postcard::to_stdvec(frame).expect("serializable");
    body.extend_from_slice(FEATURES_TAG);
    body.extend(postcard::to_stdvec(&features).expect("serializable"));
    frame_with(body)
}

/// The peer sent something that isn't a valid frame; drop the connection.
#[derive(Debug, thiserror::Error)]
#[error("malformed frame")]
pub struct BadFrame;

/// Takes one complete frame off the front of `buf`, if there is one.
pub fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Frame>, BadFrame> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(BadFrame);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let frame = postcard::from_bytes(&buf[4..4 + len]).map_err(|_| BadFrame)?;
    buf.drain(..4 + len);
    Ok(Some(frame))
}

/// Like [`take_frame`], also reading the features an
/// [`encode_frame_with_features`] frame carries (0 from older builds).
pub fn take_frame_features(buf: &mut Vec<u8>) -> Result<Option<(Frame, u32)>, BadFrame> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(BadFrame);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let (frame, rest) = postcard::take_from_bytes::<Frame>(&buf[4..4 + len]).map_err(|_| BadFrame)?;
    let features = rest
        .strip_prefix(FEATURES_TAG)
        .and_then(|f| postcard::take_from_bytes::<u32>(f).ok())
        .map_or(0, |(f, _)| f);
    buf.drain(..4 + len);
    Ok(Some((frame, features)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_survive_being_split_and_joined() {
        let frames = vec![
            Frame::Challenge { nonce: [1; 32] },
            Frame::Mls {
                id: [2; 16],
                bytes: vec![3; 1000],
            },
            Frame::Ack { id: [2; 16] },
        ];
        let wire: Vec<u8> = frames.iter().flat_map(encode_frame).collect();
        // Feed it one byte at a time.
        let (mut buf, mut out) = (Vec::new(), Vec::new());
        for b in wire {
            buf.push(b);
            while let Some(f) = take_frame(&mut buf).unwrap() {
                out.push(f);
            }
        }
        assert_eq!(out, frames);
        assert!(buf.is_empty());
    }

    #[test]
    fn oversized_or_garbage_frames_are_rejected() {
        let mut huge = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        assert!(take_frame(&mut huge).is_err());
        let mut junk = vec![0, 0, 0, 2, 0xFF, 0xFF];
        assert!(take_frame(&mut junk).is_err());
    }

    /// New variants go at the end, so older phones' frames and contents
    /// keep their numbers on the wire.
    #[test]
    fn new_variants_come_last() {
        assert_eq!(Frame::Typing { group: Vec::new() }.to_bytes()[0], 14);
        let chunk = Frame::BlobChunk { id: [0; 16], blob: [0; 16], index: 0, total: 1, bytes: Vec::new() };
        assert_eq!(chunk.to_bytes()[0], 13);
        let react = Content::React { id: [1; 16], emoji: "👍".into(), at: 2 };
        assert_eq!(react.encode()[0], 18);
        assert_eq!(Content::decode(&react.encode()), Some(react));
        assert_eq!(Content::MomentGone { id: [0; 16] }.encode()[0], 17);
        let media = Frame::CallMedia { call: [0; 16], seq: 1, sealed: Vec::new() };
        assert_eq!(media.to_bytes()[0], 15);
        assert_eq!(Content::CallInvite(CallOffer::default()).encode()[0], 19);
        assert_eq!(Content::CallAnswer(CallAnswer::default()).encode()[0], 20);
        let end = Content::CallEnd { call: [3; 16], reason: 1 };
        assert_eq!(end.encode()[0], 21);
        assert_eq!(Content::decode(&end.encode()), Some(end));
        let have = Frame::GroupHave { group: vec![1], ids: Vec::new(), blobs: Vec::new() };
        assert_eq!(have.to_bytes()[0], 16);
        assert_eq!(Frame::GroupWant { group: vec![1], ids: Vec::new() }.to_bytes()[0], 17);
        assert_eq!(Frame::BlobWant { group: vec![1], blob: [0; 16], from: 0 }.to_bytes()[0], 18);
    }

    /// Older builds read a handshake frame that carries features as if it
    /// didn't (they use [`take_frame`]); newer ones read the features.
    #[test]
    fn features_ride_along_with_the_handshake() {
        let hello = Frame::Hello { from: [4; 32], signature: vec![5; 64] };
        let mut old = encode_frame_with_features(&hello, FEATURES);
        old.extend(encode_frame(&Frame::Ping));
        assert_eq!(take_frame(&mut old).unwrap(), Some(hello.clone()));
        assert_eq!(take_frame(&mut old).unwrap(), Some(Frame::Ping));
        assert!(old.is_empty());

        let mut new = encode_frame_with_features(&hello, FEATURES);
        assert_eq!(take_frame_features(&mut new).unwrap(), Some((hello.clone(), FEATURES)));
        let mut from_old = encode_frame(&hello);
        assert_eq!(take_frame_features(&mut from_old).unwrap(), Some((hello, 0)));
        let challenge = Frame::Challenge { nonce: [6; 32] };
        let mut both = encode_frame_with_features(&challenge, FEATURES);
        assert_eq!(take_frame(&mut both).unwrap(), Some(challenge));
    }

    #[test]
    fn content_roundtrip() {
        let c = Content::TempText {
            temp: [5; 16],
            body: "hi".into(),
            sent_at: 9,
        };
        assert_eq!(Content::decode(&c.encode()), Some(c));
        assert_eq!(Content::decode(&[0xFF]), None);
    }
}
