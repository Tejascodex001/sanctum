import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import '../identity/secrets.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/identity.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';

/// Which profile the app shows: the real one, or the decoy opened by the
/// decoy PIN (an empty-looking Sanctum with its own identity and data).
/// While the decoy shows, the real profile keeps receiving in the
/// background, silently: no notifications until a real unlock.
abstract final class Profile {
  static final decoy = ValueNotifier<bool>(false);

  /// The decoy's key and sigil, once opened.
  static Uint8List? decoyPublicKey;
  static SigilSpec? decoySigil;

  /// Opens the decoy's database if a decoy PIN is set (at startup, or
  /// when one is first set).
  static Future<void> prepareDecoy() async {
    if (!await hasDecoyPin() || decoyPublicKey != null) return;
    final secret = await loadOrCreateDecoyIdentitySecret();
    await decoyOpen(
      identitySecret: secret,
      dir: (await getApplicationSupportDirectory()).path,
      storeKey: await loadOrCreateDecoyStoreKey(),
    );
    decoyPublicKey = await identityPublicKey(secret: secret);
    decoySigil = await sigilForKey(publicKey: decoyPublicKey!);
  }

  static Future<void> enter({required bool decoy}) async {
    if (decoy) await prepareDecoy();
    final use = decoy && decoyPublicKey != null;
    // The decoy shows no calls: end the real profile's call first.
    if (use) {
      try {
        await callEnd();
      } catch (_) {}
    }
    await profileUse(decoy: use);
    Profile.decoy.value = use;
  }
}
