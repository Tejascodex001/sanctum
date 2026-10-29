import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/tokens.dart';

/// The Sanctuary door from the logo, drawn so it can open: the gold frame,
/// and a door leaf (with the keyhole) that swings towards its hinge as
/// [open] goes from 0 to 1, letting the light out.
class DoorPainter extends CustomPainter {
  DoorPainter({required this.open, required this.gold, required this.leaf, this.glow = 0});

  /// 0: shut, as in the logo; 1: wide open.
  final double open;
  final Color gold;

  /// The door leaf's colour (whatever is behind the logo).
  final Color leaf;

  /// Light spilling out around the door, 0 to 1.
  final double glow;

  @override
  void paint(Canvas canvas, Size size) {
    // The drawing's own box is x 24..76, y 21..80 (see docs/brand).
    final s = math.min(size.width / 52, size.height / 59);
    canvas.translate(size.width / 2 - 50 * s, size.height / 2 - 50.5 * s);
    canvas.scale(s);

    Path arch(double left, double right, double top, double bottom) {
      final r = (right - left) / 2;
      return Path()
        ..moveTo(left, bottom)
        ..lineTo(left, top + r)
        ..arcToPoint(Offset(right, top + r), radius: Radius.circular(r))
        ..lineTo(right, bottom)
        ..close();
    }

    final outer = arch(24, 76, 21, 80);
    final inner = arch(38.5, 61.5, 40, 80);

    if (glow > 0) {
      canvas.drawCircle(
        const Offset(50, 60),
        34,
        Paint()
          ..shader = RadialGradient(
            colors: [gold.withValues(alpha: 0.45 * glow), gold.withValues(alpha: 0)],
          ).createShader(Rect.fromCircle(center: const Offset(50, 60), radius: 34)),
      );
    }
    // The light inside the doorway.
    if (open > 0) {
      canvas.drawPath(
        inner,
        Paint()
          ..shader = LinearGradient(
            begin: Alignment.bottomCenter,
            end: Alignment.topCenter,
            colors: [Color.lerp(gold, Colors.white, 0.55)!, gold],
          ).createShader(const Rect.fromLTRB(38.5, 40, 61.5, 80)),
      );
    }
    // The leaf turns on its left hinge: narrower as it opens.
    canvas.save();
    canvas.clipPath(inner);
    canvas.translate(38.5, 0);
    canvas.scale(math.max(0.0, 1 - open * 0.92), 1);
    canvas.translate(-38.5, 0);
    canvas.drawPath(inner, Paint()..color = Color.lerp(leaf, Colors.black, open * 0.3)!);
    final key = Paint()..color = gold.withValues(alpha: 1 - open);
    canvas.drawCircle(const Offset(50, 60), 4.6, key);
    canvas.drawPath(
      Path()
        ..moveTo(52.4, 62)
        ..lineTo(54, 72)
        ..lineTo(46, 72)
        ..lineTo(47.6, 62)
        ..close(),
      key,
    );
    canvas.restore();

    canvas.drawPath(
      Path.combine(PathOperation.difference, outer, inner),
      Paint()..color = gold,
    );
  }

  @override
  bool shouldRepaint(DoorPainter old) => old.open != open || old.glow != glow || old.gold != gold || old.leaf != leaf;
}

/// The small door beside "Sanctum": opens and closes again when tapped.
class TappableDoor extends StatefulWidget {
  const TappableDoor({super.key, this.size = 26});

  final double size;

  @override
  State<TappableDoor> createState() => TappableDoorState();
}

class TappableDoorState extends State<TappableDoor> with SingleTickerProviderStateMixin {
  late final _swing = AnimationController(vsync: this, duration: const Duration(milliseconds: 1100));

  /// Swings the door open and shut.
  void knock() {
    if (_swing.isAnimating) return;
    _swing.forward(from: 0);
  }

