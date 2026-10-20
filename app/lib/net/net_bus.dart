import 'dart:async';

import 'package:flutter/foundation.dart';

import '../src/rust/api/session.dart';

/// Live updates from Rust: new messages, delivery receipts, temp chats
/// appearing or being wiped, and the Tor connection status.
class NetBus {
  NetBus._();

  static final instance = NetBus._();

  final status = ValueNotifier<NetInfo>(
    const NetInfo(status: NetStatus.off, progress: 0, detail: ''),
  );
  final _events = StreamController<AppEvent>.broadcast();
  StreamSubscription<AppEvent>? _sub;
  Future<void> Function()? _connect;

  Stream<AppEvent> get events => _events.stream;

  void start() {
    _sub ??= appEvents().listen((e) {
      if (e.kind == AppEventKind.status) status.value = e.net;
      _events.add(e);
    });
  }

  /// Starts Tor in the background; chats work offline meanwhile.
  Future<void> connect({
    required String stateDir,
    required String cacheDir,
    required List<int> identitySecret,
  }) {
    _connect = () => netStart(stateDir: stateDir, cacheDir: cacheDir, identitySecret: identitySecret);
    return _connect!();
  }

  /// Tries again after Tor failed to start.
  Future<void> retry() async => _connect?.call();
}
