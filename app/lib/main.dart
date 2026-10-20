import 'package:flutter/material.dart';

import 'home/home_screen.dart';
import 'identity/identity_store.dart';
import 'security/app_lock.dart';
import 'sigil/sigil_painter.dart';
import 'src/rust/api/sigil.dart';
import 'src/rust/frb_generated.dart';
import 'theme/sigil_theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  final publicKey = await loadOrCreateIdentity();
  final mySigil = await sigilForKey(publicKey: publicKey);
  runApp(SanctumApp(
    home: HomeScreen(publicKey: publicKey, mySigil: mySigil),
    mySigil: mySigil,
  ));
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
