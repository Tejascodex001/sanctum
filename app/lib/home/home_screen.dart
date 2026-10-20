import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../chat/chat_screen.dart';
import '../chat/message_source.dart';
import '../deck/chat_card.dart';
import '../deck/deck_view.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/temp_chat.dart';

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.publicKey, required this.mySigil});

  final Uint8List publicKey;
  final SigilSpec mySigil;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  late final _deck = [
    ChatCard(
      id: 0,
      title: 'Notes to self',
      sigil: widget.mySigil,
      source: MemorySource(),
    ),
  ];
  final _temp = <ChatCard>[];
  bool _showTemp = false;
  Timer? _purge;

  @override
  void initState() {
    super.initState();
    _purge = Timer.periodic(const Duration(minutes: 1), (_) => _syncTemp());
  }

  @override
  void dispose() {
    _purge?.cancel();
    super.dispose();
  }

  /// Drops cards whose temp chat Rust has burned.
  Future<void> _syncTemp() async {
    final live = {for (final id in await tempChatIds()) id.toInt()};
    if (!mounted) return;
    setState(() => _temp.removeWhere((c) => !live.contains(c.id)));
  }

  Future<void> _newTemp() async {
    HapticFeedback.mediumImpact();
    final id = DateTime.now().microsecondsSinceEpoch;
    await tempOpen(id: id);
    final card = ChatCard(
      id: id,
      title: 'Temp ${_temp.length + 1}',
      sigil: await sigilRandom(),
      source: TempSource(id),
      temp: true,
    );
    setState(() => _temp.insert(0, card));
    _open(card);
  }

  Future<void> _wipe(ChatCard card) async {
    await tempWipe(id: card.id);
    setState(() => _temp.remove(card));
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

  void _open(ChatCard card) {
    Navigator.push(
      context,
      PageRouteBuilder(
        transitionDuration: const Duration(milliseconds: 450),
        reverseTransitionDuration: const Duration(milliseconds: 350),
        pageBuilder: (_, _, _) => ChatScreen(
          card: card,
          onWipe: card.temp ? () => _wipe(card) : null,
        ),
        transitionsBuilder: (_, anim, _, child) =>
            FadeTransition(opacity: anim, child: child),
      ),
    );
  }

  void _showMe() {
    showModalBottomSheet(
      context: context,
      backgroundColor: const Color(0xFF0B0B10),
      builder: (_) => SizedBox(
        height: 420,
        child: Column(
          children: [
            const SizedBox(height: 24),
            const Text('Your sigil',
                style: TextStyle(letterSpacing: 3, color: Colors.white70)),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: ClipOval(child: LivingSigil(spec: widget.mySigil)),
              ),
            ),
            const Padding(
              padding: EdgeInsets.fromLTRB(32, 0, 32, 32),
              child: Text(
                'Made from your key. Friends will add you by scanning your QR in person.',
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
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(24, 12, 16, 0),
              child: Row(
                children: [
                  _Tab(label: 'DECK', active: !_showTemp, onTap: () => setState(() => _showTemp = false)),
                  const SizedBox(width: 20),
                  _Tab(label: 'TEMP', active: _showTemp, onTap: () => setState(() => _showTemp = true)),
                  const Spacer(),
                  GestureDetector(
                    onTap: _showMe,
                    child: SizedBox.square(
                      dimension: 40,
                      child: ClipOval(child: LivingSigil(spec: widget.mySigil)),
                    ),
                  ),
                ],
              ),
            ),
            Expanded(
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 300),
                child: _showTemp
                    ? DeckView(
                        key: const ValueKey('temp'),
                        cards: _temp,
                        onOpen: _open,
                        onLongPress: _wipe,
                        subtitle: (_) => 'Messages burn 24h after sending · hold to wipe',
                        emptyLabel: 'NO TEMP CHATS',
                      )
                    : DeckView(
                        key: const ValueKey('deck'),
                        cards: _deck,
                        onOpen: _open,
                        subtitle: (_) => 'Swipe up to open',
                      ),
              ),
            ),
            if (_showTemp)
              Padding(
                padding: const EdgeInsets.fromLTRB(24, 0, 24, 16),
                child: Row(
                  children: [
                    if (_temp.isNotEmpty)
                      TextButton(
                        onPressed: _wipeAll,
                        child: const Text('Wipe all', style: TextStyle(color: Colors.redAccent)),
                      ),
                    const Spacer(),
                    FilledButton.tonal(onPressed: _newTemp, child: const Text('New temp chat')),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab({required this.label, required this.active, required this.onTap});

  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      onTap: () {
        HapticFeedback.selectionClick();
        onTap();
      },
      child: AnimatedDefaultTextStyle(
        duration: const Duration(milliseconds: 200),
        style: TextStyle(
          fontSize: 15,
          letterSpacing: 4,
          fontWeight: FontWeight.w600,
          color: active ? Colors.white : Colors.white30,
        ),
        child: Text(label),
      ),
    );
  }
}
