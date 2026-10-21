import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import '../src/rust/api/sigil.dart';
import 'sigil_look.dart';

extension HslColorX on HslColor {
  Color get color => HSLColor.fromAHSL(1, h, s.clamp(0, 1), l.clamp(0, 1)).toColor();
}

/// Renders a [SigilSpec] (computed in Rust from a key fingerprint) as crisp
/// vector line art. No blur: every edge is sharp at any size.
///
/// [phase] (0..1, looping) only adds slow rotation; the emblem itself depends
/// solely on the spec, so both friends always see the same sigil.
class SigilPainter extends CustomPainter {
  SigilPainter(
    this.spec, {
    this.phase = 0,
    this.opacity = 1,
    this.background = true,
  });

  final SigilSpec spec;
  final double phase;

  /// Fades the art (chat backdrops use a low value).
  final double opacity;

  /// Whether to fill the canvas with the sigil's own background.
  final bool background;

  late final _bg = spec.background.color;
  late final _accent = spec.accent.color;
  late final _palette = [for (final c in spec.palette) c.color];

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    if (background) {
      canvas.drawRect(
        rect,
        Paint()
          ..shader = RadialGradient(
            colors: [Color.lerp(_bg, _accent, 0.12)!, _bg],
          ).createShader(rect),
      );
    }

    final r = size.shortestSide / 2 * 0.84;
    final rnd = math.Random(spec.seed);
    canvas.save();
    canvas.translate(size.width / 2, size.height / 2);
    canvas.rotate(spec.rotation * math.pi / 180 + phase * 2 * math.pi);
    if (opacity < 1) {
      canvas.saveLayer(null, Paint()..color = Color.fromRGBO(0, 0, 0, opacity));
    }

    switch (spec.form) {
      case SigilForm.rosette:
        _rosette(canvas, r, rnd);
      case SigilForm.constellation:
        _constellation(canvas, r, rnd);
      case SigilForm.shards:
        _shards(canvas, r, rnd);
      case SigilForm.orbits:
        _orbits(canvas, r, rnd);
      case SigilForm.lattice:
        _lattice(canvas, r, rnd);
      case SigilForm.glyph:
        _glyph(canvas, r, rnd);
    }

