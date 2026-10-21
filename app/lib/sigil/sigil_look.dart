import 'dart:math' as math;

import 'package:flutter/painting.dart';

import '../src/rust/api/sigil.dart';
import '../theme/appearance.dart';
import '../theme/tokens.dart';

/// How a sigil is shown: in the accent colour picked in Settings (its
/// other colours turn with it), and light in light mode. Applying it twice
/// changes nothing.
///
/// Comparing sigils with a friend in person uses their exact colours
/// instead (`exact: true` on the sigil widgets), since those must match on
/// both phones.
abstract final class SigilLook {
  static final _cache = <(SigilSpec, int?, bool), SigilSpec>{};

  static SigilSpec apply(SigilSpec s) {
    final pick = Appearance.accent.value;
    final light = Palette.light;
    if (pick == null && !light) return s;
    final key = (s, pick?.toARGB32(), light);
    final hit = _cache[key];
    if (hit != null) return hit;
    if (_cache.length > 400) _cache.clear();

    var accent = s.accent;
    var palette = s.palette;
    var background = s.background;
    if (pick != null) {
      final p = HSLColor.fromColor(pick);
      final turn = p.hue - accent.h;
      HslColor turned(HslColor c) => HslColor(h: (c.h + turn) % 360, s: c.s, l: c.l);
      accent = HslColor(h: p.hue, s: p.saturation, l: p.lightness);
      palette = [for (final c in palette) turned(c)];
      background = turned(background);
    }
    if (light) {
      // Pale background; lines dark enough to see on it.
      HslColor deep(HslColor c) => HslColor(h: c.h, s: c.s, l: math.min(c.l, 0.48));
      background = HslColor(h: background.h, s: math.min(background.s, 0.4), l: 0.94);
      accent = deep(accent);
      palette = [for (final c in palette) deep(c)];
    }
    return _cache[key] = SigilSpec(
      accent: accent,
      palette: palette,
      background: background,
      form: s.form,
      symmetry: s.symmetry,
      layers: s.layers,
      rotation: s.rotation,
      density: s.density,
      weight: s.weight,
      seed: s.seed,
    );
  }
}

extension SigilLookX on SigilSpec {
  /// This sigil as shown (see [SigilLook]).
  SigilSpec get look => SigilLook.apply(this);
}
