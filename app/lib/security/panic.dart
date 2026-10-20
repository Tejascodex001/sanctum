import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import '../background/background.dart';
import '../identity/secrets.dart';
import '../src/rust/api/session.dart';
import '../theme/tokens.dart';

/// Erases Sanctum from this phone and closes it. The next start is a fresh
/// install: new identity, no friends, no chats.
///
/// Keys go first (crypto-shredding: the encrypted database is unreadable
/// without them, whatever flash storage keeps), then the files, then the
/// process with every temp chat in its memory.
Future<void> panicWipe() async {
  await wipeSecrets();
  await Background.stop();
  await Background.cancelAll();
  try {
    final support = (await getApplicationSupportDirectory()).path;
    final cache = (await getApplicationCacheDirectory()).path;
    await wipeEverything(paths: [
      for (final suffix in ['', '-wal', '-shm', '-journal']) '$support/sanctum.db$suffix',
      '$support/tor-state',
      '$cache/tor-cache',
    ]);
  } catch (_) {
    // Already unreadable without the keys; leftovers are noise.
  }
  exit(0);
}

/// Press and hold to wipe: impossible to trigger by accident, quick on
/// purpose.
class HoldToWipe extends StatefulWidget {
  const HoldToWipe({super.key});

  @override
  State<HoldToWipe> createState() => _HoldToWipeState();
}

class _HoldToWipeState extends State<HoldToWipe> with SingleTickerProviderStateMixin {
  late final _hold = AnimationController(vsync: this, duration: const Duration(seconds: 2))
    ..addStatusListener((s) {
      if (s == AnimationStatus.completed) {
        HapticFeedback.heavyImpact();
        panicWipe();
      }
    });

  @override
  void dispose() {
    _hold.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      onTapDown: (_) {
        HapticFeedback.mediumImpact();
        _hold.forward();
      },
      onTapUp: (_) => _hold.isCompleted ? null : _hold.reverse(),
      onTapCancel: () => _hold.isCompleted ? null : _hold.reverse(),
      child: AnimatedBuilder(
        animation: _hold,
        builder: (context, _) => Container(
          height: 52,
          clipBehavior: Clip.antiAlias,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(Radii.md),
            border: Border.all(color: Palette.danger.withValues(alpha: 0.7)),
          ),
          child: Stack(
            fit: StackFit.expand,
            children: [
              FractionallySizedBox(
                alignment: Alignment.centerLeft,
                widthFactor: _hold.value,
                child: const ColoredBox(color: Palette.danger),
              ),
              Center(
                child: Text(
                  _hold.value == 0 ? 'Hold to wipe everything' : 'Keep holding…',
                  style: AppText.bodyStrong.copyWith(color: _hold.value > 0.5 ? Colors.white : Palette.danger),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// What a panic wipe does, with the hold button.
Future<void> showPanicSheet(BuildContext context) => showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (context) => const SafeArea(
        child: Padding(
          padding: EdgeInsets.fromLTRB(Space.xl, 0, Space.xl, Space.xl),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text('Panic wipe', style: AppText.title),
              SizedBox(height: Space.sm),
              Text(
                'Erases your identity, friends, every chat and temp chat from this phone, '
                'and closes Sanctum. It can\'t be undone. Your friends keep their copies.',
                style: AppText.caption,
              ),
              SizedBox(height: Space.lg),
              HoldToWipe(),
            ],
          ),
        ),
      ),
    );
