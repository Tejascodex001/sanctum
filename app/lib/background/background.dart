import 'package:flutter/services.dart';

import '../identity/secrets.dart';
import '../security/app_lock.dart';

/// What a phone can play and record in calls, best first.
typedef CallCodecs = ({List<String> play, List<String> record, List<String> videoPlay, List<String> videoRecord});

/// How to show a call's picture: its size, how far to turn it clockwise,
/// and whether to mirror it (our own front camera).
typedef VideoFormat = ({String which, int width, int height, int rotation, bool mirror});

/// Staying reachable while the app is closed: an Android foreground service
/// keeps the process (and Tor) alive, and notifications are posted on the
/// phone itself. See SanctumApplication.kt and ConnectionService.kt.
abstract final class Background {
  static const _bg = MethodChannel('sanctum/background');
  static const _app = MethodChannel('sanctum/app');

  static const messages = 'messages';
  static const requests = 'requests';

  /// On unless the user turns it off.
  static Future<bool> enabled() => loadFlag('background', fallback: true);

  static Future<void> setEnabled(bool on) async {
    await saveFlag('background', on);
    on ? await start() : await stop();
  }

  static Future<void> start() => _call(_bg, 'start');
  static Future<void> stop() => _call(_bg, 'stop');

  static Future<void> notify({
    required int id,
    required String channel,
    required String title,
    required String body,
    String? open,
    int count = 0,
    String? redacted,
  }) =>
      _call(_bg, 'notify', {
        'id': id,
        'channel': channel,
        'title': title,
        'body': body,
        'open': open,
        'count': count,
        'redacted': redacted,
      });

  static Future<void> cancel(int id) => _call(_bg, 'cancel', {'id': id});
  static Future<void> cancelAll() => _call(_bg, 'cancelAll');

  static Future<bool> canNotify() async => await _call<bool>(_bg, 'canNotify') ?? false;

  /// Asks once (Android 13+ needs the user's permission to notify).
  static Future<bool> requestNotifications() async {
    if (await canNotify()) return true;
    if (await loadFlag('asked_notifications', fallback: false)) return false;
    await saveFlag('asked_notifications', true);
    return await _call<bool>(_app, 'requestNotifications') ?? false;
  }

  /// Android's notification settings for Sanctum.
  static Future<void> openNotificationSettings() => _call(_app, 'openNotificationSettings');

  static Future<bool> batteryUnrestricted() async => await _call<bool>(_bg, 'batteryUnrestricted') ?? true;
  static Future<void> requestUnrestricted() => _call(_app, 'requestUnrestricted');

  /// The phone's maker, and whether it's known to stop background apps
  /// (Xiaomi, Oppo, Vivo, Samsung, Huawei, OnePlus and their brands).
  static Future<({String brand, bool aggressive})> brand() async {
    final m = await _call<Map<Object?, Object?>>(_app, 'backgroundBrand');
    return (brand: (m?['brand'] as String?) ?? '', aggressive: (m?['aggressive'] as bool?) ?? false);
  }

  /// Opens the maker's own "let it run in the background" page directly
  /// (else Sanctum's app settings).
  static Future<void> openBackgroundSettings() async {
    await _call(_app, 'openBackgroundSettings');
    await saveFlag('opened_background_settings', true);
  }

  /// Whether the user still has a step to make background delivery
  /// reliable: the battery exemption, then (on strict phones) the maker's page.
  static Future<bool> needsReliabilityStep() async {
    if (!await enabled()) return false;
    if (!await batteryUnrestricted()) return true;
    return (await brand()).aggressive && !await loadFlag('opened_background_settings', fallback: false);
  }

  /// Nearby on or off (remembered), with what Android needs to hear
  /// friends' beacons.
  static Future<bool> nearbyEnabled() => loadFlag('nearby', fallback: false);

  static Future<void> nearbyLock(bool on) => _call(_bg, 'nearbyLock', {'on': on});

  /// This app's build number (its Android version code).
  static Future<int> buildNumber() async => await _call<int>(_bg, 'buildNumber') ?? 0;

  /// The phone's maker and model ("Samsung SM-A546B") and Android's
  /// version ("14"), for feedback.
  static Future<({String phone, String android})> deviceInfo() async {
    final m = await _call<Map<Object?, Object?>>(_bg, 'deviceInfo');
    final maker = (m?['maker'] as String?)?.trim() ?? '';
    final model = (m?['model'] as String?)?.trim() ?? '';
    // Many models already start with the maker's name.
    final phone = model.toLowerCase().startsWith(maker.toLowerCase()) || maker.isEmpty
        ? model
        : '${maker[0].toUpperCase()}${maker.substring(1)} $model';
    return (phone: phone, android: (m?['android'] as String?) ?? '');
  }

