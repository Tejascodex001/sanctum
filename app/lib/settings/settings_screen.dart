import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../identity/secrets.dart';
import '../security/panic.dart';
import '../security/pin_settings.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/call.dart';
import '../src/rust/api/moment.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../theme/appearance.dart';
import '../theme/tokens.dart';
import 'connection_settings.dart';
import 'feedback_screen.dart';
import '../sigil/sigil_look.dart';

/// Everything you can change, opened from your sigil: your name, how the
/// app looks, calls, the lock, staying connected, nearby, updates and
/// the panic wipe.
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
              color: sigil.look.background.color,
              borderRadius: BorderRadius.circular(Radii.xl),
              border: Border.all(color: sigil.look.accent.color.withValues(alpha: 0.35)),
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
                        style: AppText.title.copyWith(color: Palette.onColor(sigil.look.background.color)),
                      ),
                      const SizedBox(height: Space.xs),
                      Text(
                        'Your sigil is made just for you. A friend and you see the same one for your chat.',
                        style: AppText.caption.copyWith(
                          color: Palette.onColor(sigil.look.background.color).withValues(alpha: 0.7),
                        ),
                      ),
                      const SizedBox(height: Space.sm),
                      OutlinedButton.icon(
                        onPressed: _rename,
                        style: OutlinedButton.styleFrom(
                          minimumSize: const Size(0, 36),
                          foregroundColor: Palette.onColor(sigil.look.background.color),
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
          const SizedBox(height: Space.xl),
          _Group(
            children: [
              _Entry(
                icon: Icons.palette_rounded,
                color: const Color(0xFF8B7CFF),
                title: 'Appearance',
                subtitle: 'Light or dark, and your colour',
                page: () => _Page('Appearance', [
                  _Group(children: [_AppearanceSettings(sigilAccent: sigil.accent.color)]),
                ]),
              ),
              _Entry(
                icon: Icons.photo_camera_rounded,
                color: const Color(0xFFFFB238),
                title: 'Moments',
                subtitle: 'Who saw your photo of the day',
                page: () => const _Page('Moments', [
                  _Group(children: [MomentSettings()]),
                ]),
              ),
              _Entry(
                icon: Icons.call_rounded,
                color: const Color(0xFF3CC87A),
                title: 'Calls',
                subtitle: 'Private or faster calls',
                page: () => const _Page('Calls', [
                  _Group(children: [CallsSettings()]),
                ]),
              ),
            ],
          ),
          const SizedBox(height: Space.lg),
          _Group(
            children: [
              _Entry(
                icon: Icons.lock_rounded,
                color: const Color(0xFF5B9DFF),
                title: 'Lock',
                subtitle: 'PIN, fingerprint and decoy',
                page: () => const _Page('Lock', [PinSettings()]),
              ),
              _Entry(
                icon: Icons.wifi_tethering_rounded,
                color: const Color(0xFF3CC8C8),
                title: 'Staying connected',
                subtitle: 'Messages while closed, and nearby',
                page: () => const _Page('Staying connected', [
                  BackgroundSettings(),
                  SizedBox(height: Space.md),
                  NearbySettings(),
                ]),
              ),
            ],
          ),
          const SizedBox(height: Space.lg),
          _Group(
            children: [
              _Entry(
                icon: Icons.system_update_rounded,
                color: const Color(0xFFFF8A4C),
                title: 'Updates',
                subtitle: 'New versions of Sanctum',
                page: () => const _Page('Updates', [UpdateSettings()]),
              ),
              _Entry(
                icon: Icons.favorite_rounded,
                color: const Color(0xFFFF5D8F),
                title: 'Feedback',
                subtitle: 'Tell the developer what you think',
                page: () => const _Page('Feedback', [FeedbackSettings()]),
              ),
            ],
          ),
          const SizedBox(height: Space.lg),
          _Group(
            children: [
              _Entry(
                icon: Icons.delete_forever_rounded,
                color: Palette.danger,
                title: 'Panic wipe',
                subtitle: 'Erase everything from this phone',
                danger: true,
                onTap: () => showPanicSheet(context),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// One row of Settings: a coloured icon tile, what it is, and an arrow to
/// its own page.
class _Entry extends StatelessWidget {
  const _Entry({
    required this.icon,
    required this.color,
    required this.title,
    required this.subtitle,
    this.page,
    this.onTap,
    this.danger = false,
  });

  final IconData icon;
  final Color color;
  final String title;
  final String subtitle;
  final Widget Function()? page;
  final VoidCallback? onTap;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    return InkWell(
      onTap: () {
        HapticFeedback.selectionClick();
        if (page != null) {
          Navigator.push(context, MaterialPageRoute<void>(builder: (_) => page!()));
        } else {
          onTap?.call();
        }
      },
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Space.lg, vertical: Space.md),
        child: Row(
          children: [
            Container(
              width: 40,
              height: 40,
              decoration: BoxDecoration(
                color: color.withValues(alpha: Palette.light ? 0.16 : 0.2),
                borderRadius: BorderRadius.circular(Radii.sm),
              ),
              child: Icon(icon, color: color, size: 22),
            ),
            const SizedBox(width: Space.md),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: AppText.bodyStrong.copyWith(color: danger ? Palette.danger : Palette.text)),
                  const SizedBox(height: 2),
                  Text(subtitle, style: AppText.caption),
                ],
              ),
            ),
            if (page != null) Icon(Icons.chevron_right_rounded, color: Palette.textFaint),
          ],
        ),
      ),
    );
  }
}

