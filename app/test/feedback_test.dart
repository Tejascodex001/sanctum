import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/settings/feedback_screen.dart';
import 'package:sanctum/settings/settings_screen.dart';
import 'package:sanctum/sigil/sigil_look.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/src/rust/api/feedback.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/session.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/api/store.dart';
import 'package:sanctum/src/rust/frb_generated.dart';
import 'package:sanctum/theme/appearance.dart';
import 'package:sanctum/theme/sigil_theme.dart';

import 'home_test.dart' show loadFonts, settle;

final _shotKey = GlobalKey();

Future<void> _shot(WidgetTester tester, String name) async {
  final dir = Platform.environment['SCREENSHOT_DIR'];
  if (dir == null) return;
  await tester.runAsync(() async {
    final boundary = _shotKey.currentContext!.findRenderObject()! as RenderRepaintBoundary;
    final image = await boundary.toImage(pixelRatio: 1);
    final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
    File('$dir/$name.png').writeAsBytesSync(bytes!.buffer.asUint8List());
  });
}

// Runs the real Rust core on the host: `cargo build -p rust_lib_sanctum` first.
void main() {
  late SigilSpec mine;

  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
    await loadFonts();
    FlutterSecureStorage.setMockInitialValues({});
    final dir = Directory.systemTemp.createTempSync('sanctum_feedback_test');
    final secret = await identityNew();
    await sessionOpen(identitySecret: secret, dir: dir.path, storeKey: await storeNewKey());
    mine = await sigilForKey(publicKey: await identityPublicKey(secret: secret));
  });

  Future<void> show(WidgetTester tester, Widget screen) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(RepaintBoundary(
      key: _shotKey,
      child: Restyler(
        builder: (_) => MaterialApp(theme: sanctumTheme(accent: mine.look.accent.color), home: screen),
      ),
    ));
    await settle(tester);
  }

  testWidgets('the form shows what will be sent, and waits for an address', (tester) async {
    await show(tester, const FeedbackScreen());
    expect(find.text('WHAT WILL BE SENT'), findsOneWidget);
    // This build has no feedback address yet: it says so, and won't send.
    expect(feedbackOpen(), isFalse);
    expect(find.textContaining('isn\'t open yet'), findsOneWidget);
    await tester.tap(find.byTooltip('4 of 5'));
    await tester.enterText(find.byType(TextField), 'Love the sigils. Calls drop on my train ride.');
    await settle(tester, 3);
    expect(find.text('4 of 5 stars'), findsOneWidget);
    await _shot(tester, '20-feedback');
    // Phone details can be left out.
    await tester.tap(find.text('Include phone details'));
    await settle(tester, 3);
    expect(find.text('Not included'), findsNWidgets(3));
    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.light));
    await settle(tester);
    await _shot(tester, '21-feedback-light');
    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.dark));
    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });

  testWidgets('seven taps on the version open the inbox, which gives the address', (tester) async {
    await show(tester, SettingsScreen(mySigil: mine));
    await tester.scrollUntilVisible(find.text('Send feedback'), 300);
    await settle(tester, 2);
    expect(find.text('Feedback inbox'), findsNothing);
    for (var i = 0; i < 7; i++) {
      await tester.tap(find.textContaining('Sanctum beta'));
      await tester.pump(const Duration(milliseconds: 50));
    }
    await settle(tester, 3);
    expect(find.text('Feedback inbox'), findsOneWidget);
    await tester.scrollUntilVisible(find.text('Feedback inbox'), 200);
    await settle(tester, 2);
    await tester.tap(find.text('Feedback inbox'));
    await settle(tester);
    expect(find.text('NOTHING YET'), findsOneWidget);
    await tester.tap(find.text('Receive feedback'));
    await settle(tester);
    final address = await tester.runAsync(feedbackAddress);
    expect(address, endsWith('.onion'));
    expect(find.text(address!), findsOneWidget);
    await _shot(tester, '22-feedback-inbox');
    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });
}
