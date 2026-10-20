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
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let body = postcard::to_stdvec(frame).expect("serializable");
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

/// Takes one complete frame off the front of `buf`, if there is one.
/// `Err` means the peer is sending garbage; drop the connection.
pub fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Frame>, ()> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(());
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let frame = postcard::from_bytes(&buf[4..4 + len]).map_err(|_| ())?;
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
