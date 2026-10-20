import 'package:local_auth/local_auth.dart';

import '../background/background.dart';
import '../identity/secrets.dart';
import 'app_lock.dart';

/// Screenshots are blocked everywhere by default. A chat can allow them,
/// but only after biometric or device-PIN confirmation, and only while
/// that chat is open.
abstract final class Screenshots {
  static String _flag(String chatKey) => 'shots_$chatKey';

  static Future<bool> allowed(String chatKey) => loadFlag(_flag(chatKey), fallback: false);

  /// Allowing needs the owner's confirmation; blocking again doesn't.
  /// Returns the new setting.
  static Future<bool> set(String chatKey, bool allow) async {
    if (allow) {
      final ok = await AppLock.whileAway(() async {
        try {
          final auth = LocalAuthentication();
          return !await auth.isDeviceSupported() ||
              await auth.authenticate(localizedReason: 'Allow screenshots in this chat');
        } on LocalAuthException {
          return false;
        }
      });
      if (!ok) return false;
    }
    await saveFlag(_flag(chatKey), allow);
    await Background.setSecure(!allow);
    return allow;
  }

  /// Applies a chat's setting while it's open...
  static Future<void> enter(String chatKey) async {
    if (await allowed(chatKey)) await Background.setSecure(false);
  }

  /// ...and blocks again when it closes.
  static Future<void> leave() => Background.setSecure(true);
}
