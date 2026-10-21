# Working on Sanctum (for coding agents)

Sanctum is a private, serverless Android chat app for a small circle of friends. Read `README.md` for features and status, and `docs/DESIGN.md` for how everything works. This file is about how to work here.

## Non-negotiables

- **Privacy:**
  - The app connects to friends, update servers and the Fediverse sites Reels reads **only through Tor**. Two opt-in exceptions, both off by default: Nearby (the local Wi-Fi and Bluetooth), and "Faster reels", which fetches Reels (and only Reels) directly so the video sites see the phone's address. With Faster reels off, video plays through the on-phone relay (`net/src/relay.rs`); never hand a player or WebView a remote URL then.
  - No analytics, no crash reporting, no Google services (no ML Kit, no Firebase, no Play Services). Notifications are posted on the phone itself.
  - Message content never leaves the encrypted store or MLS: not in notifications, logs or URLs.
- **Strictly on-device:** no backups (`allowBackup` stays off), no cloud.
- **Never commit secrets:** no keystores (`*.jks`), no `key.properties`, no passwords. Release signing comes from the GitHub secrets in CI only.

## Git workflow

- Commit and push straight to `main`. Don't create branches or pull requests unless asked.
- **No `Co-Authored-By` lines** in commit messages.
- Author: `Tejascodex001 <158545390+Tejascodex001@users.noreply.github.com>`. Set it in a fresh clone with `git config user.name` and `git config user.email`.
- **Commit messages become the in-app changelog** (`.github/scripts/changelog.py`), which **ordinary users** read. Write them for users:
  - A short subject saying what changed.
  - A blank line, then `- ` bullets with details. Bullets can continue on lines indented by two spaces.
  - Use plain words: no jargon (MLS, Argon2, crate or file names, protocol details).
  - Start the subject with `Internal:`, `Docs:` or `CI:` for developer-only changes; they're left out of the changelog.
- Every push to `main` builds and publishes release `build-<run number>`. A red build means no release; fix it before moving on.

## Layout

| Path | What |
|---|---|
| `core/src/session.rs` | The protocol engine: friends, sending, frames in and out, deleting. I/O-free; tested by passing frames between in-memory "phones" |
| `core/src/group.rs` | Groups: single admin, invites, welcomes, broadcasting |
| `core/src/poll.rs` | Polls, date polls and plans, and everyone's answers |
| `core/src/recall.rs` | Search across chats and the "While you were away" catch-up |
| `core/src/reaction.rs` | Emoji reactions: one per person per message, newest wins |
| `core/src/typing.rs` | Typing signals: link-level, never stored or queued |
| `core/src/moment.rs` | Daily moments: one photo a day for every friend, gone after 24 hours |
| `core/src/store.rs` | The encrypted SQLite store. Every row is sealed; index columns are keyed hashes |
| `core/src/protocol.rs` | Wire types: `Frame` (link level) and `Content` (inside MLS) |
| `core/src/mls.rs` | The OpenMLS wrapper |
| `net/src/engine.rs` | The `Node`: handshakes, live links, keepalive, reconnect with backoff, resends |
| `net/src/tor.rs` | Arti (Tor): onion service, dialing, a connector for HTTPS |
| `net/src/nearby.rs` | Local-network discovery (per-friend rotating tags) and the `Reach` transport |
| `core/src/nearby.rs` | Rotating Bluetooth IDs and the friends-only nearby key exchange |
| `net/src/https.rs` | A small HTTPS client for updates and Reels, through Tor |
| `net/src/fedi.rs` | Reels: reading short videos from PeerTube and Mastodon sites anonymously |
| `net/src/relay.rs` | The on-phone relay that streams Reels videos through Tor |
| `app/rust/src/api/` | The Dart-facing API (flutter_rust_bridge). Keep it thin; logic belongs in `core` |
| `app/lib/settings/` | The Settings screen (opened from your sigil) |
| `app/lib/theme/appearance.dart` | Light, dark or like the phone, and the accent colour. `Palette` is read at build time, so `Restyler` redraws every screen when it changes: don't make widgets using `Palette` colours `const` |
| `app/lib/` | The Flutter UI. Design tokens are in `app/lib/theme/tokens.dart`; use them, never one-off values |
| `app/android/app/src/main/kotlin/` | The scanner, background service, notifications, installer, OEM battery settings and Bluetooth nearby (`BluetoothNearby.kt`) |

## Rules that keep data and the wire compatible

- **Stored formats are postcard, which isn't self-describing:**
  - Never reorder or remove fields of stored structs (`ChatMeta`, `StoredMessage`, `OutboxItem`…).
  - To add a field, rename the current struct to a `…V<n>` "old format" type, add the field to the new one, and decode newest first. Look at `decode_chat` and `decode_message`; an older record fails to decode as a newer one because it ends early.
  - Add a test that writes the old format and reads it back (see `chats_from_before_hidden_chats_upgrade`).
- **Wire enums (`Frame`, `Content`, `OutboxState`): add variants at the end only.**
- **Message sequence numbers are never reused** (`chat_seq`), because pending delivery receipts point at them.

## Building and testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets      # keep it warning-free
cargo build -p rust_lib_sanctum             # before flutter test, after any Rust change
cd app && flutter analyze && flutter test   # analyze must report no issues (CI fails on infos)
```

- **After changing anything in `app/rust/src/api/`,** regenerate the bindings with `flutter_rust_bridge_codegen generate` (run in `app/`) and commit the generated files. Then rebuild the host library, or Flutter tests fail with a content-hash mismatch.
- **Format Dart** with `dart format -l 120 --language-version=3.6 <files>`, which is the existing style. Don't reformat files you didn't touch.
- **Tests:**
  - New protocol behaviour gets a test in `core` using the `Phone` + `connect()` helpers.
  - Network behaviour gets a test in `net` over loopback TCP.
  - Run timing-sensitive network tests several times; they must not be flaky.
- **No Android SDK locally:** CI compiles the Kotlin and the APK, so review Kotlin changes carefully and check the CI run after pushing.
- `net/src/https.rs` has an ignored live test against GitHub: `cargo test -p sanctum-net live -- --ignored`. Behind a TLS-inspecting proxy, set `EXTRA_CA`.

## Product notes

- **Everything the app shows is for its users, who are not developers.** Don't mention keys, builds, Tor internals, onion addresses, servers, protocols or Android versions unless the user must act on it, and never show raw error text. Explain in plain words what happens and what to do.

- The owner tests on real phones by installing release APKs, and is not a developer. Explain changes plainly, and say which build to install.
- **UI:** the custom "Sigil" design system on Material 3. It should look polished but striking, and stay smooth: sigils are cached images, and there are no backdrop blurs.
- **Roadmap and status:** the feature table in `README.md`. Update it when a feature lands.
