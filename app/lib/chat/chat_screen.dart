import 'dart:async';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../deck/chat_card.dart';
import '../deck/chat_tile.dart';
import '../net/net_bus.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
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
  StreamSubscription<AppEvent>? _events;
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
    // Friends' messages and delivery receipts arrive live.
    _events = NetBus.instance.events.listen((e) {
      if (e.id.toInt() != widget.card.id) return;
      final temp = widget.card.temp;
      if (temp && e.kind == AppEventKind.tempGone) {
        if (!mounted) return;
        ScaffoldMessenger.maybeOf(context)
            ?.showSnackBar(const SnackBar(content: Text('This temp chat was wiped.')));
        widget.close();
      } else if ((temp && e.kind == AppEventKind.temp) || (!temp && e.kind == AppEventKind.chat)) {
        _reload();
      }
    });
    // Temp messages count down and disappear while the chat is open.
    if (widget.card.temp) {
      _tick = Timer.periodic(const Duration(seconds: 30), (_) => _reload());
    }
  }

  @override
  void dispose() {
    _tick?.cancel();
    _events?.cancel();
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
    // Shown at once; the reload swaps in the stored copy.
    final now = DateTime.fromMillisecondsSinceEpoch(DateTime.now().millisecondsSinceEpoch ~/ 1000 * 1000);
    setState(() => _messages = [..._messages, ChatMessage(body: body, fromMe: true, sentAt: now)]);
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
    final friendChat = widget.card.friendChatId;
    if (friendChat == null || !widget.card.friend) return _messageList(accent, online: true);
    return ValueListenableBuilder<Set<int>>(
      valueListenable: NetBus.instance.online,
      builder: (context, online, _) => _messageList(accent, online: online.contains(friendChat)),
    );
  }

  Widget _messageList(Color accent, {required bool online}) {
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
          // By position: messages only append (temp ones also burn from
          // the front), so a reload never replays an entrance.
          key: ValueKey(index),
          message: m,
          accent: accent,
          showTime: !grouped,
          showTicks: widget.card.friend,
          friendOnline: online,
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
                ValueListenableBuilder<Set<int>>(
                  valueListenable: NetBus.instance.online,
                  builder: (context, online, _) {
                    final isOnline = card.friend && online.contains(card.friendChatId);
                    return AnimatedSwitcher(
                      duration: Motion.fast,
                      child: Row(
                        key: ValueKey(isOnline),
                        children: [
                          if (isOnline) ...[
                            Container(
                              width: 7,
                              height: 7,
                              decoration: const BoxDecoration(color: OnlineDot.color, shape: BoxShape.circle),
                            ),
                            const SizedBox(width: 5),
                          ],
                          Flexible(
                            child: Text(
                              isOnline
                                  ? (card.temp ? 'Online · burns after 24h' : 'Online')
                                  : card.temp
                                      ? 'Temp · burns after 24h'
                                      : card.note ?? 'Only on this device',
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: AppText.micro.copyWith(color: isOnline ? OnlineDot.color : null),
                            ),
                          ),
                        ],
                      ),
                    );
                  },
                ),
              ],
            ),
          ),
          if (card.friend && !card.temp) _VerifyButton(card: card),
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

/// Verification status, like Threema's levels: green once their key is
/// confirmed (QR scanned in person, or safety number compared on a call).
class _VerifyButton extends StatefulWidget {
  const _VerifyButton({required this.card});

  final ChatCard card;

  @override
  State<_VerifyButton> createState() => _VerifyButtonState();
}

class _VerifyButtonState extends State<_VerifyButton> {
  static const _green = Color(0xFF3DDC97);
  static const _amber = Color(0xFFFFC145);

  Future<void> _open() async {
    final number = await friendSafetyNumber(chatId: widget.card.id);
    if (!mounted) return;
    final card = widget.card;
    await showModalBottomSheet<void>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, setSheet) => SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.xl),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                SizedBox.square(dimension: 110, child: ClipOval(child: SigilView(spec: card.sigil))),
                const SizedBox(height: Space.lg),
                Text(card.verified ? '${card.title} is verified' : 'Verify ${card.title}', style: AppText.title),
                const SizedBox(height: Space.sm),
                Text(
                  card.verified
                      ? 'Their key was confirmed: by scanning their code in person, or by comparing this number.'
                      : 'Call ${card.title} and read this number to each other. If it matches on both phones, '
                          'nobody swapped your codes.',
                  textAlign: TextAlign.center,
                  style: AppText.caption,
                ),
                const SizedBox(height: Space.lg),
                const Text('SAFETY NUMBER', style: AppText.overline),
                const SizedBox(height: 6),
                SelectableText(number, style: AppText.mono.copyWith(fontSize: 26, letterSpacing: 3, color: card.sigil.accent.color)),
                const SizedBox(height: Space.xl),
                SizedBox(
                  width: double.infinity,
                  child: card.verified
                      ? OutlinedButton(
                          onPressed: () async {
                            await friendSetVerified(chatId: card.id, verified: false);
                            setSheet(() => card.verified = false);
                            setState(() {});
                          },
                          child: const Text('Mark as not verified'),
                        )
                      : FilledButton(
                          onPressed: () async {
                            HapticFeedback.heavyImpact();
                            await friendSetVerified(chatId: card.id, verified: true);
                            setSheet(() => card.verified = true);
                            setState(() {});
                          },
                          child: const Text('Numbers match'),
                        ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final verified = widget.card.verified;
    return IconButton(
      tooltip: verified ? 'Verified' : 'Not verified',
      onPressed: _open,
      icon: AnimatedSwitcher(
        duration: Motion.medium,
        child: Icon(
          verified ? Icons.verified_user_rounded : Icons.shield_outlined,
          key: ValueKey(verified),
          color: verified ? _green : _amber,
        ),
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
    required this.showTicks,
    this.friendOnline = true,
    required this.animate,
  });

  final ChatMessage message;
  final Color accent;
  final bool showTime;

  /// Sent / delivered marks on our messages to friends.
  final bool showTicks;

  /// Not delivered and they're offline: it waits on this phone.
  final bool friendOnline;
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
          if (widget.showTime || (mine && widget.showTicks)) ...[
            const SizedBox(height: 3),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (widget.showTime)
                  Text(
                    m.expiresAt == null ? _time(m.sentAt) : '${_time(m.sentAt)} · ${_burnsIn(m.expiresAt!)}',
                    style: AppText.micro.copyWith(color: fg.withValues(alpha: 0.6)),
                  ),
                if (mine && widget.showTicks) ...[
                  const SizedBox(width: 4),
                  AnimatedSwitcher(
                    duration: Motion.medium,
                    transitionBuilder: (child, a) => ScaleTransition(scale: a, child: child),
                    child: Icon(
                      m.delivered
                          ? Icons.done_all_rounded
                          : widget.friendOnline
                              ? Icons.done_rounded
                              : Icons.schedule_rounded,
                      key: ValueKey((m.delivered, widget.friendOnline)),
                      size: 14,
                      color: fg.withValues(alpha: m.delivered ? 0.9 : 0.5),
                      semanticLabel: m.delivered
                          ? 'Delivered'
                          : widget.friendOnline
                              ? 'Sending'
                              : 'Waiting until they are online',
                    ),
                  ),
                ],
              ],
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
