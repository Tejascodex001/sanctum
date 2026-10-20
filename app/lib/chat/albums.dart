import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../background/background.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';
import 'media.dart';

/// One shared album: its photos and videos, newest first.
class _Album {
  _Album(this.id, this.title);

  final List<int> id;
  String title;
  final items = <MessageView>[];
}

String _key(List<int> id) => id.join(',');

/// The chat's albums, from its messages (an album is the photos carrying
/// its id). Newest activity first.
Future<List<_Album>> _albums(int chatId) async {
  final byId = <String, _Album>{};
  for (final m in (await messageList(chatId: chatId)).reversed) {
    final a = m.album;
    if (a == null || m.media == null) continue;
    byId.putIfAbsent(_key(a.id), () => _Album(a.id, a.title)).items.add(m);
  }
  return byId.values.toList();
}

/// Picks photos and videos and adds them to [album]. Returns how many
/// couldn't be sent.
Future<int> _addTo(BuildContext context, int chatId, AlbumView album) async {
  final picked = await Background.pickMediaMany();
  var failed = 0;
  for (final p in picked) {
    try {
      await sendPicked(chatId, p, album: album);
    } catch (e) {
      failed++;
      if (context.mounted && failed == 1) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(sendErrorText(e))));
      }
    }
  }
  if (picked.isNotEmpty) HapticFeedback.lightImpact();
  return failed;
}

/// Every shared album in a chat, and a button to start one.
class AlbumsScreen extends StatefulWidget {
  const AlbumsScreen({super.key, required this.chatId, required this.title, required this.accent});

  final int chatId;
  final String title;
  final Color accent;

  @override
  State<AlbumsScreen> createState() => _AlbumsScreenState();
}

class _AlbumsScreenState extends State<AlbumsScreen> {
  List<_Album>? _list;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final list = await _albums(widget.chatId);
    if (mounted) setState(() => _list = list);
  }

  Future<void> _create() async {
    final name = TextEditingController();
    final title = await showDialog<String>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('New shared album'),
        content: TextField(
          controller: name,
          autofocus: true,
          maxLength: 64,
          textCapitalization: TextCapitalization.sentences,
          decoration: const InputDecoration(hintText: 'Name, like "Goa trip"'),
          onSubmitted: (v) => Navigator.pop(dialog, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(dialog), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(dialog, name.text), child: const Text('Choose photos')),
        ],
      ),
    );
    name.dispose();
    if (title == null || title.trim().isEmpty || !mounted) return;
    final random = Random.secure();
    final album = AlbumView(id: Uint8List.fromList(List.generate(16, (_) => random.nextInt(256))), title: title.trim());
    await _addTo(context, widget.chatId, album);
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final list = _list;
    return Scaffold(
      backgroundColor: Palette.bg,
      appBar: AppBar(title: const Text('Shared albums')),
      floatingActionButton: FloatingActionButton.extended(
        backgroundColor: widget.accent,
        foregroundColor: Palette.onColor(widget.accent),
        onPressed: _create,
        icon: const Icon(Icons.add_photo_alternate_rounded),
        label: const Text('New album'),
      ),
      body: list == null
          ? const Center(child: CircularProgressIndicator())
          : list.isEmpty
              ? Center(
                  child: Padding(
                    padding: const EdgeInsets.all(Space.xxl),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Icon(Icons.photo_library_outlined, size: 48, color: widget.accent),
                        const SizedBox(height: Space.md),
                        const Text('No albums yet', style: AppText.heading),
                        const SizedBox(height: Space.xs),
                        Text(
                          'Start one for a trip or a party. Everyone in ${widget.title} can see it and add their own '
                          'photos and videos.',
                          textAlign: TextAlign.center,
                          style: AppText.caption,
                        ),
                      ],
                    ),
                  ),
                )
              : RefreshIndicator(
                  onRefresh: _load,
                  child: GridView.builder(
                    padding: const EdgeInsets.fromLTRB(Space.lg, Space.sm, Space.lg, 96),
                    gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
                      crossAxisCount: 2,
                      mainAxisSpacing: Space.lg,
                      crossAxisSpacing: Space.md,
                      childAspectRatio: 0.82,
                    ),
                    itemCount: list.length,
                    itemBuilder: (context, i) {
                      final a = list[i];
                      final cover = a.items.first;
                      return GestureDetector(
                        onTap: () async {
                          await Navigator.of(context).push(MaterialPageRoute<void>(
                            builder: (_) => AlbumScreen(
                              chatId: widget.chatId,
                              album: AlbumView(id: Uint8List.fromList(a.id), title: a.title),
                              accent: widget.accent,
                            ),
                          ));
                          await _load();
                        },
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Expanded(
                              child: ClipRRect(
                                borderRadius: BorderRadius.circular(Radii.md),
                                child: IgnorePointer(
                                  child: MediaTile(media: cover.media!, chatId: widget.chatId, seq: cover.seq),
                                ),
                              ),
                            ),
                            const SizedBox(height: Space.sm),
                            Text(a.title, maxLines: 1, overflow: TextOverflow.ellipsis, style: AppText.bodyStrong),
                            Text(
                              a.items.length == 1 ? '1 item' : '${a.items.length} items',
                              style: AppText.micro,
                            ),
                          ],
                        ),
                      );
                    },
                  ),
                ),
    );
  }
}

/// One album's photos and videos, and a button to add more.
class AlbumScreen extends StatefulWidget {
  const AlbumScreen({super.key, required this.chatId, required this.album, required this.accent});

  final int chatId;
  final AlbumView album;
  final Color accent;

  @override
  State<AlbumScreen> createState() => _AlbumScreenState();
}

class _AlbumScreenState extends State<AlbumScreen> {
  List<MessageView>? _items;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final key = _key(widget.album.id);
    final all = await _albums(widget.chatId);
    final match = all.where((a) => _key(a.id) == key);
    if (mounted) setState(() => _items = match.isEmpty ? [] : match.first.items);
  }

  Future<void> _add() async {
    await _addTo(context, widget.chatId, widget.album);
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final items = _items;
    return Scaffold(
      backgroundColor: Palette.bg,
      appBar: AppBar(title: Text(widget.album.title)),
      floatingActionButton: FloatingActionButton.extended(
        backgroundColor: widget.accent,
        foregroundColor: Palette.onColor(widget.accent),
        onPressed: _add,
        icon: const Icon(Icons.add_photo_alternate_rounded),
        label: const Text('Add photos'),
      ),
      body: items == null
          ? const Center(child: CircularProgressIndicator())
          : RefreshIndicator(
              onRefresh: _load,
              child: GridView.builder(
                padding: const EdgeInsets.fromLTRB(2, 2, 2, 96),
                gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
                  crossAxisCount: 3,
                  mainAxisSpacing: 2,
                  crossAxisSpacing: 2,
                ),
                itemCount: items.length,
                itemBuilder: (context, i) => MediaTile(
                  key: ValueKey(items[i].seq),
                  media: items[i].media!,
                  chatId: widget.chatId,
                  seq: items[i].seq,
                ),
              ),
            ),
    );
  }
}
