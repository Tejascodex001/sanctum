import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../identity/secrets.dart';
import '../security/panic.dart';
import '../security/pin_settings.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/fedi.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../theme/appearance.dart';
import '../theme/tokens.dart';
import 'connection_settings.dart';

/// Everything you can change, opened from your sigil: your name, how the
/// app looks, the lock, staying connected, nearby, updates and the panic
/// wipe.
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({super.key, required this.mySigil});

  final SigilSpec mySigil;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  String _name = '';

  @override
  void initState() {
    super.initState();
    loadDisplayName().then((n) {
      if (mounted) setState(() => _name = n);
    });
  }

  Future<void> _rename() async {
    final input = TextEditingController(text: _name);
    final name = await showDialog<String>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Your name'),
        content: TextField(
          controller: input,
          autofocus: true,
          maxLength: 40,
          textCapitalization: TextCapitalization.words,
          decoration: const InputDecoration(hintText: 'What friends see when you add them'),
          onSubmitted: (v) => Navigator.pop(dialog, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(dialog), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(dialog, input.text), child: const Text('Save')),
        ],
      ),
    );
    input.dispose();
    if (name == null || name.trim().isEmpty) return;
    await saveDisplayName(name);
    await profileSetName(name: name.trim());
    if (mounted) setState(() => _name = name.trim());
  }

  @override
  Widget build(BuildContext context) {
    final sigil = widget.mySigil;
    return Scaffold(
      appBar: AppBar(title: const Text('Settings')),
      body: ListView(
        padding: EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.xl + MediaQuery.paddingOf(context).bottom),
        children: [
          // You: your sigil and name.
          Container(
            padding: const EdgeInsets.all(Space.lg),
            decoration: BoxDecoration(
              color: sigil.background.color,
              borderRadius: BorderRadius.circular(Radii.xl),
              border: Border.all(color: sigil.accent.color.withValues(alpha: 0.35)),
            ),
            child: Row(
              children: [
                SizedBox.square(
                  dimension: 96,
                  child: ClipOval(child: LivingSigil(spec: sigil, period: 60)),
                ),
                const SizedBox(width: Space.lg),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        _name.isEmpty ? 'Add your name' : _name,
                        style: AppText.title.copyWith(color: Palette.onColor(sigil.background.color)),
                      ),
                      const SizedBox(height: Space.xs),
                      Text(
                        'Your sigil is made just for you. A friend and you see the same one for your chat.',
                        style: AppText.caption.copyWith(
                          color: Palette.onColor(sigil.background.color).withValues(alpha: 0.7),
                        ),
                      ),
                      const SizedBox(height: Space.sm),
                      OutlinedButton.icon(
                        onPressed: _rename,
                        style: OutlinedButton.styleFrom(
                          minimumSize: const Size(0, 36),
                          foregroundColor: Palette.onColor(sigil.background.color),
                        ),
                        icon: const Icon(Icons.edit_rounded, size: 16),
                        label: const Text('Change name'),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          const _Section('APPEARANCE'),
          _Group(children: [_AppearanceSettings(sigilAccent: sigil.accent.color)]),
          const _Section('REELS'),
          const _Group(children: [_ReelsSettings()]),
          const _Section('LOCK'),
          const PinSettings(),
          const _Section('STAYING CONNECTED'),
          const BackgroundSettings(),
          const SizedBox(height: Space.md),
          const NearbySettings(),
          const _Section('UPDATES'),
          const UpdateSettings(),
          const _Section('EMERGENCY'),
          _Group(
            children: [
              ListTile(
                leading: const Icon(Icons.delete_forever_rounded, color: Palette.danger),
                title: const Text('Panic wipe', style: TextStyle(color: Palette.danger)),
                subtitle: const Text('Erase everything from this phone'),
                onTap: () => showPanicSheet(context),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Section extends StatelessWidget {
  const _Section(this.label);

  final String label;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(Space.xs, Space.xl, Space.xs, Space.sm),
        child: Text(label, style: AppText.overline),
      );
}

/// A rounded panel, like the other settings.
class _Group extends StatelessWidget {
  const _Group({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) => Material(
        color: Palette.surface,
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Radii.md),
          side: BorderSide(color: Palette.outline),
        ),
        child: Column(children: children),
      );
}

/// Dark, light or like the phone, and the accent colour.
class _AppearanceSettings extends StatelessWidget {
  const _AppearanceSettings({required this.sigilAccent});

  final Color sigilAccent;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([Appearance.theme, Appearance.accent]),
      builder: (context, _) {
        final picked = Appearance.accent.value;
        return Padding(
          padding: const EdgeInsets.all(Space.lg),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text('Theme', style: AppText.bodyStrong),
              const SizedBox(height: Space.sm),
              SegmentedButton<ThemeChoice>(
                segments: const [
                  ButtonSegment(value: ThemeChoice.dark, label: Text('Dark'), icon: Icon(Icons.dark_mode_rounded)),
                  ButtonSegment(value: ThemeChoice.light, label: Text('Light'), icon: Icon(Icons.light_mode_rounded)),
                  ButtonSegment(
                    value: ThemeChoice.phone,
                    label: Text('Like phone'),
                    icon: Icon(Icons.brightness_auto_rounded),
                  ),
                ],
                selected: {Appearance.theme.value},
                showSelectedIcon: false,
                onSelectionChanged: (s) {
                  HapticFeedback.selectionClick();
                  Appearance.setTheme(s.first);
                },
              ),
              const SizedBox(height: Space.lg),
              Text('Accent colour', style: AppText.bodyStrong),
              const SizedBox(height: Space.xs),
              Text(
                picked == null ? 'From your sigil' : 'Your pick',
                style: AppText.caption,
              ),
              const SizedBox(height: Space.md),
              Wrap(
                spacing: Space.md,
                runSpacing: Space.md,
                children: [
                  _Swatch(
                    color: sigilAccent,
                    selected: picked == null,
                    label: 'From your sigil',
                    icon: Icons.auto_awesome_rounded,
                    onTap: () => Appearance.setAccent(null),
                  ),
                  for (final c in Palette.accents)
                    _Swatch(
                      color: c,
                      selected: picked?.toARGB32() == c.toARGB32(),
                      label: 'Accent colour',
                      onTap: () => Appearance.setAccent(c),
                    ),
                ],
              ),
            ],
          ),
        );
      },
    );
  }
}

class _Swatch extends StatelessWidget {
  const _Swatch({required this.color, required this.selected, required this.label, required this.onTap, this.icon});

  final Color color;
  final bool selected;
  final String label;
  final IconData? icon;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Semantics(
        button: true,
        selected: selected,
        label: label,
        child: GestureDetector(
          onTap: () {
            HapticFeedback.selectionClick();
            onTap();
          },
          child: AnimatedContainer(
            duration: Motion.fast,
            width: 40,
            height: 40,
            padding: const EdgeInsets.all(3),
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              border: Border.all(color: selected ? Palette.text : Colors.transparent, width: 2),
            ),
            child: DecoratedBox(
              decoration: BoxDecoration(color: color, shape: BoxShape.circle),
              child: icon == null ? null : Icon(icon, size: 16, color: Palette.onColor(color)),
            ),
          ),
        ),
      );
}

/// Reels through Tor (private, slower) or straight from the sites (fast).
class _ReelsSettings extends StatefulWidget {
  const _ReelsSettings();

  @override
  State<_ReelsSettings> createState() => _ReelsSettingsState();
}

class _ReelsSettingsState extends State<_ReelsSettings> {
  bool? _direct;

  @override
  void initState() {
    super.initState();
    fediDirect().then((on) {
      if (mounted) setState(() => _direct = on);
    });
  }

  @override
  Widget build(BuildContext context) {
    final direct = _direct;
    if (direct == null) return const SizedBox(height: 64);
    return Column(
      children: [
        _switch(direct),
        ListTile(
          leading: const Icon(Icons.restart_alt_rounded),
          title: const Text('Start Reels afresh'),
          subtitle: Text('Forget what Reels learned you like', style: AppText.caption),
          onTap: _forget,
        ),
      ],
    );
  }

  Future<void> _forget() async {
    final sure = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Start Reels afresh?'),
        content: const Text('Reels forgets your hearts, what you watched and what you skipped, and starts '
            'picking from scratch.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(dialog, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(dialog, true), child: const Text('Start afresh')),
        ],
      ),
    );
    if (sure != true) return;
    await reelsForgetTaste();
    if (mounted) ScaffoldMessenger.of(context).showSnackBar(const SnackBar(content: Text('Reels will start afresh.')));
  }

  Widget _switch(bool direct) {
    return SwitchListTile(
      value: direct,
      title: const Text('Faster reels'),
      subtitle: Text(
        direct
            ? 'Reels come straight from the video sites, so they can see your internet address. Your chats stay '
                'private either way.'
            : 'Reels come privately through Tor, which is slower. Turn on to load them straight from the sites: '
                'faster, but the sites can see your internet address.',
        style: AppText.caption,
      ),
      onChanged: (v) async {
        HapticFeedback.selectionClick();
        setState(() => _direct = v);
        await fediSetDirect(enabled: v);
      },
    );
  }
}
