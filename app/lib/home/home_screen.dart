import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../chat/chat_screen.dart';
import '../chat/message_source.dart';
import '../deck/chat_card.dart';
import '../deck/chat_stack.dart';
import '../friends/add_friend_screen.dart';
import '../net/net_bus.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';
import '../theme/tokens.dart';


class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.publicKey, required this.mySigil});

  final Uint8List publicKey;
  final SigilSpec mySigil;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  final _chats = <ChatCard>[];
  final _temp = <ChatCard>[];
  final _pages = PageController();
  final _search = TextEditingController();
  final _sigils = <String, SigilSpec>{};
  double _page = 0;
  Timer? _purge;
  StreamSubscription<AppEvent>? _events;

  @override
  void initState() {
    super.initState();
    _pages.addListener(() => setState(() => _page = _pages.page ?? 0));
    _search.addListener(() => setState(() {}));
    // Temp messages burn on their own; refresh the countdowns.
    _purge = Timer.periodic(const Duration(minutes: 1), (_) => _loadTemp());
    _events = NetBus.instance.events.listen(_onEvent);
    _loadChats();
    _loadTemp();
  }

  @override
  void dispose() {
    _purge?.cancel();
    _events?.cancel();
    _pages.dispose();
    _search.dispose();
    super.dispose();
  }

  Future<SigilSpec> _sigil(List<int> key) async =>
      _sigils[key.join(',')] ??= await sigilForKey(publicKey: key);

  void _onEvent(AppEvent e) {
    switch (e.kind) {
      case AppEventKind.chat:
        final card = _chats.where((c) => c.id == e.id.toInt()).firstOrNull;
        if (card == null) {
          _loadChats();
        } else {
          card.refresh().then((_) {
            if (mounted) setState(() {});
          });
        }
      case AppEventKind.temp:
      case AppEventKind.tempGone:
        _loadTemp();
      case AppEventKind.status:
        if (mounted) setState(() {});
    }
  }

  /// Loads persistent chats from the encrypted store, creating "Notes to
  /// self" on first launch.
  Future<void> _loadChats() async {
    var stored = await chatList();
    if (!stored.any((c) => c.kind == ChatKindView.notes)) {
      await chatCreate(kind: ChatKindView.notes, title: 'Notes to self', sigilKey: widget.publicKey);
      stored = await chatList();
    }
    final cards = <ChatCard>[];
    for (final c in stored) {
      final notes = c.kind == ChatKindView.notes;
      final card = ChatCard(
        id: c.id,
        title: c.title,
        sigil: await _sigil(c.sigilKey),
        source: StoreSource(c.id),
        friend: !notes,
        verified: c.verified,
        note: notes ? 'Only on this device' : 'End-to-end encrypted · over Tor',
        updatedAt: DateTime.fromMillisecondsSinceEpoch(c.createdAt * 1000),
      )..pinned = c.pinned;
      await card.refresh();
      cards.add(card);
    }
    if (mounted) setState(() => _chats..clear()..addAll(cards));
  }

  /// Temp chats live in Rust memory; friends' new temp chats show up here
  /// as they arrive.
  Future<void> _loadTemp() async {
    final views = await tempList();
    final pinned = {for (final c in _temp) if (c.pinned) c.id};
    final cards = <ChatCard>[];
    for (final v in views) {
      final id = v.id.toInt();
      final card = ChatCard(
        id: id,
        title: v.title,
        sigil: await _sigil(v.sigilKey),
        source: TempSource(id),
        temp: true,
        friend: v.friendChatId != null,
        note: v.friendChatId != null ? 'Temp · burns after 24h · on both phones' : 'Temp · only on this phone',
        updatedAt: v.lastAt == null ? null : DateTime.fromMillisecondsSinceEpoch(v.lastAt!.toInt() * 1000),
      )
        ..pinned = pinned.contains(id)
        ..preview = v.lastBody
        ..nextBurn = v.nextBurn == null ? null : DateTime.fromMillisecondsSinceEpoch(v.nextBurn!.toInt() * 1000);
      cards.add(card);
    }
    if (mounted) setState(() => _temp..clear()..addAll(cards));
  }

  bool get _onTemp => _page > 0.5;

  Future<void> _newTemp() async {
    HapticFeedback.selectionClick();
    final friends = await friendList();
    if (!mounted) return;
    final picked = await showModalBottomSheet<(bool, int?)>(
      context: context,
      builder: (context) => _TempPicker(friends: friends, sigil: _sigil),
    );
    if (picked == null) return;
    HapticFeedback.mediumImpact();
    await tempNew(friendChatId: picked.$2);
    await _loadTemp();
  }

  Future<void> _wipe(ChatCard card) async {
    await tempWipe(id: card.id);
    await _loadTemp();
  }

  Future<void> _wipeAll() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Wipe all temp chats?'),
        content: const Text('They are erased here, and on your friends\' phones as soon as they can be reached.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Wipe all', style: TextStyle(color: Palette.danger)),
          ),
        ],
      ),
    );
    if (ok != true) return;
    HapticFeedback.heavyImpact();
    await tempWipeAll();
    await _loadTemp();
  }

  void _actions(ChatCard card) {
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: card.sigil.background.color,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: Icon(card.pinned ? Icons.push_pin_outlined : Icons.push_pin_rounded),
              title: Text(card.pinned ? 'Unpin' : 'Pin to top'),
              onTap: () {
                Navigator.pop(context);
                setState(() => card.pinned = !card.pinned);
                if (!card.temp) chatSetPinned(id: card.id, pinned: card.pinned);
              },
            ),
            if (card.temp)
              ListTile(
                leading: const Icon(Icons.local_fire_department_rounded, color: Colors.redAccent),
                title: const Text('Wipe now', style: TextStyle(color: Colors.redAccent)),
                onTap: () {
                  Navigator.pop(context);
                  HapticFeedback.heavyImpact();
                  _wipe(card);
                },
              ),
          ],
        ),
      ),
    );
  }

  Widget _openChat(ChatCard card, VoidCallback close) => ChatScreen(
        card: card,
        close: close,
        onWipe: card.temp ? () => tempWipe(id: card.id) : null,
      );

  Future<void> _closed(ChatCard card) async {
    if (card.temp) return _loadTemp();
    await card.refresh();
    if (mounted) setState(() {});
  }

  void _goTo(int page) {
    HapticFeedback.selectionClick();
    _pages.animateToPage(page, duration: const Duration(milliseconds: 380), curve: Curves.easeOutCubic);
  }

  Future<void> _addFriend() async {
    HapticFeedback.selectionClick();
    final added = await Navigator.push<bool>(
      context,
      MaterialPageRoute(builder: (_) => AddFriendScreen(mySigil: widget.mySigil)),
    );
    if (added == true) {
      await _loadChats();
      _goTo(0);
    }
  }

  void _showMe() {
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: widget.mySigil.background.color,
      showDragHandle: true,
      builder: (_) => SizedBox(
        height: 440,
        child: Column(
          children: [
            const Text('YOUR SIGIL', style: AppText.overline),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: AspectRatio(
                  aspectRatio: 1,
                  child: ClipOval(child: LivingSigil(spec: widget.mySigil, period: 60)),
                ),
              ),
            ),
            const Padding(
              padding: EdgeInsets.fromLTRB(32, 0, 32, 32),
              child: Text(
                'Made from your key. When you and a friend add each other, '
                'you both see the same sigil for your chat.',
                textAlign: TextAlign.center,
                style: AppText.caption,
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        bottom: false,
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.xl, Space.md, Space.md, Space.sm),
              child: Row(
                children: [
                  const Text('Sanctum', style: AppText.display),
                  const SizedBox(width: Space.md),
                  const _NetPill(),
                  const Spacer(),
                  AnimatedSwitcher(
                    duration: Motion.fast,
                    child: _onTemp && _temp.isNotEmpty
                        ? IconButton(
                            key: const ValueKey('wipe-all'),
                            tooltip: 'Wipe all',
                            onPressed: _wipeAll,
                            icon: const Icon(Icons.local_fire_department_rounded, color: Palette.danger),
                          )
                        : const SizedBox(key: ValueKey('none')),
                  ),
                  IconButton(
                    tooltip: 'Add a friend',
                    onPressed: _addFriend,
                    icon: const Icon(Icons.person_add_alt_1_rounded),
                  ),
                  const SizedBox(width: Space.xs),
                  GestureDetector(
                    onTap: _showMe,
                    child: Container(
                      width: 38,
                      height: 38,
                      padding: const EdgeInsets.all(1.5),
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        border: Border.all(color: widget.mySigil.accent.color.withValues(alpha: 0.6)),
                      ),
                      child: ClipOval(child: SigilView(spec: widget.mySigil)),
                    ),
                  ),
                ],
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, Space.xs, Space.lg, Space.sm),
              child: _Tabs(page: _page, onTap: _goTo, tempCount: _temp.length),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
              child: _SearchField(controller: _search),
            ),
            Expanded(
              child: PageView(
                controller: _pages,
                onPageChanged: (_) => HapticFeedback.selectionClick(),
                children: [
                  ChatStack(
                    cards: _chats,
                    query: _search.text,
                    openBuilder: _openChat,
                    onClosed: _closed,
                    onLongPress: _actions,
                    empty: const _Empty(text: 'No chats yet'),
                  ),
                  ChatStack(
                    cards: _temp,
                    query: _search.text,
                    openBuilder: _openChat,
                    onClosed: _closed,
                    onLongPress: _actions,
                    onDismiss: _wipe,
                    empty: const _Empty(
                      text: 'No temp chats',
                      detail: 'Start one with a friend. Temp chats live only in memory on both '
                          'phones, every message burns 24 hours after it is sent, and either of '
                          'you can wipe it for both at any time.',
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
      floatingActionButton: AnimatedScale(
        scale: _onTemp ? 1 : 0,
        duration: Motion.medium,
        curve: _onTemp ? Motion.decelerate : Motion.accelerate,
        child: FloatingActionButton.extended(
          onPressed: _onTemp ? _newTemp : null,
          icon: const Icon(Icons.add_rounded),
          label: const Text('New temp chat'),
        ),
      ),
    );
  }
}

/// Two segments with a pill that slides between them, tracking the swipe.
class _Tabs extends StatelessWidget {
  const _Tabs({required this.page, required this.onTap, required this.tempCount});

  final double page;
  final ValueChanged<int> onTap;
  final int tempCount;

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    return Container(
      height: 44,
      padding: const EdgeInsets.all(4),
      decoration: BoxDecoration(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.md),
        border: Border.all(color: Palette.outline),
      ),
      child: LayoutBuilder(builder: (context, box) {
        final half = box.maxWidth / 2;
        return Stack(
          children: [
            Positioned(
              left: page.clamp(0.0, 1.0) * half,
              width: half,
              top: 0,
              bottom: 0,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Palette.surfaceHigh,
                  borderRadius: BorderRadius.circular(Radii.sm),
                  border: Border.all(color: accent.withValues(alpha: 0.45)),
                ),
              ),
            ),
            Row(
              children: [
                for (final (i, label) in [(0, 'Chats'), (1, tempCount > 0 ? 'Temp · $tempCount' : 'Temp')])
                  Expanded(
                    child: GestureDetector(
                      behavior: HitTestBehavior.opaque,
                      onTap: () => onTap(i),
                      child: Center(
                        child: Text(
                          label,
                          style: AppText.bodyStrong.copyWith(
                            fontSize: 14,
                            color: Color.lerp(Palette.text, Palette.textFaint, (page - i).abs().clamp(0.0, 1.0)),
                          ),
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          ],
        );
      }),
    );
  }
}

class _SearchField extends StatelessWidget {
  const _SearchField({required this.controller});

  final TextEditingController controller;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 42,
      child: TextField(
        controller: controller,
        textInputAction: TextInputAction.search,
        decoration: InputDecoration(
          hintText: 'Search',
          prefixIcon: const Icon(Icons.search_rounded, size: 20),
          suffixIcon: controller.text.isEmpty
              ? null
              : IconButton(
                  tooltip: 'Clear',
                  icon: const Icon(Icons.close_rounded, size: 18),
                  onPressed: controller.clear,
                ),
          fillColor: Palette.surface,
          contentPadding: EdgeInsets.zero,
        ),
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty({required this.text, this.detail});

  final String text;
  final String? detail;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 40),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(text, style: AppText.heading),
            if (detail != null) ...[
              const SizedBox(height: 10),
              Text(detail!, textAlign: TextAlign.center, style: AppText.caption),
            ],
          ],
        ),
      ),
    );
  }
}

