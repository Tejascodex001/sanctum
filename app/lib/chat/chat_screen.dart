import 'dart:async';
import 'dart:io';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../deck/chat_card.dart';
import '../deck/chat_tile.dart';
import '../net/net_bus.dart';
import '../notify/notifier.dart';
import 'media.dart';
import '../security/screenshots.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';
import 'message_source.dart';

class ChatScreen extends StatefulWidget {
  const ChatScreen({super.key, required this.card, required this.close, this.onWipe, this.onGroupInfo});

  final ChatCard card;
  final VoidCallback close;

  /// Groups: members, rename, add, remove, leave.
  final VoidCallback? onGroupInfo;

  /// Temp chats only: wipes the chat; the screen animates out first.
  final Future<void> Function()? onWipe;

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> with SingleTickerProviderStateMixin {
  final _input = TextEditingController();
  final _focus = FocusNode();
  // Loaded before the screen opens, so nothing pops in mid-transition.
  late List<ChatMessage> _messages = widget.card.preloaded ?? const [];
  late final int _initialCount = _messages.length;
  Timer? _tick;
  StreamSubscription<AppEvent>? _events;
  bool _hasText = false;

  /// The message being answered, shown above the composer.
  ChatMessage? _replyTo;

  // A voice note being recorded (hold the mic; slide left to cancel).
  bool _recording = false;
  bool _cancelArmed = false;
  DateTime _recordStart = DateTime.now();
  double _level = 0;
  Timer? _recordTick;

  /// Bubbles on screen, by sequence number: tapping a quote scrolls there.
  final _bubbleKeys = <int, GlobalKey>{};
  int? _flash;
  late final _wipe = AnimationController(vsync: this, duration: Motion.slow);

  @override
  void initState() {
    super.initState();
    _input.addListener(() {
      final has = _input.text.trim().isNotEmpty;
      if (has != _hasText) setState(() => _hasText = has);
    });
    if (widget.card.preloaded == null) _reload();
    Notifier.instance
      ..viewing = _key
      ..markRead(_key);
    Screenshots.enter(_key);
    // Friends' messages and delivery receipts arrive live.
    _events = NetBus.instance.events.listen((e) {
      if (e.id.toInt() != widget.card.id) return;
      final temp = widget.card.temp;
      if (temp && e.kind == AppEventKind.tempGone) {
        if (!mounted) return;
        ScaffoldMessenger.maybeOf(context)?.showSnackBar(const SnackBar(content: Text('This temp chat was wiped.')));
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

  String get _key => widget.card.temp ? Notifier.tempKey(widget.card.id) : Notifier.chatKey(widget.card.id);

  @override
  void dispose() {
    if (Notifier.instance.viewing == _key) Notifier.instance.viewing = null;
    Screenshots.leave();
    _tick?.cancel();
    _events?.cancel();
    _recordTick?.cancel();
    if (_recording) Background.recordCancel();
    _input.dispose();
    _focus.dispose();
    _wipe.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    final List<ChatMessage> messages;
    try {
      messages = await widget.card.source.load();
    } catch (_) {
      // The chat is gone (e.g. the friend removed us from both phones).
      if (mounted) widget.close();
      return;
    }
    if (mounted) setState(() => _messages = messages);
  }

  Future<void> _send() async {
    final body = _input.text.trim();
    if (body.isEmpty) return;
    HapticFeedback.lightImpact();
    _input.clear();
    final replyTo = _replyTo;
    // Shown at once; the reload swaps in the stored copy.
    final now = DateTime.fromMillisecondsSinceEpoch(DateTime.now().millisecondsSinceEpoch ~/ 1000 * 1000);
    setState(() {
      _replyTo = null;
      _messages = [
        ..._messages,
        ChatMessage(
          body: body,
          fromMe: true,
          sentAt: now,
          reply: replyTo == null
              ? null
              : ChatQuote(
                  author: replyTo.fromMe ? 'You' : (replyTo.senderName ?? widget.card.title),
                  fromMe: replyTo.fromMe,
                  snippet: replyTo.body,
                  seq: replyTo.seq,
                ),
        ),
      ];
    });
    await widget.card.source.send(body, replyTo: replyTo?.seq);
    await _reload();
  }

  void _startReply(ChatMessage m) {
    if (!widget.card.source.replies || m.seq == null) return;
    HapticFeedback.selectionClick();
    setState(() => _replyTo = m);
    _focus.requestFocus();
  }

  /// Scrolls to a quoted message (when it's near enough to be built) and
  /// flashes it.
  void _jumpTo(int? seq) {
    final context = seq == null ? null : _bubbleKeys[seq]?.currentContext;
    if (context == null) return;
    Scrollable.ensureVisible(context, duration: Motion.medium, curve: Motion.emphasized, alignment: 0.5);
    setState(() => _flash = seq);
    Future<void>.delayed(const Duration(milliseconds: 900), () {
      if (mounted && _flash == seq) setState(() => _flash = null);
    });
  }

  Future<void> _delete(ChatMessage m, {required bool everyone}) async {
    HapticFeedback.mediumImpact();
    setState(() => _messages = [
          for (final x in _messages)
            if (!identical(x, m)) x
        ]);
    await messageDelete(chatId: widget.card.id, seq: m.seq!, forEveryone: everyone);
    await _reload();
  }

  Future<void> _actions(ChatMessage m) async {
    HapticFeedback.mediumImpact();
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (widget.card.source.replies && m.seq != null)
              ListTile(
                leading: const Icon(Icons.reply_rounded),
                title: const Text('Reply'),
                onTap: () {
                  Navigator.pop(sheet);
                  _startReply(m);
                },
              ),
            ListTile(
              leading: const Icon(Icons.copy_rounded),
              title: const Text('Copy'),
              onTap: () {
                Navigator.pop(sheet);
                Clipboard.setData(ClipboardData(text: m.body));
              },
            ),
            if (m.seq != null) ...[
              ListTile(
                leading: const Icon(Icons.delete_outline_rounded, color: Palette.danger),
                title: const Text('Delete for me', style: TextStyle(color: Palette.danger)),
                onTap: () {
                  Navigator.pop(sheet);
                  _delete(m, everyone: false);
                },
              ),
              if (m.fromMe && widget.card.friend)
                ListTile(
                  leading: const Icon(Icons.delete_forever_rounded, color: Palette.danger),
                  title: const Text('Delete for everyone', style: TextStyle(color: Palette.danger)),
                  onTap: () {
                    Navigator.pop(sheet);
                    _delete(m, everyone: true);
                  },
                ),
            ],
          ],
        ),
      ),
    );
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
                _Header(
                  card: widget.card,
                  onBack: widget.close,
                  onWipe: widget.onWipe == null ? null : _doWipe,
                  onInfo: widget.onGroupInfo,
                ),
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
                              : ImageFiltered(
                                  imageFilter: ImageFilter.blur(sigmaX: 10 * t, sigmaY: 10 * t), child: child),
                        ),
                      );
                    },
                    child: _messages.isEmpty ? _EmptyChat(card: widget.card) : _list(accent),
                  ),
                ),
                if (widget.card.active)
                  _composer(accent)
                else
                  const Padding(
                    padding: EdgeInsets.all(Space.lg),
                    child: Text(
                      'You\'re no longer in this group.',
                      textAlign: TextAlign.center,
                      style: AppText.caption,
                    ),
                  ),
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
    // Replies per message, for the thread chips.
    final replies = <int, int>{};
    for (final m in _messages) {
      final to = m.reply?.seq;
      if (to != null) replies[to] = (replies[to] ?? 0) + 1;
    }
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
        final bubble = _Bubble(
          // By position: messages only append (temp ones also burn from
          // the front), so a reload never replays an entrance.
          key: ValueKey(index),
          message: m,
          highlight: m.seq != null && m.seq == _flash,
          onQuoteTap: () => _jumpTo(m.reply?.seq),
          onLongPress: () => _actions(m),
          chatId: widget.card.id,
          accent: accent,
          showTime: !grouped,
          showTicks: widget.card.friend,
          friendOnline: online,
          animate: index >= _initialCount,
        );
        final keyed =
            m.seq == null ? bubble : KeyedSubtree(key: _bubbleKeys.putIfAbsent(m.seq!, GlobalKey.new), child: bubble);
        final item = widget.card.source.replies && m.seq != null
            ? _SwipeToReply(onReply: () => _startReply(m), fromMe: m.fromMe, child: keyed)
            : keyed;
        final count = m.seq == null ? 0 : replies[m.seq] ?? 0;
        if (count == 0) return item;
        return Column(
          crossAxisAlignment: m.fromMe ? CrossAxisAlignment.end : CrossAxisAlignment.start,
          children: [
            item,
            _ThreadChip(count: count, accent: accent, onTap: () => _openThread(m, accent, online)),
          ],
        );
      },
    );
  }

  /// A message and every reply to it (and to those), with a composer that
  /// answers in the thread.
  Future<void> _openThread(ChatMessage root, Color accent, bool online) async {
    HapticFeedback.selectionClick();
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      showDragHandle: true,
      backgroundColor: Palette.bg,
      builder: (_) => _ThreadSheet(root: root, card: widget.card, accent: accent, online: online),
    );
    await _reload();
  }

  Widget _composer(Color accent) {
    final reply = _replyTo;
    return Container(
      padding: const EdgeInsets.fromLTRB(Space.md, Space.sm, Space.md, Space.md),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          AnimatedSize(
            duration: Motion.medium,
            curve: Motion.emphasized,
            child: reply == null
                ? const SizedBox(width: double.infinity)
                : Padding(
                    padding: const EdgeInsets.only(bottom: Space.sm),
                    child: Row(
                      children: [
                        Expanded(
                          child: _QuoteBlock(
                            author: reply.fromMe ? 'You' : (reply.senderName ?? widget.card.title),
                            snippet: reply.body,
                            color: accent,
                            fg: Palette.text,
                            label: 'Replying to',
                          ),
                        ),
                        IconButton(
                          tooltip: 'Cancel reply',
                          icon: const Icon(Icons.close_rounded, size: 20),
                          onPressed: () => setState(() => _replyTo = null),
                        ),
                      ],
                    ),
                  ),
          ),
          _composerRow(accent),
        ],
      ),
    );
  }

  /// Picks a photo, video or file and sends it (with the typed text as
  /// its caption).
  Future<void> _attach() async {
    final kind = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.photo_library_rounded),
              title: const Text('Photo or video'),
              subtitle: const Text('Photos up to 10 MB (resized, hidden data like location removed), videos up to 20 MB'),
              onTap: () => Navigator.pop(sheet, 'visual'),
            ),
            ListTile(
              leading: const Icon(Icons.attach_file_rounded),
              title: const Text('File'),
              subtitle: const Text('Up to 50 MB'),
              onTap: () => Navigator.pop(sheet, 'file'),
            ),
          ],
        ),
      ),
    );
    if (kind == null) return;
    final picked = await Background.pickMedia(kind);
    if (picked == null || !mounted) return;
    final path = picked['path']! as String;
    final caption = _input.text.trim();
    final replyTo = _replyTo;
    HapticFeedback.lightImpact();
    setState(() {
      _input.clear();
      _replyTo = null;
    });
    try {
      await mediaSend(
        chatId: widget.card.id,
        path: path,
        kind: switch (picked['kind']) {
          'image' => MediaKindView.image,
          'video' => MediaKindView.video,
          'audio' => MediaKindView.audio,
          _ => MediaKindView.file,
        },
        mime: picked['mime']! as String,
        name: picked['name']! as String,
        caption: caption,
        thumb: picked['thumb']! as Uint8List,
        width: picked['width']! as int,
        height: picked['height']! as int,
        durationMs: picked['durationMs']! as int,
        replyTo: replyTo?.seq,
      );
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('Couldn\'t send: $e')));
      }
    } finally {
      // Sanctum keeps its own encrypted copy.
      try {
        File(path).deleteSync();
      } catch (_) {}
    }
    await _reload();
  }

  Future<void> _recordBegin() async {
    if (!await Background.requestMicrophone()) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('Allow the microphone to record voice notes.')));
      }
      return;
    }
    if (!await Background.recordStart()) return;
    HapticFeedback.mediumImpact();
    setState(() {
      _recording = true;
      _cancelArmed = false;
      _recordStart = DateTime.now();
    });
    _recordTick = Timer.periodic(const Duration(milliseconds: 100), (_) async {
      final level = await Background.recordLevel();
      if (mounted && _recording) setState(() => _level = (level / 20000).clamp(0.0, 1.0));
    });
  }

  Future<void> _recordEnd() async {
    _recordTick?.cancel();
    if (!_recording) return;
    final cancel = _cancelArmed;
    final replyTo = _replyTo;
    setState(() {
      _recording = false;
      _level = 0;
      if (!cancel) _replyTo = null;
    });
    if (cancel) {
      HapticFeedback.selectionClick();
      await Background.recordCancel();
      return;
    }
    final note = await Background.recordStop();
    if (note == null) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('Hold the mic to record a voice note.')));
      }
      return;
    }
    HapticFeedback.lightImpact();
    final path = note['path']! as String;
    try {
      await mediaSend(
        chatId: widget.card.id,
        path: path,
        kind: MediaKindView.audio,
        mime: 'audio/mp4',
        name: 'Voice note.m4a',
        caption: '',
        thumb: Uint8List(0),
        width: 0,
        height: 0,
        durationMs: note['durationMs']! as int,
        replyTo: replyTo?.seq,
      );
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('Couldn\'t send: $e')));
    } finally {
      try {
        File(path).deleteSync();
      } catch (_) {}
    }
    await _reload();
  }

  /// While recording: the time, a pulse with the voice, slide to cancel.
  Widget _recordingBar(Color accent) {
    final elapsed = DateTime.now().difference(_recordStart);
    return Container(
      height: 50,
      padding: const EdgeInsets.symmetric(horizontal: Space.lg),
      decoration: BoxDecoration(
        color: Palette.surfaceHigh,
        borderRadius: BorderRadius.circular(Radii.xl),
        border: Border.all(color: _cancelArmed ? Palette.danger : accent.withValues(alpha: 0.6)),
      ),
      child: Row(
        children: [
          AnimatedContainer(
            duration: const Duration(milliseconds: 100),
            width: 10 + 10 * _level,
            height: 10 + 10 * _level,
            decoration: const BoxDecoration(color: Palette.danger, shape: BoxShape.circle),
          ),
          const SizedBox(width: Space.md),
          Text(formatDuration(elapsed.inMilliseconds), style: AppText.bodyStrong),
          const Spacer(),
          Text(
            _cancelArmed ? 'Release to cancel' : '‹ Slide to cancel',
            style: AppText.caption.copyWith(color: _cancelArmed ? Palette.danger : null),
          ),
        ],
      ),
    );
  }

  Widget _composerRow(Color accent) {
    // Voice notes and attachments live on disk, so temp chats (memory
    // only) have neither.
    final voice = !widget.card.temp && !_hasText;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        if (!widget.card.temp && !_recording)
          IconButton(
            tooltip: 'Attach',
            onPressed: _attach,
            icon: const Icon(Icons.add_circle_outline_rounded),
            color: Palette.textMuted,
          ),
        if (_recording)
          Expanded(child: _recordingBar(accent))
        else
          Expanded(
            child: TextField(
              controller: _input,
              focusNode: _focus,
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
            child: voice
                ? GestureDetector(
                    onLongPressStart: (_) => _recordBegin(),
                    onLongPressMoveUpdate: (d) {
                      final armed = d.offsetFromOrigin.dx < -80;
                      if (armed != _cancelArmed) {
                        HapticFeedback.selectionClick();
                        setState(() => _cancelArmed = armed);
                      }
                    },
                    onLongPressEnd: (_) => _recordEnd(),
                    onTap: () => ScaffoldMessenger.of(context)
                      ..hideCurrentSnackBar()
                      ..showSnackBar(const SnackBar(content: Text('Hold to record a voice note.'))),
                    child: AnimatedScale(
                      scale: _recording ? 1.25 : 1,
                      duration: Motion.fast,
                      child: Icon(
                        Icons.mic_rounded,
                        semanticLabel: 'Hold to record a voice note',
                        color: _recording ? accent : Palette.textMuted,
                      ),
                    ),
                  )
                : IconButton(
                    tooltip: 'Send',
                    icon:
                        Icon(Icons.arrow_upward_rounded, color: _hasText ? Palette.onColor(accent) : Palette.textFaint),
                    onPressed: _hasText ? _send : null,
                  ),
          ),
        ),
      ],
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
  const _Header({required this.card, required this.onBack, this.onWipe, this.onInfo});

  final ChatCard card;
  final VoidCallback onBack;
  final VoidCallback? onWipe;
  final VoidCallback? onInfo;

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
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: onInfo,
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
          ),
          if (onInfo != null)
            IconButton(tooltip: 'Group info', icon: const Icon(Icons.groups_rounded, size: 20), onPressed: onInfo),
          _ShotsButton(chatKey: card.temp ? Notifier.tempKey(card.id) : Notifier.chatKey(card.id)),
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
                SelectableText(number,
                    style: AppText.mono.copyWith(fontSize: 26, letterSpacing: 3, color: card.sigil.accent.color)),
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
    this.highlight = false,
    this.onQuoteTap,
    this.onLongPress,
    required this.chatId,
  });

  final ChatMessage message;

  /// For loading the message's attachment.
  final int chatId;
  final Color accent;
  final bool showTime;

  /// Sent / delivered marks on our messages to friends.
  final bool showTicks;

  /// Not delivered and they're offline: it waits on this phone.
  final bool friendOnline;
  final bool animate;

  /// Briefly lit up: a quote pointing at it was tapped.
  final bool highlight;
  final VoidCallback? onQuoteTap;
  final VoidCallback? onLongPress;

  @override
  State<_Bubble> createState() => _BubbleState();
}

