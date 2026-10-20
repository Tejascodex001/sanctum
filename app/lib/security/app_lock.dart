import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:local_auth/local_auth.dart';

import '../sigil/sigil_painter.dart';
import '../src/rust/api/sigil.dart';

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
}

class _AppLockState extends State<AppLock> with WidgetsBindingObserver, TickerProviderStateMixin {
  final _auth = LocalAuthentication();
  bool _authenticating = false;

  /// 1 = fully locked, 0 = unlocked. Drives the unlock animation.
  late final _lock = AnimationController(vsync: this, duration: const Duration(milliseconds: 650), value: 1);
  late final _pulse = AnimationController(vsync: this, duration: const Duration(seconds: 2))..repeat(reverse: true);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    WidgetsBinding.instance.addPostFrameCallback((_) => _unlock());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _lock.dispose();
    _pulse.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // The system auth sheet itself pauses the app, so ignore that cycle.
    if (state == AppLifecycleState.hidden && !_authenticating) {
      _lock.value = 1;
    }
  }

  Future<void> _unlock() async {
    if (_authenticating) return;
    _authenticating = true;
    try {
      final supported = await _auth.isDeviceSupported();
      // Without a screen lock there is nothing to check against.
      final ok = !supported || await _auth.authenticate(localizedReason: 'Unlock Sanctum');
      if (ok) {
        HapticFeedback.mediumImpact();
        await _lock.reverse();
      }
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

  Widget _lockScreen() {
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
            Align(
              alignment: const Alignment(0, 0.82),
              child: FadeTransition(
                opacity: Tween(begin: 0.35, end: 0.9).animate(_pulse),
                child: const Text(
                  'TAP TO UNLOCK',
                  style: TextStyle(letterSpacing: 4, fontSize: 13, fontWeight: FontWeight.w600),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
