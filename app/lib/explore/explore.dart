import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player/video_player.dart';

import '../src/rust/api/fedi.dart';
import '../theme/tokens.dart';
import 'share_sheet.dart';

/// Pictures recently fetched through Tor, so swiping back doesn't fetch
/// them again.
class _Pictures {
  static const _max = 40;
  static final _bytes = <String, Uint8List>{};
  static final _pending = <String, Future<Uint8List?>>{};

  static Future<Uint8List?> load(String url) {
    if (url.isEmpty) return Future.value(null);
    final hit = _bytes.remove(url);
    if (hit != null) return Future.value(_bytes[url] = hit);
    return _pending[url] ??= fediImage(url: url).then((b) {
      _pending.remove(url);
      if (b == null) return null;
      if (_bytes.length >= _max) _bytes.remove(_bytes.keys.first);
      return _bytes[url] = b;
    });
  }
}

/// Sound on or off, for every reel.
final _muted = ValueNotifier<bool>(false);

/// Reels: short public videos from the Fediverse (Loops, PeerTube and
/// Mastodon-compatible servers), read anonymously through Tor and picked
/// on this phone for what you like. Swipe up for the next.
class ExplorePage extends StatefulWidget {
  const ExplorePage({super.key, required this.active});

  /// The Reels tab is showing (videos play only then).
  final bool active;

  @override
  State<ExplorePage> createState() => _ExplorePageState();
}

class _ExplorePageState extends State<ExplorePage> with WidgetsBindingObserver {
  List<FediSource> _sources = const [];

  /// Interests to pick before the first reels (null: not loaded yet).
  ReelInterests? _interests;
  var _picking = false;

  /// The app is on screen: reels never play in the background.
  var _awake = true;

  /// Faster reels is on: load two ahead instead of one.
  var _direct = false;
  final _posts = <FediPost>[];
  List<FediSourceProblem> _problems = const [];
  var _loading = false;
  var _started = false;

  /// Nothing more to show for now.
  var _done = false;
  var _index = 0;
  final _pages = PageController();

  @override
  void didUpdateWidget(ExplorePage old) {
    super.didUpdateWidget(old);
    // Nothing is fetched until the tab is first opened.
    if (widget.active && !_started) _restart();
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (widget.active) _restart();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _pages.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final awake = state == AppLifecycleState.resumed;
    if (awake != _awake) setState(() => _awake = awake);
  }

  Future<void> _restart() async {
    _started = true;
    final interests = await reelsInterests();
    final sources = await fediSources();
    final direct = await fediDirect();
    if (!mounted) return;
    setState(() {
      _interests = interests;
      _direct = direct;
      // First time: what do you like watching?
      _picking = !interests.asked;
    });
    if (_picking) return;
    setState(() {
      _sources = sources;
      _posts.clear();
      _problems = const [];
      _done = false;
      _index = 0;
    });
    if (_pages.hasClients) _pages.jumpToPage(0);
    await _more(fresh: true);
  }

