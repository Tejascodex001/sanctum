//! Long-term device identity. The public key *is* the user's address; there
//! are no phone numbers or usernames.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;

/// 32-byte BLAKE3 digest of a public key. Drives the sigil and verification.
pub type Fingerprint = [u8; 32];

pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&secret),
        }
    }

    pub fn secret(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint(&self.public_key())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }
}

pub fn fingerprint(public_key: &[u8; 32]) -> Fingerprint {
    blake3::derive_key("sanctum identity fingerprint v1", public_key)
}

/// Fingerprint for a 1:1 chat, identical on both sides regardless of order.
pub fn pair_fingerprint(a: &[u8; 32], b: &[u8; 32]) -> Fingerprint {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(lo);
    buf[32..].copy_from_slice(hi);
    blake3::derive_key("sanctum pair fingerprint v1", &buf)
}

/// A 12-digit number two friends read to each other (e.g. on a call) to
/// confirm nobody swapped their codes. Derived from the pair fingerprint,
/// so it is the same on both phones. Formatted as "1234 5678 9012".
pub fn safety_number(pair_fp: &[u8]) -> String {
    let h = blake3::derive_key("sanctum safety number v1", pair_fp);
    let n = u64::from_le_bytes(h[..8].try_into().unwrap()) % 1_000_000_000_000;
    let d = format!("{n:012}");
    format!("{} {} {}", &d[0..4], &d[4..8], &d[8..12])
}

pub fn verify(public_key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(public_key)
        .map(|vk| vk.verify(msg, &Signature::from_bytes(sig)).is_ok())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let id = Identity::generate();
        let sig = id.sign(b"hello");
        assert!(verify(&id.public_key(), b"hello", &sig));
        assert!(!verify(&id.public_key(), b"hell0", &sig));
    }

    #[test]
    fn safety_number_is_shared_and_formatted() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let ab = safety_number(&pair_fingerprint(&a.public_key(), &b.public_key()));
        let ba = safety_number(&pair_fingerprint(&b.public_key(), &a.public_key()));
        assert_eq!(ab, ba);
        assert_eq!(ab.len(), 14);
        assert!(ab.chars().all(|c| c.is_ascii_digit() || c == ' '));
        let c = Identity::generate();
        assert_ne!(
            ab,
            safety_number(&pair_fingerprint(&a.public_key(), &c.public_key()))
        );
    }

    #[test]
    fn pair_fingerprint_is_symmetric() {
        let (a, b) = (Identity::generate(), Identity::generate());
        assert_eq!(
            pair_fingerprint(&a.public_key(), &b.public_key()),
            pair_fingerprint(&b.public_key(), &a.public_key())
        );
    }
}
