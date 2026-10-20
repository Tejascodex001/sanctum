import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/sigil/sigil_painter.dart';
import 'package:sanctum/src/rust/api/sigil.dart';
import 'package:sanctum/src/rust/frb_generated.dart';

/// Writes a contact sheet of random sigils when SIGIL_PREVIEW is set to an
/// output path. Otherwise just checks every form renders.
void main() {
  setUpAll(() async {
    await RustLib.init(
      externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'),
    );
  });

  test('sigils render', () async {
    const cols = 6, rows = 4, cell = 240.0;
    // Four of each form, one form per column, so every family is checked.
    final byForm = {for (final f in SigilForm.values) f: <SigilSpec>[]};
    while (byForm.values.any((l) => l.length < rows)) {
      final s = await sigilRandom();
      if (byForm[s.form]!.length < rows) byForm[s.form]!.add(s);
    }
    final specs = [
      for (var r = 0; r < rows; r++)
        for (final f in SigilForm.values) byForm[f]![r],
    ];
    final recorder = ui.PictureRecorder();
    final canvas = Canvas(recorder);
    for (var i = 0; i < specs.length; i++) {
      canvas.save();
      canvas.translate((i % cols) * cell, (i ~/ cols) * cell);
      canvas.clipRRect(RRect.fromRectAndRadius(
          const Rect.fromLTWH(6, 6, cell - 12, cell - 12), const Radius.circular(28)));
      canvas.translate(6, 6);
      SigilPainter(specs[i]).paint(canvas, const Size.square(cell - 12));
      canvas.restore();
    }
    final image = await recorder.endRecording().toImage((cols * cell).toInt(), (rows * cell).toInt());
    final out = Platform.environment['SIGIL_PREVIEW'];
    if (out != null) {
      final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
      File(out).writeAsBytesSync(bytes!.buffer.asUint8List());
    }
  });
}
