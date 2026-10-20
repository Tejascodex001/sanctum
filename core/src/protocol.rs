//! What two friends' phones say to each other over a connection.
//!
//! A connection is a byte stream (a Tor onion connection in the app; an
//! in-memory pipe in tests). It starts with a handshake: the side that was
//! reached sends a random [`Frame::Challenge`], and the side that connected
//! answers with [`Frame::Hello`], signing the challenge and the reached
//! side's key with its identity key. The reached side is already
//! authenticated by Tor itself: its onion address is its identity key.
//!
//! After that, frames flow both ways. Everything a person writes travels
//! inside MLS messages ([`Frame::Mls`]) as [`Content`]; Sanctum frames only
//! carry routing, acknowledgements and the MLS setup.

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
    let body = postcard::to_stdvec(frame).expect("serializable");
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
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
