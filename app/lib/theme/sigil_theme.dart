import 'package:flutter/material.dart';

import 'tokens.dart';

/// Material 3 as the foundation (accessibility, text input, platform
/// behaviour), restyled with the Sigil tokens: neutral dark surfaces, our
/// own type, and the user's sigil accent as the only strong colour.
ThemeData sanctumTheme({Color accent = const Color(0xFF8B7CFF)}) {
  final onAccent = Palette.onColor(accent);
  final scheme = ColorScheme.fromSeed(seedColor: accent, brightness: Brightness.dark).copyWith(
    primary: accent,
    onPrimary: onAccent,
    surface: Palette.bg,
    surfaceContainerLow: Palette.surface,
    surfaceContainer: Palette.surface,
    surfaceContainerHigh: Palette.surfaceHigh,
    onSurface: Palette.text,
    onSurfaceVariant: Palette.textMuted,
    outline: Palette.outlineStrong,
    outlineVariant: Palette.outline,
    error: Palette.danger,
  );
  final shape = RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.md));
  const buttonText = TextStyle(fontFamily: 'Inter', fontSize: 15, fontWeight: FontWeight.w600);

  return ThemeData(
    useMaterial3: true,
    brightness: Brightness.dark,
    colorScheme: scheme,
    fontFamily: 'Inter',
    scaffoldBackgroundColor: Palette.bg,
    splashFactory: InkSparkle.splashFactory,
    textTheme: const TextTheme(
      displaySmall: AppText.display,
      titleLarge: AppText.title,
      titleMedium: AppText.heading,
      bodyLarge: AppText.body,
      bodyMedium: AppText.body,
      bodySmall: AppText.caption,
      labelLarge: buttonText,
      labelSmall: AppText.micro,
    ).apply(bodyColor: Palette.text, displayColor: Palette.text),
    appBarTheme: const AppBarTheme(
      backgroundColor: Colors.transparent,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      centerTitle: false,
      titleTextStyle: AppText.title,
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        minimumSize: const Size(64, 52),
        shape: shape,
        textStyle: buttonText,
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        minimumSize: const Size(64, 48),
        shape: shape,
        side: const BorderSide(color: Palette.outlineStrong),
        foregroundColor: Palette.text,
        textStyle: buttonText,
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(shape: shape, textStyle: buttonText),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: Palette.surfaceHigh,
      hintStyle: AppText.body.copyWith(color: Palette.textFaint),
      contentPadding: const EdgeInsets.symmetric(horizontal: Space.lg, vertical: 14),
      border: OutlineInputBorder(borderRadius: BorderRadius.circular(Radii.md), borderSide: BorderSide.none),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.md),
        borderSide: const BorderSide(color: Palette.outline),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.md),
        borderSide: BorderSide(color: accent.withValues(alpha: 0.7), width: 1.4),
      ),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: Palette.surfaceHigh,
      contentTextStyle: AppText.body.copyWith(color: Palette.text),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.md)),
    ),
    bottomSheetTheme: const BottomSheetThemeData(
      backgroundColor: Palette.surface,
      surfaceTintColor: Colors.transparent,
      showDragHandle: true,
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: Palette.surfaceHigh,
      surfaceTintColor: Colors.transparent,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.lg)),
      titleTextStyle: AppText.title,
    ),
    dividerTheme: const DividerThemeData(color: Palette.outline, thickness: 1, space: 1),
    progressIndicatorTheme: ProgressIndicatorThemeData(color: accent),
    pageTransitionsTheme: const PageTransitionsTheme(
      builders: {TargetPlatform.android: FadeForwardsPageTransitionsBuilder()},
    ),
  );
}
