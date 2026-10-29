import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/chat/chat_screen.dart';
import 'package:sanctum/chat/message_source.dart';
import 'package:sanctum/deck/chat_card.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/session.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/api/store.dart';
import 'package:sanctum/src/rust/frb_generated.dart';
import 'package:sanctum/sigil/sigil_look.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/theme/appearance.dart';
import 'package:sanctum/theme/sigil_theme.dart';

/// A chat whose stored copy of a sent message arrives a moment after it's
/// sent, like the real store.
class _FakeSource implements MessageSource {
  final stored = <ChatMessage>[];

  @override
  bool get replies => true;

  @override
  Future<List<ChatMessage>> load({int? limit}) async {
    await Future<void>.delayed(const Duration(milliseconds: 20));
    return List.of(stored);
  }

  @override
  Future<void> send(String body, {int? replyTo}) async {
    await Future<void>.delayed(const Duration(milliseconds: 60));
    stored.add(ChatMessage(body: body, fromMe: true, sentAt: DateTime.now(), seq: stored.length + 1, canEdit: true));
  }

  @override
  Future<void> edit(int seq, String body) async {
    final i = stored.indexWhere((m) => m.seq == seq);
    final m = stored[i];
    stored[i] = ChatMessage(body: body, fromMe: true, sentAt: m.sentAt, seq: seq, edited: true, canEdit: true);
  }
}

/// The State object of the bubble that shows [text].
State? _bubbleStateOf(WidgetTester tester, String text) {
  State? found;
  tester.element(find.text(text)).visitAncestorElements((e) {
    if (e is StatefulElement && e.state.runtimeType.toString() == '_BubbleState') {
      found = e.state;
      return false;
    }
    return true;
  });
  return found;
}

final _shotKey = GlobalKey();

/// Saves a PNG of the chat when SCREENSHOT_DIR is set.
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
  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
    FlutterSecureStorage.setMockInitialValues({});
    final dir = Directory.systemTemp.createTempSync('sanctum_chat_test');
    await sessionOpen(identitySecret: await identityNew(), dir: dir.path, storeKey: await storeNewKey());
  });

  testWidgets('a message you send keeps its bubble when the stored copy replaces it', (tester) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);

    final source = _FakeSource();
    late SigilSpec sigil;
    await tester.runAsync(() async {
      sigil = await sigilForKey(publicKey: await identityPublicKey(secret: await identityNew()));
    });
    final card = ChatCard(id: 1, title: 'Friend', sigil: sigil, source: source);
    // The keyboard channel is poked when a reply starts.
    final calls = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.textInput, (call) async {
      calls.add(call.method);
      return null;
    });
    addTearDown(() => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.textInput, null));

    await tester.pumpWidget(Restyler(
      builder: (_) => MaterialApp(
        theme: sanctumTheme(accent: sigil.look.accent.color),
        home: RepaintBoundary(key: _shotKey, child: ChatScreen(card: card, close: () {})),
      ),
    ));
    Future<void> settle([int frames = 8]) async {
      for (var i = 0; i < frames; i++) {
        await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 40)));
        await tester.pump(const Duration(milliseconds: 100));
      }
    }

    await settle();
    await tester.enterText(find.byType(TextField).last, 'hello there');
    await tester.pump();
    await tester.tap(find.byTooltip('Send'));
    // The optimistic copy, before the store has it.
    await tester.pump();
    final before = _bubbleStateOf(tester, 'hello there');
    expect(before, isNotNull);
    expect(source.stored, isEmpty);

    await settle();
    expect(source.stored, hasLength(1));
    final after = _bubbleStateOf(tester, 'hello there');
    expect(find.text('hello there'), findsOneWidget);
    expect(identical(before, after), isTrue, reason: 'the bubble must not be rebuilt (it would blink)');

    // Choosing "Reply" brings the keyboard up, even if the field still has
    // focus from before the keyboard was hidden.
    await tester.longPress(find.text('hello there'));
    await settle(4);
    calls.clear();
    await tester.tap(find.text('Reply'));
    await settle(8);
    expect(calls, contains('TextInput.show'));
    expect(find.textContaining('Replying to'), findsOneWidget);
    await tester.tap(find.byTooltip('Cancel reply'));
    await settle(4);

    // "Edit" puts the words in the box; saving changes the bubble, marked edited.
    await tester.longPress(find.text('hello there'));
    await settle(4);
    await tester.tap(find.text('Edit'));
    await settle(8);
    expect(find.text('Editing your message'), findsOneWidget);
    final field = tester.widget<TextField>(find.byType(TextField).last);
    expect(field.controller!.text, 'hello there');
    await _shot(tester, 'chat_editing');
    await tester.enterText(find.byType(TextField).last, 'hello again');
    await tester.pump();
    await tester.tap(find.byTooltip('Save'));
    await settle();
    expect(source.stored.single.body, 'hello again');
    expect(find.text('hello again'), findsOneWidget);
    expect(find.text('hello there'), findsNothing);
    expect(find.textContaining('edited'), findsOneWidget);
    expect(find.text('Editing your message'), findsNothing);
    await _shot(tester, 'chat_edited');
  });
}