class _BubbleState extends State<_Bubble> with SingleTickerProviderStateMixin {
  late final _c = AnimationController(vsync: this, duration: Motion.medium, value: widget.animate ? 0 : 1)..forward();
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
          if (m.senderName != null)
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: Text(m.senderName!, style: AppText.micro.copyWith(color: accent, fontWeight: FontWeight.w700)),
            ),
          if (m.reply != null)
            GestureDetector(
              onTap: widget.onQuoteTap,
              child: Padding(
                padding: const EdgeInsets.only(bottom: 6),
                child: _QuoteBlock(
                  author: m.reply!.author,
                  snippet: m.reply!.snippet,
                  color: mine ? fg : accent,
                  fg: fg,
                ),
              ),
            ),
          if (m.media != null)
            Padding(
              padding: EdgeInsets.only(bottom: m.body.isEmpty ? 0 : Space.xs),
              child: MediaContent(media: m.media!, chatId: widget.chatId, seq: m.seq, fg: fg),
            ),
          if (m.body.isNotEmpty || m.media == null) Text(m.body, style: AppText.body.copyWith(color: fg)),
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
      child: GestureDetector(
        onLongPress: widget.onLongPress,
        child: AnimatedScale(
          scale: widget.highlight ? 1.04 : 1,
          duration: Motion.medium,
          curve: Motion.emphasized,
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
        ),
      ),
    );
  }
}

