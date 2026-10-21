import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../sigil/sigil_painter.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';

/// Sending something (a reel's link) to chats: a grid of sigils to tap,
/// any number at once, with an optional message, like sharing on
/// Instagram. Returns how many chats it went to.
Future<int> showShareSheet(BuildContext context, {required String text}) async {
  final chats = (await chatList()).where((c) => !c.hidden && c.active).toList();
  final sigils = <int, SigilSpec>{
    for (final c in chats) c.id: await sigilForKey(publicKey: c.sigilKey),
  };
  if (!context.mounted) return 0;
  final sent = await showModalBottomSheet<int>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (_) => _ShareSheet(chats: chats, sigils: sigils, text: text),
  );
  return sent ?? 0;
}

class _ShareSheet extends StatefulWidget {
  const _ShareSheet({required this.chats, required this.sigils, required this.text});

  final List<StoredChat> chats;
  final Map<int, SigilSpec> sigils;
  final String text;

  @override
  State<_ShareSheet> createState() => _ShareSheetState();
}

class _ShareSheetState extends State<_ShareSheet> {
  final _picked = <int>{};
  final _search = TextEditingController();
  final _note = TextEditingController();
  var _sending = false;

  @override
  void initState() {
    super.initState();
    _search.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _search.dispose();
    _note.dispose();
    super.dispose();
  }

  Future<void> _send() async {
    setState(() => _sending = true);
    final note = _note.text.trim();
    final body = note.isEmpty ? widget.text : '$note\n${widget.text}';
    for (final id in _picked) {
      await messageSend(chatId: id, body: body);
    }
    HapticFeedback.lightImpact();
    if (mounted) Navigator.pop(context, _picked.length);
  }

  @override
  Widget build(BuildContext context) {
    final query = _search.text.trim().toLowerCase();
    final shown = [
      for (final c in widget.chats)
        if (query.isEmpty || c.title.toLowerCase().contains(query)) c,
    ];
    final accent = Theme.of(context).colorScheme.primary;
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: SizedBox(
        height: MediaQuery.sizeOf(context).height * 0.62,
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.sm),
              child: SizedBox(
                height: 42,
                child: TextField(
                  controller: _search,
                  decoration: const InputDecoration(
                    hintText: 'Search',
                    prefixIcon: Icon(Icons.search_rounded, size: 20),
                    contentPadding: EdgeInsets.zero,
                  ),
                ),
              ),
            ),
            Expanded(
              child: shown.isEmpty
                  ? Center(child: Text('No chats to send to', style: AppText.caption))
                  : GridView.builder(
                      padding: const EdgeInsets.symmetric(horizontal: Space.md, vertical: Space.sm),
                      gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
                        crossAxisCount: 4,
                        mainAxisSpacing: Space.sm,
                        childAspectRatio: 0.72,
                      ),
                      itemCount: shown.length,
                      itemBuilder: (context, i) {
                        final c = shown[i];
                        final on = _picked.contains(c.id);
                        return GestureDetector(
                          behavior: HitTestBehavior.opaque,
                          onTap: () {
                            HapticFeedback.selectionClick();
                            setState(() => on ? _picked.remove(c.id) : _picked.add(c.id));
                          },
                          child: Column(
                            children: [
                              Stack(
                                clipBehavior: Clip.none,
                                children: [
                                  AnimatedScale(
                                    duration: Motion.fast,
                                    scale: on ? 0.9 : 1,
                                    child: SizedBox.square(
                                      dimension: 64,
                                      child: ClipOval(child: SigilView(spec: widget.sigils[c.id]!)),
                                    ),
                                  ),
                                  if (on)
                                    Positioned(
                                      right: -2,
                                      bottom: -2,
                                      child: Container(
                                        padding: const EdgeInsets.all(2),
                                        decoration: BoxDecoration(
                                          color: accent,
                                          shape: BoxShape.circle,
                                          border: Border.all(color: Palette.surface, width: 2),
                                        ),
                                        child: Icon(Icons.check_rounded, size: 16, color: Palette.onColor(accent)),
                                      ),
                                    ),
                                ],
                              ),
                              const SizedBox(height: Space.xs),
                              Text(
                                c.title,
                                maxLines: 2,
                                textAlign: TextAlign.center,
                                overflow: TextOverflow.ellipsis,
                                style: AppText.micro.copyWith(color: on ? Palette.text : Palette.textMuted),
                              ),
                            ],
                          ),
                        );
                      },
                    ),
            ),
            AnimatedSize(
              duration: Motion.medium,
              curve: Motion.decelerate,
              child: _picked.isEmpty
                  ? const SizedBox(width: double.infinity)
                  : SafeArea(
                      top: false,
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(Space.lg, Space.sm, Space.lg, Space.md),
                        child: Column(
                          children: [
                            TextField(
                              controller: _note,
                              textCapitalization: TextCapitalization.sentences,
                              decoration: const InputDecoration(hintText: 'Write a message…'),
                            ),
                            const SizedBox(height: Space.sm),
                            SizedBox(
                              width: double.infinity,
                              child: FilledButton(
                                onPressed: _sending ? null : _send,
                                child: Text(_picked.length == 1 ? 'Send' : 'Send separately'),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
            ),
          ],
        ),
      ),
    );
  }
}
