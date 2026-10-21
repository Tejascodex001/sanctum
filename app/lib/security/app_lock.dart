import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:local_auth/local_auth.dart';

import '../identity/secrets.dart';
import '../sigil/sigil_painter.dart';
import '../src/rust/api/sigil.dart';
import '../theme/tokens.dart';
import 'panic.dart';
import 'profile.dart';

/// Covers the app until the user unlocks with biometrics or the device
/// PIN/pattern, and locks again whenever the app leaves the foreground.
///
/// The app stays mounted underneath, so unlocking returns you to exactly
/// where you were.
class AppLock extends StatefulWidget {
  const AppLock({super.key, required this.child, required this.sigil});

  final Widget child;

  /// The user's own sigil, shown on the lock screen.
  final SigilSpec sigil;

  @override
  State<AppLock> createState() => _AppLockState();

  static int _away = 0;

  /// The app's navigator: switching profile closes whatever the other
  /// profile had open.
  static final navigator = GlobalKey<NavigatorState>();

  /// Runs [f] (e.g. opening the camera scanner) without locking when the
  /// app briefly goes to the background for it.
  static Future<T> whileAway<T>(Future<T> Function() f) async {
    _away++;
    try {
      return await f();
    } finally {
      _away--;
    }
  }
}

class _AppLockState extends State<AppLock> with WidgetsBindingObserver, TickerProviderStateMixin {
  final _auth = LocalAuthentication();
  bool _authenticating = false;

  /// Ask for unlock as soon as the app is on screen. The engine can start
  /// before any screen exists (it outlives the activity, see
  /// SanctumApplication.kt), and the system prompt needs one.
  bool _promptWhenVisible = true;

  /// 1 = fully locked, 0 = unlocked. Drives the unlock animation.
  late final _lock = AnimationController(vsync: this, duration: const Duration(milliseconds: 650), value: 1);
  late final _pulse = AnimationController(vsync: this, duration: const Duration(seconds: 2))..repeat(reverse: true);

