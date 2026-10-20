//! In-person pairing by QR code. No network is involved.
//!
//! 1. Bob shows an [`Offer`]: who he is plus a one-time MLS key package,
//!    signed with his identity key.
//! 2. Alice scans it, creates the chat's MLS group with Bob in it, and shows
//!    an [`Acceptance`]: who she is plus the MLS welcome, signed and bound to
//!    Bob's offer.
//! 3. Bob scans it and joins. Both phones now derive the same pair sigil;
//!    if the two sigils match on screen, nobody swapped a code in between.

use crate::identity::{verify, Identity};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

const PREFIX: &str = "sanctum1:";

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InviteError {
    #[error("not a Sanctum code")]
    NotSanctum,
    #[error("this code is damaged")]
    Malformed,
    #[error("this code's signature is invalid")]
    BadSignature,
    #[error("expected a {expected} code")]
    WrongKind { expected: &'static str },
    #[error("this reply was made for someone else's code")]
    NotForUs,
}

/// Who someone is, as carried in a code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub identity: [u8; 32],
    pub name: String,
    /// Where to reach them over Tor, once known.
    pub onion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub card: Card,
    pub key_package: Vec<u8>,
    /// Unix seconds.
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Acceptance {
    pub card: Card,
    pub welcome: Vec<u8>,
    /// Hash of the offer this answers, so it can't be reused for anyone else.
    pub offer_hash: [u8; 32],
}

#[derive(Serialize, Deserialize)]
enum Body {
    Offer(Offer),
    Acceptance(Acceptance),
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    body: Vec<u8>,
    signature: Vec<u8>,
}

impl Offer {
    pub fn encode(&self, signer: &Identity) -> String {
        encode(signer, &Body::Offer(self.clone()))
    }

    pub fn decode(code: &str) -> Result<Self, InviteError> {
        match decode(code)? {
            Body::Offer(offer) => Ok(offer),
            Body::Acceptance(_) => Err(InviteError::WrongKind { expected: "friend" }),
        }
    }

    /// Identifies this exact offer.
    pub fn hash(&self) -> [u8; 32] {
        blake3::derive_key(
            "sanctum offer v1",
            &postcard::to_stdvec(self).expect("serializable"),
        )
    }
}

impl Acceptance {
    pub fn encode(&self, signer: &Identity) -> String {
        encode(signer, &Body::Acceptance(self.clone()))
    }

    /// Decodes a reply and checks it answers `our_offer`.
    pub fn decode(code: &str, our_offer: &Offer) -> Result<Self, InviteError> {
        match decode(code)? {
            Body::Acceptance(a) if a.offer_hash == our_offer.hash() => Ok(a),
            Body::Acceptance(_) => Err(InviteError::NotForUs),
            Body::Offer(_) => Err(InviteError::WrongKind { expected: "reply" }),
        }
    }
}

fn signed_bytes(body: &[u8]) -> Vec<u8> {
    [b"sanctum invite v1".as_slice(), body].concat()
}

fn encode(signer: &Identity, body: &Body) -> String {
    let body = postcard::to_stdvec(body).expect("serializable");
    let signature = signer.sign(&signed_bytes(&body)).to_vec();
    let envelope = postcard::to_stdvec(&Envelope { body, signature }).expect("serializable");
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(envelope))
}

fn decode(code: &str) -> Result<Body, InviteError> {
    let b64 = code
        .trim()
        .strip_prefix(PREFIX)
        .ok_or(InviteError::NotSanctum)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| InviteError::Malformed)?;
    let envelope: Envelope = from_bytes(&bytes)?;
    let body: Body = from_bytes(&envelope.body)?;
    let signer = match &body {
        Body::Offer(o) => o.card.identity,
        Body::Acceptance(a) => a.card.identity,
    };
    let signature: [u8; 64] = envelope
        .signature
        .try_into()
        .map_err(|_| InviteError::BadSignature)?;
    if !verify(&signer, &signed_bytes(&envelope.body), &signature) {
        return Err(InviteError::BadSignature);
    }
    Ok(body)
}

fn from_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, InviteError> {
    postcard::from_bytes(bytes).map_err(|_| InviteError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: &Identity) -> Offer {
        Offer {
            card: Card {
                identity: id.public_key(),
                name: "Bob".into(),
                onion: None,
            },
            key_package: vec![1, 2, 3],
            created_at: 10,
        }
    }

    #[test]
    fn offer_roundtrip_and_signature() {
        let bob = Identity::generate();
        let code = offer(&bob).encode(&bob);
        assert!(code.starts_with(PREFIX));
        assert_eq!(Offer::decode(&code).unwrap(), offer(&bob));

        // Signed by someone other than the identity it claims.
        let mallory = Identity::generate();
        assert_eq!(
            Offer::decode(&offer(&bob).encode(&mallory)),
            Err(InviteError::BadSignature)
        );
    }

    #[test]
    fn tampering_is_detected() {
        let bob = Identity::generate();
        let code = offer(&bob).encode(&bob);
        let mut bytes = URL_SAFE_NO_PAD.decode(&code[PREFIX.len()..]).unwrap();
        let i = bytes.len() / 3;
        bytes[i] ^= 0x40;
        let tampered = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        assert!(Offer::decode(&tampered).is_err());
        assert_eq!(Offer::decode("hello"), Err(InviteError::NotSanctum));
        assert_eq!(Offer::decode("sanctum1:@@@"), Err(InviteError::Malformed));
    }

    #[test]
    fn acceptance_is_bound_to_its_offer() {
        let (alice, bob, carol) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let bobs = offer(&bob);
        let accept = Acceptance {
            card: Card {
                identity: alice.public_key(),
                name: "Alice".into(),
                onion: None,
            },
            welcome: vec![9],
            offer_hash: bobs.hash(),
        }
        .encode(&alice);
        assert_eq!(
            Acceptance::decode(&accept, &bobs).unwrap().card.name,
            "Alice"
        );
        assert_eq!(
            Acceptance::decode(&accept, &offer(&carol)),
            Err(InviteError::NotForUs)
        );
        assert_eq!(
            Offer::decode(&accept).unwrap_err(),
            InviteError::WrongKind { expected: "friend" }
        );
    }
}
