import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_zxing/flutter_zxing.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:share_plus/share_plus.dart';

import '../identity/secrets.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/sigil.dart';
import '../src/rust/api/store.dart';
import 'qr_codec.dart';

/// Pairing with a friend, in person (QR) or far away (share and paste).
///
/// One person shows/shares their code, the other scans/pastes it and gets a
/// reply to show/share back, the first scans/pastes the reply. Then both
/// compare: the sigil in person, or the safety number on a call.
///
/// Pops `true` when a friend was added.
class AddFriendScreen extends StatefulWidget {
  const AddFriendScreen({super.key, required this.mySigil});

  final SigilSpec mySigil;

  @override
  State<AddFriendScreen> createState() => _AddFriendScreenState();
}

enum _Step { choose, showCode, readCode, showReply, readReply, confirm }

/// Share of the camera frame that is analysed (and outlined on screen).
const _crop = 0.85;

class _AddFriendScreenState extends State<AddFriendScreen> {
  final _name = TextEditingController();
  _Step _step = _Step.choose;
  bool _remote = false;
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

  void _go(_Step step) {
    HapticFeedback.selectionClick();
    setState(() => _step = step);
  }

  Future<void> _start({required bool showMine, required bool remote}) async {
    if (_name.text.trim().isEmpty) {
      _toast('Add your name first, so your friend knows who you are.');
      return;
    }
    await saveDisplayName(_name.text);
    _remote = remote;
    if (!showMine) return _go(_Step.readCode);
    final code = await _run(() => inviteCreate(myName: _name.text));
    if (code == null) return;
    setState(() {
      _code = code;
      _step = _Step.showCode;
    });
  }

