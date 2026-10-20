import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'home/home_screen.dart';
import 'identity/secrets.dart';
import 'security/app_lock.dart';
import 'sigil/sigil_painter.dart';
import 'src/rust/api/identity.dart';
import 'src/rust/api/session.dart';
import 'src/rust/api/sigil.dart';
import 'src/rust/frb_generated.dart';
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
    final publicKey = await identityPublicKey(secret: secret);
    final mySigil = await sigilForKey(publicKey: publicKey);
    runApp(SanctumApp(
      home: HomeScreen(publicKey: publicKey, mySigil: mySigil),
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
    return MaterialApp(
      title: 'Sanctum',
      debugShowCheckedModeBanner: false,
      theme: sanctumTheme(accent: mySigil.accent.color),
      builder: (context, child) => AppLock(sigil: mySigil, child: child!),
      home: home,
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
