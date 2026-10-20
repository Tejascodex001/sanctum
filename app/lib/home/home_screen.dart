import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:local_auth/local_auth.dart';

import '../chat/chat_screen.dart';
import '../chat/message_source.dart';
import '../deck/chat_card.dart';
import '../deck/chat_stack.dart';
import '../deck/chat_tile.dart';
import '../friends/add_friend_screen.dart';
import '../background/background.dart';
import '../identity/secrets.dart';
import '../net/net_bus.dart';
import '../notify/notifier.dart';
import '../security/app_lock.dart';
import '../security/panic.dart';
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

class _HomeScreenState extends State<HomeScreen> with WidgetsBindingObserver {
  final _chats = <ChatCard>[];
  final _temp = <ChatCard>[];
  var _requests = <RequestView>[];

  /// Friends who haven't accepted our request yet.
  final _waiting = <int>{};

  /// Hidden chats are listed only after a biometric check, until the app
  /// leaves the screen.
  bool _showHidden = false;
  List<ChatCard> get _visibleChats => [for (final c in _chats) if (_showHidden || !c.hidden) c];
  final _pages = PageController();
  final _search = TextEditingController();
  final _sigils = <String, SigilSpec>{};
  double _page = 0;
  Timer? _purge;
  StreamSubscription<AppEvent>? _events;
  StreamSubscription<String>? _opens;

  @override
  void initState() {
    super.initState();
    _pages.addListener(() => setState(() => _page = _pages.page ?? 0));
    _search.addListener(() => setState(() {}));
    // Temp messages burn on their own; refresh the countdowns.
    _purge = Timer.periodic(const Duration(minutes: 1), (_) => _loadTemp());
    _events = NetBus.instance.events.listen(_onEvent);
    _opens = Notifier.instance.opens.listen(_openFromNotification);
    WidgetsBinding.instance.addObserver(this);
    _loadChats();
    _loadTemp();
    _loadRequests();
  }

