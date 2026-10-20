//! Friend codes: who you are, small enough to scan instantly or type.
//!
//! A code holds your identity key and name. Two friends each read the
//! other's code (QR in person, or shared and pasted from afar), and both
//! phones derive the same pair sigil and safety number from the two keys.
//! Matching sigils (in person) or a matching number read aloud on a call
//! proves nobody swapped a code. Encrypted sessions are then set up over
//! Tor on first contact, authenticated by these keys; the identity key is
//! also the onion service key, so a code is all a friend needs to reach you.

use crate::base32;
use serde::{Deserialize, Serialize};

/// Every code starts with this; the rest is Crockford Base32.
pub const PREFIX: &str = "SANCTUM1:";

/// Names longer than this are cut, to keep codes short.
const MAX_NAME_BYTES: usize = 40;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InviteError {
    #[error("not a Sanctum code")]
    NotSanctum,
    #[error("this code is damaged or incomplete")]
    Malformed,
    #[error("this code has a typo")]
    Typo,
}

/// Version byte + identity + name length byte (names are at most 40 bytes,
/// so the length always fits one varint byte).
const HEADER: usize = 1 + 32 + 1;
const CHECKSUM: usize = 2;

fn checksum(bytes: &[u8]) -> [u8; CHECKSUM] {
    let h = blake3::derive_key("sanctum friend code checksum v1", bytes);
    [h[0], h[1]]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FriendCode {
    pub identity: [u8; 32],
    pub name: String,
}

impl FriendCode {
    pub fn new(identity: [u8; 32], name: &str) -> Self {
        let mut name = name.trim().to_string();
        while name.len() > MAX_NAME_BYTES {
            name.pop();
        }
        Self { identity, name }
    }

    pub fn encode(&self) -> String {
        let mut bytes = postcard::to_stdvec(&(1u8, self)).expect("serializable");
        bytes.extend(checksum(&bytes));
        format!("{PREFIX}{}", base32::encode(&bytes))
    }

    /// Finds and decodes a code in text that may be grouped with spaces,
    /// typed in lowercase, or surrounded by a shared message.
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        // Someone typing a code may leave out the prefix; the checksum
        // still tells a real code from random text.
        let body = match compact.find(PREFIX) {
            Some(start) => &compact[start + PREFIX.len()..],
            None if compact.len() >= 20 => compact.as_str(),
            None => return Err(InviteError::NotSanctum),
        };
        // The header says how long the code is, so anything after it (the
        // rest of a shared message) is ignored rather than misread.
        let header = base32::decode(body, HEADER).ok_or(InviteError::Malformed)?;
        let name_len = header[HEADER - 1] as usize;
        if header[0] != 1 || name_len > MAX_NAME_BYTES {
            return Err(InviteError::Malformed);
        }
        let len = HEADER + name_len + CHECKSUM;
        let bytes = base32::decode(body, len).ok_or(InviteError::Malformed)?;
        let (payload, sum) = bytes.split_at(len - CHECKSUM);
        if checksum(payload) != sum {
            return Err(InviteError::Typo);
        }
        match postcard::from_bytes::<(u8, FriendCode)>(payload) {
            Ok((1, code)) => Ok(code),
            _ => Err(InviteError::Malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn roundtrip_and_short() {
        let id = Identity::generate();
        let code = FriendCode::new(id.public_key(), "Tejas");
        let text = code.encode();
        assert!(text.starts_with(PREFIX));
        assert!(text.len() < 80, "code is {} chars: {text}", text.len());
        assert_eq!(FriendCode::decode(&text).unwrap(), code);
    }

    #[test]
    fn survives_display_typing_and_sharing() {
        let code = FriendCode::new(Identity::generate().public_key(), "Ana");
        let text = code.encode();
        let grouped: String = text
            .chars()
            .collect::<Vec<_>>()
            .chunks(5)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        assert_eq!(FriendCode::decode(&grouped).unwrap(), code);
        let shared = format!("Add me on Sanctum:\n\n{text}\nSent from my phone");
        assert_eq!(FriendCode::decode(&shared).unwrap(), code);
        let without_prefix = &grouped["sanct um1:".len()..];
        assert_eq!(FriendCode::decode(without_prefix).unwrap(), code);
        assert!(FriendCode::decode("this is a perfectly ordinary sentence").is_err());
    }

    #[test]
    fn typos_are_caught() {
        let text = FriendCode::new(Identity::generate().public_key(), "Kai").encode();
        let mut caught = 0;
        // Change one character at each position after the prefix.
        for i in PREFIX.len()..text.len() {
            let mut chars: Vec<char> = text.chars().collect();
            chars[i] = if chars[i] == '7' { '8' } else { '7' };
            let typo: String = chars.into_iter().collect();
            if FriendCode::decode(&typo).is_err() {
                caught += 1;
            }
        }
        let positions = text.len() - PREFIX.len();
        // A 16-bit checksum misses about 1 in 65,536 typos.
        assert!(caught >= positions - 1, "caught {caught} of {positions}");
    }

    #[test]
    fn rejects_junk_and_truncation() {
        let text = FriendCode::new(Identity::generate().public_key(), "Bo").encode();
        assert_eq!(FriendCode::decode("hello"), Err(InviteError::NotSanctum));
        assert!(FriendCode::decode(&text[..text.len() - 6]).is_err());
        assert_eq!(
            FriendCode::decode("SANCTUM1:@@@"),
            Err(InviteError::Malformed)
        );
    }

    #[test]
    fn long_names_are_cut() {
        let code = FriendCode::new([1; 32], &"x".repeat(200));
        assert_eq!(code.name.len(), MAX_NAME_BYTES);
    }
}
