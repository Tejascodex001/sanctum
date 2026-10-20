import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/home/home_screen.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/api/session.dart';
import 'package:sanctum/src/rust/api/store.dart';
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

/// The app's bundled fonts (and Material icons), so screenshots show real
/// text instead of test boxes.
Future<void> loadFonts() async {
  Future<void> load(String family, List<String> files) async {
    final loader = FontLoader(family);
    for (final f in files) {
      final file = File(f);
      if (file.existsSync()) loader.addFont(Future.value(ByteData.sublistView(file.readAsBytesSync())));
    }
    await loader.load();
  }

  await load('Inter', ['assets/fonts/Inter-400.ttf', 'assets/fonts/Inter-500.ttf', 'assets/fonts/Inter-600.ttf']);
  await load('SpaceGrotesk', ['assets/fonts/SpaceGrotesk-500.ttf', 'assets/fonts/SpaceGrotesk-700.ttf']);
  await load('JetBrainsMono', ['assets/fonts/JetBrainsMono-500.ttf']);
  final icons = '${Platform.environment['FLUTTER_ROOT'] ?? '/opt/flutter'}/bin/cache/artifacts/material_fonts/MaterialIcons-Regular.otf';
  await load('MaterialIcons', [icons]);
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
    FlutterSecureStorage.setMockInitialValues({});
    final dir = Directory.systemTemp.createTempSync('sanctum_test');
    await sessionOpen(identitySecret: await identityNew(), dir: dir.path, storeKey: await storeNewKey());
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

    // Temp chats ("Just me": a widget test runs a single phone; the Rust
    // tests cover temp chats between two phones).
    await tester.tap(find.text('Temp'));
    await settle(tester);
    for (var i = 0; i < 2; i++) {
      await tester.tap(find.text('New temp chat'));
      await settle(tester);
      // The picker offers a temp chat with yourself.
      expect(find.text('Only on this phone'), findsOneWidget);
      await tester.tap(find.text('Only on this phone'));
      await settle(tester, 6);
    }
    expect(find.text('Just me'), findsNWidgets(2));

    for (final (i, msg) in [(0, 'see you at 8'), (1, 'gone soon')]) {
      // The list re-sorts by activity, so pick the chat that is still empty.
      await tester.tap(find.text('Burns 24h after sending').first);
      await settle(tester);
      await tester.enterText(find.byType(TextField).last, msg);
      await settle(tester, 3);
      await tester.tap(find.byTooltip('Send'));
      await settle(tester);
      expect(find.text(msg), findsOneWidget);
      expect(find.textContaining('burns in 23h'), findsOneWidget);
      if (i == 1) await shot(tester, '3-temp-chat');
      await tester.tap(find.byTooltip('Back'));
      await settle(tester, 8);
    }
    await shot(tester, '2-temp-list');

    // Search filters by last message.
    await tester.enterText(find.byType(TextField).first, 'see you');
    await settle(tester, 3);
    expect(find.text('Just me'), findsOneWidget);
    await tester.enterText(find.byType(TextField).first, '');
    await settle(tester, 3);

    // Wipe from inside a chat.
    await tester.tap(find.text('gone soon'));
    await settle(tester);
    await tester.tap(find.text('Wipe'));
    await settle(tester, 14);
    expect(find.text('gone soon'), findsNothing);
    expect(find.text('see you at 8'), findsOneWidget);

    // Back to Chats for the home screenshot.
    await tester.tap(find.text('Chats'));
    await settle(tester);
    await shot(tester, '1-chats');

    // Add a friend: the screen shows our code and a way to read theirs.
    await tester.tap(find.byTooltip('Add a friend'));
    await settle(tester);
    await tester.enterText(find.byType(TextField).first, 'Tejas');
    await settle(tester, 8);
    expect(find.text('YOUR CODE'), findsOneWidget);
    expect(find.text('Scan their QR'), findsOneWidget);
    await shot(tester, '4-add-friend');
    await tester.tap(find.byTooltip('Back'));
    await settle(tester);

    // The Friends tab: empty for now, with a way to add one.
    await tester.tap(find.text('Friends'));
    await settle(tester);
    expect(find.text('No friends yet'), findsOneWidget);
    expect(find.text('Add a friend'), findsOneWidget);
    await shot(tester, '5-friends');

    // A fresh home screen reads notes back from the encrypted store. (Temp
    // chats end with the process; the Rust tests cover that.)
    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
    await tester.runAsync(() async {
      home = HomeScreen(publicKey: home.publicKey, mySigil: mine);
    });
    await tester.pumpWidget(MaterialApp(theme: sanctumTheme(accent: mine.accent.color), home: home));
    await settle(tester);
    expect(find.text('Notes to self'), findsOneWidget);
    expect(find.text('hello'), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
    await settle(tester, 2);
  });
}