  /// Opens Android's installer for a downloaded update: "started",
  /// "permission" (the user must first allow Sanctum to install apps; the
  /// settings page was opened), or "failed".
  static Future<String> installApk(String path) async =>
      await _call<String>(_app, 'installApk', {'path': path}) ?? 'failed';

  /// Bluetooth nearby (Android 10+): asks for the Nearby devices
  /// permission, then starts; returns the relay port, or null.
  static Future<int?> bluetoothStart(int rustPort) async {
    if (await _call<bool>(_app, 'requestBluetooth') != true) return null;
    final port = await _call<int>(_bg, 'btStart', {'rustPort': rustPort});
    return port == null || port < 0 ? null : port;
  }

  static Future<void> bluetoothAdvertise(Uint8List id) => _call(_bg, 'btAdvertise', {'id': id});
  static Future<void> bluetoothStop() => _call(_bg, 'btStop');

  /// A Sanctum phone was seen over Bluetooth (its advertised ID, and a
  /// handle naming the device).
  static void onBluetoothSeen(void Function(Uint8List id, int handle) handler) {
    _listen('btSeen', (args) {
      final m = args! as Map<Object?, Object?>;
      handler(m['id']! as Uint8List, m['handle']! as int);
    });
  }

  /// Calls from the Android side on the background channel, by method.
  static final _handlers = <String, void Function(Object? args)>{};

  static void _listen(String method, void Function(Object? args) handler) {
    _handlers[method] = handler;
    _bg.setMethodCallHandler((call) async => _handlers[call.method]?.call(call.arguments));
  }

  // ---- Calls (see CallAudio.kt, CallVideo.kt and SanctumApplication.kt) ----

  /// The sound and video formats this phone can play and record into,
  /// best first (no video recording without a camera).
  static Future<CallCodecs> callCodecs() async {
    final m = await _call<Map<Object?, Object?>>(_bg, 'callCodecs');
    List<String> list(Object? l) => [for (final c in (l as List<Object?>?) ?? const []) c! as String];
    return (
      play: list(m?['play']),
      record: list(m?['record']),
      videoPlay: list(m?['videoPlay']),
      videoRecord: list(m?['videoRecord']),
    );
  }

  static Future<bool> callAudioStart({required String send, required String recv, required bool speaker}) async =>
      await _call<bool>(_bg, 'callAudioStart', {'send': send, 'recv': recv, 'speaker': speaker}) ?? false;
  static Future<void> callAudioStop() => _call(_bg, 'callAudioStop');
  static Future<void> callMute(bool on) => _call(_bg, 'callMute', {'on': on});
  static Future<void> callSpeaker(bool on) => _call(_bg, 'callSpeaker', {'on': on});

  /// Rings with a notification (full screen where allowed) until stopped.
  static Future<void> callRing(String title) => _call(_bg, 'callRing', {'title': title});
  static Future<void> callRingStop() => _call(_bg, 'callRingStop');

  /// Keeps the microphone (and, with `camera`, the camera) going with the
  /// screen off or in another app. Called again when the camera turns on
  /// or off.
  static Future<void> callServiceStart(String title, {bool camera = false}) =>
      _call(_bg, 'callServiceStart', {'title': title, 'camera': camera});
  static Future<void> callServiceStop() => _call(_bg, 'callServiceStop');

  /// Opens the call's two pictures (ours and the friend's) as textures to
  /// show with `Texture`. `send` and `recv`: the agreed video formats.
  static Future<({int local, int remote})?> callVideoOpen({required String send, required String recv}) async {
    final m = await _call<Map<Object?, Object?>>(_bg, 'callVideoOpen', {'send': send, 'recv': recv});
    if (m == null) return null;
    return (local: m['local']! as int, remote: m['remote']! as int);
  }

  static Future<void> callVideoClose() => _call(_bg, 'callVideoClose');

  /// Turns our camera on (front or back) or off. False if it couldn't start.
  static Future<bool> callCamera({required bool on, bool front = true}) async =>
      await _call<bool>(_bg, 'callCamera', {'on': on, 'front': front}) ?? false;

  /// The camera permission (asked once).
  static Future<bool> requestCamera() =>
      AppLock.whileAway(() async => await _call<bool>(_app, 'requestCamera') ?? false);

