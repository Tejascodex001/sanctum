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

## Nearby, without internet
An opt-in feature in the sigil sheet (`net/src/nearby.rs`).
- **What it does:** friends on the same Wi-Fi, or on one phone's hotspot, connect directly over the local network. Temp chats and everything else work with no internet at all.
- **Beacons:** every 4 seconds each phone broadcasts a beacon (UDP port 47823) carrying an 8-byte tag per friend.
  - The tag is `BLAKE3-keyed(pair fingerprint, minute)`, so only that friend can recognize it, and strangers on the network can't link beacons from one minute to the next.
  - A friend who recognizes a tag connects to the TCP port in the beacon. From there it's the usual signed hello and MLS.
- **The trade-off:** a nearby friend sees your local address, never your internet one. That's why it's opt-in.
- **Routing:** `Reach` dials nearby friends directly first, then falls back to Tor. The network node now starts before Tor, so nearby works even when Tor can't.
- **Android:** the app takes a multicast lock while nearby is on, because some phones drop broadcasts otherwise.
- **Bluetooth (Android 10+)** is part of the same switch (`core/src/nearby.rs`, `BluetoothNearby.kt`).
  - **Advert:** a Bluetooth advert holds only a few bytes, so each phone advertises one 8-byte ID that rotates every minute, `BLAKE3-keyed(nearby key, minute)`, plus its L2CAP port.
  - **Nearby key:** derived from the identity secret, and shared only with friends inside MLS (`Content::NearbyKey`). Keys are swapped automatically after the first connection. Strangers see random bytes that can't be linked from one minute to the next.
  - **Links:** unpaired LE L2CAP channels, relayed through localhost so the Rust node treats them like any other connection. Incoming links go to the nearby listener. For outgoing links, `Reach` connects to the relay and sends the device's handle.
  - **Order:** `Reach` tries Wi-Fi first, then Bluetooth, then Tor.
- **Later:** Wi-Fi Direct (mostly covered by hotspot mode).

## Updates over the air
- **Checking:** the newest release is found through Tor, by following `github.com/<repo>/releases/latest`, which redirects to the newest `build-N` tag. That uses no API, so shared Tor exits don't hit rate limits.
- **Downloading:** the APK and `changelog.md` come from the release, also through Tor, on circuits kept apart from chat traffic.
- **Installing:** Android's installer takes it from there and refuses any APK not signed with the app's own key. The first time, the user allows Sanctum to install apps.
- **Changelog:** CI writes it from commit messages (`.github/scripts/changelog.py`), as `## Build N` sections. The app shows those newer than the installed build. Release tags point at the commit that was built.
- **When:** the app checks when Tor connects, at most every 6 hours (this can be turned off), or on demand from the sigil sheet.

## Groups
Peer to peer, with no server (`core/src/group.rs`).
- **Encryption:** each group is an MLS group. A message is encrypted once, and the same bytes go to every member over their own Tor link (`Frame::GroupMls`).
- **Delivery:** each copy is acknowledged separately. The message shows ✓✓ once every member has it, and copies for offline members wait in the outbox.
- **One admin:** the creator is the only one who commits membership changes. With a single committer, members can't disagree about the group's state, which a server would otherwise guarantee.
- **Adding members:** the admin adds friends only.
  1. It asks a friend for a key package over their link and checks it really is theirs.
  2. It commits the addition, which every existing member applies.
  3. It sends the newcomer a welcome with the group's name and members, then sends everyone the updated member list (`Content::GroupInfo`).
  - Invitees who are offline are retried until their phone is reached.
- **Members who aren't friends** connect to each other directly for the group: `Session::knows` accepts friends and fellow group members. Strangers can still only send a friend request.
- **Leaving and removal:** a member who leaves tells everyone and deletes the group from their phone; the admin then commits their removal. A removed member's copy goes inactive.
- **Out-of-order messages:** they are expected without a server.
  - Keys for 4 past epochs are kept, so a message sent just before a membership change can still be read after it.
  - A message that can't be read yet is left unacknowledged and resent (live links resend every 30 seconds). It is dropped after 5 tries.

