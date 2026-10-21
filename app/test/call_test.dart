import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/call/call_controller.dart';
import 'package:sanctum/call/call_screen.dart';
import 'package:sanctum/settings/settings_screen.dart';
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

CallInfo _call(
  CallPhaseView phase, {
  bool outgoing = true,
  CallOutcomeView? outcome,
  bool hidden = false,
  bool video = false,
}) =>
    CallInfo(
      id: '00',
      chatId: PlatformInt64Util.from(1),
      name: 'Asha',
      sigilKey: Uint8List(32),
      hidden: hidden,
      outgoing: outgoing,
      video: video,
      phase: phase,
      outcome: outcome,
      answeredAtMs:
          phase == CallPhaseView.active ? PlatformInt64Util.from(DateTime.now().millisecondsSinceEpoch - 252000) : null,
      sendCodec: 'opus',
      recvCodec: 'opus',
      sendVideoCodec: 'h264',
      recvVideoCodec: 'h264',
      nearby: false,
    );

CallMediaInfo _media({
  CallPathView path = CallPathView.private,
  CameraView camera = CameraView.off,
  CameraView peer = CameraView.off,
}) =>
    CallMediaInfo(
      path: path,
      camera: camera,
      peerCamera: peer,
      canSendVideo: true,
      canReceiveVideo: true,
      sharp: path != CallPathView.private,
    );

