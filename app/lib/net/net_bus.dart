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
  /// Friends' chats whose friend is connected right now.
  final online = ValueNotifier<Set<int>>(const {});
  final _events = StreamController<AppEvent>.broadcast();
  StreamSubscription<AppEvent>? _sub;
  Future<void> Function()? _connect;

  Stream<AppEvent> get events => _events.stream;

  void start() {
    _sub ??= appEvents().listen((e) {
      if (e.kind == AppEventKind.status) {
        status.value = e.net;
        refreshOnline();
      }
      if (e.kind == AppEventKind.presence) refreshOnline();
      _events.add(e);
    });
  }

  Future<void> refreshOnline() async {
    final chats = (await onlineChats()).map((id) => id.toInt()).toSet();
    if (!setEquals(chats, online.value)) online.value = chats;
  }

  bool isOnline(int? chatId) => chatId != null && online.value.contains(chatId);

  /// Pull to refresh: reconnect to friends now (and to Tor, if it failed).
  Future<void> refresh() async {
    if (status.value.status == NetStatus.failed) {
      await retry();
    } else {
      netRefresh();
    }
    await refreshOnline();
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
