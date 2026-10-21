import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'home/home_screen.dart';
import 'background/background.dart';
import 'call/call_controller.dart';
import 'call/call_screen.dart';
import 'identity/secrets.dart';
import 'chat/media.dart';
import 'net/net_bus.dart';
import 'notify/notifier.dart';
import 'security/app_lock.dart';
import 'security/profile.dart';
import 'sigil/sigil_painter.dart';
import 'src/rust/api/identity.dart';
import 'src/rust/api/session.dart';
import 'src/rust/api/sigil.dart';
import 'src/rust/frb_generated.dart';
import 'theme/appearance.dart';
import 'theme/sigil_theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  try {
    final secret = await loadOrCreateIdentitySecret();
    await sessionOpen(
      identitySecret: secret,
      dir: (await getApplicationSupportDirectory()).path,
      storeKey: await loadOrCreateStoreKey(),
    );
    await profileSetName(name: await loadDisplayName());
    // Decrypted copies never outlive a run.
    await clearDecryptedCopies();
    NetBus.instance.start();
    Notifier.instance.start();
    CallController.instance.start();
    await NetBus.instance.connect(
      stateDir: '${(await getApplicationSupportDirectory()).path}/tor-state',
      cacheDir: '${(await getApplicationCacheDirectory()).path}/tor-cache',
      identitySecret: secret,
    );
    if (await Background.nearbyEnabled()) {
      // Best effort: another app may hold the discovery port.
      try {
        await NetBus.instance.setNearby(true);
      } catch (_) {}
    }
    await Profile.prepareDecoy();
    await Appearance.load();
    final publicKey = await identityPublicKey(secret: secret);
    final mySigil = await sigilForKey(publicKey: publicKey);
    runApp(SanctumApp(
      // The real profile, or the decoy after the decoy PIN.
      home: ValueListenableBuilder<bool>(
        valueListenable: Profile.decoy,
        builder: (context, decoy, _) => decoy
            ? HomeScreen(
                key: const ValueKey('decoy'),
                publicKey: Profile.decoyPublicKey!,
                mySigil: Profile.decoySigil!,
              )
            : HomeScreen(key: const ValueKey('real'), publicKey: publicKey, mySigil: mySigil),
      ),
      mySigil: mySigil,
    ));
  } catch (e) {
    runApp(_StartupError(error: e));
  }
}

class SanctumApp extends StatelessWidget {
  const SanctumApp({super.key, required this.home, required this.mySigil});

  final Widget home;
  final SigilSpec mySigil;

  @override
  Widget build(BuildContext context) {
    return Restyler(
      builder: (context) => MaterialApp(
        title: 'Sanctum',
        debugShowCheckedModeBanner: false,
        navigatorKey: AppLock.navigator,
        theme: sanctumTheme(accent: Appearance.accent.value ?? mySigil.accent.color),
        // Calls show above the lock, so they can be answered without
        // unlocking (saying who only once unlocked).
        builder: (context, child) => CallLayer(child: AppLock(sigil: mySigil, child: child!)),
        home: home,
      ),
    );
  }
}

/// Shown if secrets or the encrypted store can't be opened, instead of a
/// blank screen.
class _StartupError extends StatelessWidget {
  const _StartupError({required this.error});

  final Object error;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: sanctumTheme(),
      home: Scaffold(
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Text(
              'Sanctum could not open its encrypted storage.\n\n$error',
              textAlign: TextAlign.center,
            ),
          ),
        ),
      ),
    );
  }
}
