import 'dart:async';

import 'package:flutter/foundation.dart';

import '../src/rust/api/session.dart';

/// Live updates from Rust: new messages, delivery receipts, temp chats
/// appearing or being wiped, and the Tor connection status.
class NetBus {
  NetBus._();

  static final instance = NetBus._();

  final status = ValueNotifier<NetStatus>(NetStatus.off);
  final _events = StreamController<AppEvent>.broadcast();
  StreamSubscription<AppEvent>? _sub;

  Stream<AppEvent> get events => _events.stream;

  void start() {
    _sub ??= appEvents().listen((e) {
      if (e.kind == AppEventKind.status) status.value = e.status;
      _events.add(e);
    });
  }
}
