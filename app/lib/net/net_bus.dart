import 'dart:async';

import 'package:flutter/foundation.dart';

import '../background/background.dart';
import '../update/updater.dart';
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

  /// Friends' chats whose friend is on the same Wi-Fi or hotspot.
  final nearby = ValueNotifier<Set<int>>(const {});
  Timer? _nearbyPoll;
  Timer? _advertise;

  /// Whether Bluetooth nearby started (needs Android 10+, Bluetooth on and
  /// the Nearby devices permission).
  final bluetooth = ValueNotifier<bool>(false);
  final _events = StreamController<AppEvent>.broadcast();
  StreamSubscription<AppEvent>? _sub;
  Future<void> Function()? _connect;

  Stream<AppEvent> get events => _events.stream;

  void start() {
    _sub ??= appEvents().listen((e) {
      if (e.kind == AppEventKind.status) {
        status.value = e.net;
        refreshOnline();
        if (e.net.status == NetStatus.online) Updater.instance.check();
      }
      if (e.kind == AppEventKind.presence) refreshOnline();
      _events.add(e);
    });
  }

  Future<void> refreshOnline() async {
    final chats = (await onlineChats()).map((id) => id.toInt()).toSet();
    if (!setEquals(chats, online.value)) online.value = chats;
    final near = (await nearbyChats()).map((id) => id.toInt()).toSet();
    if (!setEquals(near, nearby.value)) nearby.value = near;
  }

  /// Turns nearby (same Wi-Fi / hotspot, no internet needed) on or off.
  Future<void> setNearby(bool on) async {
    await Background.nearbyLock(on);
    await nearbySet(enabled: on);
    await _setBluetooth(on);
    _nearbyPoll?.cancel();
    // Beacons come and go without events; look every few seconds.
    _nearbyPoll = on ? Timer.periodic(const Duration(seconds: 5), (_) => refreshOnline()) : null;
    await refreshOnline();
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

  Future<void> _setBluetooth(bool on) async {
    _advertise?.cancel();
    _advertise = null;
    if (!on) {
      await Background.bluetoothStop();
      await btStop();
      bluetooth.value = false;
      return;
    }
    final rustPort = await nearbyPort();
    final relay = rustPort == null ? null : await Background.bluetoothStart(rustPort);
    if (relay == null) {
      bluetooth.value = false;
      return;
    }
    await btStart(proxyPort: relay);
    Background.onBluetoothSeen((id, handle) async {
      await btSeen(id: id, handle: BigInt.from(handle));
      await refreshOnline();
    });
    // The ID rotates every minute; re-advertise a little more often.
    Future<void> advertise() async => Background.bluetoothAdvertise(Uint8List.fromList(await btMyId()));
    await advertise();
    _advertise = Timer.periodic(const Duration(seconds: 20), (_) => advertise());
    bluetooth.value = true;
  }

  /// Tries again after Tor failed to start.
  Future<void> retry() async => _connect?.call();
}