  /// The next reels, picked on this phone for what you like.
  Future<void> _more({bool fresh = false}) async {
    if (_loading || (_done && !fresh)) return;
    setState(() => _loading = true);
    try {
      final page = await reelsNext(count: 8, fresh: fresh);
      if (!mounted) return;
      setState(() {
        _problems = page.problems;
        _posts.addAll(page.posts);
        _done = page.posts.isEmpty;
      });
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  void _onPage(int i) {
    setState(() => _index = i);
    if (i >= _posts.length - 3) _more();
  }

  Future<void> _pickInterests(List<String> ids) async {
    await reelsSetInterests(ids: ids);
    if (!mounted) return;
    setState(() => _picking = false);
    await _restart();
  }

  Future<void> _editInterests() async {
    final interests = await reelsInterests();
    if (!mounted) return;
    final ids = await showModalBottomSheet<List<String>>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (sheet) => _InterestPicker(
        items: interests.items,
        dark: false,
        onDone: (ids) => Navigator.pop(sheet, ids),
      ),
    );
    if (ids != null) await _pickInterests(ids);
  }

  Future<void> _editSources() async {
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => const _SourcesSheet(),
    );
    await _restart();
  }

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: Colors.black,
      child: Stack(
        fit: StackFit.expand,
        children: [
          _body(),
          // Over the video, below the status bar.
          Positioned(
            top: 0,
            left: 0,
            right: 0,
            child: DecoratedBox(
              decoration: const BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.topCenter,
                  end: Alignment.bottomCenter,
                  colors: [Colors.black54, Colors.transparent],
                ),
              ),
              child: SafeArea(
                bottom: false,
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(Space.xl, Space.sm, Space.sm, Space.lg),
                  child: Row(
                    children: [
                      Text('Reels', style: AppText.title.copyWith(color: Colors.white)),
                      const Spacer(),
                      if (!_picking)
                        IconButton(
                          tooltip: 'What you like',
                          color: Colors.white,
                          icon: const Icon(Icons.interests_rounded),
                          onPressed: _editInterests,
                        ),
                      IconButton(
                        tooltip: 'Where reels come from',
                        color: Colors.white,
                        icon: const Icon(Icons.tune_rounded),
                        onPressed: _editSources,
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _body() {
    final interests = _interests;
    if (_picking && interests != null) {
      return SafeArea(
        child: Padding(
          padding: const EdgeInsets.only(top: 56),
          child: _InterestPicker(items: interests.items, dark: true, onDone: _pickInterests),
        ),
      );
    }
    if (_posts.isEmpty) {
      if (_loading || !_started) return const Center(child: CircularProgressIndicator());
      return _Nothing(
        problems: {for (final p in _problems) '${p.kind.name}:${p.host}': p.problem},
        sources: _sources,
        onRetry: _restart,
        onServers: _editSources,
      );
    }
    return RefreshIndicator(
      onRefresh: _restart,
      child: PageView.builder(
        controller: _pages,
        scrollDirection: Axis.vertical,
        onPageChanged: _onPage,
        itemCount: _posts.length + (_loading || _done ? 1 : 0),
        itemBuilder: (context, i) => i >= _posts.length
            ? (_loading ? const Center(child: CircularProgressIndicator()) : _CaughtUp(onRefresh: _restart))
            : _Reel(
                key: ValueKey('${_posts[i].host}/${_posts[i].id}'),
                post: _posts[i],
                // Not while another screen covers Reels.
                playing: widget.active && _awake && TickerMode.valuesOf(context).enabled && i == _index,
                // The next ones start loading early (two with Faster reels).
                // Away from the app, the current one just pauses.
                preload: widget.active && (i == _index + 1 || (_direct && i == _index + 2) || (!_awake && i == _index)),
              ),
      ),
    );
  }
}

/// Why there's nothing to show, and what to do about it.
class _Nothing extends StatelessWidget {
  const _Nothing({required this.problems, required this.sources, required this.onRetry, required this.onServers});

  final Map<String, FediProblem> problems;
  final List<FediSource> sources;
  final VoidCallback onRetry;
  final VoidCallback onServers;

  @override
  Widget build(BuildContext context) {
    final offline = problems.values.any((p) => p == FediProblem.offline);
    final lines = <String>[
      if (sources.isEmpty) 'Add a site to watch reels from.',
      if (offline) 'Reels open once Sanctum is connected. This can take a minute after starting.',
      if (!offline)
        for (final s in sources)
          switch (problems['${s.kind.name}:${s.host}']) {
            FediProblem.notPublic => '${s.host} only shows its posts to people signed in there. Try another site.',
            FediProblem.unreachable => 'Couldn\'t reach ${s.host} right now.',
            FediProblem.badHost => '${s.host} isn\'t a site address.',
            _ => '${s.host} has no new short videos.',
          },
      if (!offline && problems.isNotEmpty) 'If reels won\'t load, try Faster reels in Settings.',
    ];
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(Space.xxl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.slow_motion_video_rounded, size: 44, color: Palette.textMuted),
            const SizedBox(height: Space.md),
            for (final l in lines)
              Padding(
                padding: const EdgeInsets.only(bottom: Space.xs),
                child: Text(l, textAlign: TextAlign.center, style: AppText.caption),
              ),
            const SizedBox(height: Space.md),
            Wrap(
              spacing: Space.sm,
              children: [
                OutlinedButton(onPressed: onRetry, child: const Text('Try again')),
                FilledButton.tonal(onPressed: onServers, child: const Text('Sites')),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

/// What do you like watching? Picked before the first reels (and changed
/// from the top bar): Reels looks for these first and sticks to them.
class _InterestPicker extends StatefulWidget {
  const _InterestPicker({required this.items, required this.dark, required this.onDone});

  final List<ReelInterest> items;

  /// Over the black Reels page, rather than in a sheet.
  final bool dark;
  final void Function(List<String> ids) onDone;

  @override
  State<_InterestPicker> createState() => _InterestPickerState();
}

class _InterestPickerState extends State<_InterestPicker> {
  late final _chosen = {
    for (final i in widget.items)
      if (i.chosen) i.id
  };

  @override
  Widget build(BuildContext context) {
    final dark = widget.dark;
    final accent = Theme.of(context).colorScheme.primary;
    final fg = dark ? Colors.white : Palette.text;
    final muted = dark ? Colors.white70 : Palette.textMuted;
    return SafeArea(
      top: false,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(Space.xl, Space.sm, Space.xl, Space.lg),
        child: Column(
          mainAxisSize: dark ? MainAxisSize.max : MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('What do you love watching?', style: AppText.title.copyWith(color: fg)),
            const SizedBox(height: Space.xs),
            Text(
              'Pick a few. Reels looks for these and learns more as you watch. It stays on this phone.',
              style: AppText.caption.copyWith(color: muted),
            ),
            const SizedBox(height: Space.lg),
            Flexible(
              child: SingleChildScrollView(
                child: Wrap(
                  spacing: Space.sm,
                  runSpacing: Space.sm,
                  children: [
                    for (final i in widget.items)
                      _InterestPill(
                        label: '${i.emoji}  ${i.label}',
                        selected: _chosen.contains(i.id),
                        accent: accent,
                        fg: fg,
                        outline: dark ? Colors.white24 : Palette.outlineStrong,
                        onTap: () {
                          HapticFeedback.selectionClick();
                          setState(() => _chosen.contains(i.id) ? _chosen.remove(i.id) : _chosen.add(i.id));
                        },
                      ),
                  ],
                ),
              ),
            ),
            const SizedBox(height: Space.lg),
            FilledButton(
              onPressed: _chosen.isEmpty ? null : () => widget.onDone(_chosen.toList()),
              child: Text(_chosen.isEmpty ? 'Pick at least one' : 'Show my reels'),
            ),
            TextButton(
              onPressed: () => widget.onDone(const []),
              style: TextButton.styleFrom(foregroundColor: muted),
              child: Text(dark ? 'Skip, show me anything' : 'No preference'),
            ),
          ],
        ),
      ),
    );
  }
}

class _InterestPill extends StatelessWidget {
  const _InterestPill({
    required this.label,
    required this.selected,
    required this.accent,
    required this.fg,
    required this.outline,
    required this.onTap,
  });

  final String label;
  final bool selected;
  final Color accent;
  final Color fg;
  final Color outline;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        selected: selected,
        child: GestureDetector(
          onTap: onTap,
          child: AnimatedContainer(
            duration: Motion.fast,
            padding: const EdgeInsets.symmetric(horizontal: Space.lg, vertical: 10),
            decoration: BoxDecoration(
              color: selected ? accent : Colors.transparent,
              borderRadius: BorderRadius.circular(Radii.xl),
              border: Border.all(color: selected ? accent : outline, width: 1.5),
            ),
            child: Text(
              label,
              style: AppText.bodyStrong.copyWith(color: selected ? Palette.onColor(accent) : fg, fontSize: 15),
            ),
          ),
        ),
      );
}

/// The end of what the sites have for now.
class _CaughtUp extends StatelessWidget {
  const _CaughtUp({required this.onRefresh});

  final VoidCallback onRefresh;

  @override
  Widget build(BuildContext context) => Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.check_circle_outline_rounded, color: Colors.white70, size: 44),
            const SizedBox(height: Space.md),
            Text("You're all caught up", style: AppText.heading.copyWith(color: Colors.white)),
            const SizedBox(height: Space.xs),
            Text('Check back later for new reels.', style: AppText.caption.copyWith(color: Colors.white70)),
            const SizedBox(height: Space.lg),
            OutlinedButton(
              onPressed: onRefresh,
              style: OutlinedButton.styleFrom(foregroundColor: Colors.white),
              child: const Text('Look again'),
            ),
          ],
        ),
      );
}

String _count(int n) => n >= 1000000
    ? '${(n / 1000000).toStringAsFixed(1)}M'
    : n >= 1000
        ? '${(n / 1000).toStringAsFixed(1)}K'
        : '$n';

String _ago(int unix) {
  if (unix <= 0) return '';
  final d = DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(unix * 1000));
  if (d.inDays >= 1) return '${d.inDays}d';
  if (d.inHours >= 1) return '${d.inHours}h';
  return '${d.inMinutes.clamp(1, 59)}m';
}

/// One reel, filling the screen: the video, who posted it and what they
/// wrote, with sound and send buttons on the side.
class _Reel extends StatefulWidget {
  const _Reel({super.key, required this.post, required this.playing, required this.preload});

  final FediPost post;
  final bool playing;
  final bool preload;

  @override
  State<_Reel> createState() => _ReelState();
}

class _ReelState extends State<_Reel> with SingleTickerProviderStateMixin {
  var _expanded = false;
  late var _liked = widget.post.liked;

  /// The big heart after a double-tap.
  late final _burst = AnimationController(vsync: this, duration: const Duration(milliseconds: 750));

  @override
  void dispose() {
    _burst.dispose();
    super.dispose();
  }

  void _signal(ReelSignal signal, [double times = 0]) =>
      reelsSignal(host: widget.post.host, id: widget.post.id, signal: signal, times: times).ignore();

  void _setLiked(bool liked) {
    if (liked == _liked) return;
    HapticFeedback.lightImpact();
    setState(() => _liked = liked);
    _signal(liked ? ReelSignal.like : ReelSignal.unlike);
  }

  void _doubleTap() {
    _burst.forward(from: 0);
    _setLiked(true);
  }

  /// How long it was watched, for learning what you like: swiped away
  /// within 2 seconds is a skip.
  void _watched(Duration watched, Duration length) {
    if (watched < const Duration(seconds: 2)) {
      _signal(ReelSignal.skipped);
    } else if (length > Duration.zero) {
      _signal(ReelSignal.watched, watched.inMilliseconds / length.inMilliseconds);
    }
  }

  Future<void> _share() async {
    final text = widget.post.text.split('\n').first;
    final sent = await showShareSheet(
      context,
      text: '${text.length > 80 ? '${text.substring(0, 80)}…' : text}\n${widget.post.link}'.trim(),
    );
    if (sent > 0) _signal(ReelSignal.share);
    if (sent > 0 && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(sent == 1 ? 'Sent' : 'Sent to $sent chats')));
    }
  }

