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

  static Future<Uint8List?> load(int chat, int seq) async {
    final key = '$chat:$seq';
    final hit = _bytes.remove(key);
    if (hit != null) return _bytes[key] = hit;
    final bytes = await mediaBytes(chatId: chat, seq: seq);
    if (bytes == null) return null;
    if (_bytes.length >= _max) _bytes.remove(_bytes.keys.first);
    return _bytes[key] = bytes;
  }
}

/// Decrypted copies for playing or opening live here, and are deleted as
/// soon as they're closed (and on every start).
Future<Directory> _openDir() async => Directory('${(await getApplicationCacheDirectory()).path}/open');

Future<void> clearDecryptedCopies() async {
  final dir = await _openDir();
  if (dir.existsSync()) dir.deleteSync(recursive: true);
}

Future<File?> _export(int chat, int seq, String name) async {
  final dir = await _openDir();
  final safe = name.replaceAll(RegExp(r'[^A-Za-z0-9._-]'), '_');
  final file = File('${dir.path}/$seq-$safe');
  if (!await mediaExport(chatId: chat, seq: seq, dest: file.path)) return null;
  return file;
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
      _ => _FileRow(media: media, chatId: chatId, seq: seq, fg: fg),
    };
  }
}

class _Visual extends StatelessWidget {
  const _Visual({required this.media, required this.chatId, required this.seq, required this.maxWidth, required this.video});

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
          : ImageScreen(chatId: chatId, seq: seq!, heroTag: 'media-$chatId-$seq'),
    ));
  }

  @override
  Widget build(BuildContext context) {
    final preview = media.thumb.isEmpty
        ? const ColoredBox(color: Palette.surface)
        : ImageFiltered(
            imageFilter: ImageFilter.blur(sigmaX: 6, sigmaY: 6),
            child: Image.memory(Uint8List.fromList(media.thumb), fit: BoxFit.cover, gaplessPlayback: true),
          );
    Widget picture = preview;
    if (!video && media.ready && seq != null) {
      picture = FutureBuilder<Uint8List?>(
        future: _ImageCache.load(chatId, seq!),
        builder: (context, snap) => snap.data == null
            ? preview
            : Hero(
                tag: 'media-$chatId-$seq',
                child: Image.memory(snap.data!, fit: BoxFit.cover, gaplessPlayback: true, cacheWidth: 900),
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
    if (seq == null || !media.ready) return;
    final file = await _export(chatId, seq!, media.name);
    if (file == null) return;
    final opened = await Background.openFile(file.path, media.mime);
    if (!opened && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('No app here can open this file.')));
    }
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
                  Text(media.name, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.bodyStrong.copyWith(color: fg)),
                  Text(
                    media.ready ? formatSize(media.size.toInt()) : 'Receiving · ${formatSize(media.size.toInt())}',
                    style: AppText.micro.copyWith(color: fg.withValues(alpha: 0.7)),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A photo, full screen: pinch to zoom.
class ImageScreen extends StatelessWidget {
  const ImageScreen({super.key, required this.chatId, required this.seq, required this.heroTag});

  final int chatId;
  final int seq;
  final String heroTag;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(backgroundColor: Colors.transparent, foregroundColor: Colors.white),
      extendBodyBehindAppBar: true,
      body: FutureBuilder<Uint8List?>(
        future: _ImageCache.load(chatId, seq),
        builder: (context, snap) => snap.data == null
            ? const Center(child: CircularProgressIndicator())
            : InteractiveViewer(
                maxScale: 6,
                child: Center(child: Hero(tag: heroTag, child: Image.memory(snap.data!))),
              ),
      ),
    );
  }
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

  @override
  void initState() {
    super.initState();
    _start();
  }

  Future<void> _start() async {
    final file = await _export(widget.chatId, widget.seq, widget.media.name);
    if (file == null || !mounted) return;
    _file = file;
    final video = VideoPlayerController.file(file);
    await video.initialize();
    if (!mounted) {
      await video.dispose();
      return;
    }
    setState(() => _video = video);
    await video.play();
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
      appBar: AppBar(backgroundColor: Colors.transparent, foregroundColor: Colors.white),
      extendBodyBehindAppBar: true,
      body: video == null
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
