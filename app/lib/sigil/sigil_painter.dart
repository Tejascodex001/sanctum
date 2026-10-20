import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../src/rust/api/sigil.dart';

extension HslColorX on HslColor {
  Color get color => HSLColor.fromAHSL(1, h, s, l).toColor();
}

/// Renders a [SigilSpec] (computed in Rust from a key fingerprint).
///
/// [phase] runs 0..1 in a loop and only drives slow rotation and breathing;
/// the shape itself depends solely on the spec, so both friends see the
/// same sigil.
class SigilPainter extends CustomPainter {
  SigilPainter(this.spec, {this.phase = 0, this.intensity = 1});

  final SigilSpec spec;
  final double phase;

  /// Dims the art behind content (chat backgrounds use ~0.35).
  final double intensity;

  @override
  void paint(Canvas canvas, Size size) {
    final center = size.center(Offset.zero);
    final radius = size.shortestSide / 2;
    final rect = Offset.zero & size;

    canvas.drawRect(
      rect,
      Paint()
        ..shader = RadialGradient(
          colors: [
            spec.palette[1].color.withValues(alpha: 0.9 * intensity),
            const Color(0xFF07070B),
          ],
          radius: 0.9,
        ).createShader(rect),
    );

    final rnd = math.Random(spec.seed);
    final turn = phase * 2 * math.pi;
    canvas.save();
    canvas.translate(center.dx, center.dy);
    canvas.rotate(spec.rotation * math.pi / 180 + turn / 12);

    for (var layer = 0; layer < spec.layers; layer++) {
      final depth = (layer + 1) / spec.layers;
      final breathe = 1 + 0.04 * math.sin(turn + layer);
      final r = radius * (0.25 + 0.6 * depth) * breathe;
      final color = Color.lerp(spec.accent.color, spec.palette[0].color,
          layer / math.max(1, spec.layers - 1))!;
      final paint = Paint()
        ..color = color.withValues(alpha: (0.55 - 0.08 * layer) * intensity)
        ..style = layer.isEven ? PaintingStyle.stroke : PaintingStyle.fill
        ..strokeWidth = 1.5 + rnd.nextDouble() * 2
        ..blendMode = BlendMode.plus
        ..maskFilter = MaskFilter.blur(BlurStyle.normal, 1 + layer * 1.5);
      final jitter = 0.85 + rnd.nextDouble() * 0.3;
      final layerTurn = (layer.isEven ? 1 : -1) * turn / (8 + layer * 4);

      for (var k = 0; k < spec.symmetry; k++) {
        canvas.save();
        canvas.rotate(k * 2 * math.pi / spec.symmetry + layerTurn);
        canvas.drawPath(_motif(r * jitter, rnd.nextDouble()), paint);
        canvas.restore();
      }
    }

    canvas.drawCircle(
      Offset.zero,
      radius * 0.06,
      Paint()
        ..color = spec.accent.color.withValues(alpha: intensity)
        ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 6),
    );
    canvas.restore();
  }

  /// One repeated element, drawn pointing "up" from the centre.
  Path _motif(double r, double v) {
    final p = Path();
    switch (spec.form) {
      case SigilForm.rings:
        p.addOval(Rect.fromCircle(
            center: Offset(0, -r * 0.5), radius: r * (0.3 + 0.2 * v)));
      case SigilForm.petals:
        p
          ..moveTo(0, 0)
          ..quadraticBezierTo(r * 0.35, -r * 0.5, 0, -r)
          ..quadraticBezierTo(-r * 0.35, -r * 0.5, 0, 0);
      case SigilForm.shards:
        p
          ..moveTo(0, -r)
          ..lineTo(r * 0.18, -r * (0.3 + 0.2 * v))
          ..lineTo(-r * 0.12, -r * 0.2)
          ..close();
      case SigilForm.waves:
        p.moveTo(0, 0);
        for (var i = 1; i <= 24; i++) {
          final t = i / 24;
          p.lineTo(math.sin(t * math.pi * (2 + 2 * v)) * r * 0.15, -t * r);
        }
      case SigilForm.lattice:
        p.addRect(Rect.fromCenter(
            center: Offset(0, -r * 0.6), width: r * 0.4, height: r * 0.4));
        p.moveTo(0, 0);
        p.lineTo(0, -r);
      case SigilForm.spiral:
        p.moveTo(0, 0);
        for (var i = 1; i <= 40; i++) {
          final t = i / 40;
          final a = t * math.pi * (1 + v);
          p.lineTo(math.sin(a) * r * t * 0.5, -math.cos(a) * r * t);
        }
    }
    return p;
  }

  @override
  bool shouldRepaint(SigilPainter old) =>
      old.spec != spec || old.phase != phase || old.intensity != intensity;
}

/// A looping, slowly animated sigil.
class LivingSigil extends StatefulWidget {
  const LivingSigil({super.key, required this.spec, this.intensity = 1});

  final SigilSpec spec;
  final double intensity;

  @override
  State<LivingSigil> createState() => _LivingSigilState();
}

class _LivingSigilState extends State<LivingSigil>
    with SingleTickerProviderStateMixin {
  late final _controller = AnimationController(
    vsync: this,
    duration: const Duration(seconds: 40),
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: AnimatedBuilder(
        animation: _controller,
        builder: (_, _) => CustomPaint(
          painter: SigilPainter(widget.spec,
              phase: _controller.value, intensity: widget.intensity),
          size: Size.infinite,
        ),
      ),
    );
  }
}
