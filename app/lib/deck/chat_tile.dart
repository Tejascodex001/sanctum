import 'package:flutter/material.dart';

import '../sigil/sigil_painter.dart';
import 'chat_card.dart';

/// The face of a chat card: emblem, name, last message, and the chat's sigil
/// as a large cropped watermark so every card looks like its own object.
class ChatTile extends StatelessWidget {
  const ChatTile({super.key, required this.card});

  final ChatCard card;

  static const height = 82.0;

  @override
  Widget build(BuildContext context) {
    final bg = card.sigil.background.color;
    final accent = card.sigil.accent.color;
    return SizedBox(
      height: height,
      child: DecoratedBox(
        decoration: BoxDecoration(
          gradient: LinearGradient(
            begin: Alignment.centerLeft,
            end: Alignment.centerRight,
            colors: [
              Color.lerp(bg, Colors.black, 0.2)!,
              Color.lerp(bg, accent, 0.10)!,
            ],
          ),
        ),
        child: Stack(
          children: [
            Positioned(
              right: -30,
              top: -34,
              width: 150,
              height: 150,
              child: SigilView(spec: card.sigil, opacity: 0.35, background: false),
            ),
            // Keeps text readable over the watermark.
            Positioned.fill(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    colors: [bg.withValues(alpha: 0.9), bg.withValues(alpha: 0)],
                    stops: const [0.45, 0.85],
                  ),
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Row(
                children: [
                  _Emblem(card: card),
                  const SizedBox(width: 12),
                  Expanded(child: _Text(card: card)),
                  const SizedBox(width: 8),
                  _Trailing(card: card),
                ],
              ),
            ),
            // A hairline of the accent along the top edge, like a card's foil.
            Positioned(
              left: 24,
              right: 24,
              top: 0,
              child: Container(
                height: 1,
                decoration: BoxDecoration(
                  gradient: LinearGradient(colors: [
                    accent.withValues(alpha: 0),
                    accent.withValues(alpha: 0.7),
                    accent.withValues(alpha: 0),
                  ]),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _Emblem extends StatelessWidget {
  const _Emblem({required this.card});

  final ChatCard card;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: 48,
      height: 48,
      padding: const EdgeInsets.all(1.5),
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        gradient: SweepGradient(colors: [
          card.sigil.accent.color,
          card.sigil.palette[0].color,
          card.sigil.accent.color,
        ]),
      ),
      child: ClipOval(child: SigilView(spec: card.sigil)),
    );
  }
}

class _Text extends StatelessWidget {
  const _Text({required this.card});

  final ChatCard card;

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            if (card.pinned) ...[
              Icon(Icons.push_pin_rounded, size: 14, color: card.sigil.accent.color),
              const SizedBox(width: 4),
            ],
            Flexible(
              child: Text(
                card.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  color: Colors.white,
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                  letterSpacing: -0.2,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 2),
        Text(
          card.preview ?? (card.temp ? 'Nothing yet · burns 24h after sending' : 'No messages yet'),
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: TextStyle(
            fontSize: 14,
            color: Colors.white.withValues(alpha: card.preview == null ? 0.4 : 0.65),
          ),
        ),
      ],
    );
  }
}

class _Trailing extends StatelessWidget {
  const _Trailing({required this.card});

  final ChatCard card;

  String _ago(DateTime t) {
    final d = DateTime.now().difference(t);
    if (d.inMinutes < 1) return 'now';
    if (d.inHours < 1) return '${d.inMinutes}m';
    if (d.inDays < 1) return '${d.inHours}h';
    return '${d.inDays}d';
  }

  @override
  Widget build(BuildContext context) {
    final accent = card.sigil.accent.color;
    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        if (card.preview != null)
          Text(_ago(card.updatedAt),
              style: const TextStyle(fontSize: 12, color: Colors.white54)),
        if (card.temp && card.nextBurn != null) ...[
          const SizedBox(height: 6),
          _BurnRing(burnsAt: card.nextBurn!, color: accent),
        ],
      ],
    );
  }
}

/// Shows how much of the 24 hours the oldest temp message has left.
class _BurnRing extends StatelessWidget {
  const _BurnRing({required this.burnsAt, required this.color});

  final DateTime burnsAt;
  final Color color;

  @override
  Widget build(BuildContext context) {
    final left = burnsAt.difference(DateTime.now());
    final fraction = (left.inSeconds / const Duration(hours: 24).inSeconds).clamp(0.0, 1.0);
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(left.inHours >= 1 ? '${left.inHours}h' : '${left.inMinutes.clamp(0, 59)}m',
            style: TextStyle(fontSize: 12, color: color, fontWeight: FontWeight.w600)),
        const SizedBox(width: 6),
        SizedBox.square(
          dimension: 16,
          child: TweenAnimationBuilder<double>(
            tween: Tween(begin: 1, end: fraction),
            duration: const Duration(milliseconds: 900),
            curve: Curves.easeOutCubic,
            builder: (_, v, _) => CircularProgressIndicator(
              value: v,
              strokeWidth: 2.5,
              color: color,
              backgroundColor: color.withValues(alpha: 0.15),
            ),
          ),
        ),
      ],
    );
  }
}