  /// A picture's shape changed: which ("local" or "remote"), its size, how
  /// far to turn it clockwise, and whether to mirror it.
  static void onCallVideoFormat(void Function(VideoFormat format) handler) {
    _listen('callVideoFormat', (args) {
      final m = args! as Map<Object?, Object?>;
      handler((
        which: m['which']! as String,
        width: m['width']! as int,
        height: m['height']! as int,
        rotation: m['rotation']! as int,
        mirror: m['mirror'] == true,
      ));
    });
  }

  /// Shows the app over the lock screen (while a call rings or goes on).
  static Future<void> callShowOverLock(bool on) => _call(_app, 'callShowOverLock', {'on': on});

  /// "decline" or "hangup", tapped on a call's notification.
  static void onCallAction(void Function(String action) handler) =>
      _listen('callAction', (args) => handler(args! as String));

  /// The system picker: "visual" (photos and videos) or "file". Returns
  /// the prepared file (path, name, mime, kind, width, height, durationMs,
  /// thumb), or null if cancelled.
  static Future<Map<Object?, Object?>?> pickMedia(String kind) =>
      AppLock.whileAway(() => _call<Map<Object?, Object?>>(_app, 'pickMedia', {'kind': kind}));

  /// A photo taken now with the camera, prepared like [pickMedia] (null if
  /// cancelled). Asks for the camera first.
  static Future<Map<Object?, Object?>?> capturePhoto() => AppLock.whileAway(() async {
        if (!(await _call<bool>(_app, 'requestCamera') ?? false)) return null;
        return _call<Map<Object?, Object?>>(_app, 'capturePhoto');
      });

  /// Several photos and videos (up to 30), each prepared like [pickMedia].
  static Future<List<Map<Object?, Object?>>> pickMediaMany() => AppLock.whileAway(() async {
        final picked = await _call<List<Object?>>(_app, 'pickMediaMany') ?? const [];
        return [for (final p in picked) p! as Map<Object?, Object?>];
      });

  /// Voice notes: the microphone permission (asked once), then recording.
  static Future<bool> requestMicrophone() =>
      AppLock.whileAway(() async => await _call<bool>(_app, 'requestMicrophone') ?? false);
  static Future<bool> recordStart() async => await _call<bool>(_app, 'recordStart') ?? false;
  static Future<int> recordLevel() async => await _call<int>(_app, 'recordLevel') ?? 0;

  /// The recording (path, durationMs), or null if it was too short.
  static Future<Map<Object?, Object?>?> recordStop() => _call<Map<Object?, Object?>>(_app, 'recordStop');
  static Future<void> recordCancel() => _call(_app, 'recordCancel');

  /// Opens a (decrypted) file in another app. "opened", "noapp" (nothing
  /// on the phone can show it), "missing" (the copy is gone) or "failed".
  static Future<String> openFile(String path, String name, String mime) => AppLock.whileAway(
      () async => await _call<String>(_app, 'openFile', {'path': path, 'name': name, 'mime': mime}) ?? 'failed');

  /// Copies a (decrypted) file into the phone's own Pictures, Movies, Music
  /// or Downloads folder (a "Sanctum" folder in it), on the user's request.
  /// "saved", "cancelled" (older phones ask where to put it), "missing" or
  /// "failed".
  static Future<String> saveToPhone(String path, String name, String mime) => AppLock.whileAway(
      () async => await _call<String>(_app, 'saveToPhone', {'path': path, 'name': name, 'mime': mime}) ?? 'failed');

  /// Blocks (or allows) screenshots and the recents preview.
  static Future<void> setSecure(bool secure) => _call(_app, 'setSecure', {'secure': secure});

  /// A chat named by a tapped notification ("c:12", "t:3"), if any.
  static Future<String?> takeOpen() => _call<String>(_app, 'takeOpen');

  /// Called when a notification is tapped while the app is running.
  static void onOpen(void Function(String key) handler) {
    _app.setMethodCallHandler((call) async {
      if (call.method == 'open') handler(call.arguments as String);
    });
  }

  /// Platform calls fail in tests and on other platforms; none of this is
  /// essential, so they're ignored there.
  static Future<T?> _call<T>(MethodChannel channel, String method, [Object? args]) async {
    try {
      return await channel.invokeMethod<T>(method, args);
    } on MissingPluginException {
      return null;
    } on PlatformException {
      return null;
    }
  }
}
