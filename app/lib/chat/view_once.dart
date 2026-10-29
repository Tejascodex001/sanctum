import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:video_player/video_player.dart';

import '../background/background.dart';
import '../security/screenshots.dart';
import '../src/rust/api/chat_extras.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';

/// Opens a friend's photo or video that can be seen once. It's gone from
/// both phones as soon as it opens; screenshots are blocked while it shows,
/// whatever the chat allows.
Future<void> openViewOnce(
  BuildContext context, {
  required int chatId,
  required int seq,
  required MediaKindView kind,
  required String chatKey,
}) async {
  final sure = await showDialog<bool>(
    context: context,
    builder: (dialog) => AlertDialog(
      title: Text(kind == MediaKindView.video ? 'Watch it now?' : 'Open it now?'),
      content: const Text('You can see it only once. When you close it, it\'s gone from both phones.'),
      actions: [
        TextButton(onPressed: () => Navigator.pop(dialog, false), child: const Text('Later')),
        FilledButton(onPressed: () => Navigator.pop(dialog, true), child: const Text('Open')),
      ],
    ),
  );
  if (sure != true || !context.mounted) return;
  HapticFeedback.mediumImpact();
  await Background.setSecure(true);
  Uint8List? bytes;
  try {
    bytes = await viewOnceOpen(chatId: chatId, seq: seq);
  } catch (_) {}
  if (!context.mounted) return;
  if (bytes == null) {
    await Screenshots.enter(chatKey);
    if (context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('This can\'t be opened anymore.')));
    }
    return;
  }
  await Navigator.of(context).push(MaterialPageRoute<void>(
    fullscreenDialog: true,
    builder: (_) => _OnceScreen(bytes: bytes!, video: kind == MediaKindView.video),
  ));
  await Screenshots.enter(chatKey);
}

class _OnceScreen extends StatefulWidget {
  const _OnceScreen({required this.bytes, required this.video});

  final Uint8List bytes;
  final bool video;

  @override
  State<_OnceScreen> createState() => _OnceScreenState();
}

class _OnceScreenState extends State<_OnceScreen> {
  VideoPlayerController? _player;
  File? _file;
  bool _failed = false;

  @override
  void initState() {
    super.initState();
    if (widget.video) _play();
  }

  /// A video plays from a copy in the app's own cache, deleted on close.
  Future<void> _play() async {
    VideoPlayerController? player;
    try {
      final dir = await getApplicationCacheDirectory();
      final file = File('${dir.path}/once-${DateTime.now().microsecondsSinceEpoch}');
      await file.writeAsBytes(widget.bytes, flush: true);
      _file = file;
      player = VideoPlayerController.file(file);
      await player.initialize().timeout(const Duration(seconds: 20));
      if (!mounted) {
        await player.dispose();
        return;
      }
      setState(() => _player = player);
      await player.play();
    } catch (_) {
      await player?.dispose();
      if (mounted) setState(() => _failed = true);
    }
  }

  @override
  void dispose() {
    _player?.dispose();
    try {
      _file?.deleteSync();
    } catch (_) {}
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final player = _player;
    const cant = Center(
      child: Padding(
        padding: EdgeInsets.all(Space.xl),
        child: Text('This can\'t be shown on this phone.', style: TextStyle(color: Colors.white)),
      ),
    );
    return Scaffold(
      backgroundColor: Colors.black,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        foregroundColor: Colors.white,
        title: const Text('Opens once', style: TextStyle(color: Colors.white70, fontSize: 15)),
      ),
      extendBodyBehindAppBar: true,
      body: !widget.video
          ? InteractiveViewer(
              maxScale: 6,
              child: Center(child: Image.memory(widget.bytes, errorBuilder: (_, __, ___) => cant)),
            )
          : _failed
              ? cant
              : player == null
                  ? const Center(child: CircularProgressIndicator())
                  : GestureDetector(
                      onTap: () => setState(() => player.value.isPlaying ? player.pause() : player.play()),
                      child: Center(
                        child: AspectRatio(aspectRatio: player.value.aspectRatio, child: VideoPlayer(player)),
                      ),
                    ),
    );
  }
}
