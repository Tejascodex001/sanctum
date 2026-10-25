import 'dart:io';
import 'dart:typed_data';
import 'dart:ui';

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:video_player/video_player.dart';

import '../background/background.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';

/// Decrypted photos recently shown, so scrolling doesn't decrypt again.
class _ImageCache {
  static const _max = 40;
  static final _bytes = <String, Uint8List>{};

  /// The photo if it's already been decrypted (so it can be drawn at once).
  static Uint8List? peek(int chat, int seq) {
    final key = '$chat:$seq';
    final hit = _bytes.remove(key);
    return hit == null ? null : _bytes[key] = hit;
  }

  static Future<Uint8List?> load(int chat, int seq) async {
    final key = '$chat:$seq';
    final hit = _bytes.remove(key);
    if (hit != null) return _bytes[key] = hit;
    final Uint8List? bytes;
    try {
      bytes = await mediaBytes(chatId: chat, seq: seq);
    } catch (_) {
      return null;
    }
    if (bytes == null) return null;
    if (_bytes.length >= _max) _bytes.remove(_bytes.keys.first);
    return _bytes[key] = bytes;
  }
}

/// Decrypted copies for playing or opening live here, and are deleted as
/// soon as they're closed (and on every start).
Future<Directory> _openDir() async => Directory('${(await getApplicationCacheDirectory()).path}/open');

/// Picked photos and files wait here while they're sent; a send that was
/// cancelled or cut short leaves its copy behind.
Future<Directory> _outgoingDir() async => Directory('${(await getApplicationCacheDirectory()).path}/outgoing');

/// Deletes every leftover plain copy (opened attachments, picked files that
/// were never sent). Run at every start.
Future<void> clearDecryptedCopies() async {
  for (final dir in [await _openDir(), await _outgoingDir()]) {
    try {
      if (dir.existsSync()) dir.deleteSync(recursive: true);
    } catch (_) {}
  }
}

/// A decrypted copy of an attachment. [tag] keeps copies made for different
/// purposes (playing, saving, opening elsewhere) from sharing a file.
Future<File?> _export(int chat, int seq, String name, {String tag = ''}) async {
  final dir = await _openDir();
  final safe = name.replaceAll(RegExp(r'[^A-Za-z0-9._-]'), '_');
  final file = File('${dir.path}/$seq$tag-$safe');
  try {
    if (!await mediaExport(chatId: chat, seq: seq, dest: file.path)) return null;
  } catch (_) {
    return null;
  }
  return file;
}

void _tell(ScaffoldMessengerState? messenger, String text) {
  messenger
    ?..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));
}

/// "Save to phone": copies an attachment into the phone's own gallery,
/// movies, music or downloads folder (in a "Sanctum" folder). Only on the
/// user's request; the temporary decrypted copy is deleted right after.
Future<void> saveMediaToPhone(BuildContext context,
    {required int chatId, required int seq, required MediaView media}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  File? file;
  var outcome = 'failed';
  try {
    final name = media.name.trim().isEmpty ? 'sanctum-$seq' : media.name;
    file = await _export(chatId, seq, name, tag: '-save');
    outcome = file == null ? 'missing' : await Background.saveToPhone(file.path, name, media.mime);
  } catch (_) {
    outcome = 'failed';
  } finally {
    try {
      file?.deleteSync();
    } catch (_) {}
  }
  switch (outcome) {
    case 'saved':
      _tell(messenger, 'Saved to your phone.');
    case 'cancelled':
      break;
    case 'missing':
      _tell(messenger, 'Couldn\'t save it, because this phone no longer has the file.');
    default:
      _tell(messenger, 'Couldn\'t save it. Check that the phone has enough free space, then try again.');
  }
}

/// Opens an attachment in another app, saying what went wrong if it can't.
Future<void> openWithOtherApp(BuildContext context,
    {required int chatId, required int seq, required MediaView media}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  var outcome = 'failed';
  try {
    final file = await _export(chatId, seq, media.name.trim().isEmpty ? 'sanctum-$seq' : media.name, tag: '-open');
    outcome = file == null ? 'missing' : await Background.openFile(file.path, media.name, media.mime);
  } catch (_) {
    outcome = 'failed';
  }
  switch (outcome) {
    case 'opened':
      break;
    case 'noapp':
      _tell(messenger,
          'No app on this phone can open this kind of file. You can use "Save to phone" and look for an app that can.');
    case 'missing':
      _tell(messenger, 'Couldn\'t find that file on this phone any more.');
    default:
      _tell(messenger, 'Couldn\'t open that file. You can use "Save to phone" instead.');
  }
}

