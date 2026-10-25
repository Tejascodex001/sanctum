# Sanctum — Design

Private, serverless chat for a circle of friends (target: up to ~200 people).
Android first; iOS later.

## Principles
1. **No servers, no tracking.** No accounts, phone numbers, analytics or crash reporters.
2. **No IP addresses by default.** Peers, the UI and logs never see an IP (enforced in `core/src/transport.rs`). The only exceptions are opt-in and say so: Nearby (the local network) and Faster calls (the friend's phone sees yours, only if both opt in).
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
- **Notifications** are posted natively on the phone, never through Google or any push service. Every message gets one, while the app is open too, except in the chat on screen. In groups they say who wrote.
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
- If a recipient is offline, the **sender's** phone keeps the sealed message in its outbox and delivers it when both phones are online at the same time (retried on every connection and every 30 s).
- **In groups, members help deliver** (see "Groups at scale"): every member keeps the group's sealed messages for a week and passes them on to other members it's linked with, so a message reaches someone who comes online after its sender went offline. Only members get them, and they could read them anyway.
- Trade-offs: messages may take a few seconds longer; the app uses some battery; two friends who are never online at the same time can't exchange 1:1 messages until they are.

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

## Feedback to the developer
Settings → **Send feedback** (`core/src/feedback.rs`, `net/src/feedback.rs`, `app/rust/src/api/feedback.rs`, `app/lib/settings/feedback_screen.dart`). No server: it goes to the developer's own Sanctum through Tor.
- **The form:** 1–5 stars, text (up to 4000 characters), and the phone's maker and model, Android version and Sanctum build, which the person can leave out. The screen lists exactly what will be sent.
- **Where it goes:** the developer's phone has a separate *feedback key* (never its identity key) and, with **Receive feedback** on, serves a second onion service for it (nickname `feedback`). Each build carries that key's onion address (`DEVELOPER` in `api/feedback.rs`); while it's empty the form says feedback isn't open yet.
- **Anonymous:** the sender dials the feedback address on circuits of its own, writes one feedback message (`SFB1`, a length, the postcard of `Feedback`) and waits for `OK`. There's no hello and no Sanctum link, so nothing says which phone or person sent it, and it never meets `Frame`.
- **Delivery:** queued sealed in the store's `kv` until the developer's phone answers, retried when Tor comes up and every 10 minutes; dropped after 30 days. At most 5 a day from one phone.
- **Inbox:** sealed in `kv`, newest first, up to 500. The developer's phone takes at most 30 an hour from everyone together, ignores anything that isn't feedback, and notifies "New feedback" (never what it says). Off while the decoy profile shows.
- **The developer's switch:** tap the version in Settings 7 times; **Feedback inbox** appears under Feedback, with the switch, the address to copy, and what arrived. Turning it off stops serving but keeps the same key.

## Groups
Peer to peer, with no server (`core/src/group.rs`).
- **Size:** up to 50 people (`MAX_GROUP`, counting the admin).
- **Encryption:** each group is an MLS group. A message is encrypted once, and the same bytes go to every member over their own Tor link (`Frame::GroupMls`).
- **Delivery:** each member confirms receipt to the sender. The message shows ✓✓ once every member has it, however it got to them; copies for offline members wait in the sender's outbox, and other members also pass the message on (see "Groups at scale").
- **Whose message:** MLS says who wrote a message, so a message passed on by another member is still shown as its author's. A current member may pass on anyone's; anyone else may only send their own.
- **One committer, several admins:** the leader (the creator, or whoever was handed leadership) is the only one who commits membership changes. With a single committer, members can't disagree about the group's state, which a server would otherwise guarantee.
  - **Admins** (up to `MAX_ADMINS` = 5 besides the leader) can ask for changes: they send `Content::GroupRequest` (add, remove, rename, demote) inside the group, and the leader's phone checks the sender (MLS says who it is) against its admin list and does it. Without the leader online, requests wait.
  - **Making an admin** is the leader's call, and only for a member who has joined and is the leader's friend. The list goes out as `Content::GroupRoles { leader, admins, at }`. Members accept it only from the current leader, and only if `at` is newer than the last one (kept in kv as `roles-at:<group>`), so a stale or forged list changes nothing.
  - **Who can remove whom:** the leader removes anyone, including admins. An admin removes ordinary members, and may demote another admin, but nobody except the leader removes an admin or the leader from the group.
  - **Handing over:** the leader names an admin as the new leader, and becomes an admin. A leader with admins must hand over before leaving; a leader with none may leave and the group carries on without one, as before.
- **Adding members:** the admin adds friends only.
  1. It asks a friend for a key package over their link and checks it really is theirs.
  2. It commits the addition, which every existing member applies.
  3. It sends the newcomer a welcome with the group's name and members, then sends everyone the updated member list (`Content::GroupInfo`).
  - Invitees who are offline are retried until their phone is reached.
- **Sharing history:** when the leader adds someone, they can also show the newcomer the last week (`HISTORY_DAYS` = 7, at most 300 text messages and about 150 KB, never photos or files).
  - The leader sends `Content::HistoryBatch` over its own direct chat with the newcomer, not into the group. If it arrives before the welcome, it waits in one kv slot (`history-stash`) and is applied when the group is joined, and only if it came from the group's leader.
  - Imported messages keep their original author and time, and are marked `shared_by` the leader; they are shown as "Earlier message". The leader's phone remembers the choice (`history-for:<group><peer>`) until the newcomer has joined.
  - Everyone in the group gets `Content::HistoryShared { with }`, shown as a notice line in the chat (`StoredMessage::notice`), so nobody is surprised by what a newcomer can read.
  - Older builds ignore the new content (it is inside MLS): they simply don't show roles, requests, history or notices.
- **Members who aren't friends** connect to each other directly for the group: `Session::knows` accepts friends and fellow group members. Strangers can still only send a friend request.
- **Leaving and removal:** a member who leaves tells everyone and deletes the group from their phone; the admin then commits their removal. A removed member's copy goes inactive.
- **Out-of-order messages:** they are expected without a server.
  - Keys for 4 past epochs are kept, so a message sent just before a membership change can still be read after it.
  - A message that can't be read yet is left unacknowledged and resent (live links resend every 30 seconds). It is dropped after 5 tries.

## Groups at scale
**Members help deliver** (`core/src/group_sync.rs`), much as in Briar or Quiet. Before this, every member's phone sent to every other member's: a photo was uploaded once per member, and a message reached someone only while its sender and they were online at the same time. A group message is encrypted once and every member gets the identical sealed bytes; a photo's pieces are sealed with its own key and are identical too. Members can already read the group, so passing those bytes on costs no privacy, and nobody outside the group gets them.
1. **Ids:** a group message is known by a hash of its sealed bytes (`sync_id`, 16 bytes of BLAKE3), the same on every phone. That's also how a copy that arrives twice (from its sender and from a member) is recognised and dropped before MLS sees it, so it causes no error or retry.
2. **Keeping:** each member keeps the sealed bytes of the group's messages it sends and receives (membership changes too) for 7 days, at most 300 and 4 MB per group, oldest dropped first: a new sealed table (`kept`), indexed by keyed hashes like the rest of the store. It also notes which of the group's files it holds in full. Leaving the group, or being removed, forgets it all.
3. **Catching up:** when a link comes up between two members, each sends, for every group they share, the list of what it keeps (`Frame::GroupHave`: ids, each with the MLS epoch it was sent in, plus file ids). The other asks for what it lacks (`Frame::GroupWant`) and gets ordinary `Frame::GroupMls`, processed like any group message. Lists come oldest first, so messages arrive in order. A phone doesn't ask for messages from before it joined, or too old for MLS to still read (4 epochs back). On a live link, what's new is swapped every 30 seconds, and the whole list every 10 minutes. At most 500 entries per list.
4. **Offers first:** what the sender still owes a member who was away is offered in the list (an entry with `owed`, its outbox id) instead of sent. A member who already got it from someone else confirms with an ordinary `Frame::Ack`, which clears the outbox item and counts as delivered; otherwise it asks, and gets it under the outbox id. So ✓✓ still means everyone has it, and nothing is uploaded twice. While an offer waits for its answer, newer messages to that member are offered too, to keep their order. A brand-new message to a member who is online goes straight away, as before (no extra round trip).
5. **Files:** the sender pushes a file to the first 3 members it reaches (`SEEDS`). Others get only an offer, with "ask me" off; they fetch the file (`Frame::BlobWant`, from the piece they have up to) from any member whose list has it, and from the sender only if nobody else turns up within 45 seconds. A fetch with no new piece for a minute goes to someone else, carrying on from where it stopped (every copy's pieces are identical). A push counts as a seed place only until that link ends without the member confirming the file (`Session::link_ended`, called by the node when a link goes): a phone that just dropped off can still look connected for a moment, and would otherwise use up a place while a member who is online is told to get it from someone else. When a place frees up, members who were told to wait are offered the file again with "ask me" on, straight away.
   - **Checked:** each piece is still checked with the file's key as it arrives, but every member has that key, so that alone wouldn't stop a member from passing on a different file. So a file's id is now a keyed hash of its contents (`blob_id`, keyed with the file's own key), and a file pieced together from someone other than its sender is checked as a whole against the id in the message before it's kept; one that doesn't match is thrown away and fetched from someone else. Pieces are taken only from the sender or the member we asked.
   - Files from older builds have random ids: they don't match, so they're never passed on, and still come from their sender only.
6. **Who:** lists are swapped only with current members of the group (by MLS), and only on links where the other phone said, in the handshake, that it understands them (`FEATURES`, see "Older builds" below). A list or request about a group from anyone else gets nothing. Members pass messages on over the links they have: friends' links, and links made to deliver something. Everyone is friends with the admin, so the admin's phone is a natural hub.
7. **Big answers** (a file asked for, a long catch-up) are written a frame at a time between reads (`Node::send_paced`, over 64 KB), so two phones sending each other a lot at once never stop reading.

**Measured** (`a_big_group_shares_the_load` and, in a release build, `big_groups_numbers`; simulated phones, every exchange on a fresh link, so the full lists go each time):

| Group | Text: what the sender uploads to everyone | 1 MB photo: the sender uploads | Without members' help |
|---|---|---|---|
| 30 | 38 KB (about 1.3 KB per member, mostly the lists) | 3.2 MB (3.1×) | 29 MB |
| 50 | 100 KB (about 2 KB per member) | 3.3 MB (3.2×) | 49 MB |

- The photo's other copies (27 MB for 30 people) are uploaded by members, each passing it to the next.
- A message sent just before its sender went offline, having reached one member, reached all the others through that member and those who got it from them.
- Setting up a 50-person group's encryption (every member applying every newcomer's addition) is the slow part: about 55 s for the simulation in a release build, and much more in a debug build, so CI runs it with 16 phones.

