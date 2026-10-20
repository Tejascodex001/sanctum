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
   ├─ crypto     OpenMLS groups (1:1 is a 2-member group)            [todo]
   ├─ store      SQLCipher, key in Android Keystore, unlocked by biometric/PIN   [todo]
   ├─ temp_chat  in-memory only, 24h TTL, wipe anytime
   └─ transport  Internet: Tor onion service per device (Arti)          [todo]
                 Nearby: Wi-Fi Direct + Bluetooth LE (temp chats)        [todo]
```

### Delivery without a server
- Each phone runs its own Tor onion service; its onion address is derived from its identity key.
- A foreground service keeps it reachable (no Google push, so Google never sees activity).
- If a recipient is offline, online group members hold the encrypted message and pass it on later (store-and-forward). They can't read it.
- Trade-offs: messages may take a few seconds longer; the app uses some battery; delivery is slow if nobody in the group is online.

## Security features
- App lock with biometric or PIN; locks again when the app goes to the background.
- Screenshots blocked by default (`FLAG_SECURE`); can be allowed per chat after a biometric check.
- Hidden chats behind a second PIN; a decoy PIN; a panic-wipe gesture.
- Verification: both people see the same sigil for their chat. If the sigils match, the chat is secure.

## UI: Sigil + Deck
- **Sigil:** each chat's art is generated from its keys and supplies the chat's wallpaper, accent color, icon and deck card face.
- **Deck:** chats are cards in a stack. Swipe up to open, sideways to switch, pinch to see the whole deck. Long-press a card for actions. Few buttons.
- **Temp deck:** a separate deck for temp chats. Each card visibly burns down as its 24 hours run out; swipe it into the fire to wipe it now.
- Dark base, haptics on every gesture, motion-led transitions.

## Roadmap
- **v1:** identity, QR invites, encrypted 1:1 and group chat over Tor, media, voice notes, threads, temp chats (internet/Wi-Fi Direct/Bluetooth), app lock, screenshot control, hidden chats.
- **v2:** Fediverse tab (Pixelfed/PeerTube, Reels-style), plans and date polls, shared albums, daily moment, on-device AI (catch-up summaries, transcription, search).
- **v3:** voice and video calls (WebRTC over Tor/relayed, no IP exposure).