String formatSize(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(0)} KB';
  return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
}

String formatDuration(int ms) {
  final s = (ms / 1000).round();
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}

/// The attachment inside a message bubble.
class MediaContent extends StatelessWidget {
  const MediaContent({super.key, required this.media, required this.chatId, required this.seq, required this.fg});

  final MediaView media;
  final int chatId;
  final int? seq;
  final Color fg;

  @override
  Widget build(BuildContext context) {
    final maxWidth = MediaQuery.sizeOf(context).width * 0.66;
    return switch (media.kind) {
      MediaKindView.image => _Visual(media: media, chatId: chatId, seq: seq, maxWidth: maxWidth, video: false),
      MediaKindView.video => _Visual(media: media, chatId: chatId, seq: seq, maxWidth: maxWidth, video: true),
      MediaKindView.audio => _VoiceNote(media: media, chatId: chatId, seq: seq, fg: fg),
      _ => _FileRow(media: media, chatId: chatId, seq: seq, fg: fg),
    };
  }
}

/// Loads a photo once and keeps it, so a rebuilt (or re-created) bubble
/// shows the picture at once instead of the blurry preview again.
class _Photo extends StatefulWidget {
  const _Photo({required this.chatId, required this.seq, required this.builder});

  final int chatId;
  final int seq;

  /// [bytes] is null until the photo is loaded; [failed] if it can't be.
  final Widget Function(BuildContext context, Uint8List? bytes, bool failed) builder;

  @override
  State<_Photo> createState() => _PhotoState();
}

class _PhotoState extends State<_Photo> {
  Uint8List? _bytes;
  bool _failed = false;

  @override
  void initState() {
    super.initState();
    _fetch();
  }

  @override
  void didUpdateWidget(_Photo old) {
    super.didUpdateWidget(old);
    if (old.chatId != widget.chatId || old.seq != widget.seq) _fetch();
  }

  void _fetch() {
    final chat = widget.chatId, seq = widget.seq;
    _failed = false;
    _bytes = _ImageCache.peek(chat, seq);
    if (_bytes != null) return;
    _ImageCache.load(chat, seq).then((bytes) {
      if (!mounted || chat != widget.chatId || seq != widget.seq) return;
      setState(() {
        _bytes = bytes;
        _failed = bytes == null;
      });
    });
  }

  @override
  Widget build(BuildContext context) => widget.builder(context, _bytes, _failed);
}

class _Visual extends StatelessWidget {
  const _Visual(
      {required this.media, required this.chatId, required this.seq, required this.maxWidth, required this.video});

  final MediaView media;
  final int chatId;
  final int? seq;
  final double maxWidth;
  final bool video;

  double get _aspect =>
      media.width > 0 && media.height > 0 ? (media.width / media.height).clamp(0.5, 2.0).toDouble() : 4 / 3;

  void _open(BuildContext context) {
    if (seq == null || !media.ready) return;
    Navigator.of(context).push(MaterialPageRoute<void>(
      builder: (_) => video
          ? VideoScreen(chatId: chatId, seq: seq!, media: media)
          : ImageScreen(chatId: chatId, seq: seq!, heroTag: 'media-$chatId-$seq', media: media),
    ));
  }

