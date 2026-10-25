# Working on Sanctum (for coding agents)

Sanctum is a private, serverless Android chat app for a small circle of friends. Read `README.md` for features and status, and `docs/DESIGN.md` for how everything works. This file is about how to work here.

## Non-negotiables

- **Privacy:**
  - The app connects to friends and update servers **only through Tor**. Two opt-in exceptions, both off by default: Nearby (the local Wi-Fi and Bluetooth); and "Faster calls", which sends a call's sealed audio and video straight between the two phones (UDP) only when **both** have it on, so each learns the other's address.
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
| `core/src/group_sync.rs` | Members help deliver: group messages kept for a week, lists of what each member has swapped on every link, offers before resends, files fetched from other members. Only with phones that said so in the handshake (`protocol::FEATURES`) |
| `core/src/poll.rs` | Polls, date polls and plans, and everyone's answers |
| `core/src/recall.rs` | Search across chats and the "While you were away" catch-up |
| `core/src/reaction.rs` | Emoji reactions: one per person per message, newest wins |
| `core/src/typing.rs` | Typing signals: link-level, never stored or queued |
| `core/src/call.rs` | 1:1 voice and video calls: signaling inside MLS, sealed media packets (`MediaHub`), replay window, jitter buffer |
| `core/src/video.rs` | Video in calls: fragments and reassembly, rate control, sound before pictures |
| `core/src/direct.rs` | Faster calls: direct routes, the address filter, probing and falling back to the link |
| `core/src/moment.rs` | Daily moments: one photo a day for every friend, gone after 24 hours |
| `core/src/store.rs` | The encrypted SQLite store. Every row is sealed; index columns are keyed hashes |
| `core/src/protocol.rs` | Wire types: `Frame` (link level) and `Content` (inside MLS) |
| `core/src/mls.rs` | The OpenMLS wrapper |
| `net/src/engine.rs` | The `Node`: handshakes, live links, keepalive, reconnect with backoff, resends |
| `net/src/tor.rs` | Arti (Tor): onion service, dialing, a connector for HTTPS |
| `net/src/nearby.rs` | Local-network discovery (per-friend rotating tags) and the `Reach` transport |
| `core/src/nearby.rs` | Rotating Bluetooth IDs and the friends-only nearby key exchange |
| `net/src/https.rs` | A small HTTPS client for updates, through Tor |
| `net/src/direct.rs` | The UDP socket for Faster calls |
| `app/rust/src/api/` | The Dart-facing API (flutter_rust_bridge). Keep it thin; logic belongs in `core` |
| `app/rust/src/call_jni.rs` | JNI functions Kotlin calls directly for call audio (`CallNative.kt`), bypassing Dart |
| `app/lib/chat/media.dart` | Photos, videos, voice notes and files in chats: viewing, opening in other apps, and "Save to phone" (with `MainActivity.kt` and `MediaPrep.kt`) |
| `app/lib/call/` | The call controller (ring, audio, service) and the call screen, shown above the app lock |
| `app/lib/settings/` | The Settings screen (opened from your sigil) |
| `app/lib/theme/appearance.dart` | Light, dark or like the phone, and the accent colour. `Palette` is read at build time, so `Restyler` redraws every screen when it changes: don't make widgets using `Palette` colours `const`. Sigils are shown through `SigilLook` (`spec.look`): in the picked accent and light in light mode. Use `spec.look.accent.color` for UI colours taken from a sigil; only comparing sigils with a friend uses `exact: true` |
| `app/lib/version.dart` | The version people see (`beta-1.0.0` while in beta; the owner decides when it leaves beta). Must match `pubspec.yaml` and the `beta-` prefix in `build.gradle.kts` (a test checks). Releases stay tagged `build-N`: installed apps look for that tag to update |
| `docs/brand/` | The logo (the Sanctuary door, gold on midnight). `make_icons.py` writes the Android icon vectors from one drawing; app icon resources are in `app/android/app/src/main/res/` |
| `app/lib/` | The Flutter UI. Design tokens are in `app/lib/theme/tokens.dart`; use them, never one-off values |
| `app/android/app/src/main/kotlin/` | The scanner, background service, notifications, installer, OEM battery settings, Bluetooth nearby (`BluetoothNearby.kt`) and calls (`CallAudio.kt`, `CallVideo.kt`, `CallService.kt`, `CallNative.kt`) |

## Rules that keep data and the wire compatible

- **Stored formats are postcard, which isn't self-describing:**
  - Never reorder or remove fields of stored structs (`ChatMeta`, `StoredMessage`, `OutboxItem`…).
  - To add a field, rename the current struct to a `…V<n>` "old format" type, add the field to the new one, and decode newest first. Look at `decode_chat` and `decode_message`; an older record fails to decode as a newer one because it ends early.
  - Add a test that writes the old format and reads it back (see `chats_from_before_hidden_chats_upgrade`).
- **Wire enums (`Frame`, `Content`, `OutboxState`): add variants at the end only.**
- **A `Frame` an older build doesn't know drops its link.** Send a new frame only to phones whose handshake said they understand it: add a bit to `protocol::FEATURES` and check what `Session::link_features` was told (see `group_sync`).
- **Secure storage:** `resetOnError` stays `false` in `app/lib/identity/secrets.dart`, and keys are created only when a read returns nothing. A failed read must stop startup, never make new keys: that would orphan the user's database.
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
- **Screenshots:** `cd app && SCREENSHOT_DIR=<dir> flutter test` saves PNGs of the main screens (dark and light, and the call screens). Look at them after UI changes.

## Product notes

- **Everything the app shows is for its users, who are not developers.** Don't mention keys, builds, Tor internals, onion addresses, servers, protocols or Android versions unless the user must act on it, and never show raw error text. Explain in plain words what happens and what to do.

- The owner tests on real phones by installing release APKs, and is not a developer. Explain changes plainly, and say which build to install.
- **UI:** the custom "Sigil" design system on Material 3. It should look polished but striking, and stay smooth: sigils are cached images, and there are no backdrop blurs.
- **Roadmap and status:** the feature table in `README.md`. Update it when a feature lands.
