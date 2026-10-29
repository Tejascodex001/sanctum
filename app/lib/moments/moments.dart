import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../app_visible.dart';
import '../net/net_bus.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/moment.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';
import '../sigil/sigil_look.dart';

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);

String _ago(DateTime t) {
  final d = DateTime.now().difference(t);
  if (d.inMinutes < 1) return 'just now';
  if (d.inHours < 1) return '${d.inMinutes}m ago';
  return '${d.inHours}h ago';
}

/// Friends' chats whose moment we haven't seen yet (their emblem gets a
/// ring in the list).
final unseenMoments = ValueNotifier<Set<int>>(const {});

/// Today's moments, above the chats: yours first, then friends'. A ring
/// marks the ones you haven't seen.
class MomentsStrip extends StatefulWidget {
  const MomentsStrip({super.key, required this.mySigil, required this.sigil});

  final SigilSpec mySigil;

  /// A chat's sigil from its key (cached by the home screen).
  final Future<SigilSpec> Function(List<int> key) sigil;

  @override
  State<MomentsStrip> createState() => _MomentsStripState();
}

class _MomentsStripState extends State<MomentsStrip> {
  List<MomentView> _moments = const [];
  StreamSubscription<AppEvent>? _events;
  Timer? _tick;

  @override
  void initState() {
    super.initState();
    _load();
    _events = NetBus.instance.events.where((e) => e.kind == AppEventKind.moments).listen((_) => _load());
    // Moments expire on their own; look again now and then.
    _tick = Timer.periodic(const Duration(minutes: 5), (_) {
      if (appVisible) _load();
    });
  }