  @override
  Widget build(BuildContext context) {
    final post = widget.post;
    final item = post.media.first;
    const white = TextStyle(color: Colors.white);
    return Stack(
      fit: StackFit.expand,
      children: [
        _Video(
          key: ValueKey(item.videoId.isEmpty ? item.url : item.videoId),
          host: post.host,
          item: item,
          playing: widget.playing,
          preload: widget.preload,
          onDoubleTap: _doubleTap,
          onWatched: _watched,
        ),
        IgnorePointer(
          child: Center(
            child: AnimatedBuilder(
              animation: _burst,
              builder: (context, _) {
                final t = _burst.value;
                if (t == 0 || t == 1) return const SizedBox();
                final scale = Curves.elasticOut.transform((t * 2).clamp(0.0, 1.0));
                return Opacity(
                  opacity: t < 0.7 ? 1 : (1 - t) / 0.3,
                  child: Transform.scale(
                    scale: scale,
                    child: const Icon(Icons.favorite_rounded, color: Palette.danger, size: 110, shadows: [
                      Shadow(blurRadius: 24, color: Colors.black38),
                    ]),
                  ),
                );
              },
            ),
          ),
        ),
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
            child: Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, Space.xxl, Space.sm, Space.xl),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Expanded(
                    child: GestureDetector(
                      onTap: () => setState(() => _expanded = !_expanded),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Text(post.author,
                              maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.bodyStrong.merge(white)),
                          Text(
                            [post.handle, _ago(post.createdAt)].where((t) => t.isNotEmpty).join(' · '),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: AppText.micro.copyWith(color: Colors.white70),
                          ),
                          if (post.text.isNotEmpty) ...[
                            const SizedBox(height: Space.xs),
                            Text(
                              post.text,
                              maxLines: _expanded ? 12 : 2,
                              overflow: TextOverflow.ellipsis,
                              style: AppText.caption.merge(white),
                            ),
                          ],
                        ],
                      ),
                    ),
                  ),
                  Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      _SideButton(
                        icon: _liked ? Icons.favorite_rounded : Icons.favorite_border_rounded,
                        color: _liked ? Palette.danger : Colors.white,
                        label: _liked ? 'Liked' : 'Like',
                        count: _count(post.likes + (_liked && !post.liked ? 1 : 0) - (!_liked && post.liked ? 1 : 0)),
                        onTap: () => _setLiked(!_liked),
                      ),
                      const SizedBox(height: Space.md),
                      ValueListenableBuilder<bool>(
                        valueListenable: _muted,
                        builder: (context, muted, _) => _SideButton(
                          icon: muted ? Icons.volume_off_rounded : Icons.volume_up_rounded,
                          label: muted ? 'Sound off' : 'Sound on',
                          onTap: () => _muted.value = !muted,
                        ),
                      ),
                      const SizedBox(height: Space.md),
                      _SideButton(icon: Icons.send_rounded, label: 'Send', onTap: _share),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ],
    );
  }
}

