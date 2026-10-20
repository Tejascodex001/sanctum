import 'package:qr/qr.dart';

/// Friend codes are `SANCTUM1:` plus Crockford Base32 (digits and letters
/// only), so QR encodes them in its compact alphanumeric mode and every
/// scanner returns them exactly. Parsing and validation happen in Rust.
const codePrefix = 'SANCTUM1:';

/// The smallest QR (medium error correction, as codes are short) for [code].
QrCode qrForCode(String code) {
  for (var type = 1; type <= 40; type++) {
    final qr = QrCode(type, QrErrorCorrectLevel.M)..addAlphaNumeric(code);
    try {
      QrImage(qr);
      return qr;
    } on InputTooLongException {
      continue;
    }
  }
  throw ArgumentError('code too long for a QR');
}

/// The code without its prefix, in groups of four, for reading or typing.
String codeGroups(String code) {
  final body = code.startsWith(codePrefix) ? code.substring(codePrefix.length) : code;
  return [
    for (var i = 0; i < body.length; i += 4) body.substring(i, i + 4 > body.length ? body.length : i + 4),
  ].join(' ');
}
