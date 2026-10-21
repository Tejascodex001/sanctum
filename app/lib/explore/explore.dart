import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:video_player/video_player.dart';

import '../src/rust/api/fedi.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';

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

/// Reels: short public videos from the Fediverse (PeerTube and
/// Mastodon-compatible servers), read anonymously through Tor. Swipe up
/// for the next.
class ExplorePage extends StatefulWidget {
  const ExplorePage({super.key, required this.active});

  /// The Reels tab is showing (videos play only then).
  final bool active;

  @override
  State<ExplorePage> createState() => _ExplorePageState();
}

class _ExplorePageState extends State<ExplorePage> {
  List<FediSource> _sources = const [];
  final _posts = <FediPost>[];
  final _seen = <String>{};
  final _next = <String, String?>{};
  final _problems = <String, FediProblem>{};
  var _loading = false;
  var _started = false;
  var _index = 0;
  final _pages = PageController();

  String _key(FediSource s) => '${s.kind.name}:${s.host}';

  @override
  void didUpdateWidget(ExplorePage old) {
    super.didUpdateWidget(old);
    // Nothing is fetched until the tab is first opened.
    if (widget.active && !_started) _restart();
  }

  @override
  void initState() {
    super.initState();
    if (widget.active) _restart();
  }

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  Future<void> _restart() async {
    _started = true;
    final sources = await fediSources();
    if (!mounted) return;
    setState(() {
      _sources = sources;
      _posts.clear();
      _seen.clear();
      _next.clear();
      _problems.clear();
      _index = 0;
    });
    if (_pages.hasClients) _pages.jumpToPage(0);
    await _more();
  }

  /// The next reels from every server that has more, mixed together.
  Future<void> _more() async {
    if (_loading) return;
    final due = [
      for (final s in _sources)
        if (!_next.containsKey(_key(s)) || _next[_key(s)] != null) s,
    ];
    if (due.isEmpty) return;
    setState(() => _loading = true);
    final pages = await Future.wait([
      for (final s in due) fediFeed(kind: s.kind, host: s.host, next: _next[_key(s)]),
    ]);
    if (!mounted) return;
    setState(() {
      _loading = false;
      final batches = <List<FediPost>>[];
      for (final (i, s) in due.indexed) {
        final page = pages[i];
        if (page.problem != null) {
          _problems[_key(s)] = page.problem!;
          _next[_key(s)] = null;
          continue;
        }
        _problems.remove(_key(s));
        _next[_key(s)] = page.next;
        batches.add([
          for (final p in page.posts)
            if (p.media.isNotEmpty && _seen.add('${p.host}/${p.id}')) p,
        ]);
      }
      // Take turns, so one busy server doesn't fill the feed.
      for (var round = 0; batches.any((b) => round < b.length); round++) {
        for (final b in batches) {
          if (round < b.length) _posts.add(b[round]);
        }
      }
    });
  }

  void _onPage(int i) {
    setState(() => _index = i);
    if (i >= _posts.length - 3) _more();
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
    if (_posts.isEmpty) {
      if (_loading || !_started) return const Center(child: CircularProgressIndicator());
      return _Nothing(problems: _problems, sources: _sources, onRetry: _restart, onServers: _editSources);
    }
    return RefreshIndicator(
      onRefresh: _restart,
      child: PageView.builder(
        controller: _pages,
        scrollDirection: Axis.vertical,
        onPageChanged: _onPage,
        itemCount: _posts.length + (_loading ? 1 : 0),
        itemBuilder: (context, i) => i >= _posts.length
            ? const Center(child: CircularProgressIndicator())
            : _Reel(
                key: ValueKey('${_posts[i].host}/${_posts[i].id}'),
                post: _posts[i],
                playing: widget.active && i == _index,
                // The next one starts loading early: Tor is slow to start.
                preload: widget.active && i == _index + 1,
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
    ];
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(Space.xxl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.slow_motion_video_rounded, size: 44, color: Palette.textMuted),
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

class _ReelState extends State<_Reel> {
  var _expanded = false;

  Future<void> _share() async {
    final chats = (await chatList()).where((c) => !c.hidden && c.active).toList();
    if (!mounted) return;
    final chat = await showModalBottomSheet<StoredChat>(
      context: context,
      showDragHandle: true,
      builder: (sheet) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            const Padding(
              padding: EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
              child: Text('SEND THE LINK TO', style: AppText.overline),
            ),
            for (final c in chats)
              ListTile(
                leading: Icon(c.kind == ChatKindView.group ? Icons.groups_rounded : Icons.person_rounded),
                title: Text(c.title),
                onTap: () => Navigator.pop(sheet, c),
              ),
          ],
        ),
      ),
    );
    if (chat == null) return;
    final text = widget.post.text.split('\n').first;
    await messageSend(
      chatId: chat.id,
      body: '${text.length > 80 ? '${text.substring(0, 80)}…' : text}\n${widget.post.link}'.trim(),
    );
    HapticFeedback.lightImpact();
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('Sent to ${chat.title}')));
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
  const _SideButton({required this.icon, required this.label, required this.onTap});

  final IconData icon;
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        label: label,
        child: InkResponse(
          onTap: onTap,
          radius: 28,
          child: Padding(
            padding: const EdgeInsets.all(Space.sm),
            child: Icon(icon, color: Colors.white, size: 30, shadows: const [Shadow(blurRadius: 8)]),
          ),
        ),
      );
}

/// A video: plays (looping) while its reel is showing, streamed through
/// Tor by Sanctum's own relay. Tap to pause. Upright videos fill the
/// screen; wide ones fit its width.
class _Video extends StatefulWidget {
  const _Video({super.key, required this.host, required this.item, required this.playing, required this.preload});

  final String host;
  final FediItem item;
  final bool playing;

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

  bool get _wanted => widget.playing || widget.preload;

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
    _video?.dispose();
    super.dispose();
  }

  void _applyVolume() => _video?.setVolume(_muted.value ? 0 : 1);

  Future<void> _start() async {
    final video = _video;
    if (video != null || _starting) {
      if (video != null && video.value.isInitialized) {
        widget.playing && !_paused ? video.play() : video.pause();
      }
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
      if (mounted && _video == fresh && widget.playing && !_paused) await fresh.play();
      if (mounted) setState(() {});
    } catch (_) {
      if (mounted) setState(() => _failed = true);
    }
  }

  void _stop() {
    // Freed when scrolled away: at most two videos stream at a time.
    final video = _video;
    if (video == null) return;
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
    _paused ? video.pause() : video.play();
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
  var _kind = FediKind.videos;

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
              const Text(
                'Reels shows public videos under 3 minutes from these sites. Sanctum watches them privately: no '
                'account, and the sites can\'t see who you are or where you are. Your list stays on this phone.',
                style: AppText.caption,
              ),
              const SizedBox(height: Space.md),
              for (final s in _sources)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: Icon(s.kind == FediKind.videos ? Icons.smart_display_outlined : Icons.forum_outlined),
                  title: Text(s.host),
                  subtitle: Text(s.kind == FediKind.videos ? 'Video site (PeerTube)' : 'Social site (Mastodon)'),
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
                  ButtonSegment(
                      value: FediKind.videos, label: Text('Video site'), icon: Icon(Icons.smart_display_outlined)),
                  ButtonSegment(value: FediKind.social, label: Text('Social site'), icon: Icon(Icons.forum_outlined)),
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
                  hintText: _kind == FediKind.videos
                      ? 'A PeerTube site, like tilvids.com'
                      : 'A Mastodon site, like mastodon.social',
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
