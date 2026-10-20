# Sanctum — Design

Private, serverless chat for a circle of friends (target: up to ~200 people).
Android first; iOS later.

## Principles
1. **No servers, no tracking.** No accounts, phone numbers, analytics or crash reporters.
2. **No IP addresses, ever.** Peers, the UI and logs never see an IP (enforced in `core/src/transport.rs`).
3. **Encrypted everywhere.** End-to-end in transit (MLS) and at rest (SQLCipher).
4. **Invite-only.** You join by scanning a friend's QR code in person.

## Architecture
```
Flutter UI (Sigil + Deck)
   │  flutter_rust_bridge
Rust core (sanctum-core)
   ├─ identity   Ed25519 key = your address; BLAKE3 fingerprints
   ├─ sigil      generative art from key fingerprints (also used for verification)
   ├─ mls        OpenMLS (RFC 9420) groups; 1:1 is a 2-member group. Identity key = MLS signing key
   ├─ store      SQLite with every row sealed (XChaCha20-Poly1305); key in Android Keystore
   ├─ temp_chat  in-memory only, 24h TTL, wipe anytime
   └─ transport  Internet: Tor onion service per device (Arti)          [todo]
                 Nearby: Wi-Fi Direct + Bluetooth LE (temp chats)        [todo]
```

### Delivery without a server
- Each phone runs its own Tor onion service; its onion address is derived from its identity key.
- A foreground service keeps it reachable (no Google push, so Google never sees activity).
- If a recipient is offline, online group members hold the encrypted message and pass it on later (store-and-forward). They can't read it.
- Trade-offs: messages may take a few seconds longer; the app uses some battery; delivery is slow if nobody in the group is online.

## Encryption
- **In transit:** MLS with ciphersuite X25519 / ChaCha20-Poly1305 / SHA-256 / Ed25519. Forward secrecy and post-compromise security from MLS ratchets. Messages padded to 64-byte blocks to hide length. A member's MLS identity must equal their signing key, so nobody can impersonate a friend inside a group. Tampered or replayed messages are rejected.
- **At rest:** every chat, message and MLS secret is sealed before SQLite sees it, with the row's identity bound as associated data so rows can't be swapped. Lookup keys for MLS state are keyed hashes, so group ids never appear on disk. `secure_delete` zeroes deleted data. Temp chats never touch disk.
- MLS state is persisted after every operation; a crash before persisting just rolls back to the last saved state.

## Pairing (in person, no network)
1. Bob shows a QR **offer**: identity key, name, a one-time MLS key package, signed.
2. Alice scans it, creates the chat's MLS group with Bob in it, and shows a QR **reply**: her identity, name and the MLS welcome, signed and bound to Bob's offer.
3. Bob scans the reply and joins. Both phones derive the same **pair sigil**; if they match on screen, no code was swapped.
QRs carry the codes as raw bytes (QR version ~14 for the offer, ~22 for the reply); the scanner analyses 85% of a high-resolution frame. QR scanning uses ZXing on-device (no Google ML Kit, which reports usage to Google).

**Far away:** the same two codes are shared through any app and pasted instead of scanned. Because that app could in theory swap them, both phones show a 12-digit **safety number** derived from the pair fingerprint, read aloud on a call to confirm. (With Tor, short spoken invite words over a direct onion connection can replace copy-paste.)

## Security features
- App lock with biometric or PIN; locks again when the app goes to the background.
- Screenshots blocked by default (`FLAG_SECURE`); can be allowed per chat after a biometric check.
- Hidden chats behind a second PIN; a decoy PIN; a panic-wipe gesture.
- Verification: both people see the same sigil for their chat. If the sigils match, the chat is secure.

## UI: Sigil + Deck
- **Sigil:** each chat's emblem is generated in Rust from its key fingerprint and drawn as crisp vector art (no blur). Six families (rosette, constellation, shards, orbits, lattice, rune), four colour harmonies, 3–12-fold symmetry and varying density and stroke weight, so two keys almost never look alike. The sigil supplies the chat's colours, card face, header emblem and slowly turning chat backdrop.
- **Deck:** chats are a stack of overlapping cards (lazy list, sigils painted once and cached), pinned first, then most recent. Search filters by name and last message. Swipe between Chats and Temp; the tab pill follows the swipe.
- **Card:** emblem, name, last message, time, and the sigil as a large cropped watermark. Temp cards show a ring counting down to the next burn. Long-press to pin or wipe; swipe a temp card left to wipe it.
- **Motion:** cards open with a container transform, enter with a staggered slide, messages spring in, the send button wakes as you type, a wiped chat blurs and dissolves, and unlocking zooms through your sigil.

## Roadmap
- **v1:** identity, QR invites, encrypted 1:1 and group chat over Tor, media, voice notes, threads, temp chats (internet/Wi-Fi Direct/Bluetooth), app lock, screenshot control, hidden chats.
- **v2:** Fediverse tab (Pixelfed/PeerTube, Reels-style), plans and date polls, shared albums, daily moment, on-device AI (catch-up summaries, transcription, search).
- **v3:** voice and video calls (WebRTC over Tor/relayed, no IP exposure).
