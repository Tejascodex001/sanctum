import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../sigil/sigil_painter.dart';
import '../theme/tokens.dart';
import 'chat_card.dart';
import 'chat_tile.dart';

/// Chats as a stack of overlapping cards. Scales to any number of chats:
/// the list is lazy, sigils are painted once and cached, and pinned chats
/// sort first, then the most recent.
class ChatStack extends StatefulWidget {
  const ChatStack({
    super.key,
    required this.cards,
    required this.openBuilder,
    required this.onClosed,
    this.onLongPress,
    this.onDismiss,
    this.query = '',
    required this.empty,
    this.onRefresh,
  });

  final List<ChatCard> cards;
  final Widget Function(ChatCard card, VoidCallback close) openBuilder;
  final ValueChanged<ChatCard> onClosed;
  final ValueChanged<ChatCard>? onLongPress;

  /// When set, cards can be swiped left to wipe them.
  final ValueChanged<ChatCard>? onDismiss;
  final String query;
  final Widget empty;

  /// Pull down to refresh, as in Slack or Instagram.
  final Future<void> Function()? onRefresh;

  @override
  State<ChatStack> createState() => _ChatStackState();
}

class _ChatStackState extends State<ChatStack> {
  /// Cards that already played their entrance, so scrolling never replays it.
  final _seen = <int>{};

  List<ChatCard> get _visible {
    final q = widget.query.trim().toLowerCase();
    final list = [
      for (final c in widget.cards)
        if (q.isEmpty ||
            c.title.toLowerCase().contains(q) ||
            (c.preview?.toLowerCase().contains(q) ?? false))
          c,
    ];
    list.sort((a, b) {
      if (a.pinned != b.pinned) return a.pinned ? -1 : 1;
      return b.updatedAt.compareTo(a.updatedAt);
    });
    return list;
  }

  @override
  Widget build(BuildContext context) {
    final list = _list();
    if (widget.onRefresh == null) return list;
    return RefreshIndicator(
      onRefresh: widget.onRefresh!,
      color: Theme.of(context).colorScheme.primary,
      backgroundColor: Palette.surfaceHigh,
      child: list,
    );
  }

  static const _physics = AlwaysScrollableScrollPhysics(parent: BouncingScrollPhysics());

  Widget _list() {
    final cards = _visible;
    if (cards.isEmpty) {
      final empty = widget.query.isEmpty
          ? widget.empty
          : Center(
              child: Text('No matches', style: TextStyle(color: Palette.textFaint)),
            );
      // Still scrollable, so an empty list can be pulled to refresh too.
      return LayoutBuilder(
        builder: (context, box) => ListView(
          physics: _physics,
          children: [SizedBox(height: box.maxHeight, child: empty)],
        ),
      );
    }
    return ListView.builder(
      physics: _physics,
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 120),
      itemCount: cards.length,
      itemBuilder: (context, i) {
        final card = cards[i];
        final animate = _seen.add(card.id);
        Widget item = _StackedCard(
          card: card,
          openBuilder: widget.openBuilder,
          onClosed: () => widget.onClosed(card),
          onLongPress: widget.onLongPress == null ? null : () => widget.onLongPress!(card),
        );
        if (widget.onDismiss != null) {
          item = Dismissible(
            key: ValueKey('dismiss-${card.id}'),
            direction: DismissDirection.endToStart,
            onDismissed: (_) {
              HapticFeedback.heavyImpact();
              widget.onDismiss!(card);
            },
            background: const _WipeBackground(),
            child: item,
          );
        }
        item = Padding(padding: const EdgeInsets.only(bottom: Space.sm), child: item);
        return animate
            ? _Entrance(key: ValueKey('in-${card.id}'), delay: i.clamp(0, 10) * 45, child: item)
            : item;
      },
    );
  }
}

class _StackedCard extends StatelessWidget {
  const _StackedCard({
    required this.card,
    required this.openBuilder,
    required this.onClosed,
    this.onLongPress,
  });

  final ChatCard card;
  final Widget Function(ChatCard card, VoidCallback close) openBuilder;
  final VoidCallback onClosed;
  final VoidCallback? onLongPress;

  Future<void> _open(BuildContext context) async {
    HapticFeedback.selectionClick();
    // Load first, so the chat appears complete on its first frame.
    card.preloaded = await card.source.load();
    if (!context.mounted) return;
    await Navigator.of(context).push(ChatRoute(builder: (ctx) => openBuilder(card, () => Navigator.pop(ctx))));
    card.preloaded = null;
    onClosed();
  }

  @override
  Widget build(BuildContext context) {
    return Material(
      color: Palette.card(card.sigil.background.color, card.sigil.accent.color),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(Radii.lg),
        side: BorderSide(color: Palette.outline),
      ),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: () => _open(context),
        onLongPress: onLongPress == null
            ? null
            : () {
                HapticFeedback.mediumImpact();
                onLongPress!();
              },
        child: ChatTile(card: card),
      ),
    );
  }
}

/// Opening a chat: the page fades and settles up into place while the
/// chat's emblem flies from the card into the header. Only opacity and
/// transforms animate, which the GPU handles without re-layout.
class ChatRoute<T> extends PageRouteBuilder<T> {
  ChatRoute({required WidgetBuilder builder})
      : super(
          transitionDuration: Motion.slow,
          reverseTransitionDuration: Motion.medium,
          pageBuilder: (context, _, _) => builder(context),
          transitionsBuilder: (context, animation, _, child) {
            final t = CurvedAnimation(parent: animation, curve: Motion.decelerate, reverseCurve: Motion.accelerate);
            return FadeTransition(
              opacity: CurvedAnimation(parent: animation, curve: const Interval(0, 0.6)),
              child: SlideTransition(
                position: Tween(begin: const Offset(0, 0.04), end: Offset.zero).animate(t),
                child: ScaleTransition(scale: Tween(begin: 0.96, end: 1.0).animate(t), child: child),
              ),
            );
          },
        );
}

class _WipeBackground extends StatelessWidget {
  const _WipeBackground();

  @override
  Widget build(BuildContext context) {
    return Container(
      alignment: Alignment.centerRight,
      padding: const EdgeInsets.only(right: 28),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(22),
        gradient: LinearGradient(
          colors: [Colors.transparent, Colors.redAccent.withValues(alpha: 0.6)],
        ),
      ),
      child: const Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.local_fire_department_rounded),
          SizedBox(width: 6),
          Text('Wipe', style: TextStyle(fontWeight: FontWeight.w600)),
        ],
      ),
    );
  }
}

/// Slides and fades a card in once, after [delay] milliseconds.
class _Entrance extends StatefulWidget {
  const _Entrance({super.key, required this.delay, required this.child});

  final int delay;
  final Widget child;

  @override
  State<_Entrance> createState() => _EntranceState();
}

class _EntranceState extends State<_Entrance> with SingleTickerProviderStateMixin {
  late final _c = AnimationController(vsync: this, duration: const Duration(milliseconds: 520));
  late final _curve = CurvedAnimation(parent: _c, curve: Curves.easeOutCubic);

  @override
  void initState() {
    super.initState();
    Future.delayed(Duration(milliseconds: widget.delay), () {
      if (mounted) _c.forward();
    });
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _curve,
      child: widget.child,
      builder: (_, child) => Opacity(
        opacity: _curve.value,
        child: Transform.translate(
          offset: Offset(0, 40 * (1 - _curve.value)),
          child: Transform.scale(scale: 0.96 + 0.04 * _curve.value, child: child),
        ),
      ),
    );
  }
}
