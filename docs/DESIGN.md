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
- **Re-pairing heals itself.** A side with no group always sends a key-package request on connect. If the friend still has a group (one of you deleted the chat and you paired again), it rebuilds the group. Messages already encrypted for the old group are dropped and stay unticked, never falsely marked delivered.
- **Live links.** While the app is open it keeps one Tor link open to every friend, pinged every 40 seconds, so messages arrive in about a second instead of waiting for a new circuit. Dropped links reconnect on their own, with backoff for friends who can't be reached: 4 seconds doubling up to 2 minutes, or up to 20 seconds while messages wait. At most 6 dials run at once. If both phones dial at once, both keep the link dialed by the smaller key, so neither closes the other's. Being connected is the online indicator. Pull to refresh, or reopening the app, retries everyone now.
- **Nothing is lost when a link drops mid-setup.** Queued messages keep their plaintext, sealed in the store, until acknowledged. If the pair's group has to be rebuilt, they are encrypted again instead of being dropped.
- **Background delivery** is on by default and can be turned off in your sigil sheet.
  - One Flutter engine lives for the whole process and is owned by `SanctumApplication`; the activity only attaches to it.
  - A `remoteMessaging` foreground service ("Connected privately", silent and minimal) keeps the process, and with it Tor and the friend links, alive while the app is closed.
  - While closed, offline friends are retried every 10 minutes at most (2 minutes when messages wait) to save battery. Opening the app reconnects everyone at once.
  - The user can grant a battery-optimization exemption for phones with aggressive battery savers.
- **Notifications** are posted natively on the phone, never through Google or any push service.
  - They say who wrote and how many messages ("3 new messages"), never what. On the lock screen even the name is hidden ("New message").
  - Tapping one opens that chat. Chat cards show unread badges.
  - Rust emits a separate `Message` event for an incoming message, so a delivery receipt never notifies.
- **Tor startup** reports bootstrap progress to the UI, gives up after 3 minutes, and turns any crash into a visible failure with a retry. rustls uses the `ring` crypto backend.
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

## Replies
- Every message has an id that is the same on both phones (the network message id).
- A reply (`Content::Reply`) carries a quote: the original's id, its author's key and its first 140 characters. It can be shown even if the original is gone, and tapping it scrolls to the original when it's nearby.
- Swipe a message right, or long-press it and choose Reply.
- Message and chat records upgrade in place: each on-disk format is tried newest first, and an older record fails to decode as a newer one because it ends before the new fields.

## Adding friends
- A **friend code** holds your identity key and name: `SANCTUM1:` + Crockford Base32 (digits and letters, no I/L/O/U), about 70–85 characters, with a 16-bit checksum that catches typos. It fits a QR version 4, encoded in alphanumeric mode, so it scans instantly and is easy to read out or type (case-insensitive; prefix optional; O/I/L forgiven).
- **One code is enough.** Adding someone's code (scan, type or paste) sends them a **friend request** over Tor. Your phone retries every 30 s until theirs is reachable. They see your name, the pair sigil and the safety number, and they accept or decline. Accepting adds you as *not verified*. The hello before a request already proves which key sent it; only the name is self-chosen. A stranger may send a request frame and nothing else, and then the connection closes. Requests are held in memory only, capped at 20, with names capped at 64 characters. Declines last until restart.
- Both phones derive the same **pair sigil** and **12-digit safety number** from the two keys. Compare sigils in person, or read the number aloud on a call. A swapped code changes both.
- **Three kinds of sigil:** *yours* (from your key: lock screen, avatar), *a chat's* (from both friends' keys combined, so it differs from either person's own and is identical on both phones once each has added the other), and *a temp chat's* (from its shared id). The confirmation screen shows you + friend = your chat's sigil.
- **Verification levels** (as in Threema): a friend added by **scanning their QR in person** is *verified* (green shield). One added by typing or pasting is *not verified* (amber) until the safety number is compared and confirmed; the safety number can be viewed in the chat at any time (as in Signal).
- The identity key is also the Tor onion service key (as in Cwtch), so a code is everything needed to reach a friend. The MLS session is set up over Tor on first contact, authenticated by these keys; messages wait on the phone until then.
- Scanning uses ZXing's native Android scanner (on-device, no Google ML Kit, which reports usage to Google). Verified in tests by decoding generated QRs with both Java ZXing and zxing-cpp, including blurred, rotated and tiny images.

## Security features
- **App lock** with biometrics or the device PIN. It locks again when the app goes to the background and prompts as soon as the app is back on screen.
- **Screenshots** are blocked by default (`FLAG_SECURE`, which also blanks the recents preview). A chat can allow them from the camera icon in its header, after a biometric check, and only while that chat is open.
- **Hidden chats.** Long-press a chat to hide it.
  - Long-press the "Sanctum" wordmark and pass a biometric check to show hidden chats until the app leaves the screen.
  - The hidden flag is stored inside the encrypted record. A hidden chat's notifications don't show its name, and hidden friends are left out of the temp-chat picker.
- **Panic wipe.** Hold for 2 seconds, from the sigil sheet or from "Emergency wipe" on the lock screen, which needs no unlock.
  - It destroys the Keystore-held keys first (crypto-shredding, since flash storage can't be reliably overwritten).
  - Then it deletes the database and Tor's files, stops the service, clears notifications and exits. Temp chats die with the process.
- **Background reliability:** one tap opens the phone maker's own "allow in background" page (Xiaomi Autostart, Oppo/Realme startup manager, Vivo, Samsung battery, Huawei, OnePlus), falling back to the app's settings.
- Decoy PIN: later.
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
