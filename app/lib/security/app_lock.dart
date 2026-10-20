import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:local_auth/local_auth.dart';

/// Covers [child] until the user unlocks with biometrics or the device
/// PIN/pattern, and locks again whenever the app leaves the foreground.
class AppLock extends StatefulWidget {
  const AppLock({super.key, required this.child, required this.background});

  final Widget child;

  /// Shown behind the unlock prompt (the user's own sigil).
  final Widget background;

  @override
  State<AppLock> createState() => _AppLockState();
}

class _AppLockState extends State<AppLock> with WidgetsBindingObserver {
  final _auth = LocalAuthentication();
  bool _locked = true;
  bool _authenticating = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    WidgetsBinding.instance.addPostFrameCallback((_) => _unlock());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // The system auth sheet itself pauses the app, so ignore that cycle.
    if (state == AppLifecycleState.hidden && !_authenticating) {
      setState(() => _locked = true);
    }
  }

  Future<void> _unlock() async {
    if (_authenticating) return;
    _authenticating = true;
    try {
      final supported = await _auth.isDeviceSupported();
      // Without a screen lock there is nothing to check against.
      final ok = !supported ||
          await _auth.authenticate(localizedReason: 'Unlock Sanctum');
      if (ok) {
        HapticFeedback.mediumImpact();
        setState(() => _locked = false);
      }
    } on LocalAuthException {
      // Cancelled or locked out; stay locked and let the user retry.
    } finally {
      _authenticating = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    if (!_locked) return widget.child;
    return Scaffold(
      body: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: _unlock,
        child: Stack(
          fit: StackFit.expand,
          children: [
            widget.background,
            Align(
              alignment: const Alignment(0, 0.8),
              child: Text(
                'Tap to unlock',
                style: Theme.of(context).textTheme.labelLarge?.copyWith(
                      letterSpacing: 3,
                      color: Colors.white70,
                    ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