  @override
  void dispose() {
    _events?.cancel();
    _tick?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final list = await momentList();
      unseenMoments.value = {
        for (final m in list)
          if (!m.mine && !m.seen && !m.spent && m.chatId != null) m.chatId!,
      };
      if (mounted) setState(() => _moments = list);
    } catch (_) {}
  }

  MomentView? get _mine => _moments.where((m) => m.mine).firstOrNull;

  Future<void> _share() async {
    final source = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
              child: Text(
                'One photo a day for all your friends. It disappears after 24 hours, and a new one replaces it.',
                style: AppText.caption,
              ),
            ),
            ListTile(
              leading: const Icon(Icons.photo_camera_rounded),
              title: const Text('Take a photo'),
              onTap: () => Navigator.pop(sheet, 'camera'),
            ),
            ListTile(
              leading: const Icon(Icons.photo_library_rounded),
              title: const Text('Choose a photo'),
              onTap: () => Navigator.pop(sheet, 'gallery'),
            ),
          ],
        ),
      ),
    );
    if (source == null) return;
    final picked = source == 'camera' ? await Background.capturePhoto() : await Background.pickMedia('image');
    if (picked == null || !mounted) return;
    final path = picked['path']! as String;
    try {
      if (picked['kind'] != 'image') {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Moments are photos only.')));
        return;
      }
      final choice = await showDialog<MomentChoice>(
        context: context,
        builder: (_) => _CaptionDialog(path: path),
      );
      if (choice == null) return;
      await momentShare(
        path: path,
        mime: picked['mime']! as String,
        name: picked['name']! as String,
        caption: choice.caption,
        seconds: choice.seconds,
        once: choice.once,
        thumb: picked['thumb']! as Uint8List,
        width: picked['width']! as int,
        height: picked['height']! as int,
      );
      HapticFeedback.lightImpact();
      await _load();
    } catch (_) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('Couldn\'t share that. Please try again.')));
      }
    } finally {
      try {
        File(path).deleteSync();
      } catch (_) {}
    }
  }

  void _alreadyOpened() {
    HapticFeedback.selectionClick();
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(const SnackBar(content: Text('You\'ve opened this one. It could only be seen once.')));
  }

  Future<void> _view(MomentView start) async {
    final shown = _moments.where((m) => !m.spent).toList();
    final index = shown.indexOf(start);
    await Navigator.of(context).push(PageRouteBuilder<void>(
      opaque: true,
      transitionDuration: Motion.medium,
      pageBuilder: (_, a, __) => FadeTransition(
        opacity: a,
        child: MomentViewer(
          moments: shown,
          initial: index,
          mySigil: widget.mySigil,
          sigil: widget.sigil,
          onReplace: _share,
        ),
      ),
    ));
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final mine = _mine;
    final others = _moments.where((m) => !m.mine).toList();
    return SizedBox(
      height: 92,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: Space.lg),
        children: [
          _Bubble(
            label: mine == null ? 'Your moment' : 'You',
            sigil: widget.mySigil,
            ring: mine == null ? null : widget.mySigil.look.accent.color,
            add: mine == null,
            thumb: mine?.thumb,
            onTap: mine == null ? _share : () => _view(mine),
          ),
          for (final m in others)
            FutureBuilder<SigilSpec>(
              future: widget.sigil(m.sigilKey),
              builder: (context, snap) => _Bubble(
                label: m.name,
                sigil: snap.data,
                ring: m.seen ? Palette.outlineStrong : (snap.data?.look.accent.color ?? Palette.text),
                // A view-once moment already opened: nothing left to show.
                thumb: m.spent ? null : m.thumb,
                faded: m.spent,
                onTap: m.spent ? _alreadyOpened : () => _view(m),
              ),
            ),
          if (others.isEmpty)
            Center(
              child: Padding(
                padding: const EdgeInsets.only(left: Space.sm, bottom: 18),
                child: Text(
                  mine == null ? 'Share a photo of your day' : 'Friends\' moments show up here',
                  style: AppText.caption,
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// One person in the strip: their photo's preview (or sigil) in a ring.
class _Bubble extends StatelessWidget {
  const _Bubble({
    required this.label,
    this.sigil,
    this.ring,
    this.add = false,
    this.thumb,
    this.faded = false,
    required this.onTap,
  });

  final String label;
  final SigilSpec? sigil;
  final Color? ring;
  final bool add;
  final Uint8List? thumb;

  /// Opened already (view once): shown dimmed.
  final bool faded;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final face = thumb != null && thumb!.isNotEmpty
        ? Image.memory(thumb!, fit: BoxFit.cover, gaplessPlayback: true)
        : sigil == null
            ? ColoredBox(color: Palette.surfaceHigh)
            : SigilView(spec: sigil!);
    return GestureDetector(
      onTap: onTap,
      child: Opacity(
        opacity: faded ? 0.45 : 1,
        child: SizedBox(
          width: 72,
          child: Column(
            children: [
              Stack(
                clipBehavior: Clip.none,
                children: [
                  Container(
                    width: 62,
                    height: 62,
                    padding: const EdgeInsets.all(3),
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      border: Border.all(color: ring ?? Palette.outline, width: ring == null ? 1 : 2.2),
                    ),
                    child: ClipOval(child: face),
                  ),
                  if (add)
                    Positioned(
                      right: -2,
                      bottom: -2,
                      child: Container(
                        padding: const EdgeInsets.all(2),
                        decoration: BoxDecoration(color: Palette.bg, shape: BoxShape.circle),
                        child: Container(
                          decoration:
                              BoxDecoration(color: sigil?.look.accent.color ?? Palette.text, shape: BoxShape.circle),
                          child: Icon(Icons.add_rounded,
                              size: 18, color: Palette.onColor(sigil?.look.accent.color ?? Palette.text)),
                        ),
                      ),
                    ),
                ],
              ),
              const SizedBox(height: Space.xs),
              Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.micro),
            ],
          ),
        ),
      ),
    );
  }
}

/// What sharing a moment asks: a caption, and how friends may look at it.
typedef MomentChoice = ({String caption, int seconds, bool once});

class _CaptionDialog extends StatefulWidget {
  const _CaptionDialog({required this.path});

  final String path;

  @override
  State<_CaptionDialog> createState() => _CaptionDialogState();
}

class _CaptionDialogState extends State<_CaptionDialog> {
  final _caption = TextEditingController();
  int _seconds = 0;
  bool _once = false;

  @override
  void dispose() {
    _caption.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Today\'s moment'),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          ClipRRect(
            borderRadius: BorderRadius.circular(Radii.md),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: 280),
              child: Image.file(File(widget.path), fit: BoxFit.cover, cacheWidth: 800),
            ),
          ),
          const SizedBox(height: Space.md),
          TextField(
            controller: _caption,
            maxLength: 200,
            textCapitalization: TextCapitalization.sentences,
            decoration: const InputDecoration(hintText: 'Add a caption (optional)'),
          ),
          Align(
            alignment: Alignment.centerLeft,
            child: Text('Each look lasts', style: AppText.caption),
          ),
          const SizedBox(height: Space.xs),
          Wrap(
            spacing: Space.sm,
            children: [
              for (final (s, label) in const [(0, 'No limit'), (5, '5 s'), (10, '10 s'), (15, '15 s')])
                ChoiceChip(
                  label: Text(label),
                  selected: _seconds == s,
                  onSelected: (_) => setState(() => _seconds = s),
                ),
            ],
          ),
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            value: _once,
            onChanged: (v) => setState(() => _once = v),
            title: const Text('View once'),
            subtitle: Text('Each friend can open it one time.', style: AppText.micro),
          ),
          Text('All your friends will see it for 24 hours.', style: AppText.micro),
        ],
      ),
      scrollable: true,
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(
          onPressed: () => Navigator.pop<MomentChoice>(
            context,
            (caption: _caption.text.trim(), seconds: _seconds, once: _once),
          ),
          child: const Text('Share'),
        ),
      ],
    );
  }
}