/// The quoted message inside a reply (and above the composer).
class _QuoteBlock extends StatelessWidget {
  const _QuoteBlock({required this.author, required this.snippet, required this.color, required this.fg, this.label});

  final String author;
  final String snippet;
  final Color color;
  final Color fg;
  final String? label;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.fromLTRB(10, 6, 10, 6),
      decoration: BoxDecoration(
        color: fg.withValues(alpha: 0.08),
        borderRadius: BorderRadius.circular(Radii.sm),
        border: Border(left: BorderSide(color: color, width: 3)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            label == null ? author : '$label $author',
            style: AppText.micro.copyWith(color: color, fontWeight: FontWeight.w700),
          ),
          const SizedBox(height: 1),
          Text(snippet,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: AppText.caption.copyWith(color: fg.withValues(alpha: 0.75))),
        ],
      ),
    );
  }
}

/// Swipe a message right to answer it, as in WhatsApp or Signal.
class _SwipeToReply extends StatefulWidget {
  const _SwipeToReply({required this.child, required this.onReply, required this.fromMe});

  final Widget child;
  final VoidCallback onReply;
  final bool fromMe;

  @override
  State<_SwipeToReply> createState() => _SwipeToReplyState();
}

class _SwipeToReplyState extends State<_SwipeToReply> with SingleTickerProviderStateMixin {
  static const _trigger = 64.0;
  late final AnimationController _back = AnimationController(vsync: this, duration: Motion.fast)
    ..addListener(() => setState(() => _dx = _from * (1 - Curves.easeOut.transform(_back.value))));
  double _dx = 0;
  double _from = 0;
  bool _armed = false;

