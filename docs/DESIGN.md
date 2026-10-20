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

### Talking to friends (implemented)
- `sanctum-net` runs connections over any transport: **Tor** (Arti) in the app, loopback TCP in tests. Every phone is an onion service whose key is its identity key; `onion_address(key)` is computed locally (checked against Arti).
- **Handshake:** the reached phone sends a random challenge; the connecting phone signs it together with the reached phone's key. Tor already authenticates the reached side (its address is its key). Strangers are cut off.
- **First contact** sets up the pair's MLS group automatically (the smaller key sends a key-package request and the welcome). Messages written before that wait in the outbox.
- **Outbox:** sealed on disk, resent on every connection and every 30 s until acknowledged; receivers recognise resends after a lost ack. Acks mark messages delivered (✓ sent, ✓✓ delivered).
- **Temp chats with friends:** opened, written and wiped through the friend's MLS group; held only in memory on both phones; a wipe wipes both. Both phones derive the temp chat's sigil from its shared id.
- The session engine is I/O-free and tested by passing frames between two phones; the network engine is tested over real loopback connections.

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
- **Three kinds of sigil:** *yours* (from your key: lock screen, avatar), *a chat's* (from both friends' keys combined, so it differs from either person's own and is identical on both phones once each has added the other), and *a temp chat's* (from its shared id). The confirmation screen shows you + friend = your chat's sigil.
- **Verification levels** (as in Threema): a friend added by **scanning their QR in person** is *verified* (green shield). One added by typing or pasting is *not verified* (amber) until the safety number is compared and confirmed; the safety number can be viewed in the chat at any time (as in Signal).
- The identity key is also the Tor onion service key (as in Cwtch), so a code is everything needed to reach a friend. The MLS session is set up over Tor on first contact, authenticated by these keys; messages wait on the phone until then.
- Scanning uses ZXing's native Android scanner (on-device, no Google ML Kit, which reports usage to Google). Verified in tests by decoding generated QRs with both Java ZXing and zxing-cpp, including blurred, rotated and tiny images.

## Security features
- App lock with biometric or PIN; locks again when the app goes to the background.
- Screenshots blocked by default (`FLAG_SECURE`); can be allowed per chat after a biometric check.
- Hidden chats behind a second PIN; a decoy PIN; a panic-wipe gesture.
- Verification: both people see the same sigil for their chat. If the sigils match, the chat is secure.

## UI: Sigil design system
Flutter with **Material 3** as the foundation (accessibility, text input, platform behaviour such as predictive back), restyled with our own tokens in `app/lib/theme/tokens.dart`; screens use only these.
- **Type:** Space Grotesk (headings), Inter (text), JetBrains Mono (codes, safety numbers). Bundled in the app (OFL 1.1), never fetched at runtime.
- **Colour:** neutral near-black surfaces; each chat's sigil accent is the only strong colour (your bubbles, focus, highlights).
- **Shape and space:** 4-pt spacing scale, radii 12/16/22/28, hairline borders instead of shadows.
- **Motion:** Material 3 emphasized curves; 160/280/420 ms. Only opacity and transforms animate.
- **Sigils** are drawn once into GPU images and cached (LRU); lists and the slowly turning chat/lock backdrops only move those images, so nothing is re-drawn per frame.
- **Opening a chat:** messages load first, then the page fades and settles up while the chat's emblem flies from the card into the header (Hero). The backdrop sigil fades in after the transition.

## Roadmap
- **v1:** identity, QR invites, encrypted 1:1 and group chat over Tor, media, voice notes, threads, temp chats (internet/Wi-Fi Direct/Bluetooth), app lock, screenshot control, hidden chats.
- **v2:** Fediverse tab (Pixelfed/PeerTube, Reels-style), plans and date polls, shared albums, daily moment, on-device AI (catch-up summaries, transcription, search).
- **v3:** voice and video calls (WebRTC over Tor/relayed, no IP exposure).
