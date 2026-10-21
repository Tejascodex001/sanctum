# Sanctum

Private chat for close friends (a circle of 100–200 people at most), on Android.

There are **no servers and no accounts**, and nothing is tracked. Phones talk directly to each other over **Tor**, so nobody, including your friends, sees your IP address (unless you turn on an opt-in faster mode for Reels or calls, which says what it shares). Everything stays on your phone, encrypted, and there are no cloud backups.

The UI is the **Sigil + Deck** design: every person and chat gets its own generated sigil, and chats are stacked cards.

## Install

1. Download `app-release.apk` from the latest [release](../../releases/latest) (`build-N`).
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
| ✅ | **Group chats** | Peer to peer, no server. Up to 20 people for now (50 once members help pass messages on). The admin manages members; members who aren't friends still reach each other |
| ✅ | **Temp chats** | Kept in memory only, on both phones. Each message burns 24 hours after it's sent, and either person can wipe the chat for both |
| ✅ | **Nearby (Wi-Fi / hotspot)** | Chat with friends on the same Wi-Fi or one phone's hotspot, with no internet. Opt-in |
| ✅ | **Nearby over Bluetooth** | For friends who've connected once before. Android 10+. Part of the same Nearby switch |
| ⏳ | Nearby over Wi-Fi Direct | Mostly covered by hotspot mode |
| ✅ | **Replies** | Swipe a message right; tap the quote to jump to the original |
| ✅ | **Threads** | Messages with replies show "↳ N replies"; tap to see the whole thread and answer in it |
| ✅ | **Delete & clear** | Delete a message (for me / for everyone), clear a chat (for me / for everyone), remove a friend (it's removed on both phones) |
| ✅ | **Background delivery** | A foreground service keeps you reachable while the app is closed. A one-tap shortcut opens your phone maker's battery page |
| ✅ | **Notifications** | For every message, even while Sanctum is open, except in the chat you're looking at. Posted on the phone itself, never through Google. They say who wrote, never what. On the lock screen they say only "New message" |
| ✅ | **App lock** | Fingerprint, face or PIN, every time the app opens |
| ✅ | **Screenshot blocking** | On everywhere. Can be allowed per chat after a fingerprint check |
| ✅ | **Hidden chats** | Long-press the wordmark and authenticate to see them; their notifications show no name |
| ✅ | **Panic wipe** | Hold 2 seconds, even from the lock screen. Destroys the keys and data, then closes the app |
| ✅ | **Encrypted storage** | SQLite with every row sealed (XChaCha20-Poly1305), keys in Android Keystore, no backups |
| ✅ | **Over-the-air updates** | Checked and downloaded through Tor, with changelogs |
| ✅ | **Media** | Photos up to 10 MB, videos up to 20 MB, files up to 50 MB, end-to-end encrypted and stored encrypted. Photos are resized and lose hidden metadata like location |
| ✅ | **Voice notes** | Hold the mic to record, release to send, slide left to cancel. Encrypted like any attachment |
| ✅ | **Sanctum PIN & decoy PIN** | An optional PIN of its own (fingerprint still works). A decoy PIN opens an empty-looking Sanctum with its own data, while your real chats stay hidden and silent |

### Milestone v2: more than messages

| | Feature | Notes |
|---|---|---|
| ✅ | **Polls, date polls and plans** | Ask a question, find a day that works for everyone, or plan something (when and where) and see who's going. In friends' chats and groups |
| ✅ | **Shared albums** | Albums in any chat or group. Everyone can add photos and videos, up to 30 at a time |
| ✅ | **Daily moments** | One photo a day for all your friends, gone after 24 hours. Take it on the spot or pick one, and see friends' moments above your chats |
| ✅ | **Reactions** | Hold a message for ❤️ 😂 😮 😢 🙏 👍 or any emoji, or double-tap for ❤️. Tap a reaction to add or take back yours; hold it to see who. Friends' chats and groups |
| ✅ | **Typing** | "Asha is typing…" in friends' chats and groups. Sent only while you're both connected, never stored, and never what you type |
| ✅ | **Settings** | Tap your sigil: your name, light or dark theme (or like the phone), accent colour (your sigil's or your pick), lock, staying connected, nearby, updates and panic wipe |
| ✅ | **Reels** | Short upright videos from Loops (plus PeerTube and Mastodon sites), full screen; swipe for the next. Pick what you love watching first (Comedy, Music, Dance, Animals…) and Reels shows only those. Heart them (or double-tap); Reels learns what you like from hearts, shares, what you watch to the end and what you skip, all on your phone, and picks the next ones for you. Watched anonymously through Tor, with no account; or turn on Faster reels in Settings to load them directly (faster, but the sites see your internet address). Pick your own sites, and send a reel's link to several chats at once from a grid of sigils, with a message. (Pixelfed can't be shown: it needs an account) |
| ✅ | **Search** | Type in the home search box to find any message, poll, plan or file name across your chats. Done on the phone; hidden chats only while revealed |
| ✅ | **Catch-up** | "While you were away": who wrote, who mentioned you, plans to answer and polls waiting for you |

### Milestone v3: calls

| | Feature | Notes |
|---|---|---|
| ✅ | **Voice calls** | Call a friend from their chat. Private through Tor by default (nobody learns who calls whom), faster on the same Wi-Fi, and nobody in between can listen. Rings like a phone call, works from the lock screen without saying who's calling, and leaves a record in the chat ("Voice call · 4:12", "Missed call") |
| ✅ | **Video calls** | Start a call with video or turn your camera on during a call; switch cameras, answer without video. Lower quality and choppier through Tor; sharp on the same Wi-Fi or with Faster calls. A hidden chat's video waits until you unlock |
| ✅ | **Faster calls** | An opt-in Settings switch: calls connect straight between your phones when your networks allow it (less delay, sharper video), else stay private through Tor. Your friend's phone learns your internet address. Used only if both of you turn it on. The call screen says Private, Nearby or Direct |

## How it works

- **`core/` (Rust):** the identity (Ed25519; the same key is your Tor onion address), MLS (OpenMLS), the encrypted store, the protocol engine, groups, temp chats and sigils. It does no I/O, so whole conversations between phones are tested in-process.
- **`net/` (Rust):** connections. Tor through Arti (onion services, plus HTTPS for updates), nearby Wi-Fi discovery, and the node that keeps live links to friends.
- **`app/` (Flutter):** the UI. `app/rust/` is the bridge to Dart (flutter_rust_bridge). Android-specific code is in `app/android/` (scanner, background service, notifications, installer, Bluetooth nearby, and call audio, which talks to Rust directly rather than through Dart).
- **Reels** read public Fediverse sites (`net/src/fedi.rs`) and are ranked on the phone (`core/src/taste.rs`); nothing about what you watch leaves the phone.
- **`docs/DESIGN.md`:** the full design: architecture, encryption, protocol, groups, nearby, calls, Reels, updates and the UI system.

## Build and test

```sh
cargo test --workspace              # core + network (Tor bootstrap needs the internet)
cargo build -p rust_lib_sanctum     # the host library the Flutter tests load
cd app && flutter analyze && flutter test
```

Every push to `main` runs the tests, builds a signed arm64 APK and publishes it as release `build-<run number>`, with a changelog made from the commit messages. The signing key lives only in the repository secrets (`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`).

Contributing with an AI agent? Read [AGENTS.md](AGENTS.md).