## Media
Photos, videos and files (`core/src/media.rs`).
- **Encryption:** each attachment gets a random key. The file is encrypted with it in 64 KiB chunks (XChaCha20-Poly1305; the nonce is the blob id and chunk number) and stored like that in the media folder on both phones.
- **Delivery:**
  - The key, a description and a tiny blurred preview travel inside MLS (`Content::Media`).
  - The encrypted chunks follow as `Frame::BlobChunk`s, useless without the key.
  - The receiver accepts only chunks for a file a message announced, from that message's sender, and checks each chunk as it arrives.
  - The message gets ✓✓ only once the file has arrived. In groups it goes to each member.
- **Resending:** big files aren't resent on every flush, only on a new link or after 5 minutes without an acknowledgement.
- **Photos** are shrunk (2048 px max) and re-encoded on the phone before sending. That also strips EXIF metadata such as location.
- **Viewing:** photos are decrypted in memory. Videos play in the app from a temporary decrypted copy, which is deleted when the player closes and on every start. Files open in another app through a read-only link.
- **Voice notes** are attachments too: AAC in .m4a, mono at 48 kbps, recorded only while the mic is held. They play in the app from a temporary decrypted copy.
- **Limits:** photos 10 MB, videos 20 MB, voice notes 10 MB (about 25 minutes), files 50 MB. Enforced when sending and when receiving: a bigger attachment is refused. Temp chats have none, because they're memory-only.
- Deleting a message or chat deletes its files; panic wipe deletes the media folder.

## Polls and plans
Polls, date polls and plans (`core/src/poll.rs`), in friends' chats and groups.
- **A poll is a message** (`Content::Poll`): a question and up to 12 choices, optionally several answers each.
  - A **date poll** carries a time per choice (`dates`), and the app shows those.
  - A **plan** has a title, a time and an optional place. Its choices are always Going, Maybe and Can't go, one answer each.
- **Answers** (`Content::Vote`) go to everyone in the chat. Each carries a millisecond time, and a phone keeps only each person's newest answer, so answers that arrive out of order settle the same way everywhere. Sending no choices takes an answer back.
- Every person's latest answer is stored with the poll's own message (`StoredMessage::votes`), so deleting or clearing removes the answers too.
- In groups an answer can arrive before its poll. It waits in the sealed key-value table (up to 200 per poll) until the poll comes.
- Choices are checked on every phone: unknown ones are dropped, and only the first counts when a poll allows one answer.

## Shared albums
- An album is the photos and videos in a chat that carry its id and name (`Album`, on `StoredMessage::album`). Whoever adds the first photo creates it: there's no separate album record to keep in step.
- Album photos travel as `Content::AlbumMedia`, which is otherwise exactly like `Content::Media` (same encryption, chunks and limits). They show in the chat too, labelled with the album's name.
- The app builds the album list from the chat's messages. Up to 30 photos and videos can be picked at once.
- Deleting a photo removes it from the album; clearing the chat clears its albums.