**Older builds:** they don't know the new frames, and a frame a phone doesn't know drops the link. So each phone now adds what it can do after its handshake frame, inside the same frame (`encode_frame_with_features`); older builds stop reading where the frame they know ends, so they don't notice. A phone that says nothing gets none of the new frames, nothing passed on by others, and its files from the sender, exactly as before; it never counts as one of the 3 a file is pushed to.

## Media
Photos, videos and files (`core/src/media.rs`).
- **Encryption:** each attachment gets a random key. The file is encrypted with it in 64 KiB chunks (XChaCha20-Poly1305; the nonce is the blob id and chunk number) and stored like that in the media folder on both phones.
- **Delivery:**
  - The key, a description and a tiny blurred preview travel inside MLS (`Content::Media`).
  - The encrypted chunks follow as `Frame::BlobChunk`s, useless without the key.
  - The receiver accepts only chunks for a file a message announced, from that message's sender (or, in groups, from the member it asked), and checks each chunk as it arrives.
  - A file's id is a keyed hash of its contents (keyed with its own key), so a whole file can be checked against its message (see "Groups at scale").
  - The message gets ✓✓ only once the file has arrived. In groups the sender pushes it to a few members and the rest fetch it from whoever has it.
- **Resending:** big files aren't resent on every flush, only on a new link or after 5 minutes without an acknowledgement.
- **Photos** are shrunk (2048 px max) and re-encoded on the phone before sending. That also strips EXIF metadata such as location.
- **Viewing:** photos are decrypted in memory. Videos play in the app from a temporary decrypted copy, which is deleted when the player closes and on every start. Files open in another app through a read-only link.
- **Voice notes** are attachments too: AAC in .m4a, mono at 48 kbps, recorded only while the mic is held. They play in the app from a temporary decrypted copy.
- **Limits:** photos 10 MB, videos 20 MB, voice notes 10 MB (about 25 minutes), files 50 MB. Enforced when sending and when receiving: a bigger attachment is refused. Temp chats have none, because they're memory-only.
- **Save to phone:** attachments are never written to the gallery or shared storage by themselves; the only copies outside the encrypted media folder are temporary and private to the app (`open/` for decrypted viewing, `outgoing/` for what's being sent), both emptied on every start. An explicit "Save to phone" (photo, video, voice note, file) writes one copy: Android 10+ through MediaStore into `Pictures`, `Movies`, `Music` or `Downloads` under `Sanctum/` (no permission), Android 9 and older through the system "save as" screen.
- **Opening files:** the type comes from the stored type, or from the file name when it is missing (`application/octet-stream`); the read grant travels with the intent (`ClipData`); a failure says which of "no app can open it", "couldn't share it" or "file missing" happened. Photos the phone can't decode (HEIC on old phones) go as plain files; videos the phone can't play say so and offer Save to phone.
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

## Reactions
Emoji reactions to any message with an id, in friends' chats and groups (`core/src/reaction.rs`). Not in temp chats.
- **One per person per message.** Reacting again replaces it; reacting with nothing takes it back.
- **On the wire:** `Content::React { id, emoji, at }` inside MLS, to everyone in the chat. It names the message by its shared id, as answers name their poll. Like answers, each carries a millisecond time and a phone keeps only each person's newest, so reactions that arrive out of order settle the same everywhere.
- **Stored with the message** (`StoredMessage::reactions`, one `Reaction { who, emoji, at }` per person), so deleting or clearing removes them too. A reaction taken back stays as an empty entry, so an older one arriving late can't bring it back. The app shows them grouped by emoji (`ReactionView`).
- **Checked on every phone:** trimmed, at most 32 bytes (enough for any emoji with modifiers), no control characters or line breaks. Only someone in the chat can react: it comes inside the chat's MLS group, from them.
- In groups a reaction can arrive before its message. It waits in the sealed key-value table (up to 200 per message) until the message comes.
- **Quiet:** a reaction refreshes the chat (`Event::Chat`) but never notifies.

## Typing
"Asha is typing…" in friends' chats and groups (`core/src/typing.rs`).
- **Ephemeral:** `Frame::Typing` is sent only to people in the chat whose link is up right now. It is never stored, never put in the outbox and never resent; anyone not connected simply doesn't see it. It carries no text, only which chat: empty for the pair's own chat, or the group's id (which `Frame::GroupMls` already carries).
- **Why a frame and not MLS:** an MLS message advances the sender's message counter. A signal encrypted but dropped (the friend wasn't connected) or overtaking a queued message could leave the counter ahead of a message still waiting in the outbox, and MLS keeps only a few skipped keys, so that message could become unreadable. The link already gives what a typing signal needs: over Tor it reaches the friend's own onion address, encrypted end to end, and the signed hello proves who dialed. On a nearby link (local Wi-Fi or Bluetooth) frames aren't encrypted by the link, so someone on that network could see that a signal passed, as they can already see that messages pass.
- **Checked on arrival:** a pair's signal counts only from that friend; a group's only from a current member of it. Anything else is ignored.
- **Pace:** the app calls `chat_typing` about every 4 seconds while someone writes. Rust sends at most one signal per chat every 3 seconds, and passes on at most one per person per chat each second. The receiver shows it until about 6 seconds pass without another, or the person's message arrives.
- **Decoy profile:** while the decoy shows, typing is neither sent nor shown for the real profile.
- **Older builds** don't know this frame and drop the link when one arrives; it reconnects on its own. Everyone should update.

