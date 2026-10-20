//! Transports. Privacy rule: **no transport ever exposes an IP address to a
//! peer or to the UI.** That is enforced by the type system: [`PeerAddress`]
//! has no variant that can hold an IP.
//!
//! - Internet: Tor onion services via Arti. Every device hosts its own onion
//!   service, so there is no Sanctum server and nobody (peers, ISPs, relays)
//!   learns who talks to whom or anyone's IP.
//! - Wi-Fi Direct / Bluetooth: local-only, used for temp chats.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransportKind {
    Internet,
    WifiDirect,
    Bluetooth,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerAddress {
    /// v3 onion address derived from the peer's identity key.
    Onion(String),
    /// Ephemeral local session id; rotated per session, never a MAC address.
    Nearby { kind: TransportKind, session: [u8; 16] },
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("peer unreachable")]
    Unreachable,
    #[error("transport unavailable: {0}")]
    Unavailable(&'static str),
}

/// Implemented by the Tor and nearby transports. Payloads are already
/// end-to-end encrypted (MLS) before they get here.
pub trait Transport: Send + Sync {
    fn kind(&self) -> TransportKind;
    fn send(&self, to: &PeerAddress, ciphertext: &[u8]) -> Result<(), TransportError>;
}
