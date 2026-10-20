import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../src/rust/api/identity.dart';
import '../src/rust/api/store.dart';

/// Device secrets and profile, kept in Keystore-backed storage. They never
/// leave the device and are lost if the app is uninstalled.
const _storage = FlutterSecureStorage();

Future<Uint8List> _loadOrCreate(String key, Future<Uint8List> Function() create) async {
  final stored = await _storage.read(key: key);
  if (stored != null) return base64Decode(stored);
  final fresh = await create();
  await _storage.write(key: key, value: base64Encode(fresh));
  return fresh;
}

/// This device's identity secret.
Future<Uint8List> loadOrCreateIdentitySecret() => _loadOrCreate('identity_secret_v1', identityNew);

/// The key that encrypts the on-device database.
Future<Uint8List> loadOrCreateStoreKey() => _loadOrCreate('store_key_v1', storeNewKey);

/// The name friends see when you pair. Empty until set.
Future<String> loadDisplayName() async => await _storage.read(key: 'display_name') ?? '';

Future<void> saveDisplayName(String name) => _storage.write(key: 'display_name', value: name.trim());

/// On/off settings, kept with the rest of the profile.
Future<bool> loadFlag(String key, {required bool fallback}) async {
  final v = await _storage.read(key: 'flag_$key');
  return v == null ? fallback : v == '1';
}

Future<void> saveFlag(String key, bool on) => _storage.write(key: 'flag_$key', value: on ? '1' : '0');

/// Panic wipe: destroys every key and setting. Without the store key the
/// database can never be decrypted, even if flash storage keeps old blocks.
Future<void> wipeSecrets() => _storage.deleteAll();
