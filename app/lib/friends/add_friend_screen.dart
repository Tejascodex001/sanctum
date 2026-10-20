import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_zxing/flutter_zxing.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../identity/secrets.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';

/// In-person pairing. One phone shows its code, the other scans it and
/// shows a reply, the first scans the reply. Then both compare sigils.
///
/// Pops `true` when a friend was added.
class AddFriendScreen extends StatefulWidget {
  const AddFriendScreen({super.key, required this.mySigil});

  final SigilSpec mySigil;

  @override
  State<AddFriendScreen> createState() => _AddFriendScreenState();
}

enum _Step { choose, showCode, scanCode, showReply, scanReply, confirm }

class _AddFriendScreenState extends State<AddFriendScreen> {
  final _name = TextEditingController();
  _Step _step = _Step.choose;
  String? _code;
  PairedView? _paired;
  SigilSpec? _pairSigil;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    loadDisplayName().then((n) {
      if (mounted) setState(() => _name.text = n);
    });
  }

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  bool get _hasName => _name.text.trim().isNotEmpty;

  void _go(_Step step) {
    HapticFeedback.selectionClick();
    setState(() => _step = step);
  }

  Future<void> _showMyCode() async {
    if (!await _saveName()) return;
    final code = await _run(() => inviteCreate(myName: _name.text));
    if (code == null) return;
    setState(() {
      _code = code;
      _step = _Step.showCode;
    });
  }

  Future<void> _scanned(String text, {required bool reply}) async {
    if (_busy) return;
    if (reply) {
      final paired = await _run(() => inviteComplete(code: text));
      if (paired != null) await _onPaired(paired);
    } else {
      final accepted = await _run(() => inviteAccept(code: text, myName: _name.text));
      if (accepted != null) {
        _code = accepted.reply;
        await _onPaired(accepted.paired, next: _Step.showReply);
      }
    }
  }

  Future<void> _onPaired(PairedView paired, {_Step next = _Step.confirm}) async {
    HapticFeedback.heavyImpact();
    final sigil = await sigilForKey(publicKey: paired.sigilKey);
    setState(() {
      _paired = paired;
      _pairSigil = sigil;
      _step = next;
    });
  }

  Future<bool> _saveName() async {
    if (!_hasName) {
      _toast('Add your name first, so your friend knows who you are.');
      return false;
    }
    await saveDisplayName(_name.text);
    return true;
  }

  /// Runs a Rust call, turning its error into a readable message.
  Future<T?> _run<T>(Future<T> Function() f) async {
    setState(() => _busy = true);
    try {
      return await f();
    } catch (e) {
      HapticFeedback.vibrate();
      _toast(_friendly(e));
      return null;
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  String _friendly(Object e) {
    final s = e.toString();
    for (final (needle, msg) in [
      ('already friends', 'You are already friends.'),
      ('own code', "That's your own code."),
      ('someone else', 'That reply was made for someone else.'),
      ('signature', "This code isn't genuine."),
      ('not a Sanctum code', "That isn't a Sanctum code."),
      ('expected a reply', 'That is a friend code. Scan the reply your friend is showing.'),
      ('expected a friend', 'That is a reply code. Scan the code your friend is showing.'),
      ('show your code first', 'Show your code first, then scan their reply.'),
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
    _toast('Friend not added. Someone may have interfered; try again face to face.');
    setState(() {
      _paired = null;
      _step = _Step.choose;
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: widget.mySigil.background.color,
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('Add a friend'),
        leading: IconButton(
          tooltip: 'Back',
          icon: const Icon(Icons.arrow_back_ios_new_rounded, size: 20),
          onPressed: () => Navigator.pop(context, _paired != null && _step == _Step.confirm),
        ),
      ),
      body: SafeArea(
        child: AnimatedSwitcher(
          duration: const Duration(milliseconds: 350),
          switchInCurve: Curves.easeOutCubic,
          transitionBuilder: (child, anim) => FadeTransition(
            opacity: anim,
            child: SlideTransition(
              position: Tween(begin: const Offset(0.06, 0), end: Offset.zero).animate(anim),
              child: child,
            ),
          ),
          child: KeyedSubtree(key: ValueKey(_step), child: _body()),
        ),
      ),
    );
  }

  Widget _body() {
    switch (_step) {
      case _Step.choose:
        return _choose();
      case _Step.showCode:
        return _qr(
          title: 'Let your friend scan this',
          detail: "Then tap below and scan the reply on their screen.",
          action: FilledButton.icon(
            onPressed: () => _go(_Step.scanReply),
            icon: const Icon(Icons.qr_code_scanner_rounded),
            label: const Text('Scan their reply'),
          ),
        );
      case _Step.scanCode:
        return _scanner('Point at the code on your friend\'s screen', reply: false);
      case _Step.showReply:
        return _qr(
          title: 'Now let ${_paired!.friend} scan this',
          detail: 'When they have, check that your sigils match.',
          action: FilledButton(onPressed: () => _go(_Step.confirm), child: const Text('They scanned it')),
        );
      case _Step.scanReply:
        return _scanner('Scan the reply on your friend\'s screen', reply: true);
      case _Step.confirm:
        return _confirm();
    }
  }

  Widget _choose() {
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const Text(
          'Pair in person. Codes are exchanged face to face, never over the internet, '
          'so nobody else can slip in.',
          style: TextStyle(color: Colors.white70, height: 1.4),
        ),
        const SizedBox(height: 24),
        TextField(
          controller: _name,
          textCapitalization: TextCapitalization.words,
          onChanged: (_) => setState(() {}),
          decoration: InputDecoration(
            labelText: 'Your name',
            helperText: 'Only shown to friends you pair with',
            filled: true,
            fillColor: Colors.white.withValues(alpha: 0.05),
            border: OutlineInputBorder(borderRadius: BorderRadius.circular(16)),
          ),
        ),
        const SizedBox(height: 28),
        _Choice(
          icon: Icons.qr_code_2_rounded,
          title: 'Show my code',
          detail: 'Your friend scans it',
          onTap: _busy ? null : _showMyCode,
        ),
        const SizedBox(height: 12),
        _Choice(
          icon: Icons.qr_code_scanner_rounded,
          title: 'Scan their code',
          detail: 'Your friend shows theirs',
          onTap: _busy
              ? null
              : () async {
                  if (await _saveName()) _go(_Step.scanCode);
                },
        ),
      ],
    );
  }

  Widget _qr({required String title, required String detail, required Widget action}) {
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 8, 24, 24),
      children: [
        Text(title, textAlign: TextAlign.center, style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600)),
        const SizedBox(height: 20),
        TweenAnimationBuilder<double>(
          tween: Tween(begin: 0.9, end: 1),
          duration: const Duration(milliseconds: 450),
          curve: Curves.easeOutBack,
          builder: (_, v, child) => Transform.scale(scale: v, child: child),
          child: Container(
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              color: Colors.white,
              borderRadius: BorderRadius.circular(28),
              boxShadow: [BoxShadow(color: widget.mySigil.accent.color.withValues(alpha: 0.35), blurRadius: 40)],
            ),
            child: QrImageView(
              data: _code!,
              errorCorrectionLevel: QrErrorCorrectLevel.L,
              backgroundColor: Colors.white,
              padding: EdgeInsets.zero,
            ),
          ),
        ),
        const SizedBox(height: 16),
        Text(detail, textAlign: TextAlign.center, style: const TextStyle(color: Colors.white60)),
        const SizedBox(height: 20),
        action,
        if (_pairSigil != null) ...[
          const SizedBox(height: 28),
          const Text('YOUR SHARED SIGIL', textAlign: TextAlign.center, style: TextStyle(letterSpacing: 3, color: Colors.white54)),
          const SizedBox(height: 12),
          Center(
            child: SizedBox.square(dimension: 96, child: ClipOval(child: SigilView(spec: _pairSigil!))),
          ),
        ],
      ],
    );
  }

  Widget _scanner(String hint, {required bool reply}) {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(24, 8, 24, 16),
          child: Text(hint, textAlign: TextAlign.center, style: const TextStyle(fontSize: 17)),
        ),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 24),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(28),
              child: Stack(
                fit: StackFit.expand,
                children: [
                  ReaderWidget(
                    codeFormat: Format.qrCode,
                    tryHarder: true,
                    showGallery: false,
                    showToggleCamera: false,
                    scannerOverlay: ScannerOverlayBorder(
                      borderColor: widget.mySigil.accent.color,
                      borderRadius: 24,
                      borderLength: 48,
                      borderWidth: 6,
                      cutOutSize: 280,
                    ),
                    onScan: (code) {
                      final text = code.text;
                      if (code.isValid && text != null && text.startsWith('sanctum1:')) {
                        _scanned(text, reply: reply);
                      }
                    },
                  ),
                  if (_busy) const ColoredBox(color: Colors.black54, child: Center(child: CircularProgressIndicator())),
                ],
              ),
            ),
          ),
        ),
      ],
    );
  }

  Widget _confirm() {
    final paired = _paired!;
    return Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        children: [
          const Spacer(),
          TweenAnimationBuilder<double>(
            tween: Tween(begin: 0, end: 1),
            duration: const Duration(milliseconds: 900),
            curve: Curves.easeOutCubic,
            builder: (_, v, child) => Opacity(opacity: v, child: Transform.scale(scale: 0.7 + 0.3 * v, child: child)),
            child: SizedBox.square(
              dimension: 220,
              child: ClipOval(child: LivingSigil(spec: _pairSigil!, period: 40)),
            ),
          ),
          const SizedBox(height: 28),
          Text(
            'Does ${paired.friend}\'s screen show this same sigil?',
            textAlign: TextAlign.center,
            style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600),
          ),
          const SizedBox(height: 10),
          const Text(
            'Matching sigils prove nobody swapped a code between you.',
            textAlign: TextAlign.center,
            style: TextStyle(color: Colors.white60),
          ),
          const Spacer(),
          SizedBox(
            width: double.infinity,
            child: FilledButton(
              onPressed: () {
                HapticFeedback.heavyImpact();
                Navigator.pop(context, true);
              },
              child: const Text('Yes, they match'),
            ),
          ),
          const SizedBox(height: 8),
          TextButton(
            onPressed: _reject,
            child: const Text("They don't match", style: TextStyle(color: Colors.redAccent)),
          ),
        ],
      ),
    );
  }
}

class _Choice extends StatelessWidget {
  const _Choice({required this.icon, required this.title, required this.detail, this.onTap});

  final IconData icon;
  final String title;
  final String detail;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final accent = Theme.of(context).colorScheme.primary;
    return Material(
      color: Colors.white.withValues(alpha: 0.05),
      borderRadius: BorderRadius.circular(22),
      child: InkWell(
        borderRadius: BorderRadius.circular(22),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Row(
            children: [
              Container(
                width: 52,
                height: 52,
                decoration: BoxDecoration(color: accent.withValues(alpha: 0.18), shape: BoxShape.circle),
                child: Icon(icon, color: accent),
              ),
              const SizedBox(width: 16),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w600)),
                    const SizedBox(height: 2),
                    Text(detail, style: const TextStyle(color: Colors.white54)),
                  ],
                ),
              ),
              const Icon(Icons.chevron_right_rounded, color: Colors.white38),
            ],
          ),
        ),
      ),
    );
  }
}
