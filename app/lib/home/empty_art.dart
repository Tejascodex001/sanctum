import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../theme/tokens.dart';
import 'sanctum_door.dart';

/// What an empty screen is about, for its drawing.
enum EmptyArt { chats, temp, friends }

/// A small drawing for an empty screen: the thing itself on a soft glow,
/// with a few gold sparkles. Drawn, not an image, so it follows the theme
/// and accent colour.
class EmptyArtView extends StatelessWidget {
  const EmptyArtView({super.key, required this.art, this.size = 150});

  final EmptyArt art;
  final double size;

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    return SizedBox.square(
      dimension: size,
      child: CustomPaint(
        painter: _ArtPainter(art: art, accent: accent, gold: Palette.gold, bg: Palette.bg, light: Palette.light),
        // The chats drawing is the door from the logo, ajar.
        child: art == EmptyArt.chats
            ? Center(
                child: CustomPaint(
                  size: Size(size * 0.36, size * 0.41),
                  painter: DoorPainter(open: 0.4, glow: 0.7, gold: Palette.gold, leaf: Palette.bg),
                ),
              )
            : null,
      ),
    );
  }
}

class _ArtPainter extends CustomPainter {
  _ArtPainter({required this.art, required this.accent, required this.gold, required this.bg, required this.light});

  final EmptyArt art;
  final Color accent;
  final Color gold;
  final Color bg;
  final bool light;

  @override
  void paint(Canvas canvas, Size size) {
    final c = size.center(Offset.zero);
    final r = size.shortestSide / 2;

    // The soft glow behind everything.
    canvas.drawCircle(
      c,
      r,
      Paint()
        ..shader = RadialGradient(
          colors: [accent.withValues(alpha: light ? 0.18 : 0.22), accent.withValues(alpha: 0)],
        ).createShader(Rect.fromCircle(center: c, radius: r)),
    );
    canvas.drawCircle(c, r * 0.62, Paint()..color = accent.withValues(alpha: light ? 0.1 : 0.12));
    canvas.drawCircle(
      c,
      r * 0.62,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.2
        ..color = accent.withValues(alpha: 0.3),
    );

    switch (art) {
      case EmptyArt.chats:
        _bubble(canvas, c + Offset(-r * 0.62, -r * 0.34), r * 0.2, accent, flip: false);
        _bubble(canvas, c + Offset(r * 0.6, r * 0.12), r * 0.16, gold, flip: true);
      case EmptyArt.temp:
        _flame(canvas, c, r * 0.42);
      case EmptyArt.friends:
        _friends(canvas, c, r * 0.4);
    }

    // Sparkles.
    for (final (dx, dy, s) in const [(-0.72, 0.42, 0.075), (0.7, -0.5, 0.1), (0.38, 0.74, 0.055), (-0.3, -0.8, 0.05)]) {
      _sparkle(canvas, c + Offset(dx * r, dy * r), s * r, gold);
    }
  }

  /// A rounded speech bubble with three dots.
  void _bubble(Canvas canvas, Offset at, double h, Color color, {required bool flip}) {
    final w = h * 1.6;
    final rect = Rect.fromCenter(center: at, width: w, height: h);
    final tailX = flip ? rect.right - w * 0.25 : rect.left + w * 0.25;
    final path = Path()
      ..addRRect(RRect.fromRectAndRadius(rect, Radius.circular(h / 2)))
      ..moveTo(tailX - h * 0.18, rect.bottom - 1)
      ..lineTo(tailX + (flip ? h * 0.3 : -h * 0.3), rect.bottom + h * 0.32)
      ..lineTo(tailX + h * 0.18, rect.bottom - 1)
      ..close();
    canvas.drawPath(path, Paint()..color = color);
    final dot = Paint()..color = Palette.onColor(color).withValues(alpha: 0.85);
    for (var i = -1; i <= 1; i++) {
      canvas.drawCircle(at + Offset(i * h * 0.3, 0), h * 0.085, dot);
    }
  }

