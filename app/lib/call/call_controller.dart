import 'dart:async';

import 'package:flutter/widgets.dart';

import '../background/background.dart';
import '../net/net_bus.dart';
import '../security/app_lock.dart';
import '../security/profile.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/session.dart';

/// The call on this phone: ringing, talking, and saying how it ended.
///
/// Rust keeps the call itself (signaling, timeouts, the sealed sound and
/// pictures); this drives the phone's side: the ring, the microphone and
/// speaker (CallAudio.kt), the camera and the pictures (CallVideo.kt), the
/// service that keeps a call going with the screen off, and what the call
/// screen shows. Only the real profile calls and rings.
class CallController with WidgetsBindingObserver {
  CallController._();

  static final instance = CallController._();

  /// The call to show, if any. An ended call stays a moment, to say how.
  final call = ValueNotifier<CallInfo?>(null);
  final muted = ValueNotifier<bool>(false);
  final speaker = ValueNotifier<bool>(false);

  /// Our camera is on (or paused while the app is in the background).
  final camera = ValueNotifier<bool>(false);
  final frontCamera = ValueNotifier<bool>(true);

  /// How the call is getting along: the path, and both cameras.
  final media = ValueNotifier<CallMediaInfo?>(null);

  /// The two pictures, as Flutter textures, once a call with video is on.
  final textures = ValueNotifier<({int local, int remote})?>(null);

  /// How to show each picture (size, turn, mirror), from the Android side.
  final localFormat = ValueNotifier<VideoFormat?>(null);
  final remoteFormat = ValueNotifier<VideoFormat?>(null);

  bool _started = false;
  bool _audio = false;
  bool _paused = false;
  Timer? _clear;
  Timer? _poll;
  CallCodecs? _codecs;

  /// How long an ended call stays on screen.
  static const _linger = Duration(milliseconds: 2500);

  /// How often the call screen's path and cameras are refreshed.
  static const _pollEvery = Duration(milliseconds: 700);