  Future<void> _received(String code, {required bool reply}) async {
    if (_busy) return;
    if (reply) {
      final paired = await _run(() => inviteComplete(code: code));
      if (paired != null) await _onPaired(paired);
    } else {
      final accepted = await _run(() => inviteAccept(code: code, myName: _name.text));
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

  Future<void> _paste({required bool reply}) async {
    final pasted = (await Clipboard.getData(Clipboard.kTextPlain))?.text;
    final code = codeFromScan(text: pasted);
    if (code == null) {
      _toast('No Sanctum code on the clipboard. Copy the whole message your friend sent.');
      return;
    }
    await _received(code, reply: reply);
  }

  Future<void> _share({required bool reply}) async {
    final intro = reply
        ? 'My Sanctum reply. Copy this whole message, then paste it in Sanctum:'
        : 'Add me on Sanctum. Copy this whole message, then paste it in Sanctum:';
    await SharePlus.instance.share(ShareParams(text: '$intro\n\n$_code'));
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
      ('damaged', 'This code is incomplete. Copy the whole message.'),
      ('not a Sanctum code', "That isn't a Sanctum code."),
      ('expected a reply', 'That is a friend code. Use the reply your friend sent back.'),
      ('expected a friend', "That is a reply. Use your friend's code."),
      ('show your code first', 'Show or share your code first, then read their reply.'),
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
    _toast('Friend not added. Someone may have interfered; try again.');
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
          child: KeyedSubtree(key: ValueKey((_step, _remote)), child: _body()),
        ),
      ),
    );
  }

  Widget _body() {
    final friend = _paired?.friend ?? 'your friend';
    return switch (_step) {
      _Step.choose => _choose(),
      _Step.showCode => _remote
          ? _shareView(
              title: 'Send your code to your friend',
              detail: 'Then paste the reply they send back.',
              reply: false,
              next: FilledButton.icon(
                onPressed: () => _paste(reply: true),
                icon: const Icon(Icons.content_paste_rounded),
                label: const Text('Paste their reply'),
              ),
            )
          : _qrView(
              title: 'Let your friend scan this',
              detail: 'Then scan the reply on their screen.',
              next: FilledButton.icon(
                onPressed: () => _go(_Step.readReply),
                icon: const Icon(Icons.qr_code_scanner_rounded),
                label: const Text('Scan their reply'),
              ),
            ),
      _Step.readCode => _remote
          ? _pasteView('Paste the code your friend sent', reply: false)
          : _scanner("Point at the code on your friend's screen", reply: false),
      _Step.showReply => _remote
          ? _shareView(
              title: 'Send this reply to $friend',
              detail: 'Once they paste it, you are connected.',
              reply: true,
              next: FilledButton(onPressed: () => _go(_Step.confirm), child: const Text('I sent it')),
            )
          : _qrView(
              title: 'Now let $friend scan this',
              detail: 'When they have, check that your sigils match.',
              next: FilledButton(onPressed: () => _go(_Step.confirm), child: const Text('They scanned it')),
            ),
      _Step.readReply => _scanner("Scan the reply on your friend's screen", reply: true),
      _Step.confirm => _confirm(),
    };
  }

  Widget _choose() {
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        TextField(
          controller: _name,
          textCapitalization: TextCapitalization.words,
          decoration: InputDecoration(
            labelText: 'Your name',
            helperText: 'Only shown to friends you pair with',
            filled: true,
            fillColor: Colors.white.withValues(alpha: 0.05),
            border: OutlineInputBorder(borderRadius: BorderRadius.circular(16)),
          ),
        ),
        const SizedBox(height: 28),
        const _Heading('TOGETHER', 'Safest: codes never leave your two phones.'),
        _Choice(
          icon: Icons.qr_code_2_rounded,
          title: 'Show my code',
          detail: 'Your friend scans it',
          onTap: _busy ? null : () => _start(showMine: true, remote: false),
        ),
        const SizedBox(height: 10),
        _Choice(
          icon: Icons.qr_code_scanner_rounded,
          title: 'Scan their code',
          detail: 'Your friend shows theirs',
          onTap: _busy ? null : () => _start(showMine: false, remote: false),
        ),
        const SizedBox(height: 28),
        const _Heading(
          'FAR AWAY',
          'Send codes through any app, then confirm the safety number on a call.',
        ),
        _Choice(
          icon: Icons.ios_share_rounded,
          title: 'Send my code',
          detail: 'Share it, then paste their reply',
          onTap: _busy ? null : () => _start(showMine: true, remote: true),
        ),
        const SizedBox(height: 10),
        _Choice(
          icon: Icons.content_paste_rounded,
          title: 'Paste their code',
          detail: 'Then send them the reply',
          onTap: _busy ? null : () => _start(showMine: false, remote: true),
        ),
      ],
    );
  }

  Widget _qrView({required String title, required String detail, required Widget next}) {
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
            padding: const EdgeInsets.all(18),
            decoration: BoxDecoration(
              color: Colors.white,
              borderRadius: BorderRadius.circular(28),
              boxShadow: [BoxShadow(color: widget.mySigil.accent.color.withValues(alpha: 0.35), blurRadius: 40)],
            ),
            child: QrImageView.withQr(
              qr: QrCode.fromUint8List(data: codeToQrBytes(_code!), errorCorrectLevel: QrErrorCorrectLevel.L),
              backgroundColor: Colors.white,
              padding: EdgeInsets.zero,
            ),
          ),
        ),
        const SizedBox(height: 16),
        Text(detail, textAlign: TextAlign.center, style: const TextStyle(color: Colors.white60)),
        const SizedBox(height: 4),
        const Text(
          'Tip: turn your screen brightness up.',
          textAlign: TextAlign.center,
          style: TextStyle(color: Colors.white38, fontSize: 12),
        ),
        const SizedBox(height: 20),
        next,
        ..._sharedSigil(),
      ],
    );
  }

  Widget _shareView({
    required String title,
    required String detail,
    required bool reply,
    required Widget next,
  }) {
    return ListView(
      padding: const EdgeInsets.fromLTRB(24, 8, 24, 24),
      children: [
        Text(title, textAlign: TextAlign.center, style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600)),
        const SizedBox(height: 12),
        Text(detail, textAlign: TextAlign.center, style: const TextStyle(color: Colors.white60)),
        const SizedBox(height: 24),
        FilledButton.tonalIcon(
          onPressed: () => _share(reply: reply),
          icon: const Icon(Icons.ios_share_rounded),
          label: Text(reply ? 'Share reply' : 'Share my code'),
        ),
        const SizedBox(height: 8),
        TextButton.icon(
          onPressed: () {
            Clipboard.setData(ClipboardData(text: _code!));
            _toast('Copied.');
          },
          icon: const Icon(Icons.copy_rounded, size: 18),
          label: const Text('Copy instead'),
        ),
        const SizedBox(height: 16),
        const Text(
          'The code holds your name and public key, nothing secret. Whatever app you send it '
          'through will see that you are pairing; Signal or a similar app is best.',
          textAlign: TextAlign.center,
          style: TextStyle(color: Colors.white38, fontSize: 12, height: 1.4),
        ),
        const SizedBox(height: 24),
        next,
        ..._sharedSigil(),
      ],
    );
  }

  Widget _pasteView(String title, {required bool reply}) {
    return Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        children: [
          const Spacer(),
          const Icon(Icons.content_paste_rounded, size: 56, color: Colors.white38),
          const SizedBox(height: 20),
          Text(title, textAlign: TextAlign.center, style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600)),
          const SizedBox(height: 10),
          const Text(
            'Copy the whole message they sent, then tap below.',
            textAlign: TextAlign.center,
            style: TextStyle(color: Colors.white60),
          ),
          const Spacer(),
          SizedBox(
            width: double.infinity,
            child: FilledButton.icon(
              onPressed: _busy ? null : () => _paste(reply: reply),
              icon: const Icon(Icons.content_paste_rounded),
              label: const Text('Paste code'),
            ),
          ),
        ],
      ),
    );
  }

  List<Widget> _sharedSigil() => [
        if (_pairSigil != null) ...[
          const SizedBox(height: 28),
          const Text('YOUR SHARED SIGIL',
              textAlign: TextAlign.center, style: TextStyle(letterSpacing: 3, color: Colors.white54)),
          const SizedBox(height: 12),
          Center(child: SizedBox.square(dimension: 96, child: ClipOval(child: SigilView(spec: _pairSigil!)))),
        ],
      ];

  Widget _scanner(String hint, {required bool reply}) {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(24, 8, 24, 4),
          child: Text(hint, textAlign: TextAlign.center, style: const TextStyle(fontSize: 17)),
        ),
        TextButton.icon(
          onPressed: _busy ? null : () => _paste(reply: reply),
          icon: const Icon(Icons.content_paste_rounded, size: 18),
          label: const Text("Can't scan? Paste code"),
        ),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 24),
            child: ClipRRect(
              borderRadius: BorderRadius.circular(28),
              child: Stack(
                fit: StackFit.expand,
                children: [
                  ReaderWidget(
                    codeFormat: Format.qrCode,
                    // Dense codes read off a screen need a sharp frame and
                    // most of it analysed; the overlay matches the analysed
                    // area exactly so the code is never cut off.
                    resolution: ResolutionPreset.veryHigh,
                    cropPercent: _crop,
                    tryHarder: true,
                    scanDelay: const Duration(milliseconds: 250),
                    scanDelaySuccess: const Duration(milliseconds: 1500),
                    showGallery: false,
                    showToggleCamera: false,
                    scannerOverlay: ScannerOverlayBorder(
                      borderColor: widget.mySigil.accent.color,
                      borderRadius: 24,
                      borderLength: 48,
                      borderWidth: 6,
                      cutOutSize: _crop,
                    ),
                    onScan: (code) {
                      final text = codeFromScan(rawBytes: code.rawBytes, text: code.text);
                      if (code.isValid && text != null) _received(text, reply: reply);
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
    return ListView(
      padding: const EdgeInsets.all(24),
      children: [
        const SizedBox(height: 12),
        Center(
          child: TweenAnimationBuilder<double>(
            tween: Tween(begin: 0, end: 1),
            duration: const Duration(milliseconds: 900),
            curve: Curves.easeOutCubic,
            builder: (_, v, child) => Opacity(opacity: v, child: Transform.scale(scale: 0.7 + 0.3 * v, child: child)),
            child: SizedBox.square(
              dimension: 200,
              child: ClipOval(child: LivingSigil(spec: _pairSigil!, period: 40)),
            ),
          ),
        ),
        const SizedBox(height: 24),
        Text(
          _remote ? 'Call ${paired.friend} and compare' : "Does ${paired.friend}'s screen show this sigil?",
          textAlign: TextAlign.center,
          style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 16),
        const Text('SAFETY NUMBER', textAlign: TextAlign.center, style: TextStyle(letterSpacing: 3, color: Colors.white54)),
        const SizedBox(height: 6),
        SelectableText(
          paired.safetyNumber,
          textAlign: TextAlign.center,
          style: TextStyle(
            fontSize: 28,
            fontWeight: FontWeight.w600,
            letterSpacing: 2,
            color: widget.mySigil.accent.color,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        const SizedBox(height: 10),
        Text(
          _remote
              ? 'Read the number to each other on a voice or video call. If it matches, nobody '
                  'swapped your codes on the way.'
              : 'Matching sigils (or numbers) prove nobody swapped a code between you.',
          textAlign: TextAlign.center,
          style: const TextStyle(color: Colors.white60, height: 1.4),
        ),
        const SizedBox(height: 28),
        FilledButton(
          onPressed: () {
            HapticFeedback.heavyImpact();
            Navigator.pop(context, true);
          },
          child: const Text('Yes, they match'),
        ),
        const SizedBox(height: 8),
        TextButton(
          onPressed: _reject,
          child: const Text("They don't match", style: TextStyle(color: Colors.redAccent)),
        ),
      ],
    );
  }
}

class _Heading extends StatelessWidget {
  const _Heading(this.title, this.detail);

  final String title;
  final String detail;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 12, left: 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: const TextStyle(letterSpacing: 3, fontSize: 12, color: Colors.white54)),
          const SizedBox(height: 4),
          Text(detail, style: const TextStyle(color: Colors.white38, fontSize: 13)),
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
          padding: const EdgeInsets.all(18),
          child: Row(
            children: [
              Container(
                width: 48,
                height: 48,
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
