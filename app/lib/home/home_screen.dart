import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../chat/chat_screen.dart';
import '../chat/message_source.dart';
import '../deck/chat_card.dart';
import '../deck/chat_stack.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';
import '../src/rust/api/temp_chat.dart';

const _tempNames = ['Ember', 'Spark', 'Flare', 'Cinder', 'Ash', 'Blaze', 'Flicker', 'Glow'];

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
  double _page = 0;
  int _tempCount = 0;
  Timer? _purge;

  @override
  void initState() {
    super.initState();
    _pages.addListener(() => setState(() => _page = _pages.page ?? 0));
    _search.addListener(() => setState(() {}));
    _purge = Timer.periodic(const Duration(minutes: 1), (_) => _syncTemp());
    _loadChats();
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
      final card = ChatCard(
        id: c.id,
        title: c.title,
        sigil: await sigilForKey(publicKey: c.sigilKey),
        source: StoreSource(c.id),
        updatedAt: DateTime.fromMillisecondsSinceEpoch(c.createdAt * 1000),
      )..pinned = c.pinned;
      await card.refresh();
      cards.add(card);
    }
    if (mounted) setState(() => _chats..clear()..addAll(cards));
  }

  @override
  void dispose() {
    _purge?.cancel();
    _pages.dispose();
    _search.dispose();
    super.dispose();
  }

  bool get _onTemp => _page > 0.5;

  /// Drops cards whose temp chat Rust has burned and refreshes countdowns.
  Future<void> _syncTemp() async {
    final live = {for (final id in await tempChatIds()) id.toInt()};
    for (final c in _temp) {
      if (live.contains(c.id)) await c.refresh();
    }
    if (!mounted) return;
    setState(() => _temp.removeWhere((c) => !live.contains(c.id)));
  }

  Future<void> _newTemp() async {
    HapticFeedback.mediumImpact();
    final id = DateTime.now().microsecondsSinceEpoch;
    await tempOpen(id: id);
    final card = ChatCard(
      id: id,
      title: '${_tempNames[_tempCount % _tempNames.length]} ${_tempCount ~/ _tempNames.length + 1}',
      sigil: await sigilRandom(),
      source: TempSource(id),
      temp: true,
    );
    _tempCount++;
    setState(() => _temp.add(card));
  }

  Future<void> _wipe(ChatCard card) async {
    await tempWipe(id: card.id);
    if (mounted) setState(() => _temp.remove(card));
  }

  Future<void> _wipeAll() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Wipe all temp chats?'),
        content: const Text('They are erased from memory immediately.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Wipe all', style: TextStyle(color: Colors.redAccent)),
          ),
        ],
      ),
    );
    if (ok != true) return;
    HapticFeedback.heavyImpact();
    await tempWipeAll();
    setState(_temp.clear);
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
    if (card.temp && !(await tempChatIds()).map((id) => id.toInt()).contains(card.id)) {
      // Wiped from inside the chat.
      if (mounted) setState(() => _temp.remove(card));
      return;
    }
    await card.refresh();
    if (mounted) setState(() {});
  }

  void _goTo(int page) {
    HapticFeedback.selectionClick();
    _pages.animateToPage(page, duration: const Duration(milliseconds: 380), curve: Curves.easeOutCubic);
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
            const Text('YOUR SIGIL', style: TextStyle(letterSpacing: 4, color: Colors.white70)),
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
                'Made from your key. Friends will add you by scanning your QR in person, '
                'and you will both see the same sigil for your chat.',
                textAlign: TextAlign.center,
                style: TextStyle(color: Colors.white54),
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
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
              child: Row(
                children: [
                  _Tabs(page: _page, onTap: _goTo, tempCount: _temp.length),
                  const Spacer(),
                  AnimatedSwitcher(
                    duration: const Duration(milliseconds: 220),
                    child: _onTemp && _temp.isNotEmpty
                        ? IconButton(
                            key: const ValueKey('wipe-all'),
                            tooltip: 'Wipe all',
                            onPressed: _wipeAll,
                            icon: const Icon(Icons.local_fire_department_rounded, color: Colors.redAccent),
                          )
                        : const SizedBox(key: ValueKey('none'), width: 48),
                  ),
                  const SizedBox(width: 4),
                  GestureDetector(
                    onTap: _showMe,
                    child: Container(
                      width: 40,
                      height: 40,
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
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
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
                      detail: 'Temp chats live only in memory. Every message burns 24 hours '
                          'after it is sent, and you can wipe them any time.',
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
        duration: const Duration(milliseconds: 280),
        curve: _onTemp ? Curves.easeOutBack : Curves.easeIn,
        child: FloatingActionButton.extended(
          onPressed: _onTemp ? _newTemp : null,
          icon: const Icon(Icons.add_rounded),
          label: const Text('New temp chat'),
        ),
      ),
    );
  }
}

/// Two labels with a pill that slides between them, tracking the page swipe.
class _Tabs extends StatelessWidget {
  const _Tabs({required this.page, required this.onTap, required this.tempCount});

  final double page;
  final ValueChanged<int> onTap;
  final int tempCount;

  static const _width = 96.0;

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    return Container(
      height: 40,
      width: _width * 2 + 8,
      padding: const EdgeInsets.all(4),
      decoration: BoxDecoration(
        color: Colors.white.withValues(alpha: 0.06),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Stack(
        children: [
          Positioned(
            left: page.clamp(0.0, 1.0) * _width,
            width: _width,
            top: 0,
            bottom: 0,
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: accent.withValues(alpha: 0.22),
                borderRadius: BorderRadius.circular(16),
                border: Border.all(color: accent.withValues(alpha: 0.5)),
              ),
            ),
          ),
          Row(
            children: [
              for (final (i, label) in [(0, 'Chats'), (1, tempCount > 0 ? 'Temp · $tempCount' : 'Temp')])
                GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: () => onTap(i),
                  child: SizedBox(
                    width: _width,
                    child: Center(
                      child: Text(
                        label,
                        style: TextStyle(
                          fontWeight: FontWeight.w600,
                          color: Color.lerp(Colors.white, Colors.white38, (page - i).abs().clamp(0.0, 1.0)),
                        ),
                      ),
                    ),
                  ),
                ),
            ],
          ),
        ],
      ),
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
          filled: true,
          fillColor: Colors.white.withValues(alpha: 0.05),
          contentPadding: EdgeInsets.zero,
          border: OutlineInputBorder(
            borderRadius: BorderRadius.circular(21),
            borderSide: BorderSide.none,
          ),
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
            Text(text, style: const TextStyle(fontSize: 17, color: Colors.white70)),
            if (detail != null) ...[
              const SizedBox(height: 10),
              Text(detail!, textAlign: TextAlign.center, style: const TextStyle(color: Colors.white38)),
            ],
          ],
        ),
      ),
    );
  }
}
