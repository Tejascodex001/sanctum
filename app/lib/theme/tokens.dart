import 'package:flutter/animation.dart';
import 'package:flutter/painting.dart';

/// The Sigil design system: every spacing, radius, colour, type style and
/// motion value the app uses. Screens use these, never one-off numbers.
abstract final class Space {
  static const xs = 4.0;
  static const sm = 8.0;
  static const md = 12.0;
  static const lg = 16.0;
  static const xl = 24.0;
  static const xxl = 32.0;
}

abstract final class Radii {
  static const sm = 12.0;
  static const md = 16.0;
  static const lg = 22.0;
  static const xl = 28.0;
}

/// Neutral surfaces; colour comes from each chat's sigil accent.
abstract final class Palette {
  static const bg = Color(0xFF09090C);
  static const surface = Color(0xFF111116);
  static const surfaceHigh = Color(0xFF191920);
  static const outline = Color(0x14FFFFFF);
  static const outlineStrong = Color(0x24FFFFFF);
  static const text = Color(0xF0FFFFFF);
  static const textMuted = Color(0x9EFFFFFF);
  static const textFaint = Color(0x66FFFFFF);
  static const danger = Color(0xFFFF5D5D);

  /// Black or white, whichever reads better on [background].
  static Color onColor(Color background) =>
      background.computeLuminance() > 0.45 ? const Color(0xFF0B0B0F) : const Color(0xFFFFFFFF);
}

abstract final class AppText {
  static const _display = 'SpaceGrotesk';
  static const _body = 'Inter';
  static const _mono = 'JetBrainsMono';

  static const display = TextStyle(fontFamily: _display, fontSize: 28, fontWeight: FontWeight.w700, letterSpacing: -0.6, height: 1.1);
  static const title = TextStyle(fontFamily: _display, fontSize: 20, fontWeight: FontWeight.w700, letterSpacing: -0.3);
  static const heading = TextStyle(fontFamily: _display, fontSize: 17, fontWeight: FontWeight.w500, letterSpacing: -0.2);
  static const body = TextStyle(fontFamily: _body, fontSize: 15.5, fontWeight: FontWeight.w400, height: 1.35);
  static const bodyStrong = TextStyle(fontFamily: _body, fontSize: 15, fontWeight: FontWeight.w600);
  static const caption = TextStyle(fontFamily: _body, fontSize: 13, fontWeight: FontWeight.w400, color: Palette.textMuted);
  static const micro = TextStyle(fontFamily: _body, fontSize: 11.5, fontWeight: FontWeight.w500, color: Palette.textFaint);
  static const overline = TextStyle(fontFamily: _body, fontSize: 11.5, fontWeight: FontWeight.w600, letterSpacing: 2.2, color: Palette.textFaint);
  static const mono = TextStyle(fontFamily: _mono, fontSize: 15, fontWeight: FontWeight.w500, letterSpacing: 1.2, height: 1.6);
}

/// Material 3 "emphasized" motion. Only GPU-cheap properties (opacity,
/// transform) are animated, so transitions hold 60–120 fps.
abstract final class Motion {
  static const fast = Duration(milliseconds: 160);
  static const medium = Duration(milliseconds: 280);
  static const slow = Duration(milliseconds: 420);

  static const emphasized = Cubic(0.2, 0, 0, 1);
  static const decelerate = Cubic(0.05, 0.7, 0.1, 1);
  static const accelerate = Cubic(0.3, 0, 0.8, 0.15);
}
