import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/home/home_screen.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/frb_generated.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/theme/sigil_theme.dart';

/// Rust calls finish on real threads, so also let real time pass between
/// frames for their continuations to land.
Future<void> settle(WidgetTester tester, [int frames = 10]) async {
  for (var i = 0; i < frames; i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 30)));
    await tester.pump(const Duration(milliseconds: 100));
  }
}

/// Real fonts, so screenshots show text instead of test boxes.
Future<void> loadFonts() async {
  final dir = '${Platform.environment['FLUTTER_ROOT'] ?? '/opt/flutter'}/bin/cache/artifacts/material_fonts';
  final roboto = FontLoader('Roboto');
  for (final f in ['Roboto-Regular.ttf', 'Roboto-Medium.ttf', 'Roboto-Bold.ttf']) {
    final file = File('$dir/$f');
    if (file.existsSync()) roboto.addFont(Future.value(ByteData.sublistView(file.readAsBytesSync())));
  }
  await roboto.load();
  final icons = File('$dir/MaterialIcons-Regular.otf');
  if (icons.existsSync()) {
    await (FontLoader('MaterialIcons')..addFont(Future.value(ByteData.sublistView(icons.readAsBytesSync())))).load();
  }
}

final _shotKey = GlobalKey();

/// Saves a PNG of the app when SCREENSHOT_DIR is set.
Future<void> shot(WidgetTester tester, String name) async {
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
  setUpAll(() async {
    await RustLib.init(
      externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'),
    );
    await loadFonts();
  });

  testWidgets('notes, temp chats, pinning and wipe work end to end', (tester) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);

    late HomeScreen home;
    late SigilSpec mine;
    await tester.runAsync(() async {
      final pk = await identityPublicKey(secret: await identityNew());
      mine = await sigilForKey(publicKey: pk);
      home = HomeScreen(publicKey: pk, mySigil: mine);
    });
    await tester.pumpWidget(RepaintBoundary(
      key: _shotKey,
      child: MaterialApp(theme: sanctumTheme(accent: mine.accent.color), home: home),
    ));
    await settle(tester);
    expect(find.text('Notes to self'), findsOneWidget);

    // Notes to self.
    await tester.tap(find.text('Notes to self'));
    await settle(tester);
    await tester.enterText(find.byType(TextField).last, 'hello');
    await settle(tester, 3);
    await tester.tap(find.byTooltip('Send'));
    await settle(tester);
    expect(find.text('hello'), findsOneWidget);
    await tester.tap(find.byTooltip('Back'));
    await settle(tester);
    expect(find.text('hello'), findsOneWidget, reason: 'card shows the last message');

    // Several temp chats.
    await tester.tap(find.text('Temp'));
    await settle(tester);
    for (var i = 0; i < 4; i++) {
      await tester.tap(find.text('New temp chat'));
      await settle(tester, 4);
    }
    expect(find.text('Ember 1'), findsOneWidget);
    expect(find.text('Cinder 1'), findsOneWidget);

    for (final (name, msg) in [('Spark 1', 'see you at 8'), ('Ember 1', 'gone soon')]) {
      await tester.tap(find.text(name));
      await settle(tester);
      await tester.enterText(find.byType(TextField).last, msg);
      await settle(tester, 3);
      await tester.tap(find.byTooltip('Send'));
      await settle(tester);
      expect(find.text(msg), findsOneWidget);
      expect(find.textContaining('burns in 23h'), findsOneWidget);
      if (name == 'Ember 1') await shot(tester, '3-temp-chat');
      await tester.tap(find.byTooltip('Back'));
      await settle(tester);
    }
    await settle(tester, 6);
    await shot(tester, '2-temp-list');

    // Pin a chat via long press.
    await tester.longPress(find.text('Flare 1'));
    await settle(tester);
    await tester.tap(find.text('Pin to top'));
    await settle(tester);
    expect(find.byIcon(Icons.push_pin_rounded), findsOneWidget);

    // Search filters the stack.
    await tester.enterText(find.byType(TextField).first, 'see you');
    await settle(tester, 3);
    expect(find.text('Spark 1'), findsOneWidget);
    expect(find.text('Cinder 1'), findsNothing);
    await tester.enterText(find.byType(TextField).first, '');
    await settle(tester, 3);

    // Wipe from inside a chat.
    await tester.tap(find.text('Ember 1'));
    await settle(tester);
    await tester.tap(find.text('Wipe'));
    await settle(tester, 14);
    expect(find.text('Ember 1'), findsNothing);
    expect(find.text('Spark 1'), findsOneWidget);

    // Back to Chats for the home screenshot.
    await tester.tap(find.text('Chats'));
    await settle(tester);
    await shot(tester, '1-chats');

    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });
}
