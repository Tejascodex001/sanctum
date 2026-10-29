<img src="docs/brand/icon_512.png" width="96" alt="Sanctum">

# Sanctum

**Private chat for you and your close friends. No servers, no accounts, no tracking.**

Sanctum is an Android app for a small circle of people who trust each other (about 200 friends at most). Your messages, photos, calls and plans go straight from your phone to your friends' phones, hidden by Tor. There's no company in the middle that stores them, reads them or sells anything about you.

> Sanctum is in **beta** (the app says *beta-1.0.0* followed by the build number, like *beta-1.0.0.66*). It's ready to use with friends, and it updates itself from inside the app.

## Why Sanctum

- **No servers.** Most chat apps send everything through a company's computers. Sanctum doesn't: your phone talks to your friends' phones directly.
- **No phone number, no email, no account.** You add a friend by scanning their QR code in person, or by typing a short code they send you.
- **Nobody sees who you talk to.** Connections go through Tor, so not even your friends learn your internet address, and nobody watching the network can tell who is talking to whom.
- **Everything stays on your phone, locked.** Chats are stored encrypted, behind your fingerprint, face or PIN. There are no cloud backups.
- **Nothing is tracked.** No ads, no analytics, no crash reports, no Google services. Notifications are made on your phone and never say what a message says.
- **Made for friends, not for the world.** No public profiles, no strangers, no follower counts.

## What you can do

- **Chat** one to one or in **groups** of up to 50, with replies, threads, reactions, edits, and the "typing…" hint.
- **Send** photos, videos, files and **voice notes**.
- **Call** a friend, with **voice or video**.
- **Plan together** with polls, "which day works?" polls and plans that show who's going.
- **Share albums** in any chat, and a **daily moment**: one photo a day for your friends, gone after 24 hours.
- **Temp chats** that live only in memory, with messages that burn after 24 hours.
- **Chat without internet** with friends on the same Wi-Fi, a phone's hotspot or Bluetooth (Nearby, off until you turn it on).
- **Stay safe** with hidden chats, screenshot blocking, a decoy PIN that opens an empty-looking Sanctum, and a panic wipe that erases everything in two seconds, even from the lock screen.

## Every chat has a sigil

Each person and each chat gets its own **sigil**: a unique piece of artwork made from their secret key. Your chat with a friend has a sigil made from both of you, and it looks the same on both phones. **If your sigils match, nobody is listening in.** That's how you check a chat is secure: no codes to read, just look.

## Good to know

- **Android only.** iPhones stop apps from staying connected in the background, and the usual way around that needs a server, which Sanctum doesn't have. (Briar, a similar app, is Android-only for the same reason.)
- **Private by default, faster when you choose.** Tor makes some things slower: video calls are choppier. A switch in Settings, *Faster calls*, speeds them up, and it says plainly what it shares.
- **Messages wait for you.** If a friend writes while you're offline, their phone keeps the message and delivers it when you're both online again. In groups, other members who already have it pass it on, so it reaches you even if the sender has gone offline.

## Install

Each release is told apart by its build number.

1. Download `sanctum.apk` from the latest [release](../../releases/latest) (`build-N`).
2. Install it. Android asks once to allow installs from your browser or file manager.
3. **After that, update from inside the app.** When a new build is out, Sanctum shows "Build N is available". **What's new** lists the changes since your build, and **Update** downloads it through Tor and opens Android's installer. Android accepts only builds signed with the same key, so a tampered APK can't be installed.

## Features

✅ done · 🚧 partly done · ⏳ planned

### Milestone v1: private messaging

