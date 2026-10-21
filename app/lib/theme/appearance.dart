import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../identity/secrets.dart';
import 'tokens.dart';

enum ThemeChoice { dark, light, phone }

/// How the app looks: dark, light or like the phone, and the accent colour
/// (the sigil's own unless one is picked). Kept with the other settings.
abstract final class Appearance {
  static final theme = ValueNotifier<ThemeChoice>(ThemeChoice.dark);

  /// Null: the sigil's accent.
  static final accent = ValueNotifier<Color?>(null);

  static Future<void> load() async {
    final t = await loadSetting('theme');
    theme.value = ThemeChoice.values.where((c) => c.name == t).firstOrNull ?? ThemeChoice.dark;
    final a = int.tryParse(await loadSetting('accent') ?? '');
    accent.value = a == null ? null : Color(a);
  }

  static Future<void> setTheme(ThemeChoice choice) async {
    theme.value = choice;
    await saveSetting('theme', choice.name);
  }

  static Future<void> setAccent(Color? color) async {
    accent.value = color;
    await saveSetting('accent', color?.toARGB32().toString());
  }

  /// Whether to draw light, given the phone's own setting.
  static bool light(Brightness phone) => switch (theme.value) {
        ThemeChoice.dark => false,
        ThemeChoice.light => true,
        ThemeChoice.phone => phone == Brightness.light,
      };

  /// Status and navigation bar icons that read on the background.
  static SystemUiOverlayStyle get systemBars =>
      (Palette.light ? SystemUiOverlayStyle.dark : SystemUiOverlayStyle.light).copyWith(
        statusBarColor: Colors.transparent,
        systemNavigationBarColor: Colors.transparent,
      );
}

/// Draws [builder] in the current appearance, and draws every screen again
/// when it changes (light, dark or accent), since the palette isn't only
/// read through the theme.
class Restyler extends StatefulWidget {
  const Restyler({super.key, required this.builder});

  final WidgetBuilder builder;

  @override
  State<Restyler> createState() => _RestylerState();
}

class _RestylerState extends State<Restyler> with WidgetsBindingObserver {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    Appearance.theme.addListener(_restyle);
    Appearance.accent.addListener(_restyle);
    _apply();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    Appearance.theme.removeListener(_restyle);
    Appearance.accent.removeListener(_restyle);
    super.dispose();
  }

  // The phone switched between light and dark.
  @override
  void didChangePlatformBrightness() => _restyle();

  void _apply() {
    Palette.light = Appearance.light(WidgetsBinding.instance.platformDispatcher.platformBrightness);
    SystemChrome.setSystemUIOverlayStyle(Appearance.systemBars);
  }

  void _restyle() {
    _apply();
    setState(() {});
    WidgetsBinding.instance.addPostFrameCallback((_) {
      void rebuild(Element e) {
        e.markNeedsBuild();
        e.visitChildren(rebuild);
      }

      if (mounted) (context as Element).visitChildren(rebuild);
    });
  }

  @override
  Widget build(BuildContext context) => widget.builder(context);
}