  @override
  Widget build(BuildContext context) {
    final preview = media.thumb.isEmpty
        ? ColoredBox(color: Palette.surface)
        : ImageFiltered(
            imageFilter: ImageFilter.blur(sigmaX: 6, sigmaY: 6),
            child: Image.memory(Uint8List.fromList(media.thumb), fit: BoxFit.cover, gaplessPlayback: true),
          );
    Widget picture = preview;
    if (!video && media.ready && seq != null) {
      picture = _Photo(
        chatId: chatId,
        seq: seq!,
        builder: (context, bytes, _) => bytes == null
            ? preview
            : Hero(
                tag: 'media-$chatId-$seq',
                child: Image.memory(bytes, fit: BoxFit.cover, gaplessPlayback: true, cacheWidth: 900),
              ),
      );
    }
    return GestureDetector(
      onTap: () => _open(context),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(Radii.sm),
        child: SizedBox(
          width: maxWidth,
          child: AspectRatio(
            aspectRatio: _aspect,
            child: Stack(
              fit: StackFit.expand,
              children: [
                picture,
                if (!media.ready)
                  const Center(
                    child: SizedBox.square(dimension: 28, child: CircularProgressIndicator(strokeWidth: 2.5)),
                  ),
                if (video && media.ready)
                  Center(
                    child: Container(
                      padding: const EdgeInsets.all(Space.sm),
                      decoration: const BoxDecoration(color: Colors.black45, shape: BoxShape.circle),
                      child: const Icon(Icons.play_arrow_rounded, color: Colors.white, size: 34),
                    ),
                  ),
                if (video && media.durationMs > 0)
                  Positioned(
                    right: Space.sm,
                    bottom: Space.xs,
                    child: Text(formatDuration(media.durationMs),
                        style: AppText.micro.copyWith(color: Colors.white, shadows: const [Shadow(blurRadius: 4)])),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _FileRow extends StatelessWidget {
  const _FileRow({required this.media, required this.chatId, required this.seq, required this.fg});

  final MediaView media;
  final int chatId;
  final int? seq;
  final Color fg;

  Future<void> _open(BuildContext context) async {
    if (seq == null || !media.ready) {
      _tell(ScaffoldMessenger.maybeOf(context), 'This file isn\'t ready yet. Try again in a moment.');
      return;
    }
    await openWithOtherApp(context, chatId: chatId, seq: seq!, media: media);
  }

  @override
  Widget build(BuildContext context) {
    return InkWell(
      onTap: () => _open(context),
      borderRadius: BorderRadius.circular(Radii.sm),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: Space.xs),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            media.ready
                ? Icon(Icons.insert_drive_file_rounded, color: fg)
                : SizedBox.square(dimension: 22, child: CircularProgressIndicator(strokeWidth: 2, color: fg)),
            const SizedBox(width: Space.sm),
            Flexible(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(media.name,
                      maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.bodyStrong.copyWith(color: fg)),
                  Text(
                    media.ready ? formatSize(media.size.toInt()) : 'Receiving · ${formatSize(media.size.toInt())}',
                    style: AppText.micro.copyWith(color: fg.withValues(alpha: 0.7)),
                  ),
                ],
              ),
            ),
            if (media.ready && seq != null)
              IconButton(
                tooltip: 'Save to phone',
                visualDensity: VisualDensity.compact,
                icon: Icon(Icons.download_rounded, color: fg, size: 22),
                onPressed: () => saveMediaToPhone(context, chatId: chatId, seq: seq!, media: media),
              ),
          ],
        ),
      ),
    );
  }
}

/// What to tell the user when an attachment can't be sent.
String sendErrorText(Object e) => '$e'.contains('too large')
    ? 'That\'s too big to send. Photos can be up to 10 MB, videos 20 MB, voice notes 10 MB and files 50 MB.'
    : 'Couldn\'t send that. Please try again.';

/// Sends something [Background.pickMedia] returned, then deletes the
/// picked copy (Sanctum keeps its own encrypted one).
Future<void> sendPicked(
  int chatId,
  Map<Object?, Object?> picked, {
  String caption = '',
  int? replyTo,
  AlbumView? album,
}) async {
  final path = picked['path']! as String;
  try {
    await mediaSend(
      chatId: chatId,
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
      replyTo: replyTo,
      album: album,
    );
  } finally {
    try {
      File(path).deleteSync();
    } catch (_) {}
  }
}

/// A square photo or video tile (albums). Tap to open it.
class MediaTile extends StatelessWidget {
  const MediaTile({super.key, required this.media, required this.chatId, required this.seq});

  final MediaView media;
  final int chatId;
  final int seq;

  bool get _video => media.kind == MediaKindView.video;

  @override
  Widget build(BuildContext context) {
    final preview = media.thumb.isEmpty
        ? ColoredBox(color: Palette.surfaceHigh)
        : ImageFiltered(
            imageFilter: ImageFilter.blur(sigmaX: 4, sigmaY: 4),
            child: Image.memory(Uint8List.fromList(media.thumb), fit: BoxFit.cover, gaplessPlayback: true),
          );
    final picture = !_video && media.ready
        ? _Photo(
            chatId: chatId,
            seq: seq,
            builder: (context, bytes, _) => bytes == null
                ? preview
                : Hero(
                    tag: 'media-$chatId-$seq',
                    child: Image.memory(bytes, fit: BoxFit.cover, gaplessPlayback: true, cacheWidth: 400),
                  ),
          )
        : preview;
    return GestureDetector(
      onTap: () {
        if (!media.ready) return;
        Navigator.of(context).push(MaterialPageRoute<void>(
          builder: (_) => _video
              ? VideoScreen(chatId: chatId, seq: seq, media: media)
              : ImageScreen(chatId: chatId, seq: seq, heroTag: 'media-$chatId-$seq', media: media),
        ));
      },
      child: Stack(
        fit: StackFit.expand,
        children: [
          picture,
          if (!media.ready)
            const Center(child: SizedBox.square(dimension: 22, child: CircularProgressIndicator(strokeWidth: 2))),
          if (_video && media.ready)
            const Center(child: Icon(Icons.play_circle_fill_rounded, color: Colors.white, size: 30)),
        ],
      ),
    );
  }
}

/// A photo, full screen: pinch to zoom.
class ImageScreen extends StatelessWidget {
  const ImageScreen({super.key, required this.chatId, required this.seq, required this.heroTag, required this.media});

  final int chatId;
  final int seq;
  final String heroTag;
  final MediaView media;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        foregroundColor: Colors.white,
        actions: [_SaveButton(chatId: chatId, seq: seq, media: media)],
      ),
      extendBodyBehindAppBar: true,
      body: _Photo(
        chatId: chatId,
        seq: seq,
        builder: (context, bytes, failed) => failed
            ? const _CantShow(text: 'This photo can\'t be shown on this phone.')
            : bytes == null
                ? const Center(child: CircularProgressIndicator())
                : InteractiveViewer(
                    maxScale: 6,
                    child: Center(
                      child: Hero(
                        tag: heroTag,
                        child: Image.memory(
                          bytes,
                          errorBuilder: (_, __, ___) =>
                              const _CantShow(text: 'This photo can\'t be shown on this phone.'),
                        ),
                      ),
                    ),
                  ),
      ),
    );
  }
}