## Daily moments
One photo a day for all your friends, gone 24 hours after it was shared (`core/src/moment.rs`).
- **Sending:** the photo is encrypted like any attachment. `Content::Moment` goes inside each friend's own MLS group, and the chunks follow. Only friends get it, never group members who aren't friends.
- **Not a message:** moments live in a sealed list of their own (in the key-value table), not in any chat. Sharing a new one replaces the old one everywhere: each phone keeps only a friend's newest.
- **Expiry:** once a moment's 24 hours are over, it's deleted with its file, on every phone. A moment that arrives after its day is over is ignored. Deleting your own moment also deletes it on friends' phones (`Content::MomentGone`).
- **Hidden chats:** a hidden friend's moment isn't shown, so it can't give the chat away.
- The photo can be taken right then with the camera app (written to Sanctum's cache through a one-off link, then deleted) or picked from the gallery. Either way it's resized and stripped of hidden data such as location.

## Explore (the Fediverse)
A fourth tab with public posts from PeerTube (videos) and Pixelfed or other Mastodon-compatible servers (photos), Reels-style (`net/src/fedi.rs`, `net/src/relay.rs`).
- **Anonymous and through Tor:** no account and nothing is posted. Every request (feeds, pictures, videos) goes through Tor with the HTTPS client, so servers see a Tor exit. The list of servers is kept sealed on the phone (`kv` "fedi:sources").
- **Feeds:** PeerTube's `/api/v1/videos` (trending, local, no sensitive videos, no live streams), and the Mastodon API's local public timeline with media only (sensitive posts left out). Servers that show their feed only to signed-in people get a plain explanation. Feeds from several servers are interleaved.
- **Videos:** the player only speaks plain HTTP, so it's given `http://127.0.0.1:port/<token>`. A relay on the phone forwards each request, ranges included, to the real `https://` address through Tor, and follows redirects itself, so the player never connects anywhere else. It listens on loopback only and answers only for random tokens it handed out. Android's network security config allows plain HTTP to 127.0.0.1 and nothing else. PeerTube videos play at up to 480p, which streams well over Tor.
- Sending a post to a chat sends its link as a message. Nothing is fetched from the link for a preview.

## Search and catch-up
On the phone, over the sealed store (`core/src/recall.rs`). Nothing is indexed in the clear: messages are opened and checked in memory.
- **Search** finds messages with all the typed words in any case, looking at text, poll questions and choices, plan places, file names and album names. It returns up to 100, newest first, and opens the chat scrolled to the message. Hidden chats are searched only while revealed.
- **Catch-up** ("While you were away") counts what friends wrote since the app was last left (`kv` "catchup:since", set when it goes to the background). It shows chats with new messages, mentions of your name (whole words), plans in the next two weeks you haven't answered, and others' polls from the last week you haven't answered. It appears only for 5 or more new messages, or a mention.
- On-device AI (transcription, written summaries) comes later and will run models on the phone only.

## Deleting
- **Delete a message:** for me, or (your own messages only) for everyone (`Content::DeleteMessage`, by the shared message id). Works in groups too.
- **Clear chat:** for me, or (friends' chats) for everyone (`Content::ClearChat`). The friendship stays.
- **Remove friend** always applies to both phones (`Content::Unfriend`).
  - The goodbye is encrypted before the chat is deleted, then sent from the outbox. The network still connects to the former friend to deliver it.
  - Their phone then deletes its chat with us.
- **Safety:** these instructions travel inside MLS, so only the chat's other phone can act on them, and only on messages its sender wrote.
- Message numbers are never reused after a deletion, because pending delivery receipts point at them.

## Replies
- Every message has an id that is the same on both phones (the network message id).
- A reply (`Content::Reply`) carries a quote: the original's id, its author's key and its first 140 characters. It can be shown even if the original is gone, and tapping it scrolls to the original when it's nearby.
- Swipe a message right, or long-press it and choose Reply.
- **Threads:** a message with replies shows "↳ N replies". Tapping it opens the thread: the message plus every reply to it, and replies to those, with a composer that answers in the thread.
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
- **Sanctum PIN and decoy PIN** (optional): both are stored only as Argon2id hashes (19 MiB, 2 passes). After 5 wrong tries, each further try waits 30 s, doubling.
  - With a PIN set, the lock screen shows a number pad. Biometrics still open the real profile.
  - The decoy PIN opens the **decoy profile**: its own identity and database (`decoy.db`), so it looks like a real, quiet Sanctum.
  - The network keeps serving the real profile in the background. Its notifications are muted until a real unlock.
  - In the decoy, the settings don't show that a decoy PIN exists.
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
Status per feature is tracked in the README's feature table. In short:
- **v1, private messaging:** done. Only nearby over Wi-Fi Direct remains, and hotspot mode mostly covers it.
- **v2:** plans and date polls, shared albums, daily moments, the Explore tab for the Fediverse, search and catch-up (done); on-device AI (voice note transcription, written summaries).
- **v3:** voice and video calls, with no IP exposure.
