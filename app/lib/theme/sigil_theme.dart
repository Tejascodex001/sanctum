import 'package:flutter/material.dart';

/// Base dark theme. Each chat overrides [ColorScheme.primary] with its
/// sigil accent, so the app's colour comes from the keys, not a brand palette.
ThemeData sanctumTheme({Color accent = const Color(0xFF8B7CFF)}) {
  final scheme = ColorScheme.fromSeed(
    seedColor: accent,
    brightness: Brightness.dark,
    surface: const Color(0xFF0B0B10),
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: scheme.surface,
    splashFactory: NoSplash.splashFactory,
  );
}