/// The download button in a full-screen photo's or video's top bar.
class _SaveButton extends StatelessWidget {
  const _SaveButton({required this.chatId, required this.seq, required this.media});

  final int chatId;
  final int seq;
  final MediaView media;

  @override
  Widget build(BuildContext context) => IconButton(
        tooltip: 'Save to phone',
        icon: const Icon(Icons.download_rounded),
        onPressed: () => saveMediaToPhone(context, chatId: chatId, seq: seq, media: media),
      );
}

/// A full-screen message in white on black: this can't be shown.
class _CantShow extends StatelessWidget {
  const _CantShow({required this.text, this.actions = const []});

  final String text;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) => Center(
        child: Padding(
          padding: const EdgeInsets.all(Space.xl),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(Icons.broken_image_outlined, color: Colors.white70, size: 40),
              const SizedBox(height: Space.md),
              Text(text, textAlign: TextAlign.center, style: AppText.body.copyWith(color: Colors.white)),
              if (actions.isNotEmpty) ...[
                const SizedBox(height: Space.lg),
                Wrap(spacing: Space.sm, runSpacing: Space.sm, alignment: WrapAlignment.center, children: actions),
              ],
            ],
          ),
        ),
      );
}

/// A video, played in Sanctum from a decrypted copy that's deleted as soon
/// as the player closes.
class VideoScreen extends StatefulWidget {
  const VideoScreen({super.key, required this.chatId, required this.seq, required this.media});

  final int chatId;
  final int seq;
  final MediaView media;

  @override
  State<VideoScreen> createState() => _VideoScreenState();
}

class _VideoScreenState extends State<VideoScreen> {
  VideoPlayerController? _video;
  File? _file;

  /// This phone can't play it (some older phones can't decode newer video).
  bool _failed = false;

  @override
  void initState() {
    super.initState();
    _start();
  }

  Future<void> _start() async {
    VideoPlayerController? video;
    try {
      final file = await _export(widget.chatId, widget.seq, widget.media.name);
      if (!mounted) {
        try {
          file?.deleteSync();
        } catch (_) {}
        return;
      }
      if (file == null) throw StateError('no copy');
      _file = file;
      video = VideoPlayerController.file(file);
      await video.initialize().timeout(const Duration(seconds: 20));
      if (!mounted) {
        await video.dispose();
        return;
      }
      video.addListener(() {
        if (mounted && video!.value.hasError && !_failed) setState(() => _failed = true);
      });
      setState(() => _video = video);
      await video.play();
    } catch (_) {
      await video?.dispose();
      if (mounted) {
        setState(() {
          _video = null;
          _failed = true;
        });
      }
    }
  }