/// Stand-ins for the camera pictures (tests have no camera or decoder).
Widget _fakePicture(List<Color> colors, double icon) => DecoratedBox(
      decoration: BoxDecoration(
        gradient: LinearGradient(begin: Alignment.topLeft, end: Alignment.bottomRight, colors: colors),
      ),
      child: Align(
        alignment: const Alignment(0, 0.35),
        child: Icon(Icons.person_rounded, size: icon, color: const Color(0x55FFFFFF)),
      ),
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
    expect(callStatus(_call(CallPhaseView.ringing, outgoing: false, video: true), 'Asha'), 'Incoming video call');
    expect(
      callStatus(_call(CallPhaseView.ended, outcome: CallOutcomeView.missed, video: true), 'Asha'),
      'Missed video call',
    );
    expect(callPath(CallPathView.private).label, 'Private');
    expect(callPath(CallPathView.nearby).label, 'Nearby');
    expect(callPath(CallPathView.direct).label, 'Direct');
    expect(videoNote(_media(), 'Asha'), isNull);
    expect(videoNote(_media(camera: CameraView.live), 'Asha'), 'Video stays private, so it may be choppy');
    expect(videoNote(_media(path: CallPathView.direct, camera: CameraView.live), 'Asha'), isNull);
    expect(videoNote(_media(peer: CameraView.paused), 'Asha'), 'Asha\'s camera is paused');
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
              onAnswer: (_) async => answered++,
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

  testWidgets('video calls: the friend full screen, ours in a corner, dark and light', (tester) async {
    tester.view.physicalSize = const Size(1080, 2340);
    tester.view.devicePixelRatio = 2.75;
    addTearDown(tester.view.reset);
    late SigilSpec sigil;
    await tester.runAsync(() async {
      sigil = await sigilForKey(publicKey: await identityPublicKey(secret: await identityNew()));
    });
    final calls = CallController.instance;
    addTearDown(() {
      calls.media.value = null;
      calls.camera.value = false;
    });
    final answers = <bool>[];
    var shown = 0;

    Future<void> show(CallInfo info) async {
      shown++;
      await tester.pumpWidget(RepaintBoundary(
        key: _shotKey,
        child: Restyler(
          builder: (_) => MaterialApp(
            theme: sanctumTheme(accent: sigil.accent.color),
            home: CallScreen(
              // A fresh screen each time (our picture back in its corner).
              key: ValueKey(shown),
              info: info,
              sigil: sigil,
              locked: false,
              onAnswer: (video) async => answers.add(video),
              onEnd: () async {},
              onMinimize: () {},
              remotePicture: _fakePicture(const [Color(0xFF3A4A5E), Color(0xFF8C6A55)], 420),
              localPicture: _fakePicture(const [Color(0xFF55606E), Color(0xFF2E3640)], 110),
            ),
          ),
        ),
      ));
      for (var i = 0; i < 6; i++) {
        await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 30)));
        await tester.pump(const Duration(milliseconds: 100));
      }
    }

    // A video call ringing: answer with the camera, or without.
    await show(_call(CallPhaseView.ringing, outgoing: false, video: true));
    expect(find.text('Incoming video call'), findsOneWidget);
    expect(find.text('Private video call'), findsOneWidget);
    await _shot(tester, 'call-4-video-incoming');
    await tester.tap(find.byIcon(Icons.videocam_rounded));
    await tester.tap(find.text('Answer without video'));
    expect(answers, [true, false]);

    // Talking, both cameras on, straight between the phones.
    calls.camera.value = true;
    calls.media.value = _media(path: CallPathView.direct, camera: CameraView.live, peer: CameraView.live);
    await show(_call(CallPhaseView.active, video: true));
    expect(find.text('Direct video call'), findsOneWidget);
    expect(find.text('Asha'), findsOneWidget);
    expect(find.text('Camera on'), findsOneWidget);
    expect(find.byKey(const ValueKey('my-camera')), findsOneWidget);
    expect(find.byIcon(Icons.flip_camera_android_rounded), findsOneWidget);
    await _shot(tester, 'call-5-video');
    // Our picture can be dragged out of the way.
    final before = tester.getTopLeft(find.byKey(const ValueKey('my-camera')));
    await tester.drag(find.byKey(const ValueKey('my-camera')), const Offset(-200, 300));
    await tester.pump();
    final after = tester.getTopLeft(find.byKey(const ValueKey('my-camera')));
    expect(after.dx, lessThan(before.dx));
    expect(after.dy, greaterThan(before.dy));

    // Through Tor, it says plainly the picture may be choppy.
    calls.media.value = _media(camera: CameraView.live, peer: CameraView.live);
    await show(_call(CallPhaseView.active, video: true));
    expect(find.text('Private video call'), findsOneWidget);
    expect(find.text('Video stays private, so it may be choppy'), findsOneWidget);
    await _shot(tester, 'call-6-video-private');

    // Their camera paused: back to the sigil, and it says so.
    calls.camera.value = false;
    calls.media.value = _media(path: CallPathView.nearby, peer: CameraView.paused);
    await show(_call(CallPhaseView.active, video: true));
    expect(find.text('Asha\'s camera is paused'), findsOneWidget);
    expect(find.text('Camera off'), findsOneWidget);
    expect(find.byKey(const ValueKey('my-camera')), findsNothing);

    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.light));
    calls.camera.value = true;
    calls.media.value = _media(path: CallPathView.nearby, camera: CameraView.live, peer: CameraView.live);
    await show(_call(CallPhaseView.active, video: true));
    expect(find.text('Nearby video call'), findsOneWidget);
    await _shot(tester, 'call-7-video-light');
    // A voice call with only our camera on, in light mode.
    calls.media.value = _media(path: CallPathView.nearby, camera: CameraView.live);
    await show(_call(CallPhaseView.active));
    await _shot(tester, 'call-8-camera-only-light');
    await tester.runAsync(() => Appearance.setTheme(ThemeChoice.dark));

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('Faster calls is off until turned on, and says what it costs', (tester) async {
    await tester.pumpWidget(MaterialApp(
      theme: sanctumTheme(accent: Palette.accents.first),
      home: const Scaffold(body: CallsSettings()),
    ));
    // The setting comes from Rust (off here: there's no profile open).
    for (var i = 0; i < 100 && find.text('Faster calls').evaluate().isEmpty; i++) {
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
      await tester.pump(const Duration(milliseconds: 20));
    }
    expect(find.text('Faster calls'), findsOneWidget);
    final tile = tester.widget<SwitchListTile>(find.byType(SwitchListTile));
    expect(tile.value, isFalse);
    expect(find.textContaining('learns your internet address'), findsOneWidget);
  });
}
