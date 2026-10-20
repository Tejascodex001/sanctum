import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/home/home_screen.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/frb_generated.dart';

/// Pumps frames for a second; the living sigils never settle.
/// Rust calls finish on real threads, so also let real time pass between
/// frames for their continuations to land.
Future<void> settle(WidgetTester tester) async {
  for (var i = 0; i < 10; i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 50)));
    await tester.pump(const Duration(milliseconds: 100));
  }
}

// Runs the real Rust core on the host: `cargo build -p rust_lib_sanctum` first.
void main() {
  setUpAll(() async {
    await RustLib.init(
      externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'),
    );
  });

  testWidgets('notes and temp chats work end to end', (tester) async {
    late HomeScreen home;
    await tester.runAsync(() async {
      final pk = await identityPublicKey(secret: await identityNew());
      home = HomeScreen(publicKey: pk, mySigil: await sigilForKey(publicKey: pk));
    });
    await tester.pumpWidget(MaterialApp(home: home));
    await tester.pump();
    expect(find.text('Notes to self'), findsOneWidget);

    await tester.tap(find.text('Notes to self'));
    await settle(tester);
    await tester.enterText(find.byType(TextField), 'hello');
    await tester.runAsync(() => tester.testTextInput.receiveAction(TextInputAction.done));
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 1500)));
    await settle(tester);
    expect(find.text('hello'), findsOneWidget);

    await tester.tap(find.byIcon(Icons.arrow_back_ios_new_rounded));
    await settle(tester);
    await tester.tap(find.text('TEMP'));
    await settle(tester);
    await tester.runAsync(() async {
      await tester.tap(find.text('New temp chat'));
      await Future<void>.delayed(const Duration(milliseconds: 1500));
    });
    await settle(tester);
    await tester.enterText(find.byType(TextField), 'gone soon');
    await tester.runAsync(() async {
      await tester.testTextInput.receiveAction(TextInputAction.done);
      await Future<void>.delayed(const Duration(milliseconds: 1500));
    });
    await settle(tester);
    expect(find.text('gone soon'), findsOneWidget);
    expect(find.textContaining('burns in 23h'), findsOneWidget);

    await tester.tap(find.text('Wipe'));
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 1500)));
    await settle(tester);
    expect(find.text('NO TEMP CHATS'), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
  });
}
