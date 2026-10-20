import 'dart:async';

import 'package:flutter/material.dart';

import '../chat/poll.dart' show formatWhen;
import '../deck/chat_card.dart';
import '../net/net_bus.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/recall.dart';
import '../src/rust/api/session.dart';
import '../theme/tokens.dart';

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);

String _when(int unix) {
  final t = _fromUnix(unix);
  final now = DateTime.now();
  if (t.year == now.year && t.month == now.month && t.day == now.day) {
    return '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';
  }
  return '${t.day}/${t.month}${t.year == now.year ? '' : '/${t.year % 100}'}';
}

/// Searching from the home screen: matching chats, then every message
/// that has all the words typed, newest first. All on this phone.
class SearchResults extends StatefulWidget {
  const SearchResults({
    super.key,
    required this.query,
    required this.chats,
    required this.hidden,
    required this.onOpenChat,
    required this.onOpenMessage,
  });

  final String query;

  /// Chats whose name matches.
  final List<ChatCard> chats;

  /// Hidden chats are revealed, so search them too.
  final bool hidden;
  final void Function(ChatCard) onOpenChat;
  final void Function(int chatId, int seq) onOpenMessage;

  @override
  State<SearchResults> createState() => _SearchResultsState();
}

class _SearchResultsState extends State<SearchResults> {
  List<SearchHit>? _hits;
  Timer? _debounce;

  @override
  void initState() {
    super.initState();
    _search();
  }

  @override
  void didUpdateWidget(SearchResults old) {
    super.didUpdateWidget(old);
    if (old.query != widget.query || old.hidden != widget.hidden) {
      _debounce?.cancel();
      _debounce = Timer(const Duration(milliseconds: 250), _search);
    }
  }

  @override
  void dispose() {
    _debounce?.cancel();
    super.dispose();
  }

  Future<void> _search() async {
    final query = widget.query;
    final hits = await searchMessages(query: query, hidden: widget.hidden);
    if (mounted && query == widget.query) setState(() => _hits = hits);
  }