  /// A Sanctum PIN is set: the lock screen shows a number pad (and
  /// the decoy PIN works).
  bool _pinMode = false;
  String _entered = '';
  int _failures = 0;
  DateTime? _waitUntil;
  bool _checking = false;
  late final _shake = AnimationController(vsync: this, duration: const Duration(milliseconds: 420));

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _refreshPinMode();
    WidgetsBinding.instance.addPostFrameCallback((_) => _maybePrompt());
  }

  Future<void> _refreshPinMode() async {
    final pin = await hasPin();
    if (mounted) setState(() => _pinMode = pin);
  }

  /// Unlocks into the real profile or the decoy.
  Future<void> _open({required bool decoy}) async {
    final changed = Profile.decoy.value != decoy;
    await Profile.enter(decoy: decoy);
    if (changed) AppLock.navigator.currentState?.popUntil((r) => r.isFirst);
    HapticFeedback.mediumImpact();
    setState(() => _entered = '');
    await _lock.reverse();
  }

  Future<void> _submitPin() async {
    if (_checking || _entered.length < 4) return;
    final wait = _waitUntil;
    if (wait != null && DateTime.now().isBefore(wait)) return;
    setState(() => _checking = true);
    final match = await checkPin(_entered);
    if (!mounted) return;
    setState(() => _checking = false);
    if (match == PinMatch.wrong) {
      _failures++;
      // After 5 wrong PINs, wait: 30 s, then doubling.
      if (_failures >= 5) _waitUntil = DateTime.now().add(Duration(seconds: 30 << (_failures - 5).clamp(0, 6)));
      HapticFeedback.heavyImpact();
      _shake.forward(from: 0);
      setState(() => _entered = '');
      return;
    }
    _failures = 0;
    _waitUntil = null;
    await _open(decoy: match == PinMatch.decoy);
  }

  void _digit(String d) {
    if (_entered.length >= 8 || _checking) return;
    HapticFeedback.selectionClick();
    setState(() => _entered += d);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _lock.dispose();
    _pulse.dispose();
    _shake.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // The system auth sheet itself pauses the app, so ignore that cycle.
    if (state == AppLifecycleState.hidden && !_authenticating && AppLock._away == 0) {
      _lock.value = 1;
      _promptWhenVisible = true;
      _entered = '';
      _refreshPinMode();
    }
    if (state == AppLifecycleState.resumed) _maybePrompt();
  }

  /// Once per lock: after a cancel, the user taps to try again.
  void _maybePrompt() {
    if (!_promptWhenVisible || _lock.value < 1) return;
    if (WidgetsBinding.instance.lifecycleState != AppLifecycleState.resumed) return;
    _promptWhenVisible = false;
    _unlock();
  }

  Future<void> _unlock() async {
    if (_authenticating) return;
    _authenticating = true;
    try {
      final supported = await _auth.isDeviceSupported();
      // Without a screen lock there is nothing to check against (with a
      // Sanctum PIN, the PIN is).
      if (!supported && _pinMode) return;
      final ok = !supported || await _auth.authenticate(localizedReason: 'Unlock Sanctum');
      // Biometrics always open the real profile.
      if (ok) await _open(decoy: false);
    } on LocalAuthException {
      // Cancelled or locked out; stay locked and let the user retry.
    } finally {
      _authenticating = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _lock,
      builder: (context, child) {
        final t = Curves.easeInOutCubic.transform(_lock.value);
        return Stack(
          fit: StackFit.expand,
          children: [
            // Hidden entirely (not just covered) while locked.
            Offstage(
              offstage: t == 1,
              child: Transform.scale(scale: 1 + 0.04 * t, child: child),
            ),
            if (t > 0)
              Opacity(
                opacity: t,
                child: Transform.scale(scale: 1 + 0.25 * (1 - t), child: _lockScreen()),
              ),
          ],
        );
      },
      child: widget.child,
    );
  }

  Widget _pinScreen() {
    final accent = widget.sigil.accent.color;
    final waiting = _waitUntil != null && DateTime.now().isBefore(_waitUntil!);
    Widget key(Widget label, VoidCallback? onTap, {String? tooltip}) => SizedBox(
          width: 76,
          height: 64,
          child: TextButton(
            onPressed: onTap,
            style: TextButton.styleFrom(foregroundColor: Palette.text, shape: const CircleBorder()),
            child: tooltip == null ? label : Tooltip(message: tooltip, child: label),
          ),
        );
    Widget digit(String d) => key(Text(d, style: AppText.title.copyWith(fontSize: 26)), () => _digit(d));
    return Material(
      color: widget.sigil.background.color,
      child: SafeArea(
        child: Column(
          children: [
            const Spacer(),
            SizedBox.square(dimension: 110, child: LivingSigil(spec: widget.sigil, period: 60)),
            const SizedBox(height: Space.md),
            const Text('Sanctum', style: AppText.display),
            const SizedBox(height: Space.lg),
            AnimatedBuilder(
              animation: _shake,
              builder: (context, child) => Transform.translate(
                offset: Offset(12 * (1 - _shake.value) * math.sin(_shake.value * math.pi * 6), 0),
                child: child,
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  for (var i = 0; i < (_entered.length < 4 ? 4 : _entered.length); i++)
                    Container(
                      width: 14,
                      height: 14,
                      margin: const EdgeInsets.symmetric(horizontal: 7),
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        color: i < _entered.length ? accent : Colors.transparent,
                        border: Border.all(color: accent.withValues(alpha: 0.7), width: 1.5),
                      ),
                    ),
                ],
              ),
            ),
            SizedBox(
              height: 28,
              child: Center(
                child: Text(
                  waiting ? 'Too many tries: wait a moment' : '',
                  style: AppText.caption.copyWith(color: Palette.danger),
                ),
              ),
            ),
            for (final row in const [
              ['1', '2', '3'],
              ['4', '5', '6'],
              ['7', '8', '9'],
            ])
              Row(mainAxisAlignment: MainAxisAlignment.center, children: [for (final d in row) digit(d)]),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                key(const Icon(Icons.fingerprint_rounded, size: 30), _unlock, tooltip: 'Fingerprint or face'),
                digit('0'),
                key(
                  const Icon(Icons.backspace_outlined),
                  _entered.isEmpty ? null : () => setState(() => _entered = _entered.substring(0, _entered.length - 1)),
                  tooltip: 'Delete',
                ),
              ],
            ),
            const SizedBox(height: Space.sm),
            SizedBox(
              width: 228,
              child: FilledButton(
                onPressed: _entered.length >= 4 && !_checking && !waiting ? _submitPin : null,
                child: _checking
                    ? const SizedBox.square(dimension: 18, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Text('Unlock'),
              ),
            ),
            const Spacer(),
            TextButton(
              onPressed: () => showPanicSheet(context),
              child: Text('Emergency wipe', style: AppText.micro.copyWith(color: Palette.textFaint)),
            ),
          ],
        ),
      ),
    );
  }

  Widget _lockScreen() {
    if (_pinMode) return _pinScreen();
    return Material(
      color: widget.sigil.background.color,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: _unlock,
        child: Stack(
          fit: StackFit.expand,
          children: [
            Center(
              child: FractionallySizedBox(
                widthFactor: 0.82,
                child: AspectRatio(
                  aspectRatio: 1,
                  child: LivingSigil(spec: widget.sigil, period: 60),
                ),
              ),
            ),
            const Align(
              alignment: Alignment(0, -0.78),
              child: Text('Sanctum', style: AppText.display),
            ),
            Align(
              alignment: const Alignment(0, 0.8),
              child: FadeTransition(
                opacity: Tween(begin: 0.35, end: 0.9).animate(_pulse),
                child: Text('TAP TO UNLOCK', style: AppText.overline),
              ),
            ),
            // In an emergency, wiping mustn't need unlocking first.
            Align(
              alignment: Alignment.bottomCenter,
              child: SafeArea(
                child: TextButton(
                  onPressed: () => showPanicSheet(context),
                  child: Text('Emergency wipe', style: AppText.micro.copyWith(color: Palette.textFaint)),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
