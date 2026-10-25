import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:local_auth/local_auth.dart';
import 'package:sanctum/security/app_lock.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/frb_generated.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/theme/sigil_theme.dart';

/// A fingerprint check that fails or hangs on demand, like a phone whose
/// system sheet never reports back.
class _FakeAuth extends LocalAuthentication {
  Future<bool> Function() onAuthenticate = () async => false;
  int authenticateCalls = 0;
  int stopCalls = 0;

  @override
  Future<bool> isDeviceSupported() async => true;

  @override
  Future<bool> authenticate({
    required String localizedReason,
    Iterable<Object> authMessages = const <Object>[],
    bool biometricOnly = false,
    bool sensitiveTransaction = true,
    bool persistAcrossBackgrounding = false,
  }) {
    authenticateCalls++;
    return onAuthenticate();
  }

  @override
  Future<bool> stopAuthentication() async {
    stopCalls++;
    return true;
  }
}

void main() {
  late SigilSpec sigil;
  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
    FlutterSecureStorage.setMockInitialValues({});
  });

  Future<_FakeAuth> show(WidgetTester tester, Future<bool> Function() onAuthenticate) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);
    await tester.runAsync(() async {
      sigil = await sigilForKey(publicKey: await identityPublicKey(secret: await identityNew()));
    });
    final auth = _FakeAuth()..onAuthenticate = onAuthenticate;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(
      theme: sanctumTheme(accent: sigil.accent.color),
      home: AppLock(sigil: sigil, auth: auth, child: const Text('inside')),
    ));
    await tester.pump(const Duration(milliseconds: 100));
    return auth;
  }

  const message = 'Couldn\'t check your fingerprint. Tap to try again.';

  testWidgets('a stuck "already in progress" is cancelled, and the screen offers a retry', (tester) async {
    final auth = await show(tester, () async {
      throw const LocalAuthException(code: LocalAuthExceptionCode.authInProgress);
    });
    await tester.pump(const Duration(seconds: 1));
    expect(auth.stopCalls, greaterThanOrEqualTo(1));
    expect(auth.authenticateCalls, 2, reason: 'one more try after cancelling the stuck one');
    expect(find.text(message), findsOneWidget);
    expect(find.text('Error: authInProgress'), findsOneWidget);
    expect(find.text('Try again'), findsOneWidget);

    // Tapping again asks again.
    await tester.tap(find.text('Try again'));
    await tester.pump(const Duration(seconds: 1));
    expect(auth.authenticateCalls, 4);
  });

  testWidgets('any other error is caught and shows its short code', (tester) async {
    await show(tester, () async => throw PlatformException(code: 'weird_code'));
    await tester.pump(const Duration(seconds: 1));
    expect(find.text(message), findsOneWidget);
    expect(find.text('Error: weird_code'), findsOneWidget);
  });

  testWidgets('a check that never answers times out and can be retried', (tester) async {
    final hang = Completer<bool>();
    final auth = await show(tester, () => hang.future);
    expect(auth.authenticateCalls, 1);
    expect(find.text(message), findsNothing);
    await tester.pump(const Duration(seconds: 31));
    expect(auth.stopCalls, 1);
    expect(find.text('Error: timeout'), findsOneWidget);
    expect(find.text(message), findsOneWidget);

    await tester.tap(find.text('Try again'));
    await tester.pump(const Duration(seconds: 1));
    expect(auth.authenticateCalls, 2);
    await tester.pump(const Duration(seconds: 31));
  });

  testWidgets('a person cancelling is quiet: no error, tap to unlock stays', (tester) async {
    await show(tester, () async => throw const LocalAuthException(code: LocalAuthExceptionCode.userCanceled));
    await tester.pump(const Duration(seconds: 1));
    expect(find.text(message), findsNothing);
    expect(find.text('TAP TO UNLOCK'), findsOneWidget);
  });

  testWidgets('tapping while a check hangs cancels it and starts afresh', (tester) async {
    final auth = await show(tester, () => Completer<bool>().future);
    expect(auth.authenticateCalls, 1);
    await tester.pump(const Duration(seconds: 1));
    await tester.tap(find.text('TAP TO UNLOCK'));
    await tester.pump();
    expect(auth.authenticateCalls, 1, reason: 'too soon: the first sheet may still be open');
    await tester.pump(const Duration(seconds: 4));
    await tester.tap(find.text('TAP TO UNLOCK'));
    await tester.pump();
    expect(auth.authenticateCalls, 2);
    expect(auth.stopCalls, 1);
    await tester.pump(const Duration(seconds: 40));
  });
}
