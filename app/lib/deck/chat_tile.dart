import 'package:flutter/material.dart';

import '../net/net_bus.dart';
import '../notify/notifier.dart';
import '../sigil/sigil_painter.dart';
import '../theme/tokens.dart';
import 'chat_card.dart';

/// The face of a chat card: emblem, name, last message, and the chat's sigil
/// as a large cropped watermark so every card looks like its own object.
class ChatTile extends StatelessWidget {
  const ChatTile({super.key, required this.card});

  final ChatCard card;

  static const height = 78.0;

  @override
  Widget build(BuildContext context) {
    final bg = card.sigil.background.color;
    return SizedBox(
      height: height,
      child: Stack(
        children: [
          Positioned(
            right: -28,
            top: -36,
            width: 150,
            height: 150,
            child: SigilView(spec: card.sigil, opacity: 0.28, background: false),
          ),
          // Keeps text readable over the watermark.
          Positioned.fill(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  colors: [bg, bg.withValues(alpha: 0.85), bg.withValues(alpha: 0)],
                  stops: const [0, 0.55, 0.9],
                ),
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Space.lg),
            child: Row(
              children: [
                Stack(
                  clipBehavior: Clip.none,
                  children: [
                    Hero(tag: 'emblem-${card.id}', child: _Emblem(card: card)),
                    if (card.friendChatId != null)
                      Positioned(right: 0, bottom: 0, child: OnlineDot(chatId: card.friendChatId!)),
                  ],
                ),
                const SizedBox(width: Space.md),
                Expanded(child: _Text(card: card)),
                const SizedBox(width: Space.sm),
                _Trailing(card: card),
              ],
            ),
          ),
        ],
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
      width: 46,
      height: 46,
      padding: const EdgeInsets.all(1.5),
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        border: Border.all(color: card.sigil.accent.color.withValues(alpha: 0.55), width: 1.2),
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
              Icon(Icons.push_pin_rounded, size: 13, color: card.sigil.accent.color),
              const SizedBox(width: Space.xs),
            ],
            Flexible(
              child: Text(card.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.heading),
            ),
          ],
        ),
        const SizedBox(height: 3),
        Text(
          card.preview ?? (card.temp ? 'Burns 24h after sending' : 'No messages yet'),
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: AppText.caption.copyWith(color: card.preview == null ? Palette.textFaint : Palette.textMuted),
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
        if (card.preview != null) Text(_ago(card.updatedAt), style: AppText.micro),
        ValueListenableBuilder<Map<String, int>>(
          valueListenable: Notifier.instance.unread,
          builder: (context, unread, _) {
            final n = unread[card.temp ? Notifier.tempKey(card.id) : Notifier.chatKey(card.id)] ?? 0;
            return AnimatedScale(
              scale: n > 0 ? 1 : 0,
              duration: Motion.medium,
              curve: Motion.emphasized,
              child: Container(
                margin: const EdgeInsets.only(top: Space.xs),
                constraints: const BoxConstraints(minWidth: 20),
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(color: accent, borderRadius: BorderRadius.circular(Radii.sm)),
                child: Text(
                  n > 99 ? '99+' : '$n',
                  textAlign: TextAlign.center,
                  style: AppText.micro.copyWith(color: Palette.onColor(accent), fontWeight: FontWeight.w700),
                ),
              ),
            );
          },
        ),
        if (card.temp && card.nextBurn != null) ...[
          const SizedBox(height: Space.xs + 2),
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
        Text(
          left.inHours >= 1 ? '${left.inHours}h' : '${left.inMinutes.clamp(0, 59)}m',
          style: AppText.micro.copyWith(color: color, fontWeight: FontWeight.w600),
        ),
        const SizedBox(width: 6),
        SizedBox.square(
          dimension: 14,
          child: CircularProgressIndicator(
            value: fraction,
            strokeWidth: 2.2,
            color: color,
            backgroundColor: color.withValues(alpha: 0.15),
          ),
        ),
      ],
    );
  }
}

/// Green while the friend is connected; nothing otherwise.
class OnlineDot extends StatelessWidget {
  const OnlineDot({super.key, required this.chatId, this.size = 12});

  final int chatId;
  final double size;

  static const color = Color(0xFF3DDC97);

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<Set<int>>(
      valueListenable: NetBus.instance.online,
      builder: (context, online, _) => AnimatedScale(
        scale: online.contains(chatId) ? 1 : 0,
        duration: Motion.medium,
        curve: Motion.emphasized,
        child: Container(
          width: size,
          height: size,
          decoration: BoxDecoration(
            color: color,
            shape: BoxShape.circle,
            border: Border.all(color: Palette.bg, width: 2),
          ),
        ),
      ),
    );
  }
}