class _SideButton extends StatelessWidget {
  const _SideButton({required this.icon, required this.label, required this.onTap, this.color, this.count});

  final IconData icon;
  final String label;
  final VoidCallback onTap;
  final Color? color;

  /// Shown under the icon (likes).
  final String? count;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        label: label,
        child: InkResponse(
          onTap: onTap,
          radius: 28,
          child: Padding(
            padding: const EdgeInsets.all(Space.sm),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                AnimatedSwitcher(
                  duration: Motion.fast,
                  transitionBuilder: (child, a) => ScaleTransition(scale: a, child: child),
                  child: Icon(
                    icon,
                    key: ValueKey(icon),
                    color: color ?? Colors.white,
                    size: 30,
                    shadows: const [Shadow(blurRadius: 8)],
                  ),
                ),
                if (count != null)
                  Text(count!, style: AppText.micro.copyWith(color: Colors.white, fontWeight: FontWeight.w600)),
              ],
            ),
          ),
        ),
      );
}

/// A video: plays (looping) while its reel is showing, streamed through
/// Tor by Sanctum's own relay. Tap to pause. Upright videos fill the
/// screen; wide ones fit its width.
class _Video extends StatefulWidget {
  const _Video({
    super.key,
    required this.host,
    required this.item,
    required this.playing,
    required this.preload,
    this.onDoubleTap,
    this.onWatched,
  });

