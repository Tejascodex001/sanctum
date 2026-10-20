import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../src/rust/api/identity.dart';

/// Loads this device's identity secret from Keystore-backed storage, creating
/// one on first launch. Returns the public key.
Future<Uint8List> loadOrCreateIdentity() async {
  const storage = FlutterSecureStorage();
  const key = 'identity_secret_v1';
  var stored = await storage.read(key: key);
  if (stored == null) {
    stored = base64Encode(await identityNew());
    await storage.write(key: key, value: stored);
  }
  return identityPublicKey(secret: base64Decode(stored));
}