/// Moments full screen; swipe between them.
class MomentViewer extends StatefulWidget {
  const MomentViewer({
    super.key,
    required this.moments,
    required this.initial,
    required this.mySigil,
    required this.sigil,
    required this.onReplace,
  });

  final List<MomentView> moments;
  final int initial;
  final SigilSpec mySigil;
  final Future<SigilSpec> Function(List<int> key) sigil;
  final Future<void> Function() onReplace;

  @override
  State<MomentViewer> createState() => _MomentViewerState();
}

class _MomentViewerState extends State<MomentViewer> with SingleTickerProviderStateMixin {
  late final _pages = PageController(initialPage: widget.initial);

  /// Runs while a friend's moment with a time limit is on screen.
  late final _clock = AnimationController(vsync: this)..addStatusListener(_timeUp);

  /// View-once moments opened here: their photos are deleted once we move on.
  final _opened = <String>{};

  /// View-once moments already moved on from (swiping back shows nothing).
  final _spent = <String>{};
  int _index = 0;
  bool? _viewsShared;

  static String _key(MomentView m) => m.id.join(',');

  @override
  void initState() {
    super.initState();
    _index = widget.initial;
    momentViewsShared().then((on) {
      if (mounted) setState(() => _viewsShared = on);
    }, onError: (_) {});
    _show(widget.initial);
  }

  @override
  void dispose() {
    for (final m in widget.moments) {
      if (_opened.contains(_key(m))) momentSpent(id: m.id);
    }
    _clock.dispose();
    _pages.dispose();
    super.dispose();
  }

  void _show(int i) {
    final m = widget.moments[i];
    if (!m.seen) momentSeen(id: m.id);
    final theirs = !m.mine && m.ready && !_spent.contains(_key(m));
    if (theirs && m.once) _opened.add(_key(m));
    _clock.stop();
    _clock.value = 0;
    if (theirs && m.seconds > 0) {
      _clock.duration = Duration(seconds: m.seconds);
      _clock.forward();
    }
  }

  void _turned(int i) {
    // Leaving a view-once moment ends the look.
    final left = widget.moments[_index];
    if (_opened.remove(_key(left))) {
      _spent.add(_key(left));
      momentSpent(id: left.id);
    }
    setState(() => _index = i);
    _show(i);
  }

  void _timeUp(AnimationStatus status) {
    if (status != AnimationStatus.completed || !mounted) return;
    if (_index + 1 < widget.moments.length) {
      _pages.nextPage(duration: Motion.medium, curve: Motion.emphasized);
    } else {
      Navigator.pop(context);
    }
  }