  final String host;
  final FediItem item;
  final bool playing;
  final VoidCallback? onDoubleTap;

  /// When it stops: how long it played on screen, and its length.
  final void Function(Duration watched, Duration length)? onWatched;

  /// Load it now, to play as soon as it's swiped to.
  final bool preload;

  @override
  State<_Video> createState() => _VideoState();
}

class _VideoState extends State<_Video> {
  VideoPlayerController? _video;
  var _failed = false;
  var _paused = false;
  var _starting = false;

  /// Time spent playing on screen.
  final _watch = Stopwatch();

  bool get _wanted => widget.playing || widget.preload;

  /// Plays (and times it) or pauses, as the reel's place says.
  void _run(VideoPlayerController video) {
    if (widget.playing && !_paused) {
      video.play();
      _watch.start();
    } else {
      video.pause();
      _watch.stop();
    }
  }

  @override
  void initState() {
    super.initState();
    _muted.addListener(_applyVolume);
    if (_wanted) _start();
  }

  @override
  void didUpdateWidget(_Video old) {
    super.didUpdateWidget(old);
    if (_wanted) {
      _start();
    } else {
      _stop();
    }
  }

  @override
  void dispose() {
    _muted.removeListener(_applyVolume);
    _report();
    _video?.dispose();
    super.dispose();
  }