  @override
  void dispose() {
    _video?.dispose();
    try {
      _file?.deleteSync();
    } catch (_) {}
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final video = _video;
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        foregroundColor: Colors.white,
        actions: [_SaveButton(chatId: widget.chatId, seq: widget.seq, media: widget.media)],
      ),
      extendBodyBehindAppBar: true,
      body: _failed
          ? _CantShow(
              text: 'This video can\'t be played on this phone.',
              actions: [
                FilledButton.icon(
                  onPressed: () =>
                      saveMediaToPhone(context, chatId: widget.chatId, seq: widget.seq, media: widget.media),
                  icon: const Icon(Icons.download_rounded),
                  label: const Text('Save to phone'),
                ),
                OutlinedButton.icon(
                  onPressed: () =>
                      openWithOtherApp(context, chatId: widget.chatId, seq: widget.seq, media: widget.media),
                  style: OutlinedButton.styleFrom(foregroundColor: Colors.white),
                  icon: const Icon(Icons.open_in_new_rounded),
                  label: const Text('Open with another app'),
                ),
              ],
            )
          : video == null
              ? const Center(child: CircularProgressIndicator())
              : GestureDetector(
                  onTap: () => setState(() => video.value.isPlaying ? video.pause() : video.play()),
                  child: Stack(
                    alignment: Alignment.bottomCenter,
                    children: [
                      Center(child: AspectRatio(aspectRatio: video.value.aspectRatio, child: VideoPlayer(video))),
                      VideoProgressIndicator(video, allowScrubbing: true, padding: const EdgeInsets.all(Space.lg)),
                    ],
                  ),
                ),
    );
  }
}

/// A voice note: play/pause and progress. Plays from a decrypted copy
/// that's deleted when the bubble goes away.
class _VoiceNote extends StatefulWidget {
  const _VoiceNote({required this.media, required this.chatId, required this.seq, required this.fg});

  final MediaView media;
  final int chatId;
  final int? seq;
  final Color fg;

  @override
  State<_VoiceNote> createState() => _VoiceNoteState();
}

class _VoiceNoteState extends State<_VoiceNote> {
  VideoPlayerController? _player;
  File? _file;
  bool _loading = false;

  @override
  void dispose() {
    _player?.dispose();
    try {
      _file?.deleteSync();
    } catch (_) {}
    super.dispose();
  }

  Future<void> _toggle() async {
    final player = _player;
    if (player != null) {
      if (player.value.isPlaying) {
        await player.pause();
      } else {
        if (player.value.position >= player.value.duration) await player.seekTo(Duration.zero);
        await player.play();
      }
      return;
    }
    if (widget.seq == null || !widget.media.ready || _loading) return;
    setState(() => _loading = true);
    final messenger = ScaffoldMessenger.maybeOf(context);
    VideoPlayerController? created;
    try {
      final file = await _export(widget.chatId, widget.seq!, 'voice-${widget.seq}.m4a');
      if (file == null) throw StateError('no copy');
      _file = file;
      created = VideoPlayerController.file(file);
      await created.initialize().timeout(const Duration(seconds: 20));
      created.addListener(() {
        if (mounted) setState(() {});
      });
      if (!mounted) {
        await created.dispose();
        return;
      }
      setState(() {
        _player = created;
        _loading = false;
      });
      await created.play();
    } catch (_) {
      await created?.dispose();
      if (mounted) {
        setState(() {
          _player = null;
          _loading = false;
        });
      }
      _tell(messenger, 'This voice note can\'t be played on this phone. You can use "Save to phone" instead.');
    }
  }

  @override
  Widget build(BuildContext context) {
    final fg = widget.fg;
    final player = _player;
    final total = widget.media.durationMs;
    final position = player?.value.position.inMilliseconds ?? 0;
    final playing = player?.value.isPlaying ?? false;
    final progress = total == 0 ? 0.0 : (position / total).clamp(0.0, 1.0);
    return SizedBox(
      width: 220,
      child: Row(
        children: [
          IconButton(
            tooltip: playing ? 'Pause' : 'Play',
            onPressed: widget.media.ready ? _toggle : null,
            icon: !widget.media.ready || _loading
                ? SizedBox.square(dimension: 22, child: CircularProgressIndicator(strokeWidth: 2, color: fg))
                : Icon(playing ? Icons.pause_rounded : Icons.play_arrow_rounded, color: fg, size: 30),
          ),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                ClipRRect(
                  borderRadius: BorderRadius.circular(2),
                  child: LinearProgressIndicator(
                    value: progress,
                    minHeight: 3,
                    color: fg,
                    backgroundColor: fg.withValues(alpha: 0.25),
                  ),
                ),
                const SizedBox(height: Space.xs),
                Text(
                  playing || position > 0 ? formatDuration(position) : formatDuration(total),
                  style: AppText.micro.copyWith(color: fg.withValues(alpha: 0.75)),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