    if (opacity < 1) canvas.restore();
    canvas.restore();
  }

  double _stroke(double r, [double scale = 1]) => r * (0.007 + 0.016 * spec.weight) * scale;

  Paint _line(Color c, double width) => Paint()
    ..color = c
    ..style = PaintingStyle.stroke
    ..strokeWidth = width
    ..strokeCap = StrokeCap.round
    ..strokeJoin = StrokeJoin.round
    ..isAntiAlias = true;

  Color _layerColor(int i) => i == 0 ? _accent : _palette[(i - 1) % 3];

  /// Spirograph curves: z(t) = e^{it} + c·e^{-i(k-1)t}, which has k-fold symmetry.
  void _rosette(Canvas canvas, double r, math.Random rnd) {
    final k = spec.symmetry;
    for (var layer = 0; layer < spec.layers; layer++) {
      final c = 0.15 + rnd.nextDouble() * 0.7;
      final scale = r * (1 - layer * 0.14) / (1 + c);
      final path = Path();
      const steps = 720;
      for (var i = 0; i <= steps; i++) {
        final t = i / steps * 2 * math.pi;
        final x = math.cos(t) + c * math.cos(-(k - 1) * t);
        final y = math.sin(t) + c * math.sin(-(k - 1) * t);
        i == 0 ? path.moveTo(x * scale, y * scale) : path.lineTo(x * scale, y * scale);
      }
      canvas.drawPath(
        path,
        _line(_layerColor(layer).withValues(alpha: layer == 0 ? 1 : 0.75), _stroke(r, layer == 0 ? 1.2 : 0.8)),
      );
    }
    canvas.drawCircle(Offset.zero, r * 0.05, Paint()..color = _accent);
  }

  /// Rings of stars, each joined to stars on the next ring.
  void _constellation(Canvas canvas, double r, math.Random rnd) {
    final rings = <List<Offset>>[];
    for (var ring = 0; ring < spec.layers; ring++) {
      final radius = r * (0.25 + 0.75 * (ring + 1) / spec.layers);
      final count = spec.symmetry * (1 + (ring.isOdd && spec.density > 0.5 ? 1 : 0));
      final offset = rnd.nextDouble() * math.pi;
      rings.add([
        for (var i = 0; i < count; i++) Offset.fromDirection(offset + i * 2 * math.pi / count, radius),
      ]);
    }
    final edge = _line(_palette[0].withValues(alpha: 0.7), _stroke(r, 0.6));
    for (var ring = 0; ring < rings.length; ring++) {
      final here = rings[ring];
      final skip = 1 + rnd.nextInt(3);
      // Chords within the ring.
      for (var i = 0; i < here.length; i++) {
        canvas.drawLine(here[i], here[(i + skip) % here.length], edge);
      }
      // Spokes to the next ring out.
      if (ring + 1 < rings.length) {
        final next = rings[ring + 1];
        for (var i = 0; i < here.length; i++) {
          canvas.drawLine(here[i], next[(i * next.length ~/ here.length) % next.length],
              _line(_palette[1].withValues(alpha: 0.6), _stroke(r, 0.5)));
        }
      }
    }
    for (var ring = 0; ring < rings.length; ring++) {
      final starR = r * (0.018 + 0.02 * spec.weight) * (ring == rings.length - 1 ? 1.4 : 1);
      for (final p in rings[ring]) {
        canvas.drawCircle(p, starR, Paint()..color = ring.isEven ? _accent : _palette[2]);
      }
    }
  }

  /// A kaleidoscope: random polygons in one wedge, mirrored all the way round.
  void _shards(Canvas canvas, double r, math.Random rnd) {
    final k = spec.symmetry;
    final wedge = 2 * math.pi / k;
    final shapes = <(Path, Color)>[];
    final count = 2 + (spec.density * 3).round();
    for (var s = 0; s < count; s++) {
      final path = Path();
      for (var v = 0; v < 3; v++) {
        final p = Offset.fromDirection(rnd.nextDouble() * wedge / 2, r * (0.15 + rnd.nextDouble() * 0.85));
        v == 0 ? path.moveTo(p.dx, p.dy) : path.lineTo(p.dx, p.dy);
      }
      path.close();
      shapes.add((path, s == 0 ? _accent : _palette[s % 3]));
    }
    for (var i = 0; i < k; i++) {
      for (final mirror in [false, true]) {
        canvas.save();
        canvas.rotate(i * wedge);
        if (mirror) canvas.scale(1, -1);
        for (final (path, color) in shapes) {
          canvas.drawPath(path, Paint()..color = color.withValues(alpha: 0.55));
          canvas.drawPath(path, _line(color, _stroke(r, 0.5)));
        }
        canvas.restore();
      }
    }
  }

  /// Concentric broken rings with satellites.
  void _orbits(Canvas canvas, double r, math.Random rnd) {
    final rings = 3 + spec.layers + (spec.density * 3).round();
    for (var i = 0; i < rings; i++) {
      final radius = r * (0.2 + 0.8 * (i + 1) / rings);
      final width = _stroke(r, 0.6 + rnd.nextDouble() * 2.2);
      final segments = 1 + rnd.nextInt(spec.symmetry);
      final gap = 0.15 + rnd.nextDouble() * 0.5;
      final start = rnd.nextDouble() * 2 * math.pi;
      final sweep = 2 * math.pi / segments;
      final paint = _line(_layerColor(i % 4), width);
      for (var s = 0; s < segments; s++) {
        canvas.drawArc(
            Rect.fromCircle(center: Offset.zero, radius: radius), start + s * sweep, sweep * (1 - gap), false, paint);
      }
      if (rnd.nextDouble() < 0.4) {
        canvas.drawCircle(Offset.fromDirection(start - 0.2, radius), width * 1.6, Paint()..color = _accent);
      }
    }
  }

  /// Polygons that twist as they shrink, producing a moiré vortex.
  void _lattice(Canvas canvas, double r, math.Random rnd) {
    final sides = 3 + spec.symmetry % 6;
    final count = 8 + (spec.density * 14).round();
    final twist = (0.04 + rnd.nextDouble() * 0.12) * (rnd.nextBool() ? 1 : -1);
    for (var i = 0; i < count; i++) {
      final t = i / count;
      final radius = r * (1 - t * 0.92);
      final path = Path();
      for (var v = 0; v <= sides; v++) {
        final p = Offset.fromDirection(i * twist + v * 2 * math.pi / sides, radius);
        v == 0 ? path.moveTo(p.dx, p.dy) : path.lineTo(p.dx, p.dy);
      }
      final color = Color.lerp(_accent, _palette[(i ~/ 4) % 3], t)!;
      canvas.drawPath(path, _line(color.withValues(alpha: 1 - t * 0.5), _stroke(r, i == 0 ? 1.4 : 0.7)));
    }
  }

  /// A rune: one continuous stroke walked on a 5×5 grid, mirrored
  /// left/right like a monogram, inside a frame. Always drawn upright.
  void _glyph(Canvas canvas, double r, math.Random rnd) {
    canvas.rotate(-(spec.rotation * math.pi / 180 + phase * 2 * math.pi));
    const n = 5, mid = 2;
    final cell = r * 1.1 / (n - 1);
    Offset pt(int x, int y) => Offset((x - mid) * cell, (y - mid) * cell);

    // Re-walk until the rune reaches the outer column and has some body;
    // the generator is seeded, so this stays deterministic.
    final steps = 6 + (spec.density * 5).round();
    const moves = [(0, 1), (-1, 0), (1, 0), (-1, 1), (1, 1), (-1, -1), (0, -1)];
    late List<(int, int)> walk;
    do {
      var x = mid, y = rnd.nextInt(2);
      walk = [(x, y)];
      for (var i = 0; i < steps; i++) {
        final (dx, dy) = moves[rnd.nextInt(moves.length)];
        final next = ((x + dx).clamp(0, mid), (y + dy).clamp(0, n - 1));
        if (walk.contains(next)) continue;
        (x, y) = next;
        walk.add(next);
      }
    } while (walk.length < 5 || !walk.any((p) => p.$1 == 0));

    Path trace(bool mirrored) {
      final path = Path();
      for (var i = 0; i < walk.length; i++) {
        final (wx, wy) = walk[i];
        final p = pt(mirrored ? n - 1 - wx : wx, wy);
        i == 0 ? path.moveTo(p.dx, p.dy) : path.lineTo(p.dx, p.dy);
      }
      return path;
    }

    // A soft under-stroke in a palette colour gives the rune depth.
    for (final mirrored in [false, true]) {
      canvas.drawPath(trace(mirrored), _line(_palette[1].withValues(alpha: 0.45), _stroke(r, 5)));
    }
    for (final mirrored in [false, true]) {
      canvas.drawPath(trace(mirrored), _line(_accent, _stroke(r, 2.4)));
    }
    final node = Paint()..color = _palette[2];
    for (final (wx, wy) in [walk.first, walk.last]) {
      canvas.drawCircle(pt(wx, wy), _stroke(r, 3.2), node);
      canvas.drawCircle(pt(n - 1 - wx, wy), _stroke(r, 3.2), node);
    }

    final frame = _line(_palette[0].withValues(alpha: 0.6), _stroke(r, 0.8));
    canvas.drawCircle(Offset.zero, r, frame);
    if (spec.layers > 3) canvas.drawCircle(Offset.zero, r * 0.92, frame);
    if (spec.symmetry.isEven) {
      for (var i = 0; i < spec.symmetry; i++) {
        canvas.drawCircle(
            Offset.fromDirection(i * 2 * math.pi / spec.symmetry, r), _stroke(r, 1.6), Paint()..color = _palette[0]);
      }
    }
  }

  @override
  bool shouldRepaint(SigilPainter old) => old.spec != spec || old.phase != phase || old.opacity != opacity;
}

