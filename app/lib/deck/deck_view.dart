import 'dart:math' as math;
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../sigil/sigil_painter.dart';
import 'chat_card.dart';

/// Chats as a stack of cards: swipe sideways to move through the deck,
/// swipe a card up to open it, long-press for actions.
class DeckView extends StatefulWidget {
  const DeckView({
    super.key,
    required this.cards,
    required this.onOpen,
    this.onLongPress,
    this.subtitle,
    this.emptyLabel = 'Nothing here yet',
  });

  final List<ChatCard> cards;
  final ValueChanged<ChatCard> onOpen;
  final ValueChanged<ChatCard>? onLongPress;
  final String Function(ChatCard)? subtitle;
  final String emptyLabel;

  @override
  State<DeckView> createState() => _DeckViewState();
}

class _DeckViewState extends State<DeckView> {
  final _pages = PageController(viewportFraction: 0.78);
  double _page = 0;

  @override
  void initState() {
    super.initState();
    _pages.addListener(() => setState(() => _page = _pages.page ?? 0));
  }

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (widget.cards.isEmpty) {
      return Center(
        child: Text(widget.emptyLabel,
            style: const TextStyle(color: Colors.white38, letterSpacing: 2)),
      );
    }
    return PageView.builder(
      controller: _pages,
      itemCount: widget.cards.length,
      onPageChanged: (_) => HapticFeedback.selectionClick(),
      itemBuilder: (context, i) {
        final offset = i - _page;
        final card = widget.cards[i];
        // Neighbouring cards tilt back and shrink, like a fanned deck.
        final transform = Matrix4.identity()
          ..setEntry(3, 2, 0.0012)
          ..rotateY(offset * -0.35)
          ..scaleByDouble(
              1 - offset.abs() * 0.12, 1 - offset.abs() * 0.12, 1, 1);
        return Transform(
          alignment: Alignment.center,
          transform: transform,
          child: Opacity(
            opacity: (1 - offset.abs() * 0.5).clamp(0.0, 1.0),
            child: _Card(
              card: card,
              subtitle: widget.subtitle?.call(card),
              onOpen: () => widget.onOpen(card),
              onLongPress: widget.onLongPress == null
                  ? null
                  : () => widget.onLongPress!(card),
            ),
          ),
        );
      },
    );
  }
}

class _Card extends StatefulWidget {
  const _Card({
    required this.card,
    required this.onOpen,
    this.subtitle,
    this.onLongPress,
  });

  final ChatCard card;
  final String? subtitle;
  final VoidCallback onOpen;
  final VoidCallback? onLongPress;

  @override
  State<_Card> createState() => _CardState();
}

class _CardState extends State<_Card> {
  double _lift = 0;

  void _end(DragEndDetails d) {
    final open = _lift < -90 || (d.primaryVelocity ?? 0) < -500;
    setState(() => _lift = 0);
    if (open) {
      HapticFeedback.mediumImpact();
      widget.onOpen();
    }
  }

  @override
  Widget build(BuildContext context) {
    final accent = widget.card.sigil.accent.color;
    return GestureDetector(
      onTap: widget.onOpen,
      onLongPress: widget.onLongPress == null
          ? null
          : () {
              HapticFeedback.heavyImpact();
              widget.onLongPress!();
            },
      onVerticalDragUpdate: (d) =>
          setState(() => _lift = math.min(0, _lift + d.delta.dy)),
      onVerticalDragEnd: _end,
      child: AnimatedContainer(
        duration:
            _lift == 0 ? const Duration(milliseconds: 250) : Duration.zero,
        curve: Curves.easeOutBack,
        transform: Matrix4.translationValues(0, _lift, 0),
        margin: const EdgeInsets.symmetric(vertical: 48, horizontal: 6),
        child: Hero(
          tag: 'chat-${widget.card.id}',
          child: Container(
            clipBehavior: Clip.antiAlias,
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(32),
              border: Border.all(color: accent.withValues(alpha: 0.5)),
              boxShadow: [
                BoxShadow(
                    color: accent.withValues(alpha: 0.25), blurRadius: 40),
              ],
            ),
            child: Stack(
              fit: StackFit.expand,
              children: [
                LivingSigil(spec: widget.card.sigil),
                Positioned(
                  left: 0,
                  right: 0,
                  bottom: 0,
                  child: ClipRect(
                    child: BackdropFilter(
                      filter: ImageFilter.blur(sigmaX: 18, sigmaY: 18),
                      child: Container(
                        color: Colors.black.withValues(alpha: 0.25),
                        padding: const EdgeInsets.fromLTRB(24, 20, 24, 28),
                        child: Material(
                          type: MaterialType.transparency,
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(widget.card.title,
                                  style: const TextStyle(
                                      fontSize: 26,
                                      fontWeight: FontWeight.w600,
                                      letterSpacing: -0.5)),
                              if (widget.subtitle != null) ...[
                                const SizedBox(height: 6),
                                Text(widget.subtitle!,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style:
                                        const TextStyle(color: Colors.white60)),
                              ],
                            ],
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