  void start() {
    if (_started) return;
    _started = true;
    NetBus.instance.events.listen((e) {
      final c = e.call;
      if (e.kind == AppEventKind.call && c != null) _update(c);
    });
    Background.onCallAction((_) => hangUp());
    Background.onCallVideoFormat((f) => (f.which == 'local' ? localFormat : remoteFormat).value = f);
    WidgetsBinding.instance.addObserver(this);
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
  Future<CallCodecs> _caps() async => _codecs ??= await Background.callCodecs();

  /// A call's notification was tapped: "call" shows it, "call:answer"
  /// picks up.
  Future<void> open(String key) async {
    await _refresh();
    if (key == 'call:answer') await answer();
  }

  /// Calls the friend in this chat (with the camera on for a video call).
  /// Returns what to tell the user if the call couldn't start, else null.
  Future<String?> startCall(int chatId, String name, {bool video = false}) async {
    if (busy) return 'You\'re already in a call.';
    if (Profile.decoy.value) return 'Calls aren\'t available right now.';
    if (!await Background.requestMicrophone()) {
      return 'Calls need the microphone. You can allow it in your phone\'s settings for Sanctum.';
    }
    final caps = await _caps();
    if (video) {
      if (caps.videoRecord.isEmpty) return 'This phone can\'t send video. You can still make a voice call.';
      if (!await Background.requestCamera()) {
        return 'Video calls need the camera. You can allow it in your phone\'s settings for Sanctum.';
      }
    }
    try {
      final info = await callStart(
        chatId: chatId,
        play: caps.play,
        record: caps.record,
        videoPlay: caps.videoPlay,
        videoRecord: caps.videoRecord,
        video: video,
      );
      // Started now, while the app is on screen: Android allows it then.
      await Background.callServiceStart(title(info), camera: video);
      _update(info);
      return null;
    } catch (_) {
      return 'You can call $name once they\'ve added you back.';
    }
  }

  /// Picks up the call ringing here; a video call with the camera on
  /// unless `video` is false (or the camera isn't allowed).
  Future<void> answer({bool video = true}) async {
    final c = call.value;
    if (c == null || c.outgoing || c.phase != CallPhaseView.ringing) return;
    await Background.callRingStop();
    if (!await Background.requestMicrophone()) {
      await hangUp();
      return;
    }
    final caps = await _caps();
    final withCamera = video && c.video && caps.videoRecord.isNotEmpty && await Background.requestCamera();
    await Background.callServiceStart(title(c), camera: withCamera);
    await callAnswer(
      play: caps.play,
      record: caps.record,
      videoPlay: caps.videoPlay,
      videoRecord: caps.videoRecord,
      video: withCamera,
    );
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

  /// Turns our camera on or off during the call.
  Future<void> toggleCamera() async {
    if (camera.value) {
      await _cameraOff();
    } else {
      await _cameraOn();
      // Video is easier to follow on the speaker.
      if (camera.value && !speaker.value) await toggleSpeaker();
    }
    await _pollMedia();
  }

  /// Front camera to back, or back.
  Future<void> flipCamera() async {
    frontCamera.value = !frontCamera.value;
    if (camera.value && !_paused) await Background.callCamera(on: true, front: frontCamera.value);
  }

  Future<bool> _cameraOn() async {
    final c = call.value;
    if (c == null || c.phase != CallPhaseView.active || c.sendVideoCodec.isEmpty) return false;
    if (!await Background.requestCamera()) return false;
    if (!await callCamera(state: CameraView.live)) return false;
    if (!await Background.callCamera(on: true, front: frontCamera.value)) {
      await callCamera(state: CameraView.off);
      return false;
    }
    camera.value = true;
    _paused = false;
    await Background.callServiceStart(title(c), camera: true);
    return true;
  }

  Future<void> _cameraOff() async {
    camera.value = false;
    _paused = false;
    await callCamera(state: CameraView.off);
    await Background.callCamera(on: false);
    final c = call.value;
    if (c != null && c.phase == CallPhaseView.active) await Background.callServiceStart(title(c));
  }

  /// Outgoing pictures stop while the app is in the background, and the
  /// friend sees "camera paused"; they start again when it's back.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (!camera.value) return;
    final away = state == AppLifecycleState.hidden || state == AppLifecycleState.paused;
    if (away && !_paused) {
      _paused = true;
      unawaited(callCamera(state: CameraView.paused));
      unawaited(Background.callCamera(on: false));
    } else if (state == AppLifecycleState.resumed && _paused) {
      _paused = false;
      unawaited(callCamera(state: CameraView.live));
      unawaited(Background.callCamera(on: true, front: frontCamera.value));
    }
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
          // A video call starts on the speaker.
          if (c.video && !speaker.value) speaker.value = true;
          unawaited(Background.callAudioStart(send: c.sendCodec, recv: c.recvCodec, speaker: speaker.value));
          unawaited(_startVideo(c));
        }
      case CallPhaseView.ended:
        _stop();
        _clear = Timer(_linger, () {
          if (identical(call.value, c)) call.value = null;
        });
    }
  }

  /// Once talking: the pictures, if either way can carry them, and our
  /// camera if the call started with it on.
  Future<void> _startVideo(CallInfo c) async {
    _poll?.cancel();
    _poll = Timer.periodic(_pollEvery, (_) => _pollMedia());
    if (c.sendVideoCodec.isNotEmpty || c.recvVideoCodec.isNotEmpty) {
      textures.value = await Background.callVideoOpen(send: c.sendVideoCodec, recv: c.recvVideoCodec);
    }
    await _pollMedia();
    if (media.value?.camera == CameraView.live && !camera.value) {
      if (!await _cameraOn()) await callCamera(state: CameraView.off);
    }
  }

  Future<void> _pollMedia() async {
    if (!busy) return;
    try {
      media.value = await callMedia();
    } catch (_) {}
  }

  void _stop() {
    _audio = false;
    _paused = false;
    _poll?.cancel();
    _poll = null;
    muted.value = false;
    speaker.value = false;
    camera.value = false;
    frontCamera.value = true;
    media.value = null;
    textures.value = null;
    localFormat.value = null;
    remoteFormat.value = null;
    AppLock.holdPrompt.value = false;
    unawaited(Background.callAudioStop());
    unawaited(Background.callCamera(on: false));
    unawaited(Background.callVideoClose());
    unawaited(Background.callRingStop());
    unawaited(Background.callServiceStop());
    unawaited(Background.callShowOverLock(false));
  }
}
