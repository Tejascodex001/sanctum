import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import '../background/background.dart';
import '../identity/secrets.dart';
import '../src/rust/api/update.dart';

/// Over-the-air updates from the project's GitHub releases, checked and
/// downloaded through Tor. Android installs an update only if it's signed
/// with the same key as this app.
class Updater {
  Updater._();

  static final instance = Updater._();

  static const _checkEvery = Duration(hours: 6);

  /// A newer release, once found.
  final available = ValueNotifier<UpdateInfo?>(null);

  /// 0–1 while downloading; null otherwise.
  final progress = ValueNotifier<double?>(null);

  /// The last problem, to show; cleared by the next attempt.
  final error = ValueNotifier<String?>(null);

  int current = 0;
  DateTime? _lastCheck;
  bool _checking = false;

  /// Looks for a newer release, at most every few hours unless [force]d.
  Future<void> check({bool force = false}) async {
    if (_checking || progress.value != null) return;
    if (!force && _lastCheck != null && DateTime.now().difference(_lastCheck!) < _checkEvery) return;
    _checking = true;
    try {
      current = await Background.buildNumber();
      if (!await loadFlag('auto_update_check', fallback: true) && !force) return;
      available.value = await updateCheck(current: current);
      _lastCheck = DateTime.now();
      error.value = null;
    } catch (e) {
      if (force) error.value = 'Couldn\'t check for updates. Try again in a moment.';
    } finally {
      _checking = false;
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
