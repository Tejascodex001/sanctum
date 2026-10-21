import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart' show Int64List;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../src/rust/api/group.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../sigil/sigil_painter.dart';
import '../theme/tokens.dart';
import '../sigil/sigil_look.dart';

/// Pick friends and a name; returns the new group's chat id.
Future<int?> showNewGroupSheet(
  BuildContext context, {
  required List<FriendView> friends,
  required Future<SigilSpec> Function(List<int>) sigil,
}) =>
    showModalBottomSheet<int>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => _FriendPicker(friends: friends, sigil: sigil, askName: true),
    );

/// Pick friends to add to an existing group.
Future<List<int>?> showAddMembersSheet(
  BuildContext context, {
  required List<FriendView> friends,
  required Future<SigilSpec> Function(List<int>) sigil,
}) =>
    showModalBottomSheet<List<int>>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => _FriendPicker(friends: friends, sigil: sigil, askName: false),
    );

class _FriendPicker extends StatefulWidget {
  const _FriendPicker({required this.friends, required this.sigil, required this.askName});

  final List<FriendView> friends;
  final Future<SigilSpec> Function(List<int>) sigil;

  /// Creating a group (name + friends) rather than adding to one.
  final bool askName;

  @override
  State<_FriendPicker> createState() => _FriendPickerState();
}

class _FriendPickerState extends State<_FriendPicker> {
  final _name = TextEditingController();
  final _picked = <int>{};
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _name.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  bool get _ready => _picked.isNotEmpty && (!widget.askName || _name.text.trim().isNotEmpty) && !_busy;