  @override
  void dispose() {
    _back.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final t = (_dx / _trigger).clamp(0.0, 1.0);
    return GestureDetector(
      onHorizontalDragUpdate: (d) {
        _back.stop();
        setState(() => _dx = (_dx + d.delta.dx).clamp(0.0, _trigger * 1.4));
        if (!_armed && _dx >= _trigger) {
          _armed = true;
          HapticFeedback.selectionClick();
        } else if (_armed && _dx < _trigger) {
          _armed = false;
        }
      },
      onHorizontalDragEnd: (_) {
        if (_armed) widget.onReply();
        _armed = false;
        _from = _dx;
        _back.forward(from: 0);
      },
      child: Stack(
        alignment: Alignment.centerLeft,
        children: [
          Positioned(
            left: 4,
            child: Opacity(
              opacity: t,
              child: Transform.scale(
                  scale: 0.6 + 0.4 * t, child: const Icon(Icons.reply_rounded, color: Palette.textMuted)),
            ),
          ),
          Transform.translate(offset: Offset(_dx, 0), child: widget.child),
        ],
      ),
    );
  }
}

/// Screenshots in this chat: blocked (default) or allowed after biometric
/// confirmation.
class _ShotsButton extends StatefulWidget {
  const _ShotsButton({required this.chatKey});

