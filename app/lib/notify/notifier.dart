import 'dart:async';

import 'package:flutter/widgets.dart';

import '../background/background.dart';
import '../net/net_bus.dart';
import '../security/profile.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';

/// Unread counts, and notifications while the app is in the background.
///
/// Chats are keyed `c:ID` (chat id) and temp chats `t:ID` (temp id). Notifications
/// say only who wrote ("New message"), never what: Sanctum's content stays
/// inside Sanctum, and on the lock screen even the name is hidden.
class Notifier with WidgetsBindingObserver {
  Notifier._();

  static final instance = Notifier._();

  final unread = ValueNotifier<Map<String, int>>(const {});

  /// The chat on screen, if any: it gets no badge and no notification.
  String? viewing;

  bool _foreground = true;
  bool _netBackground = false;
  bool _started = false;
  final _opens = StreamController<String>.broadcast();

  /// Chats to open because their notification was tapped.
  Stream<String> get opens => _opens.stream;

  static String chatKey(int id) => 'c:$id';
  static String tempKey(int id) => 't:$id';

  static int _notificationId(String key) {
    final id = int.parse(key.substring(2));
    // 1: the connection notice, 2: friend requests.
    return key.startsWith('c:') ? 1000 + id : 1000000 + id;
  }

  void start() {
    if (_started) return;
    _started = true;
    WidgetsBinding.instance.addObserver(this);
    NetBus.instance.events.listen(_onEvent);
    Background.onOpen(_opens.add);
    _resumed();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _foreground = state == AppLifecycleState.resumed;
    // Closed: pace reconnects for battery. Back: reconnect everyone now.
    final background = state == AppLifecycleState.hidden || state == AppLifecycleState.paused;
    if (background != _netBackground && (background || _foreground)) {
      _netBackground = background;
      netSetBackground(background: background);
    }
    if (_foreground) _resumed();
  }

  Future<void> _resumed() async {
    if (viewing != null) markRead(viewing!);
    final open = await Background.takeOpen();
    if (open != null) _opens.add(open);
    // Starting the service needs the app on screen (Android 12+).
    if (await Background.enabled()) {
      await Background.start();
      await Background.requestNotifications();
    }
  }

  void markRead(String key) {
    if (unread.value.containsKey(key)) unread.value = {...unread.value}..remove(key);
    Background.cancel(_notificationId(key));
  }

  Future<void> _onEvent(AppEvent e) async {
    switch (e.kind) {
      case AppEventKind.message:
        await _arrived(chatKey(e.id.toInt()));
      case AppEventKind.tempMessage:
        await _arrived(tempKey(e.id.toInt()));
      case AppEventKind.tempGone:
        markRead(tempKey(e.id.toInt()));
      case AppEventKind.request:
        if (!_foreground && !Profile.decoy.value) await _notifyRequest();
      default:
        break;
    }
  }

  Future<void> _arrived(String key) async {
    // The decoy shows: the real profile stays silent.
    if (Profile.decoy.value) return;
    if (_foreground && viewing == key) return;
    final count = (unread.value[key] ?? 0) + 1;
    unread.value = {...unread.value, key: count};
    if (_foreground) return;
    final temp = key.startsWith('t:');
    final id = int.parse(key.substring(2));
    final chat = temp ? null : (await chatList()).where((c) => c.id == id).firstOrNull;
    // A hidden chat's notification doesn't say whose it is.
    final title = temp
        ? (await tempList()).where((t) => t.id.toInt() == id).firstOrNull?.title
        : chat?.hidden ?? true
            ? null
            : chat?.title;
    final what = temp ? 'temp message' : 'message';
    await Background.notify(
      id: _notificationId(key),
      channel: Background.messages,
      title: title ?? 'Sanctum',
      body: count == 1 ? 'New $what' : '$count new ${what}s',
      open: key,
      count: count,
    );
  }

  Future<void> _notifyRequest() async {
    final requests = await requestList();
    if (requests.isEmpty) return;
    await Background.notify(
      id: 2,
      channel: Background.requests,
      title: 'Friend request',
      body: requests.length == 1 ? '${requests.first.name} wants to add you' : '${requests.length} friend requests',
      open: 'friends',
      count: requests.length,
    );
  }
}
