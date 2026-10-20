import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../deck/chat_card.dart';
import '../sigil/sigil_painter.dart';
import 'message_source.dart';

class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.card, this.onWipe});

  final ChatCard card;

  /// Temp chats only: wipes the chat and closes the screen.
  final VoidCallback? onWipe;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _input = TextEditingController();
  List<ChatMessage> _messages = [];
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    _reload();
    // Temp messages count down and disappear while the chat is open.
    if (widget.card.temp) {
      _tick = Timer.periodic(const Duration(seconds: 30), (_) => _reload());
    }
  }

  @override
  void dispose() {
    _tick?.cancel();
    _input.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    final messages = await widget.card.source.load();
    if (mounted) setState(() => _messages = messages);
  }

  Future<void> _send() async {
    final body = _input.text.trim();
    if (body.isEmpty) return;
    HapticFeedback.lightImpact();
    _input.clear();
    await widget.card.source.send(body);
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    final accent = widget.card.sigil.accent.color;
    return Scaffold(
      body: Stack(
        fit: StackFit.expand,
        children: [
          Hero(
            tag: 'chat-${widget.card.id}',
            child: LivingSigil(spec: widget.card.sigil, intensity: 0.35),
          ),
          SafeArea(
            child: Column(
              children: [
                _header(context),
                Expanded(
                  child: ListView.builder(
                    reverse: true,
                    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                    itemCount: _messages.length,
                    itemBuilder: (_, i) => _Bubble(
                      message: _messages[_messages.length - 1 - i],
                      accent: accent,
                    ),
                  ),
                ),
                _composer(accent),
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _header(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 4, 8, 4),
      child: Row(
        children: [
          IconButton(
            icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 20),
            onPressed: () => Navigator.pop(context),
          ),
          Expanded(
            child: Text(widget.card.title,
                style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600)),
          ),
          if (widget.onWipe != null)
            TextButton(
              onPressed: () {
                HapticFeedback.heavyImpact();
                widget.onWipe!();
                Navigator.pop(context);
              },
              child: const Text('Wipe', style: TextStyle(color: Colors.redAccent)),
            ),
        ],
      ),
    );
  }

  Widget _composer(Color accent) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(28),
        child: BackdropFilter(
          filter: ImageFilter.blur(sigmaX: 20, sigmaY: 20),
          child: Container(
            color: Colors.white.withValues(alpha: 0.06),
            padding: const EdgeInsets.only(left: 20, right: 6),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _input,
                    minLines: 1,
                    maxLines: 5,
                    textCapitalization: TextCapitalization.sentences,
                    decoration: InputDecoration(
                      border: InputBorder.none,
                      hintText: widget.card.temp ? 'Burns in 24h…' : 'Message',
                    ),
                    onSubmitted: (_) => _send(),
                  ),
                ),
                IconButton(
                  icon: Icon(Icons.arrow_upward_rounded, color: accent),
                  onPressed: _send,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _Bubble extends StatelessWidget {
  const _Bubble({required this.message, required this.accent});

  final ChatMessage message;
  final Color accent;

  String _time(DateTime t) =>
      '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';

  String _burnsIn(DateTime at) {
    final left = at.difference(DateTime.now());
    if (left.inHours >= 1) return 'burns in ${left.inHours}h';
    return 'burns in ${left.inMinutes.clamp(0, 59)}m';
  }

  @override
  Widget build(BuildContext context) {
    final mine = message.fromMe;
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0, end: 1),
        duration: const Duration(milliseconds: 280),
        curve: Curves.easeOutCubic,
        builder: (_, v, child) => Opacity(
          opacity: v,
          child: Transform.translate(offset: Offset(0, 12 * (1 - v)), child: child),
        ),
        child: Container(
          constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width * 0.75),
          margin: const EdgeInsets.symmetric(vertical: 4),
          padding: const EdgeInsets.fromLTRB(16, 10, 16, 8),
          decoration: BoxDecoration(
            color: mine
                ? accent.withValues(alpha: 0.28)
                : Colors.white.withValues(alpha: 0.08),
            border: Border.all(
                color: (mine ? accent : Colors.white).withValues(alpha: 0.25)),
            borderRadius: BorderRadius.only(
              topLeft: const Radius.circular(20),
              topRight: const Radius.circular(20),
              bottomLeft: Radius.circular(mine ? 20 : 6),
              bottomRight: Radius.circular(mine ? 6 : 20),
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              Text(message.body, style: const TextStyle(fontSize: 16, height: 1.3)),
              const SizedBox(height: 4),
              Text(
                message.expiresAt == null
                    ? _time(message.sentAt)
                    : '${_time(message.sentAt)} · ${_burnsIn(message.expiresAt!)}',
                style: const TextStyle(fontSize: 11, color: Colors.white54),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