/// Rendered sigils, kept as GPU images. Drawing a sigil is expensive
/// (hundreds of path segments); showing or rotating an image is nearly
/// free, so every sigil on screen comes from here.
abstract final class _SigilImages {
  static final _ready = <String, ui.Image>{};
  static final _pending = <String, Future<ui.Image>>{};
  static const _max = 120;

  static String _key(SigilSpec spec, int px, double opacity) => '${spec.hashCode}/$px/$opacity';

  static ui.Image? peek(SigilSpec spec, int px, double opacity) {
    final key = _key(spec, px, opacity);
    final image = _ready.remove(key);
    if (image != null) _ready[key] = image; // most recently used last
    return image;
  }

  static Future<ui.Image> load(SigilSpec spec, int px, double opacity) {
    final key = _key(spec, px, opacity);
    return _pending[key] ??= () async {
      final recorder = ui.PictureRecorder();
      SigilPainter(spec, opacity: opacity, background: false).paint(Canvas(recorder), Size.square(px.toDouble()));
      final image = await recorder.endRecording().toImage(px, px);
      _ready[key] = image;
      _pending.remove(key);
      // Least recently used images are dropped (not disposed: one may
      // still be on screen; the GC frees it once unused).
      while (_ready.length > _max) {
        _ready.remove(_ready.keys.first);
      }
      return image;
    }();
  }
}