  Future<void> _delete() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Delete your moment?'),
        content: const Text('It disappears from your friends\' phones too.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(dialog, false), child: const Text('Cancel')),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: Palette.danger),
            onPressed: () => Navigator.pop(dialog, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    await momentDelete();
    if (mounted) Navigator.pop(context);
  }

  Future<void> _viewers(MomentView m) async {
    HapticFeedback.selectionClick();
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text('Seen by', style: AppText.heading),
              const SizedBox(height: Space.sm),
              if (_viewsShared == false)
                Text(
                  'Turn on "Show who saw moments" in Settings to see who looked. Friends then see when you look at '
                  'theirs too.',
                  style: AppText.caption,
                )
              else if (m.viewers.isEmpty)
                Text('No one yet.', style: AppText.caption)
              else
                ConstrainedBox(
                  constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.5),
                  child: ListView(
                    shrinkWrap: true,
                    children: [
                      for (final v in m.viewers)
                        ListTile(
                          contentPadding: EdgeInsets.zero,
                          leading: SizedBox.square(
                            dimension: 36,
                            child: FutureBuilder<SigilSpec>(
                              future: widget.sigil(v.sigilKey),
                              builder: (context, snap) =>
                                  snap.data == null ? const SizedBox() : ClipOval(child: SigilView(spec: snap.data!)),
                            ),
                          ),
                          title: Text(v.name),
                          trailing: Text(_ago(_fromUnix(v.at)), style: AppText.micro),
                        ),
                    ],
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      body: PageView.builder(
        controller: _pages,
        itemCount: widget.moments.length,
        onPageChanged: _turned,
        itemBuilder: (context, i) {
          final m = widget.moments[i];
          final timed = !m.mine && m.seconds > 0;
          return GestureDetector(
            // Hold to pause the clock, as in other apps.
            onLongPressStart: timed ? (_) => _clock.stop() : null,
            onLongPressEnd: timed && i == _index ? (_) => _clock.forward() : null,
            child: _MomentPage(
              moment: m,
              gone: _spent.contains(_key(m)),
              clock: timed && i == _index ? _clock : null,
              sigil: m.mine ? Future.value(widget.mySigil) : widget.sigil(m.sigilKey),
              onDelete: m.mine ? _delete : null,
              onViewers: m.mine ? () => _viewers(m) : null,
              onReplace: m.mine
                  ? () async {
                      Navigator.pop(context);
                      await widget.onReplace();
                    }
                  : null,
            ),
          );
        },
      ),
    );
  }
}

/// How a moment may be looked at, in a few words ("10 s · view once").
String _rules(MomentView m) => [
      if (m.seconds > 0) '${m.seconds} s',
      if (m.once) 'view once',
    ].join(' · ');

class _MomentPage extends StatelessWidget {
  const _MomentPage({
    required this.moment,
    required this.sigil,
    this.gone = false,
    this.clock,
    this.onDelete,
    this.onReplace,
    this.onViewers,
  });

  final MomentView moment;
  final Future<SigilSpec> sigil;

  /// A view-once moment we've moved on from.
  final bool gone;

  /// Time left for this look, if it has a limit.
  final Animation<double>? clock;
  final VoidCallback? onDelete;
  final VoidCallback? onReplace;
  final VoidCallback? onViewers;

