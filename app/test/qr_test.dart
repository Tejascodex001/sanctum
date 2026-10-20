import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:sanctum/friends/qr_codec.dart';

String _fakeCode(int bytes) {
  final r = Random(bytes);
  final body = Uint8List.fromList(List.generate(bytes, (_) => r.nextInt(256)));
  return 'sanctum1:${base64Url.encode(body).replaceAll('=', '')}';
}

void main() {
  test('codec round-trips through QR bytes and text', () {
    for (final n in [423, 990, 1200]) {
      final code = _fakeCode(n);
      final bytes = codeToQrBytes(code);
      expect(bytes.length, n + 4);
      expect(codeFromScan(rawBytes: bytes), code);
      expect(codeFromScan(text: '  $code\n'), code);
      expect(codeFromScan(text: 'Add me on Sanctum. Copy this whole message:\n\n$code'), code,
          reason: 'shared messages carry an intro line; paste must find the code inside');
    }
    expect(codeFromScan(text: 'hello'), isNull);
    expect(codeFromScan(rawBytes: Uint8List.fromList([1, 2, 3, 4, 5])), isNull);
  });

  /// Writes QR PNGs (offer-sized and reply-sized) when QR_DIR is set, for
  /// decoding with the same ZXing engine the app uses.
  test('render QR images', () async {
    final dir = Platform.environment['QR_DIR'];
    for (final (name, n) in [('offer', 423), ('reply', 990)]) {
      final code = _fakeCode(n);
      final qr = QrCode.fromUint8List(data: codeToQrBytes(code), errorCorrectLevel: QrErrorCorrectLevel.L);
      final painter = QrPainter.withQr(qr: qr, gapless: true, eyeStyle: const QrEyeStyle(color: Colors.black),
          dataModuleStyle: const QrDataModuleStyle(color: Colors.black));
      // About what a phone renders: ~330 logical px at 3x.
      const size = 990.0, quiet = 60.0;
      final recorder = ui.PictureRecorder();
      final canvas = Canvas(recorder)..drawRect(const Rect.fromLTWH(0, 0, size + 2 * quiet, size + 2 * quiet), Paint()..color = Colors.white);
      canvas.translate(quiet, quiet);
      painter.paint(canvas, const Size.square(size));
      final image = await recorder.endRecording().toImage((size + 2 * quiet).toInt(), (size + 2 * quiet).toInt());
      if (dir != null) {
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        File('$dir/$name.png').writeAsBytesSync(png!.buffer.asUint8List());
        File('$dir/$name.txt').writeAsStringSync(code);
      }
      // ignore: avoid_print
      print('$name: QR version ${qr.typeNumber}, ${qr.moduleCount} modules');
    }
  });
}
