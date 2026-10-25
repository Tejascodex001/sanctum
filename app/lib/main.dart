import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'home/home_screen.dart';
import 'background/background.dart';
import 'call/call_controller.dart';
import 'call/call_screen.dart';
import 'identity/secrets.dart';
import 'chat/media.dart';
import 'net/net_bus.dart';
import 'update/updater.dart';
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
import 'theme/tokens.dart';
import 'sigil/sigil_look.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  await _start();
}

/// Set once the background services are running, so a retry after a
/// failed start doesn't start them twice.
bool _servicesStarted = false;

/// Opens the secrets and the store and shows the app, or a screen with a
/// Retry button if that fails. Nothing is erased or remade on failure.
Future<void> _start() async {
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
    if (!_servicesStarted) {
      _servicesStarted = true;
      NetBus.instance.start();
      Notifier.instance.start();
      CallController.instance.start();
      Updater.instance.start();
    }
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
    // A fresh key, so a second failure shows a fresh screen (button usable).
    runApp(_StartupError(key: UniqueKey(), error: e, onRetry: _start));
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
        theme: sanctumTheme(accent: mySigil.look.accent.color),
        // Calls show above the lock, so they can be answered without
        // unlocking (saying who only once unlocked).
        builder: (context, child) => CallLayer(child: AppLock(sigil: mySigil, child: child!)),
        home: home,
      ),
    );
  }
}

/// Shown if secrets or the encrypted store can't be opened, instead of a
/// blank screen. Nothing is erased: Retry just tries again.
class _StartupError extends StatefulWidget {
  const _StartupError({super.key, required this.error, required this.onRetry});

  final Object error;
  final Future<void> Function() onRetry;

  @override
  State<_StartupError> createState() => _StartupErrorState();
}

class _StartupErrorState extends State<_StartupError> {
  bool _retrying = false;

  Future<void> _retry() async {
    setState(() => _retrying = true);
    // On success this replaces the whole app; on failure a new error
    // screen takes its place.
    await widget.onRetry();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: sanctumTheme(),
      home: Scaffold(
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(Space.xxl),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Text('Sanctum couldn\'t open its secure storage',
                    textAlign: TextAlign.center, style: AppText.title),
                const SizedBox(height: Space.md),
                Text(
                  'Your chats are safe on this phone. Try again, or restart the phone.',
                  textAlign: TextAlign.center,
                  style: AppText.body.copyWith(color: Palette.textMuted),
                ),
                const SizedBox(height: Space.xl),
                FilledButton(
                  onPressed: _retrying ? null : _retry,
                  child: _retrying
                      ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Text('Try again'),
                ),
                const SizedBox(height: Space.lg),
                // For a tester to report: the kind of problem only, never details.
                Text('Error: ${widget.error.runtimeType}', textAlign: TextAlign.center, style: AppText.micro),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
