import 'dart:async';

import 'package:flutter/foundation.dart';

import '../background/background.dart';
import '../net/net_bus.dart';
import '../security/app_lock.dart';
import '../security/profile.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/session.dart';

/// The call on this phone: ringing, talking, and saying how it ended.
///
/// Rust keeps the call itself (signaling, timeouts, the sealed sound); this
/// drives the phone's side: the ring, the microphone and speaker
/// (CallAudio.kt), the service that keeps a call going with the screen off,
/// and what the call screen shows. Only the real profile calls and rings.
class CallController {
  CallController._();

  static final instance = CallController._();

  /// The call to show, if any. An ended call stays a moment, to say how.
  final call = ValueNotifier<CallInfo?>(null);
  final muted = ValueNotifier<bool>(false);
  final speaker = ValueNotifier<bool>(false);

  bool _started = false;
  bool _audio = false;
  Timer? _clear;
  ({List<String> play, List<String> record})? _codecs;

  /// How long an ended call stays on screen.
  static const _linger = Duration(milliseconds: 2500);

  void start() {
    if (_started) return;
    _started = true;
    NetBus.instance.events.listen((e) {
      final c = e.call;
      if (e.kind == AppEventKind.call && c != null) _update(c);
    });
    Background.onCallAction((_) => hangUp());
    // Opened by a call's notification: it may be ringing already.
    unawaited(_refresh());
  }

  Future<void> _refresh() async {
    try {
      final c = await callNow();
      if (c != null) _update(c);
    } catch (_) {}
  }

  bool get busy {
    final c = call.value;
    return c != null && c.phase != CallPhaseView.ended;
  }

  /// What the phone can play and record into (asked once).
  Future<({List<String> play, List<String> record})> _caps() async => _codecs ??= await Background.callCodecs();

  /// A call's notification was tapped: "call" shows it, "call:answer"
  /// picks up.
  Future<void> open(String key) async {
    await _refresh();
    if (key == 'call:answer') await answer();
  }

  /// Calls the friend in this chat. Returns what to tell the user if the
  /// call couldn't start, else null.
  Future<String?> startCall(int chatId, String name) async {
    if (busy) return 'You\'re already in a call.';
    if (Profile.decoy.value) return 'Calls aren\'t available right now.';
    if (!await Background.requestMicrophone()) {
      return 'Calls need the microphone. You can allow it in your phone\'s settings for Sanctum.';
    }
    final caps = await _caps();
    try {
      final info = await callStart(chatId: chatId, play: caps.play, record: caps.record);
      // Started now, while the app is on screen: Android allows it then.
      await Background.callServiceStart(title(info));
      _update(info);
      return null;
    } catch (_) {
      return 'You can call $name once they\'ve added you back.';
    }
  }

  /// Picks up the call ringing here.
  Future<void> answer() async {
    final c = call.value;
    if (c == null || c.outgoing || c.phase != CallPhaseView.ringing) return;
    await Background.callRingStop();
    if (!await Background.requestMicrophone()) {
      await hangUp();
      return;
    }
    final caps = await _caps();
    await Background.callServiceStart(title(c));
    await callAnswer(play: caps.play, record: caps.record);
  }

  /// Hangs up, declines, or stops calling.
  Future<void> hangUp() async {
    try {
      await callEnd();
    } catch (_) {
      _stop();
      call.value = null;
    }
  }

  Future<void> toggleMute() async {
    muted.value = !muted.value;
    await Background.callMute(muted.value);
  }

  Future<void> toggleSpeaker() async {
    speaker.value = !speaker.value;
    await Background.callSpeaker(speaker.value);
  }

  /// What notifications call the friend: a hidden chat's name stays hidden.
  static String title(CallInfo c) => c.hidden ? 'Sanctum' : c.name;

  void _update(CallInfo c) {
    // The decoy never rings (Rust doesn't tell it either).
    if (Profile.decoy.value) return;
    _clear?.cancel();
    call.value = c;
    switch (c.phase) {
      case CallPhaseView.ringing when !c.outgoing:
        AppLock.holdPrompt.value = true;
        unawaited(Background.callRing(title(c)));
        unawaited(Background.callShowOverLock(true));
      case CallPhaseView.connecting || CallPhaseView.ringing:
        AppLock.holdPrompt.value = true;
      case CallPhaseView.active:
        AppLock.holdPrompt.value = true;
        unawaited(Background.callRingStop());
        if (!_audio) {
          _audio = true;
          unawaited(Background.callAudioStart(send: c.sendCodec, recv: c.recvCodec, speaker: speaker.value));
        }
      case CallPhaseView.ended:
        _stop();
        _clear = Timer(_linger, () {
          if (identical(call.value, c)) call.value = null;
        });
    }
  }

  void _stop() {
    _audio = false;
    muted.value = false;
    speaker.value = false;
    AppLock.holdPrompt.value = false;
    unawaited(Background.callAudioStop());
    unawaited(Background.callRingStop());
    unawaited(Background.callServiceStop());
    unawaited(Background.callShowOverLock(false));
  }
}
