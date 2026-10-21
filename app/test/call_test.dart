import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/call/call_screen.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/src/rust/api/call.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/frb_generated.dart';
import 'package:sanctum/theme/appearance.dart';
import 'package:sanctum/theme/sigil_theme.dart';
import 'package:sanctum/theme/tokens.dart';

final _shotKey = GlobalKey();

Future<void> _loadFonts() async {
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
  final icons =
      '${Platform.environment['FLUTTER_ROOT'] ?? '/opt/flutter'}/bin/cache/artifacts/material_fonts/MaterialIcons-Regular.otf';
  await load('MaterialIcons', [icons]);
}

/// Saves a PNG when SCREENSHOT_DIR is set.
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

CallInfo _call(CallPhaseView phase, {bool outgoing = true, CallOutcomeView? outcome, bool hidden = false}) => CallInfo(
      id: '00',
      chatId: PlatformInt64Util.from(1),
      name: 'Asha',
      sigilKey: Uint8List(32),
      hidden: hidden,
      outgoing: outgoing,
      phase: phase,
      outcome: outcome,
      answeredAtMs:
          phase == CallPhaseView.active ? PlatformInt64Util.from(DateTime.now().millisecondsSinceEpoch - 252000) : null,
      sendCodec: 'opus',
      recvCodec: 'opus',
      nearby: false,
    );

void main() {
  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
    await _loadFonts();
    FlutterSecureStorage.setMockInitialValues({});
  });

  test('the call screen says plainly how a call is going', () {
    expect(callStatus(_call(CallPhaseView.connecting), 'Asha'), 'Connecting privately…');
    expect(callStatus(_call(CallPhaseView.ringing), 'Asha'), 'Ringing…');
    expect(callStatus(_call(CallPhaseView.ringing, outgoing: false), 'Asha'), 'Incoming call');
    expect(callStatus(_call(CallPhaseView.active), 'Asha', talked: const Duration(seconds: 252)), '4:12');
    expect(
      callStatus(_call(CallPhaseView.ended, outcome: CallOutcomeView.unreachable), 'Asha'),
      'Asha isn\'t reachable right now',
    );
    expect(callStatus(_call(CallPhaseView.ended, outcome: CallOutcomeView.busy), 'Asha'), 'Asha is on another call');
    expect(talkTime(const Duration(hours: 1, minutes: 2, seconds: 3)), '1:02:03');
  });

  testWidgets('incoming, talking and ended calls, dark and light', (tester) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);
    late SigilSpec sigil;
    await tester.runAsync(() async {
      sigil = await sigilForKey(publicKey: await identityPublicKey(secret: await identityNew()));
    });
    var answered = 0, ended = 0;

    Future<void> show(CallInfo info, {bool locked = false}) async {
      await tester.pumpWidget(RepaintBoundary(
        key: _shotKey,
        child: Restyler(
          builder: (_) => MaterialApp(
            theme: sanctumTheme(accent: sigil.accent.color),
            home: CallScreen(
              info: info,
              sigil: sigil,
              locked: locked,
              onAnswer: () async => answered++,
              onEnd: () async => ended++,
              onMinimize: () {},
            ),
          ),
        ),
      ));
      for (var i = 0; i < 6; i++) {
        await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 30)));
        await tester.pump(const Duration(milliseconds: 100));
      }
    }

    await show(_call(CallPhaseView.ringing, outgoing: false));
    expect(find.text('Asha'), findsOneWidget);
    expect(find.text('Incoming call'), findsOneWidget);
    await _shot(tester, 'call-1-incoming');
    await tester.tap(find.byIcon(Icons.call_rounded));
    await tester.tap(find.byIcon(Icons.call_end_rounded));
    expect((answered, ended), (1, 1));

    // Locked, it doesn't say who.
    await show(_call(CallPhaseView.ringing, outgoing: false), locked: true);
    expect(find.text('Asha'), findsNothing);

    await show(_call(CallPhaseView.connecting));
    expect(find.text('Connecting privately…'), findsOneWidget);

    await show(_call(CallPhaseView.active));
    expect(find.text('4:12'), findsOneWidget);
    expect(find.text('Mute'), findsOneWidget);
    expect(find.text('Earpiece'), findsOneWidget);
    await _shot(tester, 'call-2-talking');
    await tester.tap(find.byIcon(Icons.call_end_rounded));
    expect(ended, 2);

    await show(_call(CallPhaseView.ended, outcome: CallOutcomeView.unreachable));
    expect(find.text('Asha isn\'t reachable right now'), findsOneWidget);

    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.light));
    await show(_call(CallPhaseView.active));
    expect(Palette.light, isTrue);
    await _shot(tester, 'call-3-talking-light');
    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.dark));

    await tester.pumpWidget(const SizedBox());
  });
}
