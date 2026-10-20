import 'package:flutter/services.dart';

import '../identity/secrets.dart';

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
  }) =>
      _call(_bg, 'notify', {'id': id, 'channel': channel, 'title': title, 'body': body, 'open': open, 'count': count});

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
