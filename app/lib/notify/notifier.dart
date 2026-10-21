import 'dart:async';

import 'package:flutter/widgets.dart';

import '../background/background.dart';
import '../call/call_controller.dart';
import '../net/net_bus.dart';
import '../security/profile.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';

/// Unread counts, and a notification for every message, except in the chat
/// that's on screen.
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
    // 1: the connection notice, 2: friend requests, 3: feedback.
    return key.startsWith('c:') ? 1000 + id : 1000000 + id;
  }

  void start() {
    if (_started) return;
    _started = true;
    WidgetsBinding.instance.addObserver(this);
    NetBus.instance.events.listen(_onEvent);
    Background.onOpen(_opened);
    _resumed();
  }

  /// A notification was tapped: a call's goes to the call, others to
  /// their chat.
  void _opened(String key) {
    if (key.startsWith('call')) {
      CallController.instance.open(key);
    } else {
      _opens.add(key);
    }
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
    if (open != null) _opened(open);
    // Starting the service needs the app on screen (Android 12+).
    if (await Background.enabled()) {
      await Background.start();
      await Background.requestNotifications();
    }
  }

  void markRead(String key) {
    if (unread.value.containsKey(key)) unread.value = {...unread.value}..remove(key);
    Background.cancel(_notificationId(key));
    // And its missed call.
    if (key.startsWith('c:')) Background.cancel(2000000 + int.parse(key.substring(2)));
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
      case AppEventKind.missedCall:
        await _missedCall(e.id.toInt());
      case AppEventKind.feedback:
        if (!Profile.decoy.value) await _notifyFeedback();
      default:
        break;
    }
  }

  /// A call to us went unanswered: a badge on the chat, and a notification
  /// unless it's on screen. Like messages, a hidden chat's doesn't say whose.
  Future<void> _missedCall(int chatId) async {
    if (Profile.decoy.value) return;
    final key = chatKey(chatId);
    if (_foreground && viewing == key) return;
    unread.value = {...unread.value, key: (unread.value[key] ?? 0) + 1};
    final chat = (await chatList()).where((c) => c.id == chatId).firstOrNull;
    final hidden = chat?.hidden ?? true;
    await Background.notify(
      id: 2000000 + chatId,
      channel: Background.messages,
      title: hidden ? 'Sanctum' : chat?.title ?? 'Sanctum',
      body: 'Missed call',
      open: key,
      redacted: 'Missed call',
    );
  }

  Future<void> _arrived(String key) async {
    // The decoy shows: the real profile stays silent.
    if (Profile.decoy.value) return;
    if (_foreground && viewing == key) return;
    final count = (unread.value[key] ?? 0) + 1;
    unread.value = {...unread.value, key: count};
    final temp = key.startsWith('t:');
    final id = int.parse(key.substring(2));
    final chat = temp ? null : (await chatList()).where((c) => c.id == id).firstOrNull;
    // A hidden chat's notification doesn't say whose it is.
    final hidden = !temp && (chat?.hidden ?? true);
    final title = temp ? (await tempList()).where((t) => t.id.toInt() == id).firstOrNull?.title : chat?.title;
    // Groups: who wrote.
    var from = '';
    if (!hidden && chat?.kind == ChatKindView.group) {
      final names = <String>[];
      for (final m in await messageList(chatId: id, limit: count.clamp(1, 20))) {
        final name = m.senderName;
        if (!m.fromMe && name != null && !names.contains(name)) names.add(name);
      }
      if (names.isNotEmpty) from = ' from ${names.length == 1 ? names.first : '${names.first} and others'}';
    }
    // Opened or read meanwhile.
    if ((_foreground && viewing == key) || !unread.value.containsKey(key)) return;
    final what = temp ? 'temp message' : 'message';
    await Background.notify(
      id: _notificationId(key),
      channel: Background.messages,
      title: hidden ? 'Sanctum' : title ?? 'Sanctum',
      body: '${count == 1 ? 'New $what' : '$count new ${what}s'}$from',
      open: key,
      count: count,
    );
  }

  /// Feedback reached the developer's inbox. Never says what it says.
  Future<void> _notifyFeedback() async {
    _feedback++;
    await Background.notify(
      id: 3,
      channel: Background.requests,
      title: 'New feedback',
      body: _feedback == 1 ? 'Someone sent you feedback' : '$_feedback new pieces of feedback',
      open: 'feedback',
      count: _feedback,
    );
  }

  /// Feedback that arrived since the inbox was last opened.
  int _feedback = 0;

  /// The inbox is on screen: the feedback notification goes.
  void feedbackSeen() {
    _feedback = 0;
    Background.cancel(3);
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