  /// Back from the background: links may have died meanwhile, so
  /// reconnect now instead of waiting for the next retry.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.hidden && _showHidden) setState(() => _showHidden = false);
    if (state != AppLifecycleState.resumed) return;
    NetBus.instance.refresh();
    _loadChats();
    _loadTemp();
    _loadRequests();
  }

  Future<void> _loadRequests() async {
    final requests = await requestList();
    if (mounted) setState(() => _requests = requests);
  }

  Future<void> _accept(RequestView r) async {
    HapticFeedback.heavyImpact();
    await requestAccept(key: r.key);
    if (mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text('${r.name} added. Compare the safety number on a call to verify them.')));
    }
    await Future.wait([_loadRequests(), _loadChats()]);
  }

  Future<void> _decline(RequestView r) async {
    HapticFeedback.selectionClick();
    await requestDecline(key: r.key);
    await _loadRequests();
  }

  /// Pull to refresh: reconnect to friends now, and reload everything.
  Future<void> _refresh() async {
    HapticFeedback.mediumImpact();
    await Future.wait([
      NetBus.instance.refresh(),
      _loadChats(),
      _loadTemp(),
      _loadRequests(),
      // Long enough to see the spinner, and for a link or two to come up.
      Future<void>.delayed(const Duration(milliseconds: 700)),
    ]);
    await NetBus.instance.refreshOnline();
  }

  /// A notification was tapped: show that chat (or the requests).
  Future<void> _openFromNotification(String key) async {
    if (!mounted) return;
    Navigator.of(context).popUntil((route) => route.isFirst);
    if (key == 'friends') return _goTo(2);
    final temp = key.startsWith('t:');
    final id = int.tryParse(key.substring(2));
    if (temp) await _loadTemp();
    ChatCard? find() => (temp ? _temp : _chats).where((c) => c.id == id).firstOrNull;
    var card = find();
    if (card == null && !temp) {
      await _loadChats();
      card = find();
    }
    if (card == null) return;
    _goTo(temp ? 1 : 0);
    await _open(card);
  }

  /// Opens a chat from outside the deck (the Friends tab).
  Future<void> _open(ChatCard card) async {
    HapticFeedback.selectionClick();
    card.preloaded = await card.source.load();
    if (!mounted) return;
    await Navigator.of(context).push(ChatRoute(builder: (ctx) => _openChat(card, () => Navigator.pop(ctx))));
    card.preloaded = null;
    await _closed(card);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _purge?.cancel();
    _events?.cancel();
    _opens?.cancel();
    _pages.dispose();
    _search.dispose();
    super.dispose();
  }

  Future<SigilSpec> _sigil(List<int> key) async => _sigils[key.join(',')] ??= await sigilForKey(publicKey: key);

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
      case AppEventKind.request:
        HapticFeedback.mediumImpact();
        _loadRequests();
      // Dots, badges and notifications update themselves (NetBus, Notifier).
      case AppEventKind.presence || AppEventKind.message || AppEventKind.tempMessage:
        break;
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
        friendChatId: notes ? null : c.id,
        hidden: c.hidden,
        verified: c.verified,
        note: notes
            ? 'Only on this device'
            : c.waiting
                ? 'Request sent · waiting for them to accept'
                : 'End-to-end encrypted · over Tor',
        updatedAt: DateTime.fromMillisecondsSinceEpoch(c.createdAt * 1000),
      )..pinned = c.pinned;
      await card.refresh();
      cards.add(card);
    }
    if (mounted) {
      setState(() {
        _chats
          ..clear()
          ..addAll(cards);
        _waiting
          ..clear()
          ..addAll([
            for (final c in stored)
              if (c.waiting) c.id,
          ]);
      });
    }
  }

  /// Temp chats live in Rust memory; friends' new temp chats show up here
  /// as they arrive.
  Future<void> _loadTemp() async {
    final views = await tempList();
    final pinned = {
      for (final c in _temp)
        if (c.pinned) c.id,
    };
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
        friendChatId: v.friendChatId?.toInt(),
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

  bool get _onTemp => _page > 0.5 && _page < 1.5;
  bool get _onFriends => _page >= 1.5;

  Future<void> _newTemp() async {
    HapticFeedback.selectionClick();
    final friends = await friendList();
    if (!mounted) return;
    final picked = await showModalBottomSheet<(bool, int?)>(
      context: context,
      builder: (context) => _TempPicker(
        friends: [for (final f in friends) if (_showHidden || !f.hidden) f],
        sigil: _sigil,
      ),
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
            if (!card.temp && card.friendChatId != null)
              ListTile(
                leading: Icon(card.hidden ? Icons.visibility_rounded : Icons.visibility_off_rounded),
                title: Text(card.hidden ? 'Unhide chat' : 'Hide chat'),
                subtitle: card.hidden ? null : const Text('Long-press "Sanctum" to see hidden chats'),
                onTap: () {
                  Navigator.pop(context);
                  _setHidden(card, !card.hidden);
                },
              ),
            if (card.friend && !card.temp) ...[
              ListTile(
                leading: const Icon(Icons.edit_rounded),
                title: const Text('Rename'),
                onTap: () {
                  Navigator.pop(context);
                  _rename(card);
                },
              ),
              ListTile(
                leading: const Icon(Icons.delete_outline_rounded, color: Palette.danger),
                title: const Text('Delete chat', style: TextStyle(color: Palette.danger)),
                onTap: () {
                  Navigator.pop(context);
                  _delete(card);
                },
              ),
            ],
          ],
        ),
      ),
    );
  }

  Future<void> _toggleHidden() async {
    if (_showHidden) {
      HapticFeedback.selectionClick();
      setState(() => _showHidden = false);
      return;
    }
    if (!_chats.any((c) => c.hidden)) return;
    final ok = await AppLock.whileAway(() async {
      try {
        final auth = LocalAuthentication();
        return !await auth.isDeviceSupported() || await auth.authenticate(localizedReason: 'Show hidden chats');
      } on LocalAuthException {
        return false;
      }
    });
    if (!ok || !mounted) return;
    HapticFeedback.mediumImpact();
    setState(() => _showHidden = true);
    _goTo(0);
  }

  Future<void> _setHidden(ChatCard card, bool hidden) async {
    HapticFeedback.mediumImpact();
    await chatSetHidden(id: card.id, hidden: hidden);
    await _loadChats();
    if (!mounted || !hidden) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(const SnackBar(content: Text('Hidden. Long-press "Sanctum" to see hidden chats.')));
  }

  Future<void> _rename(ChatCard card) async {
    final name = await showDialog<String>(
      context: context,
      builder: (context) => _RenameDialog(initial: card.title),
    );
    if (name == null || name.trim().isEmpty) return;
    await chatRename(id: card.id, name: name);
    await _loadChats();
  }

  Future<void> _delete(ChatCard card) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('Delete chat with ${card.title}?'),
        content: const Text(
          'Messages are erased from this phone and unsent ones are dropped. '
          'They won\'t be able to reach you until you add each other again.',
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Delete', style: TextStyle(color: Palette.danger)),
          ),
        ],
      ),
    );
    if (ok != true) return;
    HapticFeedback.heavyImpact();
    await chatDelete(id: card.id);
    await _loadChats();
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
    // However the screen was left, a friend may have been added.
    await _loadChats();
    if (added == true) _goTo(0);
  }

  void _showMe() {
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: widget.mySigil.background.color,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (sheet) => ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(sheet).height * 0.88),
        child: ListView(
          shrinkWrap: true,
          children: [
            const Center(child: Text('YOUR SIGIL', style: AppText.overline)),
            Padding(
              padding: const EdgeInsets.all(24),
              child: Center(
                child: SizedBox.square(
                  dimension: 220,
                  child: ClipOval(child: LivingSigil(spec: widget.mySigil, period: 60)),
                ),
              ),
            ),
            const Padding(
              padding: EdgeInsets.fromLTRB(32, 0, 32, Space.md),
              child: Text(
                'Made from your key. When you and a friend add each other, '
                'you both see the same sigil for your chat.',
                textAlign: TextAlign.center,
                style: AppText.caption,
              ),
            ),
            const Padding(
              padding: EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.md),
              child: _BackgroundSettings(),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
              child: ListTile(
                leading: const Icon(Icons.delete_forever_rounded, color: Palette.danger),
                title: const Text('Panic wipe', style: TextStyle(color: Palette.danger)),
                subtitle: const Text('Erase everything from this phone'),
                onTap: () => showPanicSheet(context),
              ),
            ),
            SizedBox(height: MediaQuery.paddingOf(sheet).bottom),
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
                  // Long-press: reveal hidden chats (after a biometric check).
                  GestureDetector(
                    onLongPress: _toggleHidden,
                    child: const Text('Sanctum', style: AppText.display),
                  ),
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
              child: _Tabs(
                page: _page,
                onTap: _goTo,
                tabs: [
                  ('Chats', 0),
                  (_temp.isEmpty ? 'Temp' : 'Temp · ${_temp.length}', 0),
                  ('Friends', _requests.length),
                ],
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
              child: _SearchField(controller: _search),
            ),
            const _ReliabilityBanner(),
            AnimatedSize(
              duration: Motion.medium,
              curve: Motion.decelerate,
              child: _requests.isEmpty || _onFriends
                  ? const SizedBox(width: double.infinity)
                  : Padding(
                      padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
                      child: _RequestsBanner(requests: _requests, sigil: _sigil, onTap: () => _goTo(2)),
                    ),
            ),
            Expanded(
              child: PageView(
                controller: _pages,
                onPageChanged: (_) => HapticFeedback.selectionClick(),
                children: [
                  ChatStack(
                    cards: _visibleChats,
                    query: _search.text,
                    openBuilder: _openChat,
                    onClosed: _closed,
                    onLongPress: _actions,
                    onRefresh: _refresh,
                    empty: const _Empty(text: 'No chats yet'),
                  ),
                  ChatStack(
                    cards: _temp,
                    query: _search.text,
                    openBuilder: _openChat,
                    onClosed: _closed,
                    onLongPress: _actions,
                    onDismiss: _wipe,
                    onRefresh: _refresh,
                    empty: const _Empty(
                      text: 'No temp chats',
                      detail: 'Start one with a friend. Temp chats live only in memory on both '
                          'phones, every message burns 24 hours after it is sent, and either of '
                          'you can wipe it for both at any time.',
                    ),
                  ),
                  _FriendsPage(
                    requests: _requests,
                    friends: [
                      for (final c in _visibleChats)
                        if (c.friend && !_waiting.contains(c.id)) c,
                    ],
                    waiting: [
                      for (final c in _visibleChats)
                        if (_waiting.contains(c.id)) c,
                    ],
                    query: _search.text,
                    sigil: _sigil,
                    onAccept: _accept,
                    onDecline: _decline,
                    onOpen: _open,
                    onActions: _actions,
                    onRefresh: _refresh,
                    onAdd: _addFriend,
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
      floatingActionButton: AnimatedScale(
        scale: _onTemp || _onFriends ? 1 : 0,
        duration: Motion.medium,
        curve: _onTemp || _onFriends ? Motion.decelerate : Motion.accelerate,
        child: FloatingActionButton.extended(
          onPressed: _onTemp ? _newTemp : _addFriend,
          icon: Icon(_onFriends ? Icons.person_add_alt_1_rounded : Icons.add_rounded),
          label: AnimatedSwitcher(
            duration: Motion.fast,
            child: Text(_onFriends ? 'Add a friend' : 'New temp chat', key: ValueKey(_onFriends)),
          ),
        ),
      ),
    );
  }
}

/// Segments with a pill that slides between them, tracking the swipe.
class _Tabs extends StatelessWidget {
  const _Tabs({required this.page, required this.onTap, required this.tabs});

  final double page;
  final ValueChanged<int> onTap;

  /// Label, and a count to badge it with (0: none).
  final List<(String, int)> tabs;

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
      child: LayoutBuilder(
        builder: (context, box) {
          final width = box.maxWidth / tabs.length;
          return Stack(
            children: [
              Positioned(
                left: page.clamp(0.0, tabs.length - 1.0) * width,
                width: width,
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
              Positioned.fill(
                child: Row(
                  children: [
                    for (final (i, (label, badge)) in tabs.indexed)
                      Expanded(
                        child: GestureDetector(
                          behavior: HitTestBehavior.opaque,
                          onTap: () => onTap(i),
                          child: Row(
                            mainAxisAlignment: MainAxisAlignment.center,
                            crossAxisAlignment: CrossAxisAlignment.center,
                            children: [
                              Flexible(
                                child: Text(
                                  label,
                                  maxLines: 1,
                                  overflow: TextOverflow.fade,
                                  softWrap: false,
                                  style: AppText.bodyStrong.copyWith(
                                    fontSize: 14,
                                    color: Color.lerp(
                                      Palette.text,
                                      Palette.textFaint,
                                      (page - i).abs().clamp(0.0, 1.0),
                                    ),
                                  ),
                                ),
                              ),
                              AnimatedScale(
                                scale: badge > 0 ? 1 : 0,
                                duration: Motion.medium,
                                curve: Motion.emphasized,
                                child: Container(
                                  margin: const EdgeInsets.only(left: 6),
                                  padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
                                  decoration: BoxDecoration(
                                    color: _amber,
                                    borderRadius: BorderRadius.circular(Radii.sm),
                                  ),
                                  child: Text(
                                    '$badge',
                                    style: AppText.micro.copyWith(color: Palette.bg, fontWeight: FontWeight.w700),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                  ],
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}

const _amber = Color(0xFFFFC145);

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
    return ValueListenableBuilder<NetInfo>(
      valueListenable: NetBus.instance.status,
      builder: (context, net, _) {
        final (color, label) = switch (net.status) {
          NetStatus.online => (const Color(0xFF3DDC97), 'Tor'),
          NetStatus.connecting => (const Color(0xFFFFC145), 'Connecting ${net.progress}%'),
          NetStatus.failed => (Palette.danger, 'Offline · retry'),
          NetStatus.off => (Palette.textFaint, 'Offline'),
        };
        return GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => _explain(context, net),
          child: AnimatedContainer(
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
                Container(
                  width: 7,
                  height: 7,
                  decoration: BoxDecoration(color: color, shape: BoxShape.circle),
                ),
                const SizedBox(width: 6),
                Text(
                  label,
                  style: AppText.micro.copyWith(color: color, fontWeight: FontWeight.w600),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  void _explain(BuildContext context, NetInfo net) {
    HapticFeedback.selectionClick();
    final failed = net.status == NetStatus.failed;
    final text = switch (net.status) {
      NetStatus.online => 'Connected through Tor. Friends reach you at your onion address; nobody sees your IP.',
      NetStatus.connecting => 'Connecting to Tor (${net.progress}%): ${net.detail}. The first start takes longest.',
      NetStatus.failed => 'Couldn\'t connect: ${net.detail}',
      NetStatus.off => 'Not connected.',
    };
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          content: Text(text),
          action: failed ? SnackBarAction(label: 'Retry', onPressed: NetBus.instance.retry) : null,
        ),
      );
    if (failed) NetBus.instance.retry();
  }
}

/// Asks what to call a friend.
class _RenameDialog extends StatefulWidget {
  const _RenameDialog({required this.initial});

  final String initial;

  @override
  State<_RenameDialog> createState() => _RenameDialogState();
}

class _RenameDialogState extends State<_RenameDialog> {
  late final _name = TextEditingController(text: widget.initial)
    ..selection = TextSelection(baseOffset: 0, extentOffset: widget.initial.length);

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Rename'),
      content: TextField(
        controller: _name,
        autofocus: true,
        textCapitalization: TextCapitalization.words,
        onSubmitted: (v) => Navigator.pop(context, v),
        decoration: const InputDecoration(helperText: 'Only you see this name'),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        TextButton(onPressed: () => Navigator.pop(context, _name.text), child: const Text('Save')),
      ],
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
                    builder: (_, snap) =>
                        snap.data == null ? const SizedBox.shrink() : ClipOval(child: SigilView(spec: snap.data!)),
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

/// "Riya wants to add you": friend requests waiting for an answer.
class _RequestsBanner extends StatelessWidget {
  const _RequestsBanner({required this.requests, required this.sigil, required this.onTap});

  final List<RequestView> requests;
  final Future<SigilSpec> Function(List<int>) sigil;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final first = requests.first;
    final label = requests.length == 1 ? '${first.name} wants to add you' : '${requests.length} friend requests';
    return Material(
      color: Palette.surface,
      borderRadius: BorderRadius.circular(Radii.md),
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.md),
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: Space.md, vertical: 10),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(Radii.md),
            border: Border.all(color: const Color(0xFFFFC145).withValues(alpha: 0.35)),
          ),
          child: Row(
            children: [
              SizedBox(
                width: 36 + 14.0 * (requests.length.clamp(1, 3) - 1),
                height: 36,
                child: Stack(
                  children: [
                    for (final (i, r) in requests.take(3).indexed)
                      Positioned(
                        left: 14.0 * i,
                        child: _SigilDot(future: sigil(r.sigilKey), size: 36),
                      ),
                  ],
                ),
              ),
              const SizedBox(width: Space.md),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      label,
                      style: AppText.body.copyWith(fontWeight: FontWeight.w600),
                      overflow: TextOverflow.ellipsis,
                    ),
                    const Text('Tap to answer in Friends', style: AppText.caption),
                  ],
                ),
              ),
              const Icon(Icons.chevron_right_rounded, color: Palette.textFaint),
            ],
          ),
        ),
      ),
    );
  }
}

class _SigilDot extends StatelessWidget {
  const _SigilDot({required this.future, required this.size});

  final Future<SigilSpec> future;
  final double size;

  @override
  Widget build(BuildContext context) {
    return SizedBox.square(
      dimension: size,
      child: DecoratedBox(
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          border: Border.all(color: Palette.bg, width: 2),
        ),
        child: ClipOval(
          child: FutureBuilder<SigilSpec>(
            future: future,
            builder: (context, s) => s.hasData ? SigilView(spec: s.data!) : const ColoredBox(color: Palette.surface),
          ),
        ),
      ),
    );
  }
}

/// A friend request: who, the sigil your chat will have, and the safety
/// number to compare. Accepting adds them unverified.
class _RequestTile extends StatelessWidget {
  const _RequestTile({required this.request, required this.sigil, required this.onAccept, required this.onDecline});

  final RequestView request;
  final Future<SigilSpec> Function(List<int>) sigil;
  final VoidCallback onAccept;
  final VoidCallback onDecline;

  @override
  Widget build(BuildContext context) {
    return Container(
      margin: const EdgeInsets.only(bottom: Space.sm),
      padding: const EdgeInsets.all(Space.md),
      decoration: BoxDecoration(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.md),
        border: Border.all(color: _amber.withValues(alpha: 0.35)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              _SigilDot(future: sigil(request.sigilKey), size: 44),
              const SizedBox(width: Space.md),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text('${request.name} wants to add you', style: AppText.bodyStrong),
                    const SizedBox(height: 2),
                    Text(request.safetyNumber, style: AppText.mono.copyWith(fontSize: 13, letterSpacing: 1.5)),
                  ],
                ),
              ),
            ],
          ),
          const SizedBox(height: Space.md),
          Row(
            children: [
              Expanded(
                child: OutlinedButton(onPressed: onDecline, child: const Text('Decline')),
              ),
              const SizedBox(width: Space.sm),
              Expanded(
                child: FilledButton(onPressed: onAccept, child: const Text('Accept')),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Everyone: requests to answer, requests you sent, and your friends with
/// who's online right now.
class _FriendsPage extends StatelessWidget {
  const _FriendsPage({
    required this.requests,
    required this.friends,
    required this.waiting,
    required this.query,
    required this.sigil,
    required this.onAccept,
    required this.onDecline,
    required this.onOpen,
    required this.onActions,
    required this.onRefresh,
    required this.onAdd,
  });

  final List<RequestView> requests;
  final List<ChatCard> friends;
  final List<ChatCard> waiting;
  final String query;
  final Future<SigilSpec> Function(List<int>) sigil;
  final ValueChanged<RequestView> onAccept;
  final ValueChanged<RequestView> onDecline;
  final ValueChanged<ChatCard> onOpen;
  final ValueChanged<ChatCard> onActions;
  final Future<void> Function() onRefresh;
  final VoidCallback onAdd;

  bool _matches(String name) => query.trim().isEmpty || name.toLowerCase().contains(query.trim().toLowerCase());

  @override
  Widget build(BuildContext context) {
    final requests = [
      for (final r in this.requests)
        if (_matches(r.name)) r,
    ];
    final waiting = [
      for (final c in this.waiting)
        if (_matches(c.title)) c,
    ];
    return RefreshIndicator(
      onRefresh: onRefresh,
      color: Theme.of(context).colorScheme.primary,
      backgroundColor: Palette.surfaceHigh,
      child: ValueListenableBuilder<Set<int>>(
        valueListenable: NetBus.instance.online,
        builder: (context, online, _) {
          final friends = [
            for (final c in this.friends)
              if (_matches(c.title)) c,
          ]..sort((a, b) {
              final ao = online.contains(a.id), bo = online.contains(b.id);
              if (ao != bo) return ao ? -1 : 1;
              return a.title.toLowerCase().compareTo(b.title.toLowerCase());
            });
          final nOnline = friends.where((c) => online.contains(c.id)).length;
          if (requests.isEmpty && waiting.isEmpty && friends.isEmpty) {
            return LayoutBuilder(
              builder: (context, box) => ListView(
                physics: const AlwaysScrollableScrollPhysics(parent: BouncingScrollPhysics()),
                children: [
                  SizedBox(
                    height: box.maxHeight,
                    child: const _Empty(
                      text: 'No friends yet',
                      detail: 'Add a friend with their code: they get a request to accept. '
                          'Requests you get show up here too.',
                    ),
                  ),
                ],
              ),
            );
          }
          return ListView(
            physics: const AlwaysScrollableScrollPhysics(parent: BouncingScrollPhysics()),
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 120),
            children: [
              if (requests.isNotEmpty) ...[
                _SectionLabel('REQUESTS · ${requests.length}'),
                for (final r in requests)
                  _RequestTile(request: r, sigil: sigil, onAccept: () => onAccept(r), onDecline: () => onDecline(r)),
              ],
              if (waiting.isNotEmpty) ...[
                _SectionLabel('SENT · ${waiting.length}'),
                for (final c in waiting)
                  _FriendRow(
                    card: c,
                    subtitle: 'Request sent · they\'ll see it when their phone is reachable',
                    onTap: () => onOpen(c),
                    onLongPress: () => onActions(c),
                  ),
              ],
              if (friends.isNotEmpty) ...[
                _SectionLabel(nOnline > 0 ? 'FRIENDS · $nOnline ONLINE' : 'FRIENDS · ${friends.length}'),
                for (final c in friends)
                  _FriendRow(
                    card: c,
                    subtitle: online.contains(c.id)
                        ? 'Online'
                        : c.verified
                            ? 'Verified'
                            : 'Not verified yet',
                    subtitleColor: online.contains(c.id) ? OnlineDot.color : null,
                    onTap: () => onOpen(c),
                    onLongPress: () => onActions(c),
                  ),
              ],
            ],
          );
        },
      ),
    );
  }
}

class _SectionLabel extends StatelessWidget {
  const _SectionLabel(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(Space.xs, Space.md, Space.xs, Space.sm),
        child: Text(text, style: AppText.overline),
      );
}

class _FriendRow extends StatelessWidget {
  const _FriendRow({
    required this.card,
    required this.subtitle,
    this.subtitleColor,
    required this.onTap,
    required this.onLongPress,
  });

  final ChatCard card;
  final String subtitle;
  final Color? subtitleColor;
  final VoidCallback onTap;
  final VoidCallback onLongPress;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: Space.xs),
      child: Material(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.md),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.md),
          onTap: onTap,
          onLongPress: onLongPress,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: Space.md, vertical: 10),
            child: Row(
              children: [
                Stack(
                  clipBehavior: Clip.none,
                  children: [
                    // No Hero: the Chats deck already uses this chat's tag.
                    SizedBox.square(
                      dimension: 40,
                      child: ClipOval(child: SigilView(spec: card.sigil)),
                    ),
                    Positioned(right: -1, bottom: -1, child: OnlineDot(chatId: card.id)),
                  ],
                ),
                const SizedBox(width: Space.md),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(card.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.bodyStrong),
                      const SizedBox(height: 2),
                      Text(
                        subtitle,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: AppText.caption.copyWith(color: subtitleColor),
                      ),
                    ],
                  ),
                ),
                if (card.verified) const Icon(Icons.verified_user_rounded, size: 16, color: OnlineDot.color),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Whether Sanctum stays reachable while closed, and the battery exemption
/// that keeps it reliable on phones with aggressive battery savers.
class _BackgroundSettings extends StatefulWidget {
  const _BackgroundSettings();

  @override
  State<_BackgroundSettings> createState() => _BackgroundSettingsState();
}

class _BackgroundSettingsState extends State<_BackgroundSettings> with WidgetsBindingObserver {
  bool? _on;
  bool _unrestricted = true;
  bool _canNotify = true;
  ({String brand, bool aggressive}) _brand = (brand: '', aggressive: false);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _load();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  // Back from the system battery dialog.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _load();
  }

  Future<void> _load() async {
    final on = await Background.enabled();
    final unrestricted = await Background.batteryUnrestricted();
    final canNotify = await Background.canNotify();
    final brand = await Background.brand();
    if (!mounted) return;
    setState(() {
      _brand = brand;
      _on = on;
      _unrestricted = unrestricted;
      _canNotify = canNotify;
    });
  }

  @override
  Widget build(BuildContext context) {
    final on = _on;
    if (on == null) return const SizedBox(height: 64);
    return Container(
      decoration: BoxDecoration(
        color: Palette.bg.withValues(alpha: 0.4),
        borderRadius: BorderRadius.circular(Radii.md),
      ),
      child: Column(
        children: [
          SwitchListTile(
            value: on,
            title: const Text('Stay connected when closed'),
            subtitle: Text(
              on
                  ? 'Messages and requests arrive and notify you. A quiet "Connected privately" notice stays while it runs.'
                  : 'Messages wait on your friends\' phones until you open Sanctum.',
              style: AppText.caption,
            ),
            onChanged: (v) async {
              HapticFeedback.selectionClick();
              setState(() => _on = v);
              await Background.setEnabled(v);
              if (v) await Background.requestNotifications();
              await _load();
            },
          ),
          if (on && !_canNotify)
            const ListTile(
              dense: true,
              leading: Icon(Icons.notifications_off_outlined),
              title: Text('Notifications are off'),
              subtitle: Text('Turn them on for Sanctum in Android settings to hear about new messages.'),
            ),
          if (on && !_unrestricted)
            ListTile(
              dense: true,
              leading: const Icon(Icons.battery_alert_rounded, color: _amber),
              title: const Text('Allow unrestricted battery'),
              subtitle: const Text('Some phones cut background connections to save battery. Tap to allow.'),
              onTap: Background.requestUnrestricted,
            ),
          if (on)
            ListTile(
              dense: true,
              leading: const Icon(Icons.settings_suggest_rounded),
              title: Text(_brand.aggressive ? 'Let Sanctum run on ${_brandName(_brand.brand)}' : 'Background settings'),
              subtitle: Text(
                _brand.aggressive
                    ? '${_brandName(_brand.brand)} phones stop apps in the background. Opens the setting directly: '
                        'turn on Autostart / allow background activity for Sanctum.'
                    : 'Opens Sanctum\'s settings, if notifications stop after a while.',
              ),
              trailing: const Icon(Icons.open_in_new_rounded, size: 18),
              onTap: Background.openBackgroundSettings,
            ),
        ],
      ),
    );
  }
}

String _brandName(String brand) => switch (brand) {
      'xiaomi' => 'Xiaomi',
      'oppo' => 'Oppo / Realme',
      'vivo' => 'Vivo',
      'huawei' => 'Huawei / Honor',
      'samsung' => 'Samsung',
      'oneplus' => 'OnePlus',
      _ => brand.isEmpty ? 'this phone' : brand[0].toUpperCase() + brand.substring(1),
    };

/// Shown until background delivery is set up to be reliable on this phone:
/// one tap opens the exact setting.
class _ReliabilityBanner extends StatefulWidget {
  const _ReliabilityBanner();

  @override
  State<_ReliabilityBanner> createState() => _ReliabilityBannerState();
}

class _ReliabilityBannerState extends State<_ReliabilityBanner> with WidgetsBindingObserver {
  bool _show = false;
  bool _unrestricted = true;
  String _brand = '';

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _load();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _load();
  }

  Future<void> _load() async {
    final show = await Background.needsReliabilityStep() && !await loadFlag('reliability_dismissed', fallback: false);
    final unrestricted = await Background.batteryUnrestricted();
    final brand = (await Background.brand()).brand;
    if (!mounted) return;
    setState(() {
      _show = show;
      _unrestricted = unrestricted;
      _brand = brand;
    });
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedSize(
      duration: Motion.medium,
      curve: Motion.decelerate,
      child: !_show
          ? const SizedBox(width: double.infinity)
          : Container(
              margin: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
              padding: const EdgeInsets.fromLTRB(Space.md, Space.sm, Space.xs, Space.sm),
              decoration: BoxDecoration(
                color: Palette.surface,
                borderRadius: BorderRadius.circular(Radii.md),
                border: Border.all(color: _amber.withValues(alpha: 0.35)),
              ),
              child: Row(
                children: [
                  const Icon(Icons.battery_alert_rounded, color: _amber),
                  const SizedBox(width: Space.md),
                  Expanded(
                    child: Text(
                      _unrestricted
                          ? '${_brandName(_brand)} phones can stop Sanctum in the background. Allow it to keep running.'
                          : 'Allow Sanctum to run in the background, so messages reach you when it\'s closed.',
                      style: AppText.caption,
                    ),
                  ),
                  TextButton(
                    onPressed: () async {
                      HapticFeedback.selectionClick();
                      _unrestricted ? await Background.openBackgroundSettings() : await Background.requestUnrestricted();
                    },
                    child: const Text('Open'),
                  ),
                  IconButton(
                    tooltip: 'Dismiss',
                    icon: const Icon(Icons.close_rounded, size: 18),
                    onPressed: () async {
                      await saveFlag('reliability_dismissed', true);
                      _load();
                    },
                  ),
                ],
              ),
            ),
    );
  }
}
