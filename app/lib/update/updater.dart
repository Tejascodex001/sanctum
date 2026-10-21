import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import '../background/background.dart';
import '../identity/secrets.dart';
import '../net/net_bus.dart';
import '../src/rust/api/session.dart';
import '../src/rust/api/update.dart';

/// Over-the-air updates from the project's GitHub releases, checked and
/// downloaded through Tor. Android installs an update only if it's signed
/// with the same key as this app.
class Updater {
  Updater._();

  static final instance = Updater._();

  static const _checkEvery = Duration(hours: 6);

  /// After a failed check (Tor can be slow or flaky), try again this soon.
  static const _retryAfter = Duration(minutes: 15);

  /// A newer release, once found.
  final available = ValueNotifier<UpdateInfo?>(null);

  /// 0–1 while downloading; null otherwise.
  final progress = ValueNotifier<double?>(null);

  /// The last problem, to show; cleared by the next attempt.
  final error = ValueNotifier<String?>(null);

  int current = 0;
  DateTime? _lastCheck;
  DateTime? _lastFailure;
  bool _checking = false;
  Timer? _timer;

  /// Keeps checking while the app runs: the app can stay open for days
  /// (it keeps you reachable in the background), and a single check when
  /// Tor first connects may fail.
  void start() {
    _timer ??= Timer.periodic(const Duration(minutes: 5), (_) => check());
  }

  /// Looks for a newer release, at most every few hours unless [force]d
  /// (sooner after a failed check).
  Future<void> check({bool force = false}) async {
    if (_checking || progress.value != null) return;
    // Not connected yet: checking now could only fail. Connecting checks.
    if (!force && NetBus.instance.status.value.status != NetStatus.online) return;
    final now = DateTime.now();
    if (!force && _lastCheck != null && now.difference(_lastCheck!) < _checkEvery) return;
    if (!force && _lastFailure != null && now.difference(_lastFailure!) < _retryAfter) return;
    _checking = true;
    try {
      current = await Background.buildNumber();
      if (!await loadFlag('auto_update_check', fallback: true) && !force) return;
      available.value = await _checkWithRetries();
      _lastCheck = DateTime.now();
      _lastFailure = null;
      error.value = null;
    } catch (e) {
      _lastFailure = DateTime.now();
      if (force) error.value = 'Couldn\'t check for updates. Try again in a moment.';
    } finally {
      _checking = false;
    }
  }

  /// A Tor connection sometimes fails on its own; the next one usually works.
  Future<UpdateInfo?> _checkWithRetries() async {
    for (var attempt = 1;; attempt++) {
      try {
        return await updateCheck(current: current);
      } catch (_) {
        if (attempt == 3) rethrow;
        await Future<void>.delayed(Duration(seconds: 10 * attempt));
      }
    }
  }

  /// Downloads (if not yet) and opens Android's installer.
  Future<void> install() async {
    final update = available.value;
    if (update == null || progress.value != null) return;
    error.value = null;
    final dir = Directory('${(await getApplicationCacheDirectory()).path}/updates');
    final path = '${dir.path}/sanctum-${update.build}.apk';
    try {
      if (!File(path).existsSync()) {
        // Older downloads are no use any more.
        if (dir.existsSync()) dir.deleteSync(recursive: true);
        progress.value = 0;
        final poll = Timer.periodic(const Duration(milliseconds: 400), (_) async {
          progress.value = await updateProgress();
        });
        try {
          await updateDownload(url: update.url, path: path);
        } finally {
          poll.cancel();
        }
      }
      progress.value = null;
      final result = await Background.installApk(path);
      if (result == 'permission') {
        error.value = 'Allow Sanctum to install apps, then tap Update again.';
      } else if (result != 'started') {
        error.value = 'Couldn\'t open the installer.';
      }
    } catch (e) {
      progress.value = null;
      error.value = 'The download didn\'t finish. Check your connection and tap Update again.';
    }
  }
}