/// A sigil, drawn once into an image and cached. Cheap enough for long,
/// fast-scrolling lists and for animation.
class SigilView extends StatelessWidget {
  const SigilView({super.key, required this.spec, this.opacity = 1, this.background = true, this.exact = false});

  final SigilSpec spec;
  final double opacity;
  final bool background;

  /// Its own colours, not the picked accent or light mode (comparing with
  /// a friend).
  final bool exact;

  @override
  Widget build(BuildContext context) {
    final spec = exact ? this.spec : this.spec.look;
    final dpr = MediaQuery.devicePixelRatioOf(context);
    return Stack(
      fit: StackFit.expand,
      children: [
        if (background) SigilBackground(spec: spec, exact: true),
        LayoutBuilder(builder: (context, box) {
          final side = box.biggest.shortestSide;
          if (!side.isFinite || side <= 0) return const SizedBox.shrink();
          // Round up to 32 px steps so similar sizes share one image.
          final px = (((side * dpr) / 32).ceil() * 32).clamp(32, 2048);
          final ready = _SigilImages.peek(spec, px, opacity);
          final image = ready == null
              ? FutureBuilder<ui.Image>(
                  future: _SigilImages.load(spec, px, opacity),
                  builder: (_, snap) => snap.data == null
                      ? const SizedBox.shrink()
                      : TweenAnimationBuilder<double>(
                          tween: Tween(begin: 0, end: 1),
                          duration: const Duration(milliseconds: 220),
                          builder: (_, v, _) => RawImage(image: snap.data, opacity: AlwaysStoppedAnimation(v)),
                        ),
                )
              : RawImage(image: ready);
          return Center(child: SizedBox.square(dimension: side, child: image));
        }),
      ],
    );
  }
}

/// A slowly turning sigil (lock screen, chat backdrops). Only the cached
/// image rotates, on the GPU; nothing is redrawn per frame.
class LivingSigil extends StatefulWidget {
  const LivingSigil({super.key, required this.spec, this.opacity = 1, this.period = 90, this.exact = false});

  final SigilSpec spec;
  final double opacity;

  /// Its own colours (see [SigilView.exact]).
  final bool exact;

  /// Seconds per full turn.
  final int period;

  @override
  State<LivingSigil> createState() => _LivingSigilState();
}

class _LivingSigilState extends State<LivingSigil> with SingleTickerProviderStateMixin {
  late final _controller = AnimationController(
    vsync: this,
    duration: Duration(seconds: widget.period),
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final spec = widget.exact ? widget.spec : widget.spec.look;
    return Stack(
      fit: StackFit.expand,
      children: [
        SigilBackground(spec: spec, exact: true),
        RepaintBoundary(
          child: RotationTransition(
            turns: _controller,
            child: SigilView(spec: spec, opacity: widget.opacity, background: false, exact: true),
          ),
        ),
      ],
    );
  }
}

class SigilBackground extends StatelessWidget {
  const SigilBackground({super.key, required this.spec, this.exact = false});

  final SigilSpec spec;

  /// Its own colours (see [SigilView.exact]).
  final bool exact;

  @override
  Widget build(BuildContext context) {
    final spec = exact ? this.spec : this.spec.look;
    final bg = spec.background.color;
    return DecoratedBox(
      decoration: BoxDecoration(
        gradient: RadialGradient(colors: [Color.lerp(bg, spec.accent.color, 0.12)!, bg]),
      ),
    );
  }
}