  /// Tells how long it was watched, if it was ever ready on screen.
  void _report() {
    final video = _video;
    if (video == null || !video.value.isInitialized || _watch.elapsed == Duration.zero && !widget.playing) return;
    _watch.stop();
    final length =
        video.value.duration > Duration.zero ? video.value.duration : Duration(seconds: widget.item.durationS);
    widget.onWatched?.call(_watch.elapsed, length);
    _watch.reset();
  }

  void _applyVolume() => _video?.setVolume(_muted.value ? 0 : 1);

  Future<void> _start() async {
    final video = _video;
    if (video != null || _starting) {
      if (video != null && video.value.isInitialized) _run(video);
      return;
    }
    _starting = true;
    final local = await fediVideo(host: widget.host, url: widget.item.url, videoId: widget.item.videoId);
    _starting = false;
    if (!mounted || !_wanted) return;
    if (local == null) {
      setState(() => _failed = true);
      return;
    }
    final fresh = VideoPlayerController.networkUrl(Uri.parse(local));
    setState(() => _video = fresh);
    try {
      await fresh.initialize();
      await fresh.setLooping(true);
      _applyVolume();
      if (mounted && _video == fresh) _run(fresh);
      if (mounted) setState(() {});
    } catch (_) {
      if (mounted) setState(() => _failed = true);
    }
  }

  void _stop() {
    // Freed when scrolled away: at most two videos stream at a time.
    final video = _video;
    if (video == null) return;
    _report();
    _video = null;
    _paused = false;
    video.dispose();
    if (mounted) setState(() {});
  }

  void _toggle() {
    final video = _video;
    if (video == null || !video.value.isInitialized) return;
    HapticFeedback.selectionClick();
    setState(() => _paused = !_paused);
    _run(video);
  }

  @override
  Widget build(BuildContext context) {
    final video = _video;
    final ready = video != null && video.value.isInitialized;
    final upright = ready
        ? video.value.aspectRatio < 1
        : widget.item.height > 0 && widget.item.width > 0 && widget.item.height > widget.item.width;
    final fit = upright ? BoxFit.cover : BoxFit.contain;
    return GestureDetector(
      onTap: _toggle,
      onDoubleTap: widget.onDoubleTap,
      child: Stack(
        fit: StackFit.expand,
        children: [
          if (!ready)
            FutureBuilder<Uint8List?>(
              future: _Pictures.load(widget.item.preview),
              builder: (context, snap) => snap.data == null ? const SizedBox() : Image.memory(snap.data!, fit: fit),
            ),
          if (ready)
            FittedBox(
              fit: fit,
              clipBehavior: Clip.hardEdge,
              child: SizedBox(
                width: video.value.size.width,
                height: video.value.size.height,
                child: VideoPlayer(video),
              ),
            ),
          if (ready)
            Positioned(
              left: 0,
              right: 0,
              bottom: 0,
              child: VideoProgressIndicator(
                video,
                allowScrubbing: true,
                padding: EdgeInsets.zero,
                colors: const VideoProgressColors(
                  playedColor: Colors.white,
                  bufferedColor: Colors.white24,
                  backgroundColor: Colors.white10,
                ),
              ),
            ),
          if (_failed)
            const Center(
              child: Text('This video can\'t play right now.', style: TextStyle(color: Colors.white70)),
            )
          else if (widget.playing && !ready)
            const Center(child: CircularProgressIndicator(color: Colors.white))
          else if (ready && _paused)
            const Center(child: Icon(Icons.play_arrow_rounded, color: Colors.white, size: 72)),
        ],
      ),
    );
  }
}

