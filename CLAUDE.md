@AGENTS.md

## Notes for Claude

- Keep README.md's feature table, docs/DESIGN.md and AGENTS.md current when features or conventions change.
- After pushing, check the GitHub Actions run, and tell the owner which `build-N` to install (or that the in-app updater will offer it).
- The owner isn't a developer: explain in plain words what changed, what to try on the phones, and which build. Say plainly what couldn't be verified here (Android audio/video, live sites).

## Working here (lessons so far)

- **No network to outside sites from the sandbox** (Fediverse/Loops servers, phones). Test parsers with JSON fixtures, and read upstream source (e.g. raw.githubusercontent.com) to learn an API's shape. Don't claim something works against a live site you couldn't reach.
- **Look at the UI before shipping it:** `cd app && SCREENSHOT_DIR=<dir> flutter test` writes PNGs from `test/home_test.dart` and `test/call_test.dart` (light and dark); open them and check. Emoji show as boxes there (no emoji font); that's fine.
- **Disk space is limited:** use `CARGO_INCREMENTAL=0`; `cargo clean` frees ~16 GB if a build fails for space.
- **Parallel helpers** work in git worktrees under `.claude/worktrees/` (ignored). Give each a disjoint set of files; after merging, resolve conflicts in generated bindings by regenerating them (`flutter_rust_bridge_codegen generate`), then rebuild the host library. Remove the worktree afterwards.
- **CI cancels a running build when a newer push arrives**, and the changelog rolls a cancelled or failed build's commits into the next one, so a `Docs:` push right after a feature push is harmless. Avoid pushing many tiny commits in a row.
- **Kotlin can't be compiled here** (no Android SDK). Helpers have checked Kotlin by compiling against a stand-alone Android framework jar plus Flutter's `flutter.jar`; androidx code still only gets checked in CI. Watch the CI run after Kotlin changes.
- **Product decisions to keep:** Tor is the default for everything; faster direct modes ("Faster reels", "Faster calls") are opt-in and say what the other side learns. Reels show videos on black even in light mode. Picked Reels interests are a strict filter. Pixelfed and Instagram can't be used without an account. Versioning: the app is **beta-1.0.0** until the owner says to move to official versions; releases are still tagged `build-N`. The logo is the Sanctuary door, gold on midnight (`docs/brand/`). No "Mailbox" (spare always-on phone holding messages): the owner ruled it out; offline delivery comes from group members syncing with each other.