  Future<void> _done() async {
    HapticFeedback.mediumImpact();
    if (!widget.askName) return Navigator.pop(context, _picked.toList());
    setState(() => _busy = true);
    final id = await groupCreate(title: _name.text.trim(), friends: Int64List.fromList(_picked.toList()));
    if (mounted) Navigator.pop(context, id);
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.8),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
              child: Text(widget.askName ? 'New group' : 'Add members', style: AppText.title),
            ),
            if (widget.askName)
              Padding(
                padding: const EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.sm),
                child: TextField(
                  controller: _name,
                  autofocus: true,
                  maxLength: 64,
                  textCapitalization: TextCapitalization.sentences,
                  decoration: const InputDecoration(labelText: 'Group name', counterText: ''),
                ),
              ),
            Padding(
              padding: EdgeInsets.fromLTRB(Space.xl, Space.xs, Space.xl, Space.xs),
              child: Text(
                'Only friends can be added. Each joins as soon as their phone is reached; '
                'messages go straight between your phones.',
                style: AppText.caption,
              ),
            ),
            if (widget.friends.isEmpty)
              Padding(
                padding: EdgeInsets.all(Space.xl),
                child: Text('No friends to add yet.', textAlign: TextAlign.center, style: AppText.caption),
              ),
            Flexible(
              child: ListView(
                shrinkWrap: true,
                padding: const EdgeInsets.symmetric(horizontal: Space.md),
                children: [
                  for (final f in widget.friends)
                    CheckboxListTile(
                      value: _picked.contains(f.chatId.toInt()),
                      onChanged: (on) => setState(() {
                        HapticFeedback.selectionClick();
                        on == true ? _picked.add(f.chatId.toInt()) : _picked.remove(f.chatId.toInt());
                      }),
                      secondary: SizedBox.square(
                        dimension: 36,
                        child: FutureBuilder<SigilSpec>(
                          future: widget.sigil(f.sigilKey),
                          builder: (context, s) =>
                              s.hasData ? ClipOval(child: SigilView(spec: s.data!)) : const SizedBox(),
                        ),
                      ),
                      title: Text(f.name),
                      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(Radii.md)),
                    ),
                ],
              ),
            ),
            SafeArea(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(Space.xl, Space.sm, Space.xl, Space.md),
                child: FilledButton(
                  onPressed: _ready ? _done : null,
                  child: Text(widget.askName
                      ? (_picked.isEmpty ? 'Create group' : 'Create with ${_picked.length}')
                      : (_picked.isEmpty ? 'Add' : 'Add ${_picked.length}')),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Members, and (for the admin) rename / add / remove; for everyone, leave.
/// Returns true if we left the group.
Future<bool> showGroupSheet(
  BuildContext context, {
  required int chatId,
  required SigilSpec sigil,
  required Future<List<FriendView>> Function() friends,
  required Future<SigilSpec> Function(List<int>) sigilFor,
}) async =>
    await showModalBottomSheet<bool>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => _GroupSheet(chatId: chatId, sigil: sigil, friends: friends, sigilFor: sigilFor),
    ) ??
    false;

class _GroupSheet extends StatefulWidget {
  const _GroupSheet({required this.chatId, required this.sigil, required this.friends, required this.sigilFor});

  final int chatId;
  final SigilSpec sigil;
  final Future<List<FriendView>> Function() friends;
  final Future<SigilSpec> Function(List<int>) sigilFor;

  @override
  State<_GroupSheet> createState() => _GroupSheetState();
}

class _GroupSheetState extends State<_GroupSheet> {
  GroupDetails? _g;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final g = await groupDetails(chatId: widget.chatId);
    if (mounted) setState(() => _g = g);
  }

  Future<void> _rename() async {
    final controller = TextEditingController(text: _g!.title);
    final title = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Rename group'),
        content: TextField(
          controller: controller,
          autofocus: true,
          maxLength: 64,
          onSubmitted: (v) => Navigator.pop(context, v),
          decoration: const InputDecoration(helperText: 'Everyone in the group sees this name', counterText: ''),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
          TextButton(onPressed: () => Navigator.pop(context, controller.text), child: const Text('Save')),
        ],
      ),
    );
    controller.dispose();
    if (title == null || title.trim().isEmpty) return;
    await groupRename(chatId: widget.chatId, title: title);
    await _load();
  }

  Future<void> _add() async {
    final inGroup = {for (final m in _g!.members) m.key.join(',')};
    final candidates = [
      for (final f in await widget.friends())
        if (!inGroup.contains(f.key.join(','))) f
    ];
    if (!mounted) return;
    final picked = await showAddMembersSheet(context, friends: candidates, sigil: widget.sigilFor);
    if (picked == null || picked.isEmpty) return;
    await groupAdd(chatId: widget.chatId, friends: Int64List.fromList(picked));
    await _load();
  }

  Future<void> _remove(GroupMemberView m) async {
    final ok = await _confirm('Remove ${m.name}?', 'They\'re told, and stop getting this group\'s messages.', 'Remove');
    if (!ok) return;
    await groupRemove(chatId: widget.chatId, member: m.key);
    await _load();
  }

  Future<void> _leave() async {
    final admin = _g!.adminIsMe;
    final ok = await _confirm(
      'Leave ${_g!.title}?',
      admin
          ? 'You\'re its admin: after you leave, nobody can add or remove members. The group is deleted from this phone.'
          : 'Everyone is told, and the group is deleted from this phone.',
      'Leave',
    );
    if (!ok) return;
    HapticFeedback.heavyImpact();
    await groupLeave(chatId: widget.chatId);
    if (mounted) Navigator.pop(context, true);
  }

  Future<bool> _confirm(String title, String body, String action) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(title),
          content: Text(body),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('Cancel')),
            TextButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(action, style: const TextStyle(color: Palette.danger)),
            ),
          ],
        ),
      ) ??
      false;

  @override
  Widget build(BuildContext context) {
    final g = _g;
    if (g == null) return const SizedBox(height: 240, child: Center(child: CircularProgressIndicator()));
    final admin = g.adminIsMe && g.active;
    return ConstrainedBox(
      constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.85),
      child: ListView(
        shrinkWrap: true,
        padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.xl),
        children: [
          Center(child: SizedBox.square(dimension: 84, child: ClipOval(child: SigilView(spec: widget.sigil)))),
          const SizedBox(height: Space.md),
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Flexible(child: Text(g.title, textAlign: TextAlign.center, style: AppText.title)),
              if (admin)
                IconButton(tooltip: 'Rename', icon: const Icon(Icons.edit_rounded, size: 18), onPressed: _rename),
            ],
          ),
          Text(
            g.active
                ? '${g.members.length} members · end-to-end encrypted'
                : 'You\'re no longer in this group',
            textAlign: TextAlign.center,
            style: AppText.caption,
          ),
          const SizedBox(height: Space.lg),
          Row(
            children: [
              Expanded(child: Text('MEMBERS', style: AppText.overline)),
              if (admin)
                TextButton.icon(
                  onPressed: _add,
                  icon: const Icon(Icons.person_add_alt_1_rounded, size: 18),
                  label: const Text('Add'),
                ),
            ],
          ),
          for (final m in g.members)
            ListTile(
              contentPadding: const EdgeInsets.symmetric(horizontal: Space.xs),
              leading: CircleAvatar(
                backgroundColor: widget.sigil.look.accent.color.withValues(alpha: 0.2),
                child: Text(m.name.isEmpty ? '?' : m.name.characters.first.toUpperCase()),
              ),
              title: Text(m.name),
              subtitle: Text([
                if (m.admin) 'Admin',
                if (m.pending) 'Invited · joins when reachable',
              ].join(' · ')),
              trailing: admin && !m.me
                  ? IconButton(
                      tooltip: 'Remove',
                      icon: const Icon(Icons.remove_circle_outline_rounded, color: Palette.danger),
                      onPressed: () => _remove(m),
                    )
                  : null,
            ),
          const SizedBox(height: Space.lg),
          OutlinedButton.icon(
            onPressed: _leave,
            style: OutlinedButton.styleFrom(foregroundColor: Palette.danger),
            icon: const Icon(Icons.logout_rounded),
            label: Text(g.active ? 'Leave group' : 'Delete group'),
          ),
        ],
      ),
    );
  }
}
