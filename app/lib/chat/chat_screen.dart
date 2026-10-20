import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../deck/chat_card.dart';
import '../sigil/sigil_painter.dart';
import '../theme/tokens.dart';
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
  // Loaded before the screen opens, so nothing pops in mid-transition.
  late List<ChatMessage> _messages = widget.card.preloaded ?? const [];
  late final int _initialCount = _messages.length;
  Timer? _tick;
  bool _hasText = false;
  late final _wipe = AnimationController(vsync: this, duration: Motion.slow);

  @override
  void initState() {
    super.initState();
    _input.addListener(() {
      final has = _input.text.trim().isNotEmpty;
      if (has != _hasText) setState(() => _hasText = has);
    });
    if (widget.card.preloaded == null) _reload();
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
      backgroundColor: Palette.bg,
      body: Stack(
        fit: StackFit.expand,
        children: [
          // The chat's sigil turns slowly behind everything: a cached
          // image rotated on the GPU, so it costs nothing per frame.
          Positioned(
            right: -170,
            top: 60,
            width: 520,
            height: 520,
            child: IgnorePointer(
              child: RepaintBoundary(child: _Backdrop(card: widget.card)),
            ),
          ),
          SafeArea(
            child: Column(
              children: [
                _Header(card: widget.card, onBack: widget.close, onWipe: widget.onWipe == null ? null : _doWipe),
                Expanded(
                  child: AnimatedBuilder(
                    animation: _wipe,
                    builder: (context, child) {
                      final t = Motion.accelerate.transform(_wipe.value);
                      return Opacity(
                        opacity: 1 - t,
                        child: Transform.scale(
                          scale: 1 - 0.06 * t,
                          // Blur only while actually wiping.
                          child: t == 0
                              ? child
                              : ImageFiltered(imageFilter: ImageFilter.blur(sigmaX: 10 * t, sigmaY: 10 * t), child: child),
                        ),
                      );
                    },
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
      padding: const EdgeInsets.fromLTRB(Space.md, Space.sm, Space.md, Space.sm),
      itemCount: _messages.length,
      itemBuilder: (_, i) {
        final index = _messages.length - 1 - i;
        final m = _messages[index];
        final next = index + 1 < _messages.length ? _messages[index + 1] : null;
        // Messages from the same side within two minutes are grouped.
        final grouped = next != null && next.fromMe == m.fromMe && next.sentAt.difference(m.sentAt).inMinutes < 2;
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
    return Container(
      padding: const EdgeInsets.fromLTRB(Space.md, Space.sm, Space.md, Space.md),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: TextField(
              controller: _input,
              minLines: 1,
              maxLines: 5,
              textCapitalization: TextCapitalization.sentences,
              cursorColor: accent,
              style: AppText.body,
              decoration: InputDecoration(
                hintText: widget.card.temp ? 'Burns in 24h…' : 'Message',
                fillColor: Palette.surfaceHigh,
                border: OutlineInputBorder(borderRadius: BorderRadius.circular(Radii.xl), borderSide: BorderSide.none),
                enabledBorder: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(Radii.xl),
                  borderSide: const BorderSide(color: Palette.outline),
                ),
                focusedBorder: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(Radii.xl),
                  borderSide: BorderSide(color: accent.withValues(alpha: 0.6)),
                ),
              ),
              onSubmitted: (_) => _send(),
            ),
          ),
          const SizedBox(width: Space.sm),
          AnimatedScale(
            scale: _hasText ? 1 : 0.86,
            duration: Motion.medium,
            curve: Motion.emphasized,
            child: AnimatedContainer(
              duration: Motion.medium,
              curve: Motion.emphasized,
              width: 50,
              height: 50,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: _hasText ? accent : Palette.surfaceHigh,
              ),
              child: IconButton(
                tooltip: 'Send',
                icon: Icon(Icons.arrow_upward_rounded,
                    color: _hasText ? Palette.onColor(accent) : Palette.textFaint),
                onPressed: _hasText ? _send : null,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The faint, slowly turning sigil behind a chat. It fades in after the
/// open transition so the transition itself stays light.
class _Backdrop extends StatelessWidget {
  const _Backdrop({required this.card});

  final ChatCard card;

  @override
  Widget build(BuildContext context) {
    final route = ModalRoute.of(context)?.animation;
    final sigil = Opacity(
      opacity: 0.9,
      child: _RotatingImage(card: card),
    );
    if (route == null) return sigil;
    return FadeTransition(
      opacity: CurvedAnimation(parent: route, curve: const Interval(0.5, 1, curve: Curves.easeOut)),
      child: sigil,
    );
  }
}

class _RotatingImage extends StatefulWidget {
  const _RotatingImage({required this.card});

  final ChatCard card;

  @override
  State<_RotatingImage> createState() => _RotatingImageState();
}

class _RotatingImageState extends State<_RotatingImage> with SingleTickerProviderStateMixin {
  late final _turn = AnimationController(vsync: this, duration: const Duration(seconds: 160))..repeat();

  @override
  void dispose() {
    _turn.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RotationTransition(
      turns: _turn,
      child: SigilView(spec: widget.card.sigil, opacity: 0.18, background: false),
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
    return Container(
      padding: const EdgeInsets.fromLTRB(Space.xs, Space.xs, Space.sm, Space.sm),
      decoration: const BoxDecoration(border: Border(bottom: BorderSide(color: Palette.outline))),
      child: Row(
        children: [
          IconButton(
            tooltip: 'Back',
            icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 20),
            onPressed: onBack,
          ),
          Hero(
            tag: 'emblem-${card.id}',
            child: SizedBox.square(
              dimension: 40,
              child: ClipOval(child: SigilView(spec: card.sigil)),
            ),
          ),
          const SizedBox(width: Space.md),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(card.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.heading),
                const SizedBox(height: 2),
                Text(
                  card.temp ? 'Temp · burns after 24h' : card.note ?? 'Only on this device',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: AppText.micro,
                ),
              ],
            ),
          ),
          if (onWipe != null)
            TextButton.icon(
              onPressed: onWipe,
              icon: const Icon(Icons.local_fire_department_rounded, color: Palette.danger, size: 18),
              label: const Text('Wipe', style: TextStyle(color: Palette.danger)),
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
      child: Padding(
        padding: const EdgeInsets.all(Space.xxl),
        child: Text(
          card.temp ? 'Say something. It burns in 24 hours.' : 'Nothing here yet.',
          textAlign: TextAlign.center,
          style: AppText.caption,
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
  late final _c = AnimationController(vsync: this, duration: Motion.medium, value: widget.animate ? 0 : 1)
    ..forward();
  late final _curve = CurvedAnimation(parent: _c, curve: Motion.emphasized);

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  String _time(DateTime t) => '${t.hour.toString().padLeft(2, '0')}:${t.minute.toString().padLeft(2, '0')}';

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
    final fg = mine ? Palette.onColor(accent) : Palette.text;
    const r = Radius.circular(20);
    const tail = Radius.circular(6);
    final bubble = Container(
      constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width * 0.76),
      margin: EdgeInsets.only(top: 2, bottom: widget.showTime ? Space.sm : 2),
      padding: const EdgeInsets.fromLTRB(14, 9, 14, 8),
      decoration: BoxDecoration(
        color: mine ? accent : Palette.surfaceHigh,
        border: mine ? null : Border.all(color: Palette.outline),
        borderRadius: BorderRadius.only(
          topLeft: r,
          topRight: r,
          bottomLeft: mine || !widget.showTime ? r : tail,
          bottomRight: !mine || !widget.showTime ? r : tail,
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.end,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(m.body, style: AppText.body.copyWith(color: fg)),
          if (widget.showTime) ...[
            const SizedBox(height: 3),
            Text(
              m.expiresAt == null ? _time(m.sentAt) : '${_time(m.sentAt)} · ${_burnsIn(m.expiresAt!)}',
              style: AppText.micro.copyWith(color: fg.withValues(alpha: 0.6)),
            ),
          ],
        ],
      ),
    );
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      child: FadeTransition(
        opacity: _curve,
        child: ScaleTransition(
          alignment: mine ? Alignment.bottomRight : Alignment.bottomLeft,
          scale: Tween(begin: 0.9, end: 1.0).animate(_curve),
          child: SlideTransition(
            position: Tween(begin: const Offset(0, 0.25), end: Offset.zero).animate(_curve),
            child: bubble,
          ),
        ),
      ),
    );
  }
}
