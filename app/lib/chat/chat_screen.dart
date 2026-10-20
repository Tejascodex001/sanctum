import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../deck/chat_card.dart';
import '../sigil/sigil_painter.dart';
import 'message_source.dart';

class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.card, required this.close, this.onWipe});

  final ChatCard card;
  final VoidCallback close;

  /// Temp chats only: wipes the chat; the screen animates out first.
  final Future<void> Function()? onWipe;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> with SingleTickerProviderStateMixin {
  final _input = TextEditingController();
  List<ChatMessage> _messages = [];

  /// Messages already on screen when the chat opened don't animate in.
  int _initialCount = -1;
  Timer? _tick;
  bool _hasText = false;
  late final _wipe = AnimationController(vsync: this, duration: const Duration(milliseconds: 650));

  @override
  void initState() {
    super.initState();
    _input.addListener(() {
      final has = _input.text.trim().isNotEmpty;
      if (has != _hasText) setState(() => _hasText = has);
    });
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
    _wipe.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    final messages = await widget.card.source.load();
    if (!mounted) return;
    setState(() {
      _messages = messages;
      if (_initialCount < 0) _initialCount = messages.length;
    });
  }

  Future<void> _send() async {
    final body = _input.text.trim();
    if (body.isEmpty) return;
    HapticFeedback.lightImpact();
    _input.clear();
    await widget.card.source.send(body);
    await _reload();
  }

  Future<void> _doWipe() async {
    HapticFeedback.heavyImpact();
    await _wipe.forward();
    await widget.onWipe!();
    widget.close();
  }

  @override
  Widget build(BuildContext context) {
    final sigil = widget.card.sigil;
    final accent = sigil.accent.color;
    return Scaffold(
      backgroundColor: sigil.background.color,
      body: Stack(
        fit: StackFit.expand,
        children: [
          // The chat's sigil turns slowly behind everything, oversized and faint.
          Positioned(
            right: -160,
            top: 40,
            width: 520,
            height: 520,
            child: LivingSigil(spec: sigil, opacity: 0.16, period: 160),
          ),
          SafeArea(
            child: Column(
              children: [
                _Header(card: widget.card, onBack: widget.close, onWipe: widget.onWipe == null ? null : _doWipe),
                Expanded(
                  child: AnimatedBuilder(
                    animation: _wipe,
                    builder: (context, child) => Opacity(
                      opacity: 1 - Curves.easeIn.transform(_wipe.value),
                      child: Transform.scale(
                        scale: 1 - 0.08 * _wipe.value,
                        child: ImageFiltered(
                          enabled: _wipe.value > 0,
                          imageFilter: ImageFilter.blur(sigmaX: 8 * _wipe.value, sigmaY: 8 * _wipe.value),
                          child: child,
                        ),
                      ),
                    ),
                    child: _messages.isEmpty ? _EmptyChat(card: widget.card) : _list(accent),
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

  Widget _list(Color accent) {
    return ListView.builder(
      reverse: true,
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
      itemCount: _messages.length,
      itemBuilder: (_, i) {
        final index = _messages.length - 1 - i;
        final m = _messages[index];
        final next = index + 1 < _messages.length ? _messages[index + 1] : null;
        // Messages from the same side within two minutes are grouped.
        final grouped = next != null &&
            next.fromMe == m.fromMe &&
            next.sentAt.difference(m.sentAt).inMinutes < 2;
        return _Bubble(
          key: ValueKey('${m.sentAt.microsecondsSinceEpoch}-$index'),
          message: m,
          accent: accent,
          showTime: !grouped,
          animate: index >= _initialCount,
        );
      },
    );
  }

  Widget _composer(Color accent) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(28),
        child: BackdropFilter(
          filter: ImageFilter.blur(sigmaX: 24, sigmaY: 24),
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 250),
            decoration: BoxDecoration(
              color: Colors.white.withValues(alpha: 0.07),
              borderRadius: BorderRadius.circular(28),
              border: Border.all(
                color: (_hasText ? accent : Colors.white).withValues(alpha: _hasText ? 0.5 : 0.1),
              ),
            ),
            padding: const EdgeInsets.only(left: 20, right: 6),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _input,
                    minLines: 1,
                    maxLines: 5,
                    textCapitalization: TextCapitalization.sentences,
                    cursorColor: accent,
                    decoration: InputDecoration(
                      border: InputBorder.none,
                      hintText: widget.card.temp ? 'Burns in 24h…' : 'Message',
                    ),
                    onSubmitted: (_) => _send(),
                  ),
                ),
                AnimatedScale(
                  scale: _hasText ? 1 : 0.82,
                  duration: const Duration(milliseconds: 260),
                  curve: Curves.easeOutBack,
                  child: AnimatedContainer(
                    duration: const Duration(milliseconds: 220),
                    margin: const EdgeInsets.symmetric(vertical: 6),
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      color: _hasText ? accent : Colors.white.withValues(alpha: 0.08),
                    ),
                    child: IconButton(
                      tooltip: 'Send',
                      icon: Icon(Icons.arrow_upward_rounded,
                          color: _hasText ? Colors.black : Colors.white38),
                      onPressed: _hasText ? _send : null,
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

class _Header extends StatelessWidget {
  const _Header({required this.card, required this.onBack, this.onWipe});

  final ChatCard card;
  final VoidCallback onBack;
  final VoidCallback? onWipe;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 4, 8, 4),
      child: Row(
        children: [
          IconButton(
            tooltip: 'Back',
            icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 20),
            onPressed: onBack,
          ),
          SizedBox.square(
            dimension: 38,
            child: ClipOval(child: SigilView(spec: card.sigil)),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(card.title,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w600)),
                Text(
                  card.temp ? 'Temp · burns after 24h' : card.note ?? 'Only on this device',
                  style: const TextStyle(fontSize: 12, color: Colors.white54),
                ),
              ],
            ),
          ),
          if (onWipe != null)
            TextButton.icon(
              onPressed: onWipe,
              icon: const Icon(Icons.local_fire_department_rounded, color: Colors.redAccent, size: 18),
              label: const Text('Wipe', style: TextStyle(color: Colors.redAccent)),
            ),
        ],
      ),
    );
  }
}

class _EmptyChat extends StatelessWidget {
  const _EmptyChat({required this.card});

  final ChatCard card;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: 0, end: 1),
        duration: const Duration(milliseconds: 600),
        curve: Curves.easeOutCubic,
        builder: (_, v, child) => Opacity(
          opacity: v,
          child: Transform.translate(offset: Offset(0, 16 * (1 - v)), child: child),
        ),
        child: Text(
          card.temp ? 'Say something. It burns in 24 hours.' : 'Write yourself a note.',
          style: const TextStyle(color: Colors.white38),
        ),
      ),
    );
  }
}