  final String chatKey;

  @override
  State<_ShotsButton> createState() => _ShotsButtonState();
}

class _ShotsButtonState extends State<_ShotsButton> {
  bool _allowed = false;

  @override
  void initState() {
    super.initState();
    Screenshots.allowed(widget.chatKey).then((v) {
      if (mounted) setState(() => _allowed = v);
    });
  }

  Future<void> _toggle() async {
    HapticFeedback.selectionClick();
    final allowed = await Screenshots.set(widget.chatKey, !_allowed);
    if (!mounted) return;
    setState(() => _allowed = allowed);
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(
        content: Text(allowed
            ? 'Screenshots allowed in this chat (only while it\'s open).'
            : 'Screenshots blocked in this chat.'),
      ));
  }

  @override
  Widget build(BuildContext context) {
    return IconButton(
      tooltip: _allowed ? 'Screenshots allowed' : 'Screenshots blocked',
      onPressed: _toggle,
      icon: AnimatedSwitcher(
        duration: Motion.fast,
        child: Icon(
          _allowed ? Icons.screenshot_rounded : Icons.no_photography_outlined,
          key: ValueKey(_allowed),
          size: 20,
          color: _allowed ? const Color(0xFFFFC145) : Palette.textFaint,
        ),
      ),
    );
  }
}

