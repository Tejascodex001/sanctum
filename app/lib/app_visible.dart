import 'package:flutter/widgets.dart';

/// Whether the app is on screen. Timers that only refresh what's drawn skip
/// their work while it isn't: every wake-up costs battery.
bool get appVisible {
  final state = WidgetsBinding.instance.lifecycleState;
  return state != AppLifecycleState.hidden && state != AppLifecycleState.paused;
}