  @override
  Widget build(BuildContext context) {
    final hits = _hits;
    return ListView(
      padding: const EdgeInsets.fromLTRB(Space.sm, 0, Space.sm, 96),
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      children: [
        if (widget.chats.isNotEmpty) ...[
          const _Label('CHATS'),
          for (final c in widget.chats)
            ListTile(
              leading: SizedBox.square(dimension: 40, child: ClipOval(child: SigilView(spec: c.sigil))),
              title: Text(c.title, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => widget.onOpenChat(c),
            ),
        ],
        const _Label('MESSAGES'),
        if (hits == null)
          const Padding(padding: EdgeInsets.all(Space.xl), child: Center(child: CircularProgressIndicator()))
        else if (hits.isEmpty)
          const Padding(
            padding: EdgeInsets.symmetric(horizontal: Space.lg, vertical: Space.md),
            child: Text('No messages with those words.', style: AppText.caption),
          )
        else
          for (final h in hits)
            ListTile(
              title: Text(
                h.chatTitle == h.senderName || h.fromMe ? h.chatTitle : '${h.senderName} · ${h.chatTitle}',
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: AppText.bodyStrong,
              ),
              subtitle: Text(
                h.fromMe ? 'You: ${h.text}' : h.text,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
              trailing: Text(_when(h.sentAt), style: AppText.micro),
              onTap: () => widget.onOpenMessage(h.chatId, h.seq),
            ),
      ],
    );
  }
}

class _Label extends StatelessWidget {
  const _Label(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(Space.lg, Space.md, Space.lg, Space.xs),
        child: Text(text, style: AppText.overline),
      );
}

/// "While you were away": who wrote, who mentioned you, plans coming up
/// and polls waiting for your answer. Shown after time away when there's
/// something to catch up on; worked out on this phone.
class CatchUpCard extends StatefulWidget {
  const CatchUpCard({super.key, required this.hidden, required this.onOpen});

  final bool hidden;
  final void Function(int chatId, int? seq) onOpen;

  @override
  State<CatchUpCard> createState() => _CatchUpCardState();
}

class _CatchUpCardState extends State<CatchUpCard> with WidgetsBindingObserver {
  CatchUpView? _up;
  var _dismissed = false;
  StreamSubscription<AppEvent>? _events;
  Timer? _debounce;

  /// Less than this many new messages isn't worth a summary.
  static const _worth = 5;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _load();
    _events = NetBus.instance.events.where((e) => e.kind == AppEventKind.chat).listen((_) {
      _debounce?.cancel();
      _debounce = Timer(const Duration(seconds: 2), _load);
    });
  }

  @override
  void didUpdateWidget(CatchUpCard old) {
    super.didUpdateWidget(old);
    if (old.hidden != widget.hidden) _load();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _events?.cancel();
    _debounce?.cancel();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Leaving: the next catch-up starts from now.
    if (state == AppLifecycleState.paused) {
      catchUpMark();
    } else if (state == AppLifecycleState.resumed) {
      _dismissed = false;
      _load();
    }
  }

  Future<void> _load() async {
    try {
      final up = await catchUp(hidden: widget.hidden);
      if (mounted) setState(() => _up = up);
    } catch (_) {}
  }

  void _dismiss() {
    catchUpMark();
    setState(() => _dismissed = true);
  }

  @override
  Widget build(BuildContext context) {
    final up = _up;
    final total = up?.chats.fold<int>(0, (n, c) => n + c.count) ?? 0;
    final mentions = up?.chats.where((c) => c.mentions > 0).toList() ?? const [];
    final show = up != null && !_dismissed && (total >= _worth || mentions.isNotEmpty);
    return AnimatedSize(
      duration: Motion.medium,
      curve: Motion.decelerate,
      child: !show
          ? const SizedBox(width: double.infinity)
          : Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
              child: Container(
                padding: const EdgeInsets.fromLTRB(Space.lg, Space.md, Space.xs, Space.md),
                decoration: BoxDecoration(
                  color: Palette.surfaceHigh,
                  borderRadius: BorderRadius.circular(Radii.lg),
                  border: Border.all(color: Palette.outline),
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        const Icon(Icons.auto_awesome_rounded, size: 18, color: Palette.textMuted),
                        const SizedBox(width: Space.sm),
                        const Expanded(child: Text('While you were away', style: AppText.heading)),
                        IconButton(
                          tooltip: 'Done',
                          visualDensity: VisualDensity.compact,
                          icon: const Icon(Icons.close_rounded, size: 18),
                          onPressed: _dismiss,
                        ),
                      ],
                    ),
                    Text(
                      '$total new message${total == 1 ? '' : 's'} in ${up.chats.length} chat${up.chats.length == 1 ? '' : 's'}',
                      style: AppText.caption,
                    ),
                    const SizedBox(height: Space.xs),
                    for (final c in mentions.take(3))
                      _Line(
                        icon: Icons.alternate_email_rounded,
                        text: '${c.senders.length == 1 ? c.senders.first : 'Someone'} mentioned you in ${c.title}',
                        onTap: () => widget.onOpen(c.chatId, null),
                      ),
                    for (final c in up.chats.where((c) => c.mentions == 0).take(3))
                      _Line(
                        icon: Icons.chat_bubble_outline_rounded,
                        text: '${c.title}: ${c.count} new · ${c.last}',
                        onTap: () => widget.onOpen(c.chatId, null),
                      ),
                    for (final p in up.plans.where((p) => p.mine == null).take(2))
                      _Line(
                        icon: Icons.event_rounded,
                        text: '${p.title} · ${formatWhen(_fromUnix(p.when))} · are you going?',
                        onTap: () => widget.onOpen(p.chatId, p.seq),
                      ),
                    for (final p in up.polls.take(2))
                      _Line(
                        icon: Icons.poll_outlined,
                        text: '${p.chatTitle} asks: ${p.question}',
                        onTap: () => widget.onOpen(p.chatId, p.seq),
                      ),
                  ],
                ),
              ),
            ),
    );
  }
}

class _Line extends StatelessWidget {
  const _Line({required this.icon, required this.text, required this.onTap});

  final IconData icon;
  final String text;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(Radii.sm),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Row(
            children: [
              Icon(icon, size: 16, color: Palette.textMuted),
              const SizedBox(width: Space.sm),
              Expanded(child: Text(text, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.body)),
            ],
          ),
        ),
      );
}