/// "↳ 3 replies" under a message: opens its thread.
class _ThreadChip extends StatelessWidget {
  const _ThreadChip({required this.count, required this.accent, required this.onTap});

  final int count;
  final Color accent;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: Space.sm),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(Radii.sm),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Space.sm, vertical: Space.xs),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.subdirectory_arrow_right_rounded, size: 16, color: accent),
              const SizedBox(width: Space.xs),
              Text(
                count == 1 ? '1 reply' : '$count replies',
                style: AppText.micro.copyWith(color: accent, fontWeight: FontWeight.w700),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ThreadSheet extends StatefulWidget {
  const _ThreadSheet({required this.root, required this.card, required this.accent, required this.online});

  final ChatMessage root;
  final ChatCard card;
  final Color accent;
  final bool online;

  @override
  State<_ThreadSheet> createState() => _ThreadSheetState();
}

class _ThreadSheetState extends State<_ThreadSheet> {
  final _input = TextEditingController();
  List<ChatMessage> _thread = const [];
  StreamSubscription<AppEvent>? _events;

  @override
  void initState() {
    super.initState();
    _load();
    _events = NetBus.instance.events.listen((e) {
      if (e.kind == AppEventKind.chat && e.id.toInt() == widget.card.id) _load();
    });
  }

  @override
  void dispose() {
    _events?.cancel();
    _input.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final all = await widget.card.source.load();
    // The root, then (in order) anything answering a message in the thread.
    final inThread = <int>{widget.root.seq!};
    final thread = <ChatMessage>[];
    for (final m in all) {
      if (m.seq == widget.root.seq) {
        thread.add(m);
      } else if (m.reply?.seq != null && inThread.contains(m.reply!.seq)) {
        inThread.add(m.seq!);
        thread.add(m);
      }
    }
    if (mounted) setState(() => _thread = thread);
  }

  Future<void> _send() async {
    final body = _input.text.trim();
    if (body.isEmpty) return;
    HapticFeedback.lightImpact();
    _input.clear();
    await widget.card.source.send(body, replyTo: widget.root.seq);
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: MediaQuery.sizeOf(context).height * 0.8,
      child: Column(
        children: [
          const Text('THREAD', style: AppText.overline),
          Expanded(
            child: ListView(
              padding: const EdgeInsets.all(Space.md),
              children: [
                for (final (i, m) in _thread.indexed) ...[
                  _Bubble(
                    key: ValueKey('thread-${m.seq}'),
                    message: m,
                    chatId: widget.card.id,
                    accent: widget.accent,
                    showTime: true,
                    showTicks: widget.card.friend,
                    friendOnline: widget.online,
                    animate: false,
                  ),
                  if (i == 0 && _thread.length > 1) const Divider(height: Space.xl),
                ],
              ],
            ),
          ),
          Padding(
            padding:
                EdgeInsets.fromLTRB(Space.md, Space.sm, Space.md, Space.md + MediaQuery.viewInsetsOf(context).bottom),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _input,
                    minLines: 1,
                    maxLines: 4,
                    textCapitalization: TextCapitalization.sentences,
                    onSubmitted: (_) => _send(),
                    decoration: InputDecoration(
                      hintText: 'Reply in thread',
                      border: OutlineInputBorder(borderRadius: BorderRadius.circular(Radii.xl)),
                    ),
                  ),
                ),
                const SizedBox(width: Space.sm),
                IconButton.filled(
                  tooltip: 'Send',
                  onPressed: _send,
                  icon: const Icon(Icons.arrow_upward_rounded),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