class _Bubble extends StatefulWidget {
  const _Bubble({
    super.key,
    required this.message,
    required this.accent,
    required this.showTime,
    required this.animate,
  });

  final ChatMessage message;
  final Color accent;
  final bool showTime;
  final bool animate;

  @override
  State<_Bubble> createState() => _BubbleState();
}

class _BubbleState extends State<_Bubble> with SingleTickerProviderStateMixin {
  late final _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 420),
    value: widget.animate ? 0 : 1,
  )..forward();
  late final _spring = CurvedAnimation(parent: _c, curve: Curves.easeOutBack);
  late final _fade = CurvedAnimation(parent: _c, curve: const Interval(0, 0.6));

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  String _time(DateTime t) =>
      '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';

  String _burnsIn(DateTime at) {
    final left = at.difference(DateTime.now());
    if (left.inHours >= 1) return 'burns in ${left.inHours}h';
    return 'burns in ${left.inMinutes.clamp(0, 59)}m';
  }

  @override
  Widget build(BuildContext context) {
    final m = widget.message;
    final mine = m.fromMe;
    final accent = widget.accent;
    final bubble = Container(
      constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width * 0.76),
      margin: EdgeInsets.only(top: 2, bottom: widget.showTime ? 8 : 2),
      padding: const EdgeInsets.fromLTRB(15, 10, 15, 9),
      decoration: BoxDecoration(
        gradient: mine
            ? LinearGradient(
                begin: Alignment.topLeft,
                end: Alignment.bottomRight,
                colors: [accent.withValues(alpha: 0.42), accent.withValues(alpha: 0.24)],
              )
            : null,
        color: mine ? null : Colors.white.withValues(alpha: 0.08),
        border: Border.all(color: (mine ? accent : Colors.white).withValues(alpha: 0.22)),
        borderRadius: BorderRadius.only(
          topLeft: const Radius.circular(20),
          topRight: const Radius.circular(20),
          bottomLeft: Radius.circular(mine || !widget.showTime ? 20 : 6),
          bottomRight: Radius.circular(!mine || !widget.showTime ? 20 : 6),
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.end,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(m.body, style: const TextStyle(fontSize: 16, height: 1.3)),
          if (widget.showTime) ...[
            const SizedBox(height: 3),
            Text(
              m.expiresAt == null
                  ? _time(m.sentAt)
                  : '${_time(m.sentAt)} · ${_burnsIn(m.expiresAt!)}',
              style: TextStyle(
                fontSize: 11,
                color: m.expiresAt == null ? Colors.white54 : accent.withValues(alpha: 0.9),
              ),
            ),
          ],
        ],
      ),
    );
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      child: FadeTransition(
        opacity: _fade,
        child: AnimatedBuilder(
          animation: _spring,
          child: bubble,
          builder: (_, child) => Transform.translate(
            offset: Offset(0, 18 * (1 - _spring.value)),
            child: Transform.scale(
              alignment: mine ? Alignment.bottomRight : Alignment.bottomLeft,
              scale: 0.86 + 0.14 * _spring.value,
              child: child,
            ),
          ),
        ),
      ),
    );
  }
}
