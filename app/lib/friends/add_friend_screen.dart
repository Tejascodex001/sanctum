import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:share_plus/share_plus.dart';

import '../identity/secrets.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';
import '../theme/tokens.dart';
import 'qr_codec.dart';
import 'scanner.dart';

/// Adding a friend: each person reads the other's code. In person, scan
/// each other's QR; far away, share your code and type or paste theirs.
/// Then both compare the shared sigil, or the safety number on a call.
///
/// Pops `true` when a friend was added.
class AddFriendScreen extends StatefulWidget {
  const AddFriendScreen({super.key, required this.mySigil});

  final SigilSpec mySigil;

  @override
  State<AddFriendScreen> createState() => _AddFriendScreenState();
}

class _AddFriendScreenState extends State<AddFriendScreen> {
  final _name = TextEditingController();
  final _typed = TextEditingController();
  String? _myCode;
  PairedView? _paired;
  SigilSpec? _pairSigil;
  SigilSpec? _friendSigil;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    loadDisplayName().then((n) {
      _name.text = n;
      _refreshCode();
    });
  }

  @override
  void dispose() {
    _name.dispose();
    _typed.dispose();
    super.dispose();
  }

  Future<void> _refreshCode() async {
    final code = await myCode(myName: _name.text);
    if (mounted) setState(() => _myCode = code);
  }

  Future<void> _nameChanged(String name) async {
    await saveDisplayName(name);
    await _refreshCode();
  }

  Future<void> _add(String text, {required bool scanned}) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      final paired = await friendAdd(code: text, scanned: scanned);
      final sigil = await sigilForKey(publicKey: paired.sigilKey);
      _friendSigil = await sigilForKey(publicKey: paired.friendKey);
      HapticFeedback.heavyImpact();
      setState(() {
        _paired = paired;
        _pairSigil = sigil;
      });
    } catch (e) {
      HapticFeedback.vibrate();
      _toast(_friendly(e));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    try {
      final text = await scanQr(prompt: "Point at your friend's Sanctum code");
      if (text != null) await _add(text, scanned: true);
    } on PlatformException catch (e) {
      _toast('Camera unavailable: ${e.message ?? e.code}');
    }
  }

  Future<void> _paste() async {
    final text = (await Clipboard.getData(Clipboard.kTextPlain))?.text;
    if (text == null || text.trim().isEmpty) {
      _toast('Nothing to paste.');
      return;
    }
    _typed.text = text.trim();
    setState(() {});
  }

  String _friendly(Object e) {
    final s = e.toString();
    for (final (needle, msg) in [
      ('already friends', 'You are already friends.'),
      ('own code', "That's your own code."),
      ('typo', 'That code has a typo. Check it and try again.'),
      ('damaged', 'That code is incomplete.'),
      ('not a Sanctum code', "That isn't a Sanctum code."),
    ]) {
      if (s.contains(needle)) return msg;
    }
    return "Couldn't read that code. Try again.";
  }

  void _toast(String msg) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(msg)));
  }

  Future<void> _reject() async {
    final paired = _paired;
    if (paired != null) await chatDelete(id: paired.chatId);
    if (!mounted) return;
    _toast('Friend not added. Someone may have swapped a code; try again.');
    setState(() {
      _paired = null;
      _typed.clear();
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: Palette.bg,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('Add a friend'),
        leading: IconButton(
          tooltip: 'Back',
          icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 20),
          onPressed: () => Navigator.pop(context, false),
        ),
      ),
      body: SafeArea(
        child: AnimatedSwitcher(
          duration: const Duration(milliseconds: 380),
          switchInCurve: Curves.easeOutCubic,
          transitionBuilder: (child, anim) => FadeTransition(
            opacity: anim,
            child: ScaleTransition(scale: Tween(begin: 0.97, end: 1.0).animate(anim), child: child),
          ),
          child: _paired == null ? _exchange() : _confirm(),
        ),
      ),
    );
  }

  Widget _exchange() {
    final accent = widget.mySigil.accent.color;
    return ListView(
      key: const ValueKey('exchange'),
      padding: const EdgeInsets.fromLTRB(20, 4, 20, 32),
      children: [
        const Text(
          'Each of you reads the other\'s code. Together: scan each other\'s QR. '
          'Far away: share yours, and type or paste theirs.',
          style: AppText.caption,
        ),
        const SizedBox(height: 20),
        _Section(
          title: 'YOUR CODE',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _name,
                textCapitalization: TextCapitalization.words,
                textInputAction: TextInputAction.done,
                onSubmitted: _nameChanged,
                onTapOutside: (_) {
                  FocusScope.of(context).unfocus();
                  _nameChanged(_name.text);
                },
                decoration: InputDecoration(
                  labelText: 'Your name',
                  helperText: 'Shown only to friends who read your code',
                  isDense: true,
                ),
              ),
              const SizedBox(height: 18),
              Center(
                child: AnimatedSwitcher(
                  duration: const Duration(milliseconds: 250),
                  child: _myCode == null
                      ? const SizedBox.square(dimension: 240)
                      : Container(
                          key: ValueKey(_myCode),
                          width: 240,
                          height: 240,
                          padding: const EdgeInsets.all(16),
                          decoration: BoxDecoration(
                            color: Colors.white,
                            borderRadius: BorderRadius.circular(24),
                            boxShadow: [BoxShadow(color: accent.withValues(alpha: 0.3), blurRadius: 32)],
                          ),
                          child: QrImageView.withQr(
                            qr: qrForCode(_myCode!),
                            backgroundColor: Colors.white,
                            padding: EdgeInsets.zero,
                          ),
                        ),
                ),
              ),
              const SizedBox(height: 16),
              if (_myCode != null)
                SelectableText(
                  codeGroups(_myCode!),
                  textAlign: TextAlign.center,
                  style: AppText.mono.copyWith(fontSize: 16),
                ),
              const SizedBox(height: 12),
              Row(
                children: [
                  Expanded(
                    child: OutlinedButton.icon(
                      onPressed: _myCode == null
                          ? null
                          : () => SharePlus.instance.share(
                                ShareParams(text: 'Add me on Sanctum:\n\n$_myCode'),
                              ),
                      icon: const Icon(Icons.ios_share_rounded, size: 18),
                      label: const Text('Share'),
                    ),
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: OutlinedButton.icon(
                      onPressed: _myCode == null
                          ? null
                          : () {
                              Clipboard.setData(ClipboardData(text: _myCode!));
                              _toast('Copied');
                            },
                      icon: const Icon(Icons.copy_rounded, size: 18),
                      label: const Text('Copy'),
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        _Section(
          title: 'THEIR CODE',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              FilledButton.icon(
                style: FilledButton.styleFrom(minimumSize: const Size.fromHeight(56)),
                onPressed: _busy ? null : _scan,
                icon: const Icon(Icons.qr_code_scanner_rounded),
                label: const Text('Scan their QR'),
              ),
              const SizedBox(height: 14),
              TextField(
                controller: _typed,
                minLines: 2,
                maxLines: 4,
                autocorrect: false,
                enableSuggestions: false,
                textCapitalization: TextCapitalization.characters,
                style: AppText.mono,
                onChanged: (_) => setState(() {}),
                decoration: InputDecoration(
                  hintText: 'or type / paste their code',
                  suffixIcon: IconButton(tooltip: 'Paste', icon: const Icon(Icons.content_paste_rounded), onPressed: _paste),
                ),
              ),
              const SizedBox(height: 10),
              FilledButton.tonal(
                onPressed: _busy || _typed.text.trim().isEmpty ? null : () => _add(_typed.text, scanned: false),
                child: _busy
                    ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Text('Add friend'),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _confirm() {
    final paired = _paired!;
    final accent = widget.mySigil.accent.color;
    const verifiedColor = Color(0xFF3DDC97);
    const pendingColor = Color(0xFFFFC145);
    return ListView(
      key: const ValueKey('confirm'),
      padding: const EdgeInsets.all(Space.xl),
      children: [
        _SigilEquation(mine: widget.mySigil, theirs: _friendSigil!, shared: _pairSigil!, friend: paired.friend),
        const SizedBox(height: Space.xl),
        Center(
          child: _Badge(
            color: paired.verified ? verifiedColor : pendingColor,
            icon: paired.verified ? Icons.verified_user_rounded : Icons.shield_outlined,
            label: paired.verified ? 'Verified in person' : 'Not verified yet',
          ),
        ),
        const SizedBox(height: Space.lg),
        Text(
          paired.verified ? '${paired.friend} is added' : 'Is this really ${paired.friend}?',
          textAlign: TextAlign.center,
          style: AppText.title,
        ),
        const SizedBox(height: Space.sm),
        Text(
          'Your chat\'s sigil is made from both your keys. Once ${paired.friend} adds your code too, '
          'their phone shows this same sigil and number.'
          '${paired.verified ? '' : ' Call them and compare the number: if it matches, nobody swapped your codes.'}',
          textAlign: TextAlign.center,
          style: AppText.caption,
        ),
        const SizedBox(height: Space.xl),
        const Text('SAFETY NUMBER', textAlign: TextAlign.center, style: AppText.overline),
        const SizedBox(height: 6),
        SelectableText(
          paired.safetyNumber,
          textAlign: TextAlign.center,
          style: AppText.mono.copyWith(fontSize: 30, letterSpacing: 3, color: accent),
        ),
        const SizedBox(height: Space.xl),
        if (paired.verified)
          FilledButton(onPressed: _done, child: const Text('Done'))
        else ...[
          FilledButton(
            onPressed: () async {
              await friendSetVerified(chatId: paired.chatId, verified: true);
              _done();
            },
            child: const Text('Numbers match'),
          ),
          const SizedBox(height: Space.sm),
          OutlinedButton(onPressed: _done, child: const Text('Verify later')),
        ],
        const SizedBox(height: Space.sm),
        TextButton(
          onPressed: _reject,
          child: const Text("They don't match", style: TextStyle(color: Palette.danger)),
        ),
      ],
    );
  }

  void _done() {
    HapticFeedback.heavyImpact();
    Navigator.pop(context, true);
  }
}

/// You + friend = your chat: shows that the shared sigil comes from both
/// keys, which is why it differs from either person's own sigil.
class _SigilEquation extends StatelessWidget {
  const _SigilEquation({required this.mine, required this.theirs, required this.shared, required this.friend});

  final SigilSpec mine;
  final SigilSpec theirs;
  final SigilSpec shared;
  final String friend;

  @override
  Widget build(BuildContext context) {
    Widget appear(double from, Widget child) => TweenAnimationBuilder<double>(
          tween: Tween(begin: 0, end: 1),
          duration: Duration(milliseconds: (from * 1000).round() + 500),
          curve: Interval(from / (from + 0.5), 1, curve: Motion.decelerate),
          builder: (_, v, c) => Opacity(opacity: v, child: Transform.scale(scale: 0.8 + 0.2 * v, child: c)),
          child: child,
        );
    Widget small(SigilSpec spec, String label) => Column(
          children: [
            SizedBox.square(dimension: 60, child: ClipOval(child: SigilView(spec: spec))),
            const SizedBox(height: 6),
            Text(label, style: AppText.micro, maxLines: 1, overflow: TextOverflow.ellipsis),
          ],
        );
    const op = TextStyle(fontFamily: 'SpaceGrotesk', fontSize: 22, color: Palette.textFaint);
    return Column(
      children: [
        Row(
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            appear(0, small(mine, 'You')),
            const Padding(padding: EdgeInsets.fromLTRB(14, 0, 14, 18), child: Text('+', style: op)),
            appear(0.25, small(theirs, friend)),
          ],
        ),
        const SizedBox(height: Space.sm),
        appear(0.5, const Icon(Icons.keyboard_double_arrow_down_rounded, color: Palette.textFaint)),
        const SizedBox(height: Space.sm),
        appear(
          0.75,
          Column(
            children: [
              SizedBox.square(dimension: 170, child: ClipOval(child: LivingSigil(spec: shared, period: 40))),
              const SizedBox(height: 8),
              const Text('YOUR CHAT\'S SIGIL', style: AppText.overline),
            ],
          ),
        ),
      ],
    );
  }
}

class _Badge extends StatelessWidget {
  const _Badge({required this.color, required this.icon, required this.label});

  final Color color;
  final IconData icon;
  final String label;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(Radii.sm),
        border: Border.all(color: color.withValues(alpha: 0.4)),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 16, color: color),
          const SizedBox(width: 6),
          Text(label, style: AppText.micro.copyWith(color: color, fontWeight: FontWeight.w600)),
        ],
      ),
    );
  }
}

class _Section extends StatelessWidget {
  const _Section({required this.title, required this.child});

  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.fromLTRB(Space.lg, Space.lg, Space.lg, Space.lg + 2),
      decoration: BoxDecoration(
        color: Palette.surface,
        borderRadius: BorderRadius.circular(Radii.xl),
        border: Border.all(color: Palette.outline),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(title, style: AppText.overline),
          const SizedBox(height: 14),
          child,
        ],
      ),
    );
  }
}
