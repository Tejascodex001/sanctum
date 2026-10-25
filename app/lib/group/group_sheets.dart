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
      builder: (_) => _FriendPicker(friends: friends, sigil: sigil, askName: true, room: groupMaxMembers() - 1),
    );

/// Who to add to a group, and whether they're shown the last week of it.
typedef AddedMembers = ({List<int> friends, bool history});

/// Pick friends to add to an existing group.
/// [room]: how many more people fit in the group.
Future<AddedMembers?> showAddMembersSheet(
  BuildContext context, {
  required List<FriendView> friends,
  required Future<SigilSpec> Function(List<int>) sigil,
  required int room,
}) =>
    showModalBottomSheet<AddedMembers>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (_) => _FriendPicker(friends: friends, sigil: sigil, askName: false, room: room),
    );

class _FriendPicker extends StatefulWidget {
  const _FriendPicker({required this.friends, required this.sigil, required this.askName, required this.room});

  final List<FriendView> friends;
  final Future<SigilSpec> Function(List<int>) sigil;

  /// Creating a group (name + friends) rather than adding to one.
  final bool askName;

  /// How many can be picked: groups have a size limit.
  final int room;

  @override
  State<_FriendPicker> createState() => _FriendPickerState();
}

class _FriendPickerState extends State<_FriendPicker> {
  final _name = TextEditingController();
  final _picked = <int>{};
  bool _history = false;
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
    if (!widget.askName) return Navigator.pop(context, (friends: _picked.toList(), history: _history));
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
                'Only friends can be added, up to ${groupMaxMembers()} people. Each joins as soon as their '
                'phone is reached. Messages go straight between your phones, and members pass them on, so '
                'they reach people even when whoever sent them is offline.',
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
                      // Full: only un-picking is possible.
                      onChanged: _picked.length >= widget.room && !_picked.contains(f.chatId.toInt())
                          ? null
                          : (on) => setState(() {
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
            if (!widget.askName)
              SwitchListTile(
                contentPadding: const EdgeInsets.symmetric(horizontal: Space.xl),
                value: _history,
                onChanged: (on) => setState(() => _history = on),
                title: const Text('Show them the last week'),
                subtitle: const Text(
                  'They see the text messages from the past 7 days (no photos or files), marked as earlier '
                  'messages. Everyone in the group is told.',
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

  /// Plain words for a refused change: the group's own rules are written for
  /// people; anything else gets a general line.
  String _explain(Object e) {
    final text = e.toString().replaceAll(RegExp(r'^\w*Exception\(|\)$'), '').trim();
    return text.endsWith('.') && text.length < 160 ? text : 'That didn\'t work. Try again in a moment.';
  }

  Future<void> _try(Future<void> Function() change, {String? done}) async {
    try {
      await change();
      if (done != null && mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(done)));
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(_explain(e))));
    }
    await _load();
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
    await _try(
      () => groupRename(chatId: widget.chatId, title: title),
      done: _g!.adminIsMe ? null : 'Asked the leader\'s phone to rename the group.',
    );
  }

  Future<void> _add() async {
    final inGroup = {for (final m in _g!.members) m.key.join(',')};
    final candidates = [
      for (final f in await widget.friends())
        if (!inGroup.contains(f.key.join(','))) f
    ];
    if (!mounted) return;
    final room = groupMaxMembers() - _g!.members.length;
    if (room <= 0) {
      ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('This group is full: groups can have up to ${groupMaxMembers()} people.')));
      return;
    }
    final picked = await showAddMembersSheet(context, friends: candidates, sigil: widget.sigilFor, room: room);
    if (picked == null || picked.friends.isEmpty) return;
    await _try(
      () => groupAdd(
        chatId: widget.chatId,
        friends: Int64List.fromList(picked.friends),
        shareHistory: picked.history,
      ),
      done: _g!.adminIsMe ? null : 'Asked the leader\'s phone to add them. They join once it\'s online.',
    );
  }

  Future<void> _remove(GroupMemberView m) async {
    final ok = await _confirm('Remove ${m.name}?', 'They\'re told, and stop getting this group\'s messages.', 'Remove');
    if (!ok) return;
    await _try(
      () => groupRemove(chatId: widget.chatId, member: m.key),
      done: _g!.adminIsMe ? null : 'Asked the leader\'s phone to remove ${m.name}.',
    );
  }

  Future<void> _makeAdmin(GroupMemberView m) async {
    final ok = await _confirm(
      'Make ${m.name} an admin?',
      'Admins can add and remove members and rename the group. The requests go through your phone, so you stay '
          'the leader.',
      'Make admin',
    );
    if (!ok) return;
    await _try(() => groupMakeAdmin(chatId: widget.chatId, member: m.key));
  }

  Future<void> _removeAdmin(GroupMemberView m) async {
    final ok = await _confirm('Remove ${m.name} as admin?', 'They stay in the group as an ordinary member.', 'Remove');
    if (!ok) return;
    await _try(() => groupRemoveAdmin(chatId: widget.chatId, member: m.key));
  }

  Future<void> _handOver(GroupMemberView m) async {
    final ok = await _confirm(
      'Make ${m.name} the leader?',
      'They take over the group and you become an admin. Only the leader can make or remove admins, so you '
          'can\'t undo this yourself.',
      'Hand over',
    );
    if (!ok) return;
    await _try(() => groupHandOver(chatId: widget.chatId, member: m.key));
  }

  Future<void> _leave() async {
    final g = _g!;
    final others = g.members.where((m) => m.admin && !m.me).length;
    if (g.adminIsMe && others > 0) {
      await showDialog<void>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('Hand over leadership first'),
          content: const Text(
            'You\'re the group\'s leader. Pick one of the admins and choose "Make leader" from their menu, '
            'then you can leave.',
          ),
          actions: [TextButton(onPressed: () => Navigator.pop(context), child: const Text('OK'))],
        ),
      );
      return;
    }
    final ok = await _confirm(
      'Leave ${g.title}?',
      g.adminIsMe
          ? 'You\'re its leader: after you leave, nobody can add or remove members. The group is deleted from this phone.'
          : 'Everyone is told, and the group is deleted from this phone.',
      'Leave',
    );
    if (!ok) return;
    HapticFeedback.heavyImpact();
    try {
      await groupLeave(chatId: widget.chatId);
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(_explain(e))));
      return;
    }
    if (mounted) Navigator.pop(context, true);
  }

  /// What the leader or an admin can do with [m].
  Widget? _menu(GroupDetails g, GroupMemberView m) {
    if (!g.active || m.me || m.leader) return null;
    final leader = g.adminIsMe;
    final items = <(String, VoidCallback, bool)>[
      if (leader && !m.admin && !m.pending) ('Make admin', () => _makeAdmin(m), false),
      if (m.admin && g.iAmAdmin) ('Remove as admin', () => _removeAdmin(m), false),
      if (leader && m.admin) ('Make leader', () => _handOver(m), false),
      // Only the leader can remove an admin from the group.
      if (leader || !m.admin) ('Remove from group', () => _remove(m), true),
    ];
    if (items.isEmpty) return null;
    return PopupMenuButton<int>(
      tooltip: 'Options for ${m.name}',
      icon: const Icon(Icons.more_vert_rounded),
      onSelected: (i) => items[i].$2(),
      itemBuilder: (_) => [
        for (var i = 0; i < items.length; i++)
          PopupMenuItem(
            value: i,
            child: Text(items[i].$1, style: items[i].$3 ? const TextStyle(color: Palette.danger) : null),
          ),
      ],
    );
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
    final admin = g.iAmAdmin && g.active;
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
            g.active ? '${g.members.length} members · end-to-end encrypted' : 'You\'re no longer in this group',
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
                if (m.leader) 'Leader' else if (m.admin) 'Admin',
                if (m.pending) 'Invited · joins when reachable',
              ].join(' · ')),
              trailing: admin ? _menu(g, m) : null,
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