  @override
  void dispose() {
    _swing.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _swing,
      builder: (context, _) {
        // Up and back down: open fast, close softly.
        final t = _swing.value;
        final open =
            t < 0.35 ? Curves.easeOutCubic.transform(t / 0.35) : 1 - Curves.easeInOut.transform((t - 0.35) / 0.65);
        return CustomPaint(
          size: Size.square(widget.size),
          painter: DoorPainter(open: open, glow: open, gold: Palette.gold, leaf: Palette.bg),
        );
      },
    );
  }
}

/// What tapping "Sanctum" seven times shows: the door opens wide, and a few
/// words about your sanctuary.
Future<void> showSanctuary(BuildContext context, {required int days, required int friends, required int groups}) {
  HapticFeedback.heavyImpact();
  return Navigator.of(context).push(PageRouteBuilder<void>(
    opaque: false,
    barrierDismissible: true,
    barrierColor: Colors.black54,
    transitionDuration: Motion.medium,
    pageBuilder: (_, a, __) => FadeTransition(
      opacity: a,
      child: _Sanctuary(days: days, friends: friends, groups: groups),
    ),
  ));
}

class _Sanctuary extends StatefulWidget {
  const _Sanctuary({required this.days, required this.friends, required this.groups});

  final int days;
  final int friends;
  final int groups;

  @override
  State<_Sanctuary> createState() => _SanctuaryState();
}

class _SanctuaryState extends State<_Sanctuary> with SingleTickerProviderStateMixin {
  late final _open = AnimationController(vsync: this, duration: const Duration(milliseconds: 1800))..forward();

  @override
  void dispose() {
    _open.dispose();
    super.dispose();
  }

  String _plural(int n, String one) => '$n $one${n == 1 ? '' : 's'}';

  @override
  Widget build(BuildContext context) {
    const midnight = Color(0xFF0E1220);
    const gold = Color(0xFFE8BE62);
    final lines = [
      widget.days <= 1 ? 'Your first day here' : 'Here for ${widget.days} days',
      '${_plural(widget.friends, 'friend')} · ${_plural(widget.groups, 'group')}',
      '0 servers · 0 ads · 0 trackers',
      'What you say stays between you and them.',
    ];
    return GestureDetector(
      onTap: () => Navigator.pop(context),
      child: Scaffold(
        backgroundColor: Colors.transparent,
        body: Center(
          child: Container(
            margin: const EdgeInsets.all(Space.xl),
            padding: const EdgeInsets.fromLTRB(Space.xl, Space.xxl, Space.xl, Space.xl),
            decoration: BoxDecoration(
              gradient: const LinearGradient(
                begin: Alignment.topLeft,
                end: Alignment.bottomRight,
                colors: [midnight, Color(0xFF262E47)],
              ),
              borderRadius: BorderRadius.circular(Radii.xl),
              border: Border.all(color: gold.withValues(alpha: 0.35)),
            ),
            child: AnimatedBuilder(
              animation: _open,
              builder: (context, _) {
                final t = _open.value;
                final open = Curves.easeInOutCubic.transform((t / 0.6).clamp(0.0, 1.0));
                final words = Curves.easeOut.transform(((t - 0.45) / 0.55).clamp(0.0, 1.0));
                return Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    CustomPaint(
                      size: const Size(150, 170),
                      painter: DoorPainter(open: open, glow: open, gold: gold, leaf: midnight),
                    ),
                    const SizedBox(height: Space.lg),
                    Opacity(
                      opacity: words,
                      child: Transform.translate(
                        offset: Offset(0, 12 * (1 - words)),
                        child: Column(
                          children: [
                            Text(
                              'Welcome to your sanctuary',
                              textAlign: TextAlign.center,
                              style: AppText.title.copyWith(color: gold),
                            ),
                            const SizedBox(height: Space.md),
                            for (final l in lines)
                              Padding(
                                padding: const EdgeInsets.only(bottom: Space.xs),
                                child: Text(
                                  l,
                                  textAlign: TextAlign.center,
                                  style: AppText.body.copyWith(color: Colors.white.withValues(alpha: 0.82)),
                                ),
                              ),
                          ],
                        ),
                      ),
                    ),
                  ],
                );
              },
            ),
          ),
        ),
      ),
    );
  }
}