/// The sites Reels come from, and adding or removing them.
class _SourcesSheet extends StatefulWidget {
  const _SourcesSheet();

  @override
  State<_SourcesSheet> createState() => _SourcesSheetState();
}

class _SourcesSheetState extends State<_SourcesSheet> {
  List<FediSource> _sources = const [];
  final _host = TextEditingController();
  var _kind = FediKind.loops;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _host.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final list = await fediSources();
    if (mounted) setState(() => _sources = list);
  }

  Future<void> _add() async {
    if (_host.text.toLowerCase().contains('pixelfed')) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(
          content: Text('Pixelfed only shows its posts to people signed in there, so Sanctum can\'t show it.')));
      return;
    }
    final added = await fediAddSource(kind: _kind, host: _host.text);
    if (!mounted) return;
    if (added == null) {
      ScaffoldMessenger.of(context)
          .showSnackBar(const SnackBar(content: Text('That doesn\'t look like a site address, like tilvids.com.')));
      return;
    }
    _host.clear();
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: SafeArea(
        child: SingleChildScrollView(
          padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.lg),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              const Text('Where reels come from', style: AppText.title),
              const SizedBox(height: Space.xs),
              Text(
                'Reels shows public videos under 3 minutes from these sites, picked for what you like: hearts, '
                'what you watch to the end and what you skip. That stays on this phone, and you need no account.',
                style: AppText.caption,
              ),
              const SizedBox(height: Space.md),
              for (final s in _sources)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: Icon(switch (s.kind) {
                    FediKind.loops => Icons.slow_motion_video_rounded,
                    FediKind.videos => Icons.smart_display_outlined,
                    FediKind.social => Icons.forum_outlined,
                  }),
                  title: Text(s.host),
                  subtitle: Text(switch (s.kind) {
                    FediKind.loops => 'Short videos (Loops)',
                    FediKind.videos => 'Video site (PeerTube)',
                    FediKind.social => 'Social site (Mastodon)',
                  }),
                  trailing: IconButton(
                    tooltip: 'Remove',
                    icon: const Icon(Icons.close_rounded),
                    onPressed: () async {
                      await fediRemoveSource(kind: s.kind, host: s.host);
                      await _load();
                    },
                  ),
                ),
              const SizedBox(height: Space.md),
              SegmentedButton<FediKind>(
                segments: const [
                  ButtonSegment(value: FediKind.loops, label: Text('Loops')),
                  ButtonSegment(value: FediKind.videos, label: Text('Video site')),
                  ButtonSegment(value: FediKind.social, label: Text('Social site')),
                ],
                selected: {_kind},
                showSelectedIcon: false,
                onSelectionChanged: (s) => setState(() => _kind = s.first),
              ),
              const SizedBox(height: Space.sm),
              TextField(
                controller: _host,
                autocorrect: false,
                keyboardType: TextInputType.url,
                decoration: InputDecoration(
                  hintText: switch (_kind) {
                    FediKind.loops => 'A Loops site, like loops.video',
                    FediKind.videos => 'A PeerTube site, like tilvids.com',
                    FediKind.social => 'A Mastodon site, like mastodon.social',
                  },
                  suffixIcon: IconButton(tooltip: 'Add', icon: const Icon(Icons.add_rounded), onPressed: _add),
                ),
                onSubmitted: (_) => _add(),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
