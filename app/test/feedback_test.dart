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

  testWidgets('the form shows what will be sent, and queues it for the developer', (tester) async {
    await show(tester, const FeedbackScreen());
    expect(find.text('WHAT WILL BE SENT'), findsOneWidget);
    // This build knows the developer's feedback address.
    expect(feedbackOpen(), isTrue);
    expect(find.textContaining('isn\'t open yet'), findsNothing);
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
    // Without Tor it waits on this phone, to go once connected.
    final sent = await tester.runAsync(
      () => feedbackSend(form: FeedbackForm(phone: '', android: '', build: 0, rating: 4, text: 'Hello')),
    );
    expect(sent, FeedbackSent.queued);
    expect(await tester.runAsync(feedbackWaiting), 1);
    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });

  testWidgets('seven taps on the version open the inbox, which gives the address', (tester) async {
    await show(tester, SettingsScreen(mySigil: mine));
    await tester.scrollUntilVisible(find.text('Updates'), 300);
    await settle(tester, 2);
    await tester.tap(find.text('Updates'));
    await settle(tester);
    for (var i = 0; i < 7; i++) {
      await tester.tap(find.textContaining('Sanctum beta'));
      await tester.pump(const Duration(milliseconds: 50));
    }
    await settle(tester, 3);
    await tester.pageBack();
    await settle(tester);
    await tester.tap(find.text('Feedback'));
    await settle(tester);
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

  testWidgets('the inbox filters by stars, version, words and search', (tester) async {
    FeedbackItem item(int at, int rating, int build, String text, String phone) =>
        FeedbackItem(at: at, phone: phone, android: '14', build: build, rating: rating, text: text);
    final sample = [
      item(1792600000, 5, 66, 'Love the sigils. Calls are clear on Wi-Fi.', 'Samsung Galaxy A54'),
      item(1792590000, 2, 66, 'Video calls freeze on mobile data.', 'Redmi Note 12'),
      item(1792580000, 4, 65, '', 'Pixel 7'),
      item(1792570000, 5, 65, 'Temp chats are great for planning surprises.', 'OnePlus Nord'),
      item(1792560000, 0, 0, 'Can groups have more than 50 people?', ''),
    ];
    await show(tester, FeedbackInboxScreen(sample: sample));
    await tester.scrollUntilVisible(find.text('5 RECEIVED · AVERAGE 4.0 STARS'), 300,
        scrollable: find.byType(Scrollable).first);
    await settle(tester, 2);
    await tester.ensureVisible(find.text('5 stars'));
    await settle(tester, 2);
    await _shot(tester, '23-feedback-filters');
    await tester.tap(find.text('5 stars'));
    await settle(tester, 2);
    expect(find.text('2 OF 5 SHOWN · AVERAGE 5.0 STARS'), findsOneWidget);
    await tester.tap(find.text('Clear filters'));
    await settle(tester, 2);
    await tester.ensureVisible(find.text('Has words'));
    await tester.tap(find.text('Has words'));
    await settle(tester, 2);
    expect(find.textContaining('4 OF 5 SHOWN'), findsOneWidget);
    await tester.enterText(find.byType(TextField), 'redmi');
    await settle(tester, 2);
    expect(find.textContaining('1 OF 5 SHOWN'), findsOneWidget);
    expect(find.text('Video calls freeze on mobile data.'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });
}
