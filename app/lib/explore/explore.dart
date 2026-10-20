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

enum _Filter { all, videos, photos }

/// Public Fediverse posts, Reels-style: PeerTube videos and Pixelfed
/// photos, read anonymously through Tor. Swipe up for the next.
class ExplorePage extends StatefulWidget {
  const ExplorePage({super.key, required this.active});

  /// The Explore tab is showing (videos play only then).
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
  var _filter = _Filter.all;
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

  Iterable<FediSource> get _active => _sources.where((s) => switch (_filter) {
        _Filter.all => true,
        _Filter.videos => s.kind == FediKind.videos,
        _Filter.photos => s.kind == FediKind.photos,
      });

  /// The next page from every server that has more, mixed together.
  Future<void> _more() async {
    if (_loading) return;
    final due = [
      for (final s in _active)
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
            if (_seen.add('${p.host}/${p.id}')) p,
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
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.sm, Space.sm),
          child: Row(
            children: [
              Expanded(
                child: SegmentedButton<_Filter>(
                  segments: const [
                    ButtonSegment(value: _Filter.all, label: Text('All')),
                    ButtonSegment(value: _Filter.videos, label: Text('Videos')),
                    ButtonSegment(value: _Filter.photos, label: Text('Photos')),
                  ],
                  selected: {_filter},
                  showSelectedIcon: false,
                  style: const ButtonStyle(visualDensity: VisualDensity.compact),
                  onSelectionChanged: (s) {
                    _filter = s.first;
                    _restart();
                  },
                ),
              ),
              IconButton(tooltip: 'Servers', icon: const Icon(Icons.tune_rounded), onPressed: _editSources),
            ],
          ),
        ),
        Expanded(child: _body()),
      ],
    );
  }

  Widget _body() {
    if (_posts.isEmpty) {
      if (_loading || !_started) return const Center(child: CircularProgressIndicator());
      return _Nothing(problems: _problems, sources: _active.toList(), onRetry: _restart, onServers: _editSources);
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
            : Padding(
                padding: const EdgeInsets.fromLTRB(Space.sm, 0, Space.sm, Space.sm),
                child: _PostCard(
                  key: ValueKey('${_posts[i].host}/${_posts[i].id}'),
                  post: _posts[i],
                  playing: widget.active && i == _index,
                ),
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
      if (sources.isEmpty) 'Add a server to see what people are sharing.',
      if (offline) 'Explore opens once Sanctum is connected. This can take a minute after starting.',
      if (!offline)
        for (final s in sources)
          switch (problems['${s.kind.name}:${s.host}']) {
            FediProblem.notPublic => '${s.host} only shows its posts to people signed in there. Try another server.',
            FediProblem.unreachable => 'Couldn\'t reach ${s.host} right now.',
            FediProblem.badHost => '${s.host} isn\'t a server address.',
            _ => '${s.host} has nothing new.',
          },
    ];
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(Space.xxl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.travel_explore_rounded, size: 44, color: Palette.textMuted),
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
                FilledButton.tonal(onPressed: onServers, child: const Text('Servers')),
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

/// One post, filling the feed: its photos or video, who posted it and
/// what they wrote.
class _PostCard extends StatefulWidget {
  const _PostCard({super.key, required this.post, required this.playing});

  final FediPost post;
  final bool playing;

  @override
  State<_PostCard> createState() => _PostCardState();
}

class _PostCardState extends State<_PostCard> {
  var _shown = 0;
  var _expanded = false;

  FediItem get _item => widget.post.media[_shown];

  void _tap(TapUpDetails d, double width) {
    final media = widget.post.media;
    if (media.length < 2) return;
    setState(() {
      _shown =
          d.localPosition.dx > width / 2 ? (_shown + 1) % media.length : (_shown - 1 + media.length) % media.length;
    });
  }

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
              child: Text('Send the link to', style: AppText.overline),
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
    final item = _item;
    return ClipRRect(
      borderRadius: BorderRadius.circular(Radii.lg),
      child: ColoredBox(
        color: Colors.black,
        child: LayoutBuilder(
          builder: (context, box) => GestureDetector(
            onTapUp: item.video ? null : (d) => _tap(d, box.maxWidth),
            child: Stack(
              fit: StackFit.expand,
              children: [
                item.video
                    ? _Video(
                        key: ValueKey(item.videoId.isEmpty ? item.url : item.videoId),
                        host: post.host,
                        item: item,
                        playing: widget.playing,
                      )
                    : _Picture(key: ValueKey(item.url), item: item),
                if (post.media.length > 1)
                  Positioned(
                    top: Space.md,
                    left: 0,
                    right: 0,
                    child: Row(
                      mainAxisAlignment: MainAxisAlignment.center,
                      children: [
                        for (var i = 0; i < post.media.length; i++)
                          AnimatedContainer(
                            duration: Motion.fast,
                            margin: const EdgeInsets.symmetric(horizontal: 2),
                            width: i == _shown ? 18 : 6,
                            height: 6,
                            decoration: BoxDecoration(
                              color: Colors.white.withValues(alpha: i == _shown ? 0.95 : 0.5),
                              borderRadius: BorderRadius.circular(3),
                            ),
                          ),
                      ],
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
                      padding: const EdgeInsets.fromLTRB(Space.lg, Space.xxl, Space.sm, Space.lg),
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
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: AppText.bodyStrong.copyWith(color: Colors.white)),
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
                                      style: AppText.caption.copyWith(color: Colors.white),
                                    ),
                                  ],
                                ],
                              ),
                            ),
                          ),
                          IconButton(
                            tooltip: 'Send to a chat',
                            color: Colors.white,
                            icon: const Icon(Icons.send_rounded),
                            onPressed: _share,
                          ),
                        ],
                      ),
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

/// A photo: its small preview first, then the full picture.
class _Picture extends StatelessWidget {
  const _Picture({super.key, required this.item});

  final FediItem item;

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<Uint8List?>(
      future: _Pictures.load(item.url),
      builder: (context, full) {
        if (full.data != null) {
          return Image.memory(full.data!, fit: BoxFit.contain, gaplessPlayback: true, cacheWidth: 1440);
        }
        return Stack(
          fit: StackFit.expand,
          children: [
            FutureBuilder<Uint8List?>(
              future: _Pictures.load(item.preview),
              builder: (context, small) => small.data == null
                  ? const SizedBox()
                  : Image.memory(small.data!, fit: BoxFit.contain, gaplessPlayback: true),
            ),
            if (full.connectionState != ConnectionState.done) const Center(child: CircularProgressIndicator()),
            if (full.connectionState == ConnectionState.done)
              const Center(child: Icon(Icons.broken_image_outlined, color: Colors.white54, size: 40)),
          ],
        );
      },
    );
  }
}

/// A video: plays (looping) while its page is showing, streamed through
/// Tor by Sanctum's own relay. Tap to pause.
class _Video extends StatefulWidget {
  const _Video({super.key, required this.host, required this.item, required this.playing});

  final String host;
  final FediItem item;
  final bool playing;

  @override
  State<_Video> createState() => _VideoState();
}

class _VideoState extends State<_Video> {
  VideoPlayerController? _video;
  var _failed = false;
  var _paused = false;
  var _starting = false;

  @override
  void initState() {
    super.initState();
    if (widget.playing) _start();
  }

  @override
  void didUpdateWidget(_Video old) {
    super.didUpdateWidget(old);
    if (widget.playing && !old.playing) {
      _start();
    } else if (!widget.playing && old.playing) {
      _stop();
    }
  }

  @override
  void dispose() {
    _video?.dispose();
    super.dispose();
  }

  Future<void> _start() async {
    if (_video != null || _starting) {
      if (!_paused) _video?.play();
      return;
    }
    _starting = true;
    final local = await fediVideo(host: widget.host, url: widget.item.url, videoId: widget.item.videoId);
    _starting = false;
    if (!mounted || !widget.playing) return;
    if (local == null) {
      setState(() => _failed = true);
      return;
    }
    final video = VideoPlayerController.networkUrl(Uri.parse(local));
    setState(() => _video = video);
    try {
      await video.initialize();
      await video.setLooping(true);
      if (mounted && widget.playing) await video.play();
      if (mounted) setState(() {});
    } catch (_) {
      if (mounted) setState(() => _failed = true);
    }
  }

  void _stop() {
    // Freed when scrolled away: only one video streams at a time.
    final video = _video;
    _video = null;
    video?.dispose();
    if (mounted) setState(() {});
  }

  void _toggle() {
    final video = _video;
    if (video == null || !video.value.isInitialized) return;
    setState(() => _paused = !_paused);
    _paused ? video.pause() : video.play();
  }

  @override
  Widget build(BuildContext context) {
    final video = _video;
    final ready = video != null && video.value.isInitialized;
    return GestureDetector(
      onTap: _toggle,
      child: Stack(
        fit: StackFit.expand,
        children: [
          if (!ready)
            FutureBuilder<Uint8List?>(
              future: _Pictures.load(widget.item.preview),
              builder: (context, snap) =>
                  snap.data == null ? const SizedBox() : Image.memory(snap.data!, fit: BoxFit.contain),
            ),
          if (ready)
            Center(
              child: AspectRatio(aspectRatio: video.value.aspectRatio, child: VideoPlayer(video)),
            ),
          if (ready)
            Positioned(
              left: 0,
              right: 0,
              bottom: 0,
              child: VideoProgressIndicator(video, allowScrubbing: true, padding: EdgeInsets.zero),
            ),
          if (_failed)
            const Center(
              child: Text('This video can\'t play right now.', style: TextStyle(color: Colors.white70)),
            )
          else if (widget.playing && !ready)
            const Center(child: CircularProgressIndicator())
          else if (ready && _paused)
            const Center(child: Icon(Icons.play_arrow_rounded, color: Colors.white, size: 64)),
        ],
      ),
    );
  }
}

/// The servers Explore reads, and adding or removing them.
class _SourcesSheet extends StatefulWidget {
  const _SourcesSheet();

  @override
  State<_SourcesSheet> createState() => _SourcesSheetState();
}

class _SourcesSheetState extends State<_SourcesSheet> {
  List<FediSource> _sources = const [];
  final _host = TextEditingController();
  var _kind = FediKind.photos;

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
    final added = await fediAddSource(kind: _kind, host: _host.text);
    if (!mounted) return;
    if (added == null) {
      ScaffoldMessenger.of(context).showSnackBar(
          const SnackBar(content: Text('That doesn\'t look like a server address, like pixelfed.social.')));
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
              const Text('Servers', style: AppText.title),
              const SizedBox(height: Space.xs),
              const Text(
                'Explore shows public posts from these servers. Sanctum reads them privately: no account, and the '
                'servers can\'t see who you are or where you are. Which servers you follow stays on this phone.',
                style: AppText.caption,
              ),
              const SizedBox(height: Space.md),
              for (final s in _sources)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: Icon(s.kind == FediKind.videos ? Icons.smart_display_outlined : Icons.photo_outlined),
                  title: Text(s.host),
                  subtitle: Text(s.kind == FediKind.videos ? 'Videos (PeerTube)' : 'Photos (Pixelfed)'),
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
                  ButtonSegment(value: FediKind.photos, label: Text('Photos'), icon: Icon(Icons.photo_outlined)),
                  ButtonSegment(
                      value: FediKind.videos, label: Text('Videos'), icon: Icon(Icons.smart_display_outlined)),
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
                      ? 'A PeerTube server, like framatube.org'
                      : 'A Pixelfed server, like pixelfed.social',
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
