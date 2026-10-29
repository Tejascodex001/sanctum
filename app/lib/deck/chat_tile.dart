import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../moments/moments.dart';
import '../net/net_bus.dart';
import '../notify/notifier.dart';
import '../sigil/emblem.dart';
import '../sigil/sigil_painter.dart';
import '../theme/tokens.dart';
import 'chat_card.dart';
import '../sigil/sigil_look.dart';

/// The face of a chat card: emblem, name, last message, and the chat's sigil
/// as a large cropped watermark so every card looks like its own object.
class ChatTile extends StatelessWidget {
  const ChatTile({super.key, required this.card});

  final ChatCard card;

  static const height = 70.0;

  @override
  Widget build(BuildContext context) {
    final bg = Palette.card(card.sigil.look.background.color, card.sigil.look.accent.color);
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
            child: ValueListenableBuilder<Map<String, int>>(
              valueListenable: Notifier.instance.unread,
              builder: (context, unread, _) {
                final n = unread[card.temp ? Notifier.tempKey(card.id) : Notifier.chatKey(card.id)] ?? 0;
                return Row(
                  children: [
                    Stack(
                      clipBehavior: Clip.none,
                      children: [
                        UnreadGlow(
                          count: n,
                          color: card.sigil.look.accent.color,
                          square: card.group,
                          child: Hero(tag: 'emblem-${card.id}', child: _Emblem(card: card)),
                        ),
                        if (card.friendChatId != null)
                          Positioned(right: 0, bottom: 0, child: OnlineDot(chatId: card.friendChatId!)),
                      ],
                    ),
                    const SizedBox(width: Space.md),
                    Expanded(child: _Text(card: card, unread: n > 0)),
                    const SizedBox(width: Space.sm),
                    _Trailing(card: card),
                  ],
                );
              },
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
    // A gold ring while this friend has a moment we haven't seen.
    final friend = card.friend && !card.group && !card.temp;
    if (!friend) return ChatEmblem(sigil: card.sigil, group: card.group);
    return ValueListenableBuilder<Set<int>>(
      valueListenable: unseenMoments,
      builder: (context, unseen, _) =>
          ChatEmblem(sigil: card.sigil, ring: unseen.contains(card.id) ? Palette.gold : null),
    );
  }
}

class _Text extends StatelessWidget {
  const _Text({required this.card, required this.unread});

  final ChatCard card;

  /// New messages wait: the last one reads brighter and bolder.
  final bool unread;

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisAlignment: MainAxisAlignment.center,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            if (card.hidden) ...[
              Icon(Icons.visibility_off_rounded, size: 13, color: Palette.textFaint),
              const SizedBox(width: Space.xs),
            ],
            if (card.pinned) ...[
              Icon(Icons.push_pin_rounded, size: 13, color: card.sigil.look.accent.color),
              const SizedBox(width: Space.xs),
            ],
            Flexible(
              child: Text(card.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.heading),
            ),
            if (card.muted) ...[
              const SizedBox(width: Space.xs),
              Icon(Icons.notifications_off_rounded, size: 13, color: Palette.textFaint, semanticLabel: 'Muted'),
            ],
          ],
        ),
        const SizedBox(height: 2),
        if (card.draft.trim().isNotEmpty && !unread)
          Text.rich(
            TextSpan(children: [
              TextSpan(
                text: 'Draft: ',
                style: TextStyle(color: card.sigil.look.accent.color, fontWeight: FontWeight.w600),
              ),
              TextSpan(text: card.draft.trim().replaceAll('\n', ' ')),
            ]),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: AppText.caption.copyWith(color: Palette.textMuted),
          )
        else
          Text(
            card.preview ?? (card.temp ? 'Burns 24h after sending' : 'No messages yet'),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: AppText.caption.copyWith(
              color: card.preview == null
                  ? Palette.textFaint
                  : unread
                      ? Palette.text
                      : Palette.textMuted,
              fontWeight: unread ? FontWeight.w700 : null,
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
    final accent = card.sigil.look.accent.color;
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

/// New messages: a soft glow in the chat's colour stays behind the emblem
/// while any wait, and each new one sends a ripple out and makes the emblem
/// bounce once. Nothing animates while the count stays the same.
class UnreadGlow extends StatefulWidget {
  const UnreadGlow({super.key, required this.count, required this.color, this.square = false, required this.child});

  final int count;
  final Color color;

  /// Groups have rounded-square emblems.
  final bool square;
  final Widget child;

  @override
  State<UnreadGlow> createState() => _UnreadGlowState();
}

class _UnreadGlowState extends State<UnreadGlow> with SingleTickerProviderStateMixin {
  late final _pulse = AnimationController(vsync: this, duration: const Duration(milliseconds: 900));

  @override
  void didUpdateWidget(UnreadGlow old) {
    super.didUpdateWidget(old);
    if (widget.count > old.count) _pulse.forward(from: 0);
  }

  @override
  void dispose() {
    _pulse.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final on = widget.count > 0;
    final shape = widget.square ? BoxShape.rectangle : BoxShape.circle;
    final radius = widget.square ? BorderRadius.circular(14) : null;
    return AnimatedContainer(
      duration: Motion.slow,
      curve: Motion.emphasized,
      decoration: BoxDecoration(
        shape: shape,
        borderRadius: radius,
        boxShadow: [
          BoxShadow(
            color: widget.color.withValues(alpha: on ? 0.55 : 0),
            blurRadius: on ? 18 : 0,
            spreadRadius: on ? 1 : 0,
          ),
        ],
      ),
      child: AnimatedBuilder(
        animation: _pulse,
        child: widget.child,
        builder: (context, child) {
          // The same tree every frame, so the emblem is never rebuilt.
          final t = _pulse.isAnimating ? _pulse.value : 1.0;
          final bounce = 1 + 0.14 * math.sin(math.pi * Curves.easeOut.transform((t / 0.45).clamp(0.0, 1.0)));
          final ripple = Curves.easeOutCubic.transform(t);
          return Stack(
            clipBehavior: Clip.none,
            alignment: Alignment.center,
            children: [
              Positioned.fill(
                child: Transform.scale(
                  scale: 1 + 0.7 * ripple,
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      shape: shape,
                      borderRadius: radius,
                      border: Border.all(color: widget.color.withValues(alpha: 0.8 * (1 - ripple)), width: 2),
                    ),
                  ),
                ),
              ),
              Transform.scale(scale: bounce, child: child),
            ],
          );
        },
      ),
    );
  }
}