| | Feature | Notes |
|---|---|---|
| ✅ | **Friend codes** | QR in person, or a short typeable code (Crockford Base32, typo-checked) for far-away friends |
| ✅ | **Friend requests** | One person enters a code and the other accepts. Requests show the pair's sigil and safety number |
| ✅ | **Verification** | Scanning a QR in person verifies a friend. Otherwise, compare the 12-digit safety number (as in Signal and Threema) |
| ✅ | **1:1 chat over Tor** | MLS end-to-end encryption, onion services, IPs hidden. Live links: messages arrive in about a second, with online indicators and ✓/✓✓ ticks |
| ✅ | **Group chats** | Peer to peer, no server. Up to 50 people. Members pass messages and photos on to each other, so a message reaches you even if whoever sent it has gone offline since, and a photo isn't uploaded by its sender once per member. The leader manages members and can make up to 5 admins who add or remove people for the group; leadership can be handed over. When adding someone, the leader can show them the last week of text messages (everyone is told). Members who aren't friends still reach each other. Groups have square emblems with a badge, and each person's name has its own colour |
| ✅ | **Temp chats** | Kept in memory only, on both phones. Each message burns 24 hours after it's sent, and either person can wipe the chat for both |
| ✅ | **Nearby (Wi-Fi / hotspot)** | Chat with friends on the same Wi-Fi or one phone's hotspot, with no internet. Opt-in |
| ✅ | **Nearby over Bluetooth** | For friends who've connected once before. Android 10+. Part of the same Nearby switch |
| ⏳ | Nearby over Wi-Fi Direct | Mostly covered by hotspot mode |
| ✅ | **Replies** | Swipe a message right; tap the quote to jump to the original |
| ✅ | **Threads** | Messages with replies show "↳ N replies"; tap to see the whole thread and answer in it |
| ✅ | **Delete & clear** | Delete a message (for me / for everyone), clear a chat (for me / for everyone), remove a friend (it's removed on both phones) |
| ✅ | **Background delivery** | A foreground service keeps you reachable while the app is closed. A one-tap shortcut opens your phone maker's battery page |
| ✅ | **Notifications** | For every message, even while Sanctum is open, except in the chat you're looking at. Posted on the phone itself, never through Google. They say who wrote, never what. On the lock screen they say only "New message" |
| ✅ | **App lock** | Fingerprint, face or PIN, every time the app opens. If the fingerprint check gets stuck on a phone, it gives up after a moment and offers "Try again" |
| ✅ | **Screenshot blocking** | On everywhere. Can be allowed per chat after a fingerprint check |
| ✅ | **Hidden chats** | Long-press the wordmark and authenticate to see them; their notifications show no name |
| ✅ | **Panic wipe** | Hold 2 seconds, even from the lock screen. Destroys the keys and data, then closes the app |
| ✅ | **Encrypted storage** | SQLite with every row sealed (XChaCha20-Poly1305), keys in Android Keystore, no backups |
| ✅ | **Over-the-air updates** | Checked and downloaded through Tor, with changelogs |
| ✅ | **Send feedback** | Stars, what you think, and (if you like) your phone's model and Android version, sent straight to the developer's own Sanctum through Tor. Nobody sees who sent it |
| ✅ | **Media** | Photos up to 10 MB, videos up to 20 MB, files up to 50 MB, end-to-end encrypted and stored encrypted. Photos are resized and lose hidden metadata like location. Nothing goes to your gallery by itself: use **Save to phone** on a photo, video, voice note or file to keep a copy (a Sanctum folder, no extra permission) |
| ✅ | **Voice notes** | Hold the mic to record, release to send, slide left to cancel. Encrypted like any attachment |
| ✅ | **Sanctum PIN & decoy PIN** | An optional PIN of its own (fingerprint still works). A decoy PIN opens an empty-looking Sanctum with its own data, while your real chats stay hidden and silent |

### Milestone v2: more than messages

| | Feature | Notes |
|---|---|---|
| ✅ | **Polls, date polls and plans** | Ask a question, find a day that works for everyone, or plan something (when and where) and see who's going. In friends' chats and groups |
| ✅ | **Shared albums** | Albums in any chat or group. Everyone can add photos and videos, up to 30 at a time |
| ✅ | **Daily moments** | One photo a day for all your friends, gone after 24 hours. Take it on the spot or pick one, and see friends' moments above your chats. Limit each look to a few seconds or make it view once, and see who saw yours (you can turn that off; then they don't learn when you look either). A gold ring marks a friend with a new moment |
| ✅ | **Reactions** | Hold a message for ❤️ 😂 😮 😢 🙏 👍 or any emoji, or double-tap for ❤️. Tap a reaction to add or take back yours; hold it to see who. Friends' chats and groups |
| ✅ | **Edit messages** | Hold your message and pick Edit to fix its words or a caption, within 15 minutes of sending (notes any time). Everyone sees the new words, marked "edited". Friends' chats and groups |
| ✅ | **Mute** | ⋮ in a chat: silence its notifications for an hour, 8 hours, a week or until you turn them back on. Messages still arrive and count as unread; a muted chat shows a small bell |
| ✅ | **Drafts** | What you type but don't send stays in the box when you leave the chat, and the chat list shows "Draft:" |
| ✅ | **Pin a message** | Hold a message and pick Pin: it shows at the top of the chat for everyone in it. Tap the bar to jump to it. Friends' chats, groups and notes |
| ✅ | **Forward** | Hold a message and pick Forward to send a copy of its words, photo, file or place to one or more of your chats |
| ✅ | **Disappearing messages** | ⋮ in a friend's chat or group: new messages are deleted from every phone after 1 hour, 8 hours, a day, a week or 90 days. Everyone sees who changed it; older messages stay |
| ✅ | **Open once** | + → Open once in a friend's chat: a photo or video with no preview that they can open one time. Then it's gone from both phones. It can't be saved, copied or forwarded (nothing stops another camera) |
| ✅ | **Share a place** | + → Location: sends where your phone is, once (it doesn't follow you). Found by the phone itself, no Google. Shown as numbers with "Open in maps app"; no map is fetched |
| ✅ | **Send later** | Type a message, then + → Send later and pick a day and time. ⋮ → Scheduled messages lists them to cancel. The phone has to be on with Sanctum running then; otherwise it goes as soon as it is |
| ✅ | **Typing** | "Asha is typing…" in friends' chats and groups. Sent only while you're both connected, never stored, and never what you type |
| ✅ | **Settings** | Tap your sigil: your name, then tidy cards that each open their own page: appearance (light or dark, or like the phone, and an accent colour: your sigil's or your pick), moments, calls, lock, staying connected and nearby, updates, feedback and panic wipe |
| ✅ | **The look** | One bold, rounded font throughout. A chat with new messages glows in its own colour and pulses when another arrives. Pull down on Chats or Friends and the Sanctum door opens to check for news. Chats greets you by name with the date, light mode is warm paper rather than cold white, empty screens have little drawings, and the bottom bar highlights the open tab with its name |
| ✅ | **Search** | Type in the home search box to find any message, poll, plan or file name across your chats. Done on the phone; hidden chats only while revealed |
| ✅ | **Catch-up** | "While you were away": who wrote, who mentioned you, plans to answer and polls waiting for you |

### Milestone v3: calls

| | Feature | Notes |
|---|---|---|
| ✅ | **Voice calls** | Call a friend from their chat. Private through Tor by default (nobody learns who calls whom), faster on the same Wi-Fi, and nobody in between can listen. Rings like a phone call, works from the lock screen without saying who's calling, and leaves a record in the chat ("Voice call · 4:12", "Missed call") |
| ✅ | **Video calls** | Start a call with video or turn your camera on during a call; switch cameras, answer without video. Lower quality and choppier through Tor; sharp on the same Wi-Fi or with Faster calls. A hidden chat's video waits until you unlock |
| ✅ | **Faster calls** | An opt-in Settings switch: calls connect straight between your phones when your networks allow it (less delay, sharper video), else stay private through Tor. Your friend's phone learns your internet address. Used only if both of you turn it on. The call screen says Private, Nearby or Direct |

## For developers

Everything below is for people working on the code.

### How it works

- **`core/` (Rust):** the identity (Ed25519; the same key is your Tor onion address), MLS (OpenMLS), the encrypted store, the protocol engine, groups, temp chats and sigils. It does no I/O, so whole conversations between phones are tested in-process.
- **`net/` (Rust):** connections. Tor through Arti (onion services, plus HTTPS for updates), nearby Wi-Fi discovery, and the node that keeps live links to friends.
- **`app/` (Flutter):** the UI. `app/rust/` is the bridge to Dart (flutter_rust_bridge). Android-specific code is in `app/android/` (scanner, background service, notifications, installer, Bluetooth nearby, and call audio, which talks to Rust directly rather than through Dart).
- **`docs/DESIGN.md`:** the full design: architecture, encryption, protocol, groups, nearby, calls, updates and the UI system.

### Build and test

```sh
cargo test --workspace              # core + network (Tor bootstrap needs the internet)
cargo build -p rust_lib_sanctum     # the host library the Flutter tests load
cd app && flutter analyze && flutter test
```

Every push to `main` runs the tests, builds a signed arm64 APK and publishes it as release `build-N` (the last published build plus one), with a changelog made from the commit messages. The signing key lives only in the repository secrets (`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`).

The website lives in `website/` and is published to GitHub Pages on its own; changing only the website doesn't make a new app build.

Contributing with an AI agent? Read [AGENTS.md](AGENTS.md).
