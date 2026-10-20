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

## Adding friends
- A **friend code** holds your identity key and name: `SANCTUM1:` + Crockford Base32 (digits and letters, no I/L/O/U), about 70–85 characters, with a 16-bit checksum that catches typos. It fits a QR version 4, encoded in alphanumeric mode, so it scans instantly and is easy to read out or type (case-insensitive; prefix optional; O/I/L forgiven).
- Each friend reads the other's code: **together** by scanning each other's QR; **far away** by sharing their code and typing or pasting the other's.
- Both phones derive the same **pair sigil** and **12-digit safety number** from the two keys. Compare sigils in person, or read the number aloud on a call. A swapped code changes both.
- The identity key is also the Tor onion service key (as in Cwtch), so a code is everything needed to reach a friend. The MLS session is set up over Tor on first contact, authenticated by these keys; messages wait on the phone until then.
- Scanning uses ZXing's native Android scanner (on-device, no Google ML Kit, which reports usage to Google). Verified in tests by decoding generated QRs with both Java ZXing and zxing-cpp, including blurred, rotated and tiny images.

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
