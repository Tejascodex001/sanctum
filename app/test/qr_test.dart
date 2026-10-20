import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:sanctum/friends/qr_codec.dart';
import 'package:sanctum/src/rust/api/identity.dart';
import 'package:sanctum/src/rust/api/session.dart';
import 'package:sanctum/src/rust/api/store.dart';
import 'package:sanctum/src/rust/frb_generated.dart';

void main() {
  late String code;

  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
    final dir = Directory.systemTemp.createTempSync('sanctum_qr');
    await sessionOpen(identitySecret: await identityNew(), dir: dir.path, storeKey: await storeNewKey());
    code = await myCode(myName: 'Tejas Gowda');
  });

  test('friend code is short and alphanumeric', () {
    expect(code, startsWith(codePrefix));
    expect(code.length, lessThan(90));
    expect(RegExp(r'^[0-9A-Z:]+$').hasMatch(code), isTrue, reason: code);
    // ignore: avoid_print
    print('code: ${code.length} chars, QR version ${qrForCode(code).typeNumber}; groups: ${codeGroups(code)}');
  });

  test('typed groups, lowercase, without prefix, reach the Rust core', () async {
    // It is our own code, so a correct parse is reported as "own code".
    for (final typed in [code, codeGroups(code), codeGroups(code).toLowerCase(), 'Add me on Sanctum:\n\n$code']) {
      await expectLater(
        friendAdd(code: typed),
        throwsA(predicate((e) => e.toString().contains('own code'))),
        reason: typed,
      );
    }
  });

  /// Writes the QR as a PNG when QR_DIR is set, for decoding with the ZXing
  /// library the app's scanner uses.
  test('render QR image', () async {
    final dir = Platform.environment['QR_DIR'];
    final painter = QrPainter.withQr(
      qr: qrForCode(code),
      gapless: true,
      eyeStyle: const QrEyeStyle(color: Colors.black),
      dataModuleStyle: const QrDataModuleStyle(color: Colors.black),
    );
    const size = 624.0, quiet = 48.0; // 208 logical px at 3x, as on screen.
    final recorder = ui.PictureRecorder();
    final canvas = Canvas(recorder)
      ..drawRect(const Rect.fromLTWH(0, 0, size + 2 * quiet, size + 2 * quiet), Paint()..color = Colors.white);
    canvas.translate(quiet, quiet);
    painter.paint(canvas, const Size.square(size));
    final image = await recorder.endRecording().toImage((size + 2 * quiet).toInt(), (size + 2 * quiet).toInt());
    if (dir != null) {
      final png = await image.toByteData(format: ui.ImageByteFormat.png);
      File('$dir/friend.png').writeAsBytesSync(png!.buffer.asUint8List());
      File('$dir/friend.txt').writeAsStringSync(code);
    }
  });
}