## Calls
1:1 voice and video calls with a friend (`core/src/call.rs`, pictures in `core/src/video.rs`, the direct path in `core/src/direct.rs` and `net/src/direct.rs`). Not in groups, temp chats or notes to self, and only once the friend has added us back (the call needs the pair's encryption).
- **The path:** over the friends' own link, so through Tor by default: nobody learns who talks to whom, and neither phone sees the other's IP. If the friend is nearby (same Wi-Fi or Bluetooth) the call uses that link instead, which is much faster (see "Nearby link first" below). With "Faster calls" on both phones, the call's packets can also go straight between the phones (see "Faster calls" below).
- **Signaling** travels inside the pair's MLS group like a message, through the outbox, so it's end-to-end encrypted and keeps its order with everything else:
  - `Content::CallInvite(CallOffer)`: the call id, a fresh random 32-byte call key, the time it was made (ms), whether the caller's camera starts on (`video`), what this phone can play and record (`audio_in`, `audio_out`: "opus", "amr-wb"; `video_in`, `video_out`: "h264", "vp8"; best first), and `routes` (ways to reach this phone directly; only with Faster calls on). Codecs are names, not an enum, so an unknown one from a newer phone is just skipped.
  - `Content::CallAnswer(CallAnswer)`: what each side records into, picked from the other's lists, for sound and pictures; whether the callee's camera starts on; and the callee's `routes`. Each direction can differ: the caller records into the first of the callee's playable formats it can record; the callee into the first of the caller's it can record. A direction with no video format in common simply has no pictures; the call goes on with sound.
  - `Content::CallEnd { call, reason }`: hang-up, declined, busy, cancelled or failed, as a number (unknown numbers mean hang-up).
  - Why MLS and not link frames like typing: these must arrive, once, in order, even after a reconnect, and are rare, so the outbox is right for them.
  - Older builds put nothing in the video lists and drop `routes`, so calls with them are voice calls over the link, as before.
- **Ringing:** an invite arriving more than 45 seconds after it was made doesn't ring: it becomes a "Missed call". The caller shows "Connecting privately…" until the friend's phone acknowledges the invite, then "Ringing…". It gives up after 25 seconds if the invite never got there ("Asha isn't reachable right now") or after 45 seconds unanswered ("No answer"); the phone called stops ringing by itself after 50. Timeouts run with `Session::purge`, every few seconds.
- **Busy and both calling at once:** a call while in another call is answered "busy" and recorded as missed. If both friends call each other at once, the call made by the phone with the smaller key wins on both phones; the other is dropped quietly (its invite is taken back if not yet encrypted).
- **Records:** each phone records the call in the chat itself ("Voice call · 4:12", "Video call · 4:12", "Missed call", "Missed video call", "Declined call", "No answer"…), as a `StoredMessage` with `call: Some(CallLog)` (`video` set if either camera was on at any point) and the words in `body` (so previews and search need nothing special). Records are local: no id, no replies, reactions or delete-for-everyone. Missed calls notify ("Missed call", the name hidden for hidden chats and on the lock screen).
- **Media:** `Frame::CallMedia { call, seq, sealed }`, a link-level frame like typing: sent only over a live link (or the direct path), never stored, queued or resent (late sound is useless), and taken only from the friend in the call.
  - **Sealing:** ChaCha20-Poly1305. Each direction has its own key, `BLAKE3-derive_key(direction, call key)`; the nonce is the direction and `seq` (a per-direction counter shared by sound, pictures and control packets), and the call id, direction and `seq` are bound as associated data. A replay window (128 packets) takes each `seq` once. So Tor relays, the local network (nearby links aren't encrypted by the link) or anyone else can neither listen nor replay. The call key never leaves MLS.
  - **Inside:** `MediaPacket { kind, index, at_ms, data }`. Kind 0 is sound: `index` its packet number, `data` 40 ms (two 20 ms codec frames, each with a length). Kind 1 is a piece of a video frame: `index` the frame's number. Kind 2 is a control message (`Control`: our camera's state, "send a keyframe", direct-path probes and their answers). `at_ms` is the sender's media clock (milliseconds since it started talking), shared by its sound and pictures for lip-sync. Kinds a build doesn't know are ignored.
  - **Fast path:** the network hands `CallMedia` frames to the call's `MediaHub` without the session's lock (which may be busy encrypting or saving). A `MediaHub::tick` every 50 ms (`Node::call_started`) sends probes, keepalives, camera news and keyframe requests.
- **Sound first:** on a link, sound and pictures queue separately (`video::Backlog`) and the link writes sound (and everything else) before any picture. Each link queues at most 12 sound packets (about half a second); beyond that new ones are dropped until it catches up, since old sound is worse than none. Pictures queue only while at most 2 sound packets wait, and only up to 40 KB (a keyframe always fits an empty queue): whole frames are dropped beyond that, never parts of one.
- **Jitter buffer** (`JitterBuffer`): holds packets until their turn, in order. It tracks how late each packet arrives compared with the earliest (relative to when it was made) and waits as long as the latest 5% of recent packets needed, between 60 and 400 ms. Tor delivers in order but in bursts, so mostly the wait grows; when the network calms, it skips a packet now and then (at most once a second) to shrink it again. A missing packet with later ones waiting is lost (the phone plays a fading copy of the last sound); with nothing waiting the network has stalled, so it waits, and refills before playing again.
- **Codecs:** Opus at 20 kbps where the phone can record it (Android 10+); otherwise AMR-WB at 23.85 kbps. Every supported Android decodes both. Video in H.264 (Baseline where the encoder takes it), else VP8: every Android 7+ phone decodes both, and practically all record H.264 in hardware. All through Android's own `MediaCodec`, nothing bundled.
- **Pictures** (`video.rs`):
  - **Fragments:** each encoded frame is split into pieces of at most 1100 bytes (so a packet fits one UDP datagram, and sound slips in between pieces on a link). Each piece has an 8-byte header: which piece of how many, keyframe or not, which way up (quarter turns), and the picture's size. H.264's parameter sets go with every keyframe, so a picture can start (or start again) from any keyframe.
  - **Reassembly** (`Reassembler`): pieces may come late, twice, out of order or not at all. Frames are handed on in order once complete. A frame still incomplete when a later one has been complete for 80 ms (or after 600 ms) is lost; nothing after a loss decodes until a keyframe, so frames are skipped until one arrives, and the sender is asked for one (`Control::Keyframe`, at most every 600 ms). The phone starts each picture from a keyframe.
  - **Sender:** a frame the link has no room for is dropped, and then nothing but a keyframe goes next. Keyframes are forced (on request, or after a drop) at most every 700 ms, and come every 4 seconds anyway.
  - **Quality** (`RateControl`): through Tor, about 240p at 12 frames a second, 80–250 kbps (starting at 180); over nearby or direct paths, about 480p at 24 frames a second, 200 kbps–1 Mbps (starting at 600). A dropped frame or a keyframe request (the friend lost something) cuts the bitrate by 30% (at most twice a second); after 2 calm seconds it climbs back by 5% of the most each second. A path change restarts the camera at the new size.
  - **Lip-sync:** a picture is shown once the sound made at the same moment leaves the speaker (the jitter buffer knows which sound is playing and when, less about 80 ms for the speaker), up to 20 ms early; a picture never waits more than 600 ms, and with more than 6 waiting they're shown at once to catch up.
  - **Cameras:** either side turns its camera on or off at any time, during a voice call too; the change goes out at once as a control packet and is repeated every 2 seconds. While the app is in the background the camera stops and the friend sees "camera paused" (`Camera::Paused`).
- **Faster calls** (opt-in, off by default; `kv` "calls:direct", sealed): calls connect straight between the phones when both have it on. The trade-off, said plainly in Settings: the friend's phone learns your internet address.
  - **No servers.** A phone with the setting on opens a UDP socket for the call and lists the addresses it knows for itself on that port (`net::direct::local_routes`): global IPv6 addresses (most Indian mobile carriers hand them out) and local-network IPv4 ones, never loopback, link-local or carrier-internal ones. Routes are a tag, the address and the port, at most 8.
  - **Only if both want it:** the caller puts its routes in the invite only with the setting on; the phone called answers with its own only if it has the setting on *and* the caller offered theirs. Each phone starts the direct path only once the call is answered and both offered routes.
  - **Hole punching:** both phones send probes to each other's addresses at once, every 250 ms for 10 seconds, then every 2 seconds. A probe going out opens the phone's own firewall for answers from that address, which is how typical IPv6 firewalls and a shared Wi-Fi let the two meet. A probe is an ordinary sealed call packet (`Control::Probe`), so only the friend in the call can make one, and replays are refused; it's answered with `Control::ProbeAck`. The first address whose answer comes back is the path.
  - **Switching:** the call starts on the link and moves to the direct path once it's up: the same sealed packets go as UDP datagrams (`S1`, call id, `seq`, sealed packet), and the replay window and jitter buffer cope with the overlap. Keepalive probes go every second; when nothing has come over the direct path for 3 seconds, the call goes back to the link and probing starts again.
  - **Safety:** packets go only to addresses the friend listed inside the encrypted signaling, never to loopback, multicast, unspecified or link-local addresses (`direct::public_only`; tests on one machine allow loopback). UDP packets are taken only from those addresses, and only if they open with the call's key.
- **Android** (`CallAudio.kt`, `CallVideo.kt`, `CallService.kt`, `CallNative.kt`):
  - Recording with the voice-call microphone setting plus the phone's echo canceller and noise suppressor when it has them; playing as a voice call. Earpiece by default (a wired or Bluetooth headset wins on Android 12+), with a speaker toggle and mute (muted sends silence, so the stream stays steady). Near the ear, the screen goes off. Video calls start on the speaker.
  - **Camera:** Camera2 draws into the hardware encoder's input surface and into a small preview; the size is the camera's closest to 240 or 480 lines (4:3 preferred), at the frame rate the path allows. Frames are sent as the sensor sees them, with how far to turn them (the sensor's mounting and how the phone is held); the screen showing them does the turning, and mirrors our own front camera.
  - **Showing pictures:** frames are decoded by `MediaCodec` straight onto Flutter textures (`TextureRegistry.SurfaceProducer`); Dart shows them with `Texture`. A decoder that has to start over (new size, or its surface was taken away in the background) waits for a keyframe and asks for one.
  - **Rust ↔ Kotlin:** sound and pictures never pass through Dart. Kotlin loads the same Rust library (`System.loadLibrary`; Android keeps one copy per process, the one Dart opened) and calls JNI functions in `app/rust/src/call_jni.rs`: `send` (seal and send 40 ms) and `pull` (the next 40 ms from the jitter buffer: a packet, lost, or nothing yet); `sendVideo` (one encoded frame), `videoTarget` (bitrate, frame rate, size, keyframe now), `pullVideo` (the next frame that's due) and `askKeyframe`. The playing thread is paced by Android's audio output, so it pulls exactly as fast as it plays.
  - A foreground service runs from when the call is made or answered (both while the app is on screen, as Android requires) until it ends, with a "Hang up" action: of type `microphone`, plus `camera` while our camera is on (Android 14 rules; started again on each change).
  - Incoming calls ring with a notification on their own "Calls" channel (the phone's ringtone, insistent), full screen where Android allows it, in Android's call style (Answer/Decline) on Android 12+. On the lock screen it says only "Incoming call".
- **The app:** call and video-call buttons in a friend's chat header; a call screen above everything, the app lock included, so a call can be answered and ended without unlocking. While the app is locked, or for a hidden chat, it doesn't say who is calling. An incoming video call can be answered with the camera or without. During a call: mute, camera on/off, end, speaker; with the friend's camera on, their picture fills the screen and ours floats in a corner (drag it anywhere; a button on it switches front and back camera). The top says how the call travels: "Private" (Tor), "Nearby" or "Direct"; video through Tor says plainly it may be choppy. It can shrink to a bar to use the app meanwhile. The unlock prompt waits while a call rings or goes on.
- **Decoy profile:** only the real profile calls and rings, and never while the decoy shows: a call nobody sees stops ringing by itself and becomes a missed call. Entering the decoy ends any call.
- **Older builds** don't know call content: they acknowledge and drop it, so calling them just ends as "No answer".
- **Nearby link first:** links now know whether they're nearby (Wi-Fi or Bluetooth, `Transport::dial_ranked`, `Node::accept_nearby`). A nearby link replaces a Tor link to the same friend, on both phones, and a Tor link never replaces a nearby one. A friend linked over Tor who turns up nearby is offered the nearby link (at most once a minute, or at once when a call starts).

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

## Search and catch-up
On the phone, over the sealed store (`core/src/recall.rs`). Nothing is indexed in the clear: messages are opened and checked in memory.
- **Search** finds messages with all the typed words in any case, looking at text, poll questions and choices, plan places, file names and album names. It returns up to 100, newest first, and opens the chat scrolled to the message. Hidden chats are searched only while revealed.
- **Catch-up** ("While you were away") counts what friends wrote since the app was last left (`kv` "catchup:since", set when it goes to the background). It shows chats with new messages, mentions of your name (whole words), plans in the next two weeks you haven't answered, and others' polls from the last week you haven't answered. It appears only for 5 or more new messages, or a mention.

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
  - Some phones close the app's screen around the fingerprint prompt and leave Android's "check in progress" flag set, which used to block every later try. The check has a timeout; on any failure the pending check is cancelled and retried (at most twice by itself), a tap on "Try again" can replace a hung check, and the screen shows a short error code testers can report.
  - **Secure storage never makes new keys by accident.** The keys are read from Android's Keystore-backed storage with `resetOnError: false`. Only a read that returns *nothing* (first run) creates keys; a read that fails stops startup with a "Try again" screen instead of minting a new identity that couldn't open the old database.
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
- **Verification belongs to people, not groups.** A group has no verify button; a member we verified in a direct chat stays verified when we meet them in a group, and updates to a group's member list (someone joins, a rename) keep what we know about everyone already there.
- **Battery:** while the app isn't on screen (and no call is going) upkeep runs every 15 s instead of 3 s, housekeeping once a minute, unreachable friends are retried every 30 minutes at most (2 minutes when messages wait), and timers that only refresh what's drawn do nothing. Link pings stay at 40 s: a phone that pinged less often would look dead to older builds and to phones with the app open.

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
- **v2:** plans and date polls, shared albums, daily moments, search and catch-up (done). On-device AI was dropped.
- **v3:** voice and video calls, with no IP exposure. 1:1 voice and video calls over Tor (or nearby) are in, with an opt-in direct mode ("Faster calls").
