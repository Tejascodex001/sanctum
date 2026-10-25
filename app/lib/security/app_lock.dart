import 'dart:async';
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
import '../sigil/sigil_look.dart';

/// Covers the app until the user unlocks with biometrics or the device
/// PIN/pattern, and locks again whenever the app leaves the foreground.
///
/// The app stays mounted underneath, so unlocking returns you to exactly
/// where you were.
class AppLock extends StatefulWidget {
  const AppLock({super.key, required this.child, required this.sigil, @visibleForTesting this.auth});

  final Widget child;

  /// The user's own sigil, shown on the lock screen.
  final SigilSpec sigil;

  /// The system fingerprint / screen-lock check (tests pass a fake).
  final LocalAuthentication? auth;

  @override
  State<AppLock> createState() => _AppLockState();

  static int _away = 0;

  /// Whether the app is locked right now (a call screen hides who's
  /// calling then).
  static final locked = ValueNotifier<bool>(true);

  /// A call is ringing or going on: don't pop up the unlock prompt over
  /// it. Tapping the lock screen still unlocks.
  static final holdPrompt = ValueNotifier<bool>(false);

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
  late final LocalAuthentication _auth = widget.auth ?? LocalAuthentication();
  bool _authenticating = false;

  /// Which attempt is current. A stuck attempt that the system never
  /// finished (some phones restart the screen around the fingerprint
  /// sheet) is replaced, and the old one's late answer is ignored.
  int _run = 0;

  /// The current attempt has gone on for a few seconds, so a tap may replace it.
  bool _hung = false;
  Timer? _hungTimer;

  /// Short identifier of the last failed check (null: none, or just
  /// cancelled). Shown small so a tester can report it.
  String? _failure;

  /// Automatic tries since the last tap, and when the next may start:
  /// keeps a failing phone from flashing the sheet again and again.
  int _autoRetries = 0;
  DateTime _retryNotBefore = DateTime.fromMillisecondsSinceEpoch(0);

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
    _lock.addListener(() => AppLock.locked.value = _lock.value > 0);
    _refreshPinMode();
    WidgetsBinding.instance.addPostFrameCallback((_) => _maybePrompt());
  }

  Future<void> _refreshPinMode() async {
    try {
      final pin = await hasPin();
      if (mounted) setState(() => _pinMode = pin);
    } catch (_) {
      // Unreadable storage: keep what the screen shows now.
    }
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
    PinMatch match;
    try {
      match = await checkPin(_entered);
    } catch (_) {
      // Storage couldn't be read: never let the PIN screen hang.
      match = PinMatch.wrong;
    }
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
    _hungTimer?.cancel();
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
      _failure = null;
      _refreshPinMode();
    }
    if (state == AppLifecycleState.resumed) _maybePrompt();
  }

  /// Once per lock: after a cancel, the user taps to try again.
  void _maybePrompt() {
    if (!_promptWhenVisible || _lock.value < 1) return;
    if (WidgetsBinding.instance.lifecycleState != AppLifecycleState.resumed) return;
    if (AppLock.holdPrompt.value) return;
    if (DateTime.now().isBefore(_retryNotBefore)) return;
    _promptWhenVisible = false;
    _unlock();
  }

  /// [manual]: the person tapped. A check that has hung for a few seconds
  /// is then cancelled and started afresh.
  Future<void> _unlock({bool manual = false}) async {
    if (_authenticating) {
      if (!manual || !_hung) return;
      unawaited(_stopAuth());
    }
    if (manual) _autoRetries = 0;
    final run = ++_run;
    _authenticating = true;
    _hung = false;
    _hungTimer?.cancel();
    _hungTimer = Timer(const Duration(seconds: 3), () => _hung = true);
    if (_failure != null && mounted) setState(() => _failure = null);
    var ok = false;
    String? failure;
    try {
      ok = await _check();
    } catch (e) {
      failure = _failureCode(e);
      // Whatever the system left half-open must not block the next try.
      if (run == _run) await _stopAuth();
    }
    if (run != _run) return; // replaced by a newer attempt
    _authenticating = false;
    _hungTimer?.cancel();
    if (failure != null) {
      // A cancel is the person's choice; anything else is worth a try
      // again later (a couple of times) and a visible way to retry.
      if (failure != _cancelled && _autoRetries < 2) {
        _autoRetries++;
        _promptWhenVisible = true;
        _retryNotBefore = DateTime.now().add(const Duration(seconds: 2));
      }
      if (mounted) setState(() => _failure = failure == _cancelled ? null : failure);
      return;
    }
    // Biometrics always open the real profile.
    if (ok) {
      _autoRetries = 0;
      await _open(decoy: false);
    }
  }

  static const _cancelled = 'userCanceled';

  /// Asks the system for the unlock; throws if it fails or hangs.
  Future<bool> _check() async {
    for (var attempt = 0;; attempt++) {
      try {
        final supported = await _auth.isDeviceSupported().timeout(const Duration(seconds: 10));
        // Without a screen lock there is nothing to check against (with a
        // Sanctum PIN, the PIN is).
        if (!supported && _pinMode) return false;
        if (!supported) return true;
        return await _auth.authenticate(localizedReason: 'Unlock Sanctum').timeout(const Duration(seconds: 30));
      } on LocalAuthException catch (e) {
        // An earlier sheet the system never closed: cancel it, try once more.
        if (e.code != LocalAuthExceptionCode.authInProgress || attempt > 0) rethrow;
        await _stopAuth();
        await Future<void>.delayed(const Duration(milliseconds: 250));
      }
    }
  }

  Future<void> _stopAuth() async {
    try {
      await _auth.stopAuthentication().timeout(const Duration(seconds: 3));
    } catch (_) {}
  }

  /// A short, harmless identifier of what went wrong (never any user data).
  static String _failureCode(Object e) {
    if (e is LocalAuthException) return e.code.name;
    if (e is PlatformException) return e.code;
    if (e is TimeoutException) return 'timeout';
    return 'unknown';
  }

  /// What the lock screen says when the check failed, with the small
  /// identifier under it.
  Widget _failureNote({required String message}) => Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(message, textAlign: TextAlign.center, style: AppText.caption.copyWith(color: Palette.danger)),
          const SizedBox(height: Space.xs),
          Text('Error: $_failure', textAlign: TextAlign.center, style: AppText.micro),
        ],
      );

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
    final accent = widget.sigil.look.accent.color;
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
      color: widget.sigil.look.background.color,
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
              height: 56,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: Space.xl),
                child: Center(
                  child: waiting
                      ? Text('Too many tries: wait a moment', style: AppText.caption.copyWith(color: Palette.danger))
                      : _failure == null
                          ? const SizedBox.shrink()
                          : _failureNote(message: 'Couldn\'t check your fingerprint. Enter your PIN or try again.'),
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
                key(const Icon(Icons.fingerprint_rounded, size: 30), () => _unlock(manual: true),
                    tooltip: 'Fingerprint or face'),
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
      color: widget.sigil.look.background.color,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: () => _unlock(manual: true),
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
              alignment: Alignment(0, _failure != null ? 0.6 : 0.8),
              child: _failure != null
                  ? Padding(
                      padding: const EdgeInsets.symmetric(horizontal: Space.xl),
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          _failureNote(message: 'Couldn\'t check your fingerprint. Tap to try again.'),
                          const SizedBox(height: Space.md),
                          FilledButton.tonal(onPressed: () => _unlock(manual: true), child: const Text('Try again')),
                        ],
                      ),
                    )
                  : FadeTransition(
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
