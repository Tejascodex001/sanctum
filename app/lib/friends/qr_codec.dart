import 'dart:convert';
import 'dart:typed_data';

/// Pairing codes travel as raw bytes in QR byte mode rather than base64
/// text, which makes each QR about a quarter less dense and much easier to
/// scan off another phone's screen.
///
/// Bytes on the wire: "SNC1" + the code's binary envelope. The Rust side
/// keeps using the text form `sanctum1:<base64url>`.
const _prefix = 'sanctum1:';
final _magic = ascii.encode('SNC1');

Uint8List codeToQrBytes(String code) {
  final body = base64Url.decode(base64Url.normalize(code.substring(_prefix.length)));
  return Uint8List.fromList([..._magic, ...body]);
}

/// The code inside a scanned QR or pasted text, or null if there is none.
String? codeFromScan({Uint8List? rawBytes, String? text}) {
  if (rawBytes != null && rawBytes.length > _magic.length) {
    final head = rawBytes.sublist(0, _magic.length);
    if (_listEquals(head, _magic)) {
      return _prefix + base64Url.encode(rawBytes.sublist(_magic.length)).replaceAll('=', '');
    }
  }
  // Shared messages carry an intro line, so find the code anywhere in the
  // text: it runs from the prefix to the next whitespace.
  final match = text == null ? null : RegExp(r'sanctum1:[A-Za-z0-9_-]+').firstMatch(text);
  return match?.group(0);
}

bool _listEquals(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}