  /// A flame: temp chats burn away.
  void _flame(Canvas canvas, Offset c, double s) {
    Path flame(double k, double lift) {
      final top = c + Offset(0, -s * 1.15 * k - lift);
      final bottom = c.dy + s * 0.9 * k;
      return Path()
        ..moveTo(top.dx, top.dy)
        ..cubicTo(c.dx + s * 0.95 * k, c.dy - s * 0.2 * k, c.dx + s * 0.85 * k, bottom, c.dx, bottom)
        ..cubicTo(c.dx - s * 0.85 * k, bottom, c.dx - s * 0.95 * k, c.dy - s * 0.1 * k, c.dx - s * 0.3 * k,
            c.dy - s * 0.55 * k)
        ..cubicTo(c.dx - s * 0.2 * k, c.dy - s * 0.15 * k, c.dx - s * 0.05 * k, c.dy - s * 0.2 * k, top.dx, top.dy)
        ..close();
    }

    const ember = Color(0xFFFF8A4C);
    canvas.drawPath(
      flame(1, 0),
      Paint()
        ..shader = LinearGradient(
          begin: Alignment.bottomCenter,
          end: Alignment.topCenter,
          colors: [ember, Color.lerp(ember, const Color(0xFFFF5D5D), 0.6)!],
        ).createShader(Rect.fromCircle(center: c, radius: s * 1.2)),
    );
    canvas.save();
    canvas.translate(0, s * 0.32);
    canvas.drawPath(flame(0.55, 0), Paint()..color = gold);
    canvas.restore();
  }

  /// Two friends' rings, overlapping, with a heart where they meet.
  void _friends(Canvas canvas, Offset c, double s) {
    final ring = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = s * 0.16;
    final left = c + Offset(-s * 0.45, 0), right = c + Offset(s * 0.45, 0);
    canvas.drawCircle(left, s * 0.72, Paint()..color = accent.withValues(alpha: 0.25));
    canvas.drawCircle(right, s * 0.72, Paint()..color = gold.withValues(alpha: 0.25));
    canvas.drawCircle(left, s * 0.72, ring..color = accent);
    canvas.drawCircle(right, s * 0.72, ring..color = gold);
    // A small sigil-like mark in each.
    final mark = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = s * 0.07
      ..strokeCap = StrokeCap.round;
    for (final (at, color) in [(left + Offset(-s * 0.15, 0), accent), (right + Offset(s * 0.15, 0), gold)]) {
      mark.color = color;
      for (var i = 0; i < 3; i++) {
        final a = i * math.pi / 3;
        final d = Offset(math.cos(a), math.sin(a)) * s * 0.26;
        canvas.drawLine(at - d, at + d, mark);
      }
    }
    // The heart.
    final h = s * 0.32;
    final heart = Path()
      ..moveTo(c.dx, c.dy + h * 0.75)
      ..cubicTo(c.dx - h * 1.2, c.dy - h * 0.05, c.dx - h * 0.55, c.dy - h * 0.95, c.dx, c.dy - h * 0.35)
      ..cubicTo(c.dx + h * 0.55, c.dy - h * 0.95, c.dx + h * 1.2, c.dy - h * 0.05, c.dx, c.dy + h * 0.75)
      ..close();
    canvas.drawPath(heart, Paint()..color = const Color(0xFFFF5D8F));
  }

  /// A four-pointed sparkle.
  void _sparkle(Canvas canvas, Offset at, double s, Color color) {
    final k = s * 0.22;
    final path = Path()
      ..moveTo(at.dx, at.dy - s)
      ..quadraticBezierTo(at.dx + k, at.dy - k, at.dx + s, at.dy)
      ..quadraticBezierTo(at.dx + k, at.dy + k, at.dx, at.dy + s)
      ..quadraticBezierTo(at.dx - k, at.dy + k, at.dx - s, at.dy)
      ..quadraticBezierTo(at.dx - k, at.dy - k, at.dx, at.dy - s)
      ..close();
    canvas.drawPath(path, Paint()..color = color.withValues(alpha: 0.85));
  }

  @override
  bool shouldRepaint(_ArtPainter old) =>
      old.art != art || old.accent != accent || old.gold != gold || old.bg != bg || old.light != light;
}
