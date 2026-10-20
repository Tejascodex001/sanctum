# Sanctum

Private, serverless chat for friends. No accounts, no tracking, no IP exposure.

- `core/`: Rust core (identity, sigils, temp chats, transports)
- `app/`: Flutter Android app (Sigil + Deck UI); `app/rust/` is the Dart bridge over the core
- `docs/DESIGN.md`: architecture and roadmap

```
cargo test --workspace
cargo build -p rust_lib_sanctum   # host library for Flutter tests
cd app && flutter test
```

Every push to `main` builds a release APK and publishes it under Releases.
