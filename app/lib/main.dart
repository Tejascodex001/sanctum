import 'package:flutter/material.dart';

import 'theme/sigil_theme.dart';

void main() => runApp(const SanctumApp());

class SanctumApp extends StatelessWidget {
  const SanctumApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Sanctum',
      theme: sanctumTheme(),
      home: const Scaffold(body: Center(child: Text('Sanctum'))),
    );
  }
}
