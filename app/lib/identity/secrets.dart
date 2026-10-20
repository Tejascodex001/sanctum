import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../src/rust/api/identity.dart';
import '../src/rust/api/store.dart';

/// Device secrets, kept in Keystore-backed storage. They never leave the
/// device and are lost if the app is uninstalled.
const _storage = FlutterSecureStorage();

Future<Uint8List> _loadOrCreate(String key, Future<Uint8List> Function() create) async {
  final stored = await _storage.read(key: key);
  if (stored != null) return base64Decode(stored);
  final fresh = await create();
  await _storage.write(key: key, value: base64Encode(fresh));
  return fresh;
}

/// This device's identity; returns the public key.
Future<Uint8List> loadOrCreateIdentity() async =>
    identityPublicKey(secret: await _loadOrCreate('identity_secret_v1', identityNew));

/// The key that encrypts the on-device database.
Future<Uint8List> loadOrCreateStoreKey() => _loadOrCreate('store_key_v1', storeNewKey);
