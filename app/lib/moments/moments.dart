import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../net/net_bus.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/moment.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';

DateTime _fromUnix(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);

String _ago(DateTime t) {
  final d = DateTime.now().difference(t);
  if (d.inMinutes < 1) return 'just now';
  if (d.inHours < 1) return '${d.inMinutes}m ago';
  return '${d.inHours}h ago';
}

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
    _tick = Timer.periodic(const Duration(minutes: 5), (_) => _load());
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
            const Padding(
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
      final caption = await showDialog<String>(
        context: context,
        builder: (_) => _CaptionDialog(path: path),
      );
      if (caption == null) return;
      await momentShare(
        path: path,
        mime: picked['mime']! as String,
        name: picked['name']! as String,
        caption: caption,
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

  Future<void> _view(int index) async {
    await Navigator.of(context).push(PageRouteBuilder<void>(
      opaque: true,
      transitionDuration: Motion.medium,
      pageBuilder: (_, a, __) => FadeTransition(
        opacity: a,
        child: MomentViewer(
          moments: _moments,
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
            ring: mine == null ? null : widget.mySigil.accent.color,
            add: mine == null,
            thumb: mine?.thumb,
            onTap: mine == null ? _share : () => _view(_moments.indexOf(mine)),
          ),
          for (final m in others)
            FutureBuilder<SigilSpec>(
              future: widget.sigil(m.sigilKey),
              builder: (context, snap) => _Bubble(
                label: m.name,
                sigil: snap.data,
                ring: m.seen ? Palette.outlineStrong : (snap.data?.accent.color ?? Palette.text),
                thumb: m.thumb,
                onTap: () => _view(_moments.indexOf(m)),
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
  const _Bubble({required this.label, this.sigil, this.ring, this.add = false, this.thumb, required this.onTap});

  final String label;
  final SigilSpec? sigil;
  final Color? ring;
  final bool add;
  final Uint8List? thumb;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final face = thumb != null && thumb!.isNotEmpty
        ? Image.memory(thumb!, fit: BoxFit.cover, gaplessPlayback: true)
        : sigil == null
            ? const ColoredBox(color: Palette.surfaceHigh)
            : SigilView(spec: sigil!);
    return GestureDetector(
      onTap: onTap,
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
                      decoration: const BoxDecoration(color: Palette.bg, shape: BoxShape.circle),
                      child: Container(
                        decoration: BoxDecoration(color: sigil?.accent.color ?? Palette.text, shape: BoxShape.circle),
                        child: Icon(Icons.add_rounded,
                            size: 18, color: Palette.onColor(sigil?.accent.color ?? Palette.text)),
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
    );
  }
}

class _CaptionDialog extends StatefulWidget {
  const _CaptionDialog({required this.path});

  final String path;

  @override
  State<_CaptionDialog> createState() => _CaptionDialogState();
}

class _CaptionDialogState extends State<_CaptionDialog> {
  final _caption = TextEditingController();

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
          const Text('All your friends will see it for 24 hours.', style: AppText.micro),
        ],
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(onPressed: () => Navigator.pop(context, _caption.text.trim()), child: const Text('Share')),
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

class _MomentViewerState extends State<MomentViewer> {
  late final _pages = PageController(initialPage: widget.initial);

  @override
  void initState() {
    super.initState();
    _seen(widget.initial);
  }

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  void _seen(int i) {
    final m = widget.moments[i];
    if (!m.seen) momentSeen(id: m.id);
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

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      body: PageView.builder(
        controller: _pages,
        itemCount: widget.moments.length,
        onPageChanged: _seen,
        itemBuilder: (context, i) => _MomentPage(
          moment: widget.moments[i],
          sigil: widget.moments[i].mine ? Future.value(widget.mySigil) : widget.sigil(widget.moments[i].sigilKey),
          onDelete: widget.moments[i].mine ? _delete : null,
          onReplace: widget.moments[i].mine
              ? () async {
                  Navigator.pop(context);
                  await widget.onReplace();
                }
              : null,
        ),
      ),
    );
  }
}

class _MomentPage extends StatelessWidget {
  const _MomentPage({required this.moment, required this.sigil, this.onDelete, this.onReplace});

  final MomentView moment;
  final Future<SigilSpec> sigil;
  final VoidCallback? onDelete;
  final VoidCallback? onReplace;

  @override
  Widget build(BuildContext context) {
    final m = moment;
    final left = _fromUnix(m.expiresAt).difference(DateTime.now());
    return Stack(
      fit: StackFit.expand,
      children: [
        m.ready
            ? FutureBuilder<Uint8List?>(
                future: momentBytes(id: m.id),
                builder: (context, snap) => snap.data == null
                    ? const Center(child: CircularProgressIndicator())
                    : InteractiveViewer(maxScale: 5, child: Center(child: Image.memory(snap.data!))),
              )
            : Stack(
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
              child: Padding(
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
                            '${_ago(_fromUnix(m.sentAt))} · gone in ${left.inHours > 0 ? '${left.inHours}h' : '${left.inMinutes.clamp(1, 59)}m'}',
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
            ),
          ),
        ),
        if (m.caption.isNotEmpty)
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
                  child: Text(m.caption, style: AppText.body.copyWith(color: Colors.white)),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