/// Tor connection status, beside the wordmark.
class _NetPill extends StatelessWidget {
  const _NetPill();

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<NetStatus>(
      valueListenable: NetBus.instance.status,
      builder: (context, status, _) {
        final (color, label) = switch (status) {
          NetStatus.online => (const Color(0xFF3DDC97), 'Tor'),
          NetStatus.connecting => (const Color(0xFFFFC145), 'Connecting'),
          NetStatus.failed => (Palette.danger, 'Offline'),
          NetStatus.off => (Palette.textFaint, 'Offline'),
        };
        return AnimatedContainer(
          duration: Motion.medium,
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
          decoration: BoxDecoration(
            color: color.withValues(alpha: 0.12),
            borderRadius: BorderRadius.circular(Radii.sm),
            border: Border.all(color: color.withValues(alpha: 0.35)),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(width: 7, height: 7, decoration: BoxDecoration(color: color, shape: BoxShape.circle)),
              const SizedBox(width: 6),
              Text(label, style: AppText.micro.copyWith(color: color, fontWeight: FontWeight.w600)),
            ],
          ),
        );
      },
    );
  }
}

/// Whom to start a temp chat with.
class _TempPicker extends StatelessWidget {
  const _TempPicker({required this.friends, required this.sigil});

  final List<FriendView> friends;
  final Future<SigilSpec> Function(List<int>) sigil;

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.7),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
          children: [
            const Padding(
              padding: EdgeInsets.fromLTRB(Space.sm, 0, Space.sm, Space.md),
              child: Text('New temp chat', style: AppText.title),
            ),
            for (final f in friends)
              ListTile(
                shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.md)),
                leading: SizedBox.square(
                  dimension: 40,
                  child: FutureBuilder<SigilSpec>(
                    future: sigil(f.sigilKey),
                    builder: (_, snap) => snap.data == null
                        ? const SizedBox.shrink()
                        : ClipOval(child: SigilView(spec: snap.data!)),
                  ),
                ),
                title: Text(f.name, style: AppText.heading),
                subtitle: const Text('Both phones · burns after 24h', style: AppText.caption),
                onTap: () => Navigator.pop(context, (true, f.chatId.toInt())),
              ),
            if (friends.isEmpty)
              const Padding(
                padding: EdgeInsets.all(Space.sm),
                child: Text('Add a friend to start temp chats with them.', style: AppText.caption),
              ),
            const Divider(height: Space.xl),
            ListTile(
              shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.md)),
              leading: const SizedBox.square(dimension: 40, child: Icon(Icons.person_outline_rounded)),
              title: const Text('Just me', style: AppText.heading),
              subtitle: const Text('Only on this phone', style: AppText.caption),
              onTap: () => Navigator.pop(context, (true, null)),
            ),
          ],
        ),
      ),
    );
  }
}