/// A settings page of its own.
class _Page extends StatelessWidget {
  const _Page(this.title, this.children);

  final String title;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(title: Text(title)),
        body: ListView(
          padding: EdgeInsets.fromLTRB(Space.lg, Space.sm, Space.lg, Space.xl + MediaQuery.paddingOf(context).bottom),
          children: children,
        ),
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

/// Whether friends learn when you've seen their moments, and you see who saw
/// yours: both or neither.
class MomentSettings extends StatefulWidget {
  const MomentSettings({super.key});

  @override
  State<MomentSettings> createState() => _MomentSettingsState();
}

class _MomentSettingsState extends State<MomentSettings> {
  bool? _shared;

  @override
  void initState() {
    super.initState();
    momentViewsShared().then((on) {
      if (mounted) setState(() => _shared = on);
    }, onError: (_) {
      if (mounted) setState(() => _shared = true);
    });
  }

  @override
  Widget build(BuildContext context) {
    final shared = _shared;
    if (shared == null) return const SizedBox(height: 64);
    return SwitchListTile(
      value: shared,
      title: const Text('Show who saw moments'),
      subtitle: Text(
        shared
            ? 'Friends see when you\'ve looked at their moment, and you see who looked at yours.'
            : 'Friends don\'t learn when you look at their moment, and you don\'t see who looked at yours.',
        style: AppText.caption,
      ),
      onChanged: (v) async {
        HapticFeedback.selectionClick();
        setState(() => _shared = v);
        await setMomentViewsShared(shared: v);
      },
    );
  }
}

/// Calls through Tor (private, a little slower) or straight between the
/// phones when both friends allow it ("Faster calls").
class CallsSettings extends StatefulWidget {
  const CallsSettings({super.key});

  @override
  State<CallsSettings> createState() => _CallsSettingsState();
}

class _CallsSettingsState extends State<CallsSettings> {
  bool? _direct;

  @override
  void initState() {
    super.initState();
    callsDirect().then((on) {
      if (mounted) setState(() => _direct = on);
    }, onError: (_) {
      if (mounted) setState(() => _direct = false);
    });
  }

  @override
  Widget build(BuildContext context) {
    final direct = _direct;
    if (direct == null) return const SizedBox(height: 64);
    return SwitchListTile(
      value: direct,
      title: const Text('Faster calls'),
      subtitle: Text(
        direct
            ? 'When a friend has this on too, calls connect straight between your phones: less delay and sharper '
                'video, but their phone learns your internet address. Your messages stay private either way.'
            : 'Calls go privately through Tor, so nobody learns who you talk to, with a little delay. Turn on to '
                'connect straight between your phones when a friend has it on too: less delay, but their phone '
                'learns your internet address.',
        style: AppText.caption,
      ),
      onChanged: (v) async {
        HapticFeedback.selectionClick();
        setState(() => _direct = v);
        await callsSetDirect(enabled: v);
      },
    );
  }
}