  @override
  Widget build(BuildContext context) {
    final m = moment;
    final left = _fromUnix(m.expiresAt).difference(DateTime.now());
    final rules = _rules(m);
    return Stack(
      fit: StackFit.expand,
      children: [
        if (gone)
          Center(
            child: Text('Opened. It could only be seen once.', style: AppText.body.copyWith(color: Colors.white70)),
          )
        else if (m.ready)
          FutureBuilder<Uint8List?>(
            future: momentBytes(id: m.id),
            builder: (context, snap) => snap.data == null
                ? const Center(child: CircularProgressIndicator())
                : InteractiveViewer(maxScale: 5, child: Center(child: Image.memory(snap.data!))),
          )
        else
          Stack(
            fit: StackFit.expand,
            children: [
              if (m.thumb.isNotEmpty) Image.memory(m.thumb, fit: BoxFit.cover),
              const Center(child: CircularProgressIndicator()),
            ],
          ),
        // Header and caption, over a soft gradient for legibility.
        Positioned(
          left: 0,
          right: 0,
          top: 0,
          child: DecoratedBox(
            decoration: const BoxDecoration(
              gradient: LinearGradient(
                begin: Alignment.topCenter,
                end: Alignment.bottomCenter,
                colors: [Colors.black87, Colors.transparent],
              ),
            ),
            child: SafeArea(
              bottom: false,
              child: Column(
                children: [
                  if (clock != null)
                    Padding(
                      padding: const EdgeInsets.fromLTRB(Space.md, Space.xs, Space.md, 0),
                      child: AnimatedBuilder(
                        animation: clock!,
                        builder: (context, _) => ClipRRect(
                          borderRadius: BorderRadius.circular(2),
                          child: LinearProgressIndicator(
                            value: 1 - clock!.value,
                            minHeight: 3,
                            color: Colors.white,
                            backgroundColor: Colors.white24,
                          ),
                        ),
                      ),
                    ),
                  Padding(
                    padding: const EdgeInsets.fromLTRB(Space.xs, Space.xs, Space.sm, Space.xl),
                    child: Row(
                      children: [
                        IconButton(
                          tooltip: 'Close',
                          color: Colors.white,
                          icon: const Icon(Icons.close_rounded),
                          onPressed: () => Navigator.pop(context),
                        ),
                        SizedBox.square(
                          dimension: 34,
                          child: FutureBuilder<SigilSpec>(
                            future: sigil,
                            builder: (context, snap) =>
                                snap.data == null ? const SizedBox() : ClipOval(child: SigilView(spec: snap.data!)),
                          ),
                        ),
                        const SizedBox(width: Space.sm),
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(m.name, style: AppText.bodyStrong.copyWith(color: Colors.white)),
                              Text(
                                [
                                  _ago(_fromUnix(m.sentAt)),
                                  'gone in ${left.inHours > 0 ? '${left.inHours}h' : '${left.inMinutes.clamp(1, 59)}m'}',
                                  if (rules.isNotEmpty) rules,
                                ].join(' · '),
                                style: AppText.micro.copyWith(color: Colors.white70),
                              ),
                            ],
                          ),
                        ),
                        if (onReplace != null)
                          IconButton(
                            tooltip: 'Share a new one',
                            color: Colors.white,
                            icon: const Icon(Icons.add_a_photo_outlined),
                            onPressed: onReplace,
                          ),
                        if (onDelete != null)
                          IconButton(
                            tooltip: 'Delete',
                            color: Colors.white,
                            icon: const Icon(Icons.delete_outline_rounded),
                            onPressed: onDelete,
                          ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
        if (m.caption.isNotEmpty || onViewers != null)
          Positioned(
            left: 0,
            right: 0,
            bottom: 0,
            child: DecoratedBox(
              decoration: const BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.bottomCenter,
                  end: Alignment.topCenter,
                  colors: [Colors.black87, Colors.transparent],
                ),
              ),
              child: SafeArea(
                top: false,
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(Space.xl, Space.xxl, Space.xl, Space.xl),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (m.caption.isNotEmpty) Text(m.caption, style: AppText.body.copyWith(color: Colors.white)),
                      if (onViewers != null) ...[
                        if (m.caption.isNotEmpty) const SizedBox(height: Space.md),
                        TextButton.icon(
                          onPressed: onViewers,
                          style: TextButton.styleFrom(foregroundColor: Colors.white, padding: EdgeInsets.zero),
                          icon: const Icon(Icons.visibility_outlined, size: 18),
                          label: Text(m.viewers.isEmpty ? 'Seen by' : 'Seen by ${m.viewers.length}'),
                        ),
                      ],
                    ],
                  ),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
