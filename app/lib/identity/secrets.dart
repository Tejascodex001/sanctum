import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show visibleForTesting;
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../src/rust/api/identity.dart';
import '../src/rust/api/session.dart' show pinHash;
import '../src/rust/api/store.dart';

/// Device secrets and profile, kept in Keystore-backed storage. They never
/// leave the device and are lost if the app is uninstalled.
///
/// `resetOnError: false`: if the phone's key store can't decrypt something
/// (some phones' system software is flaky about it), the storage must
/// report an error and keep what it has. The default would silently erase
/// everything, and the keys below would then be made afresh, locking you
/// out of your own chats.
FlutterSecureStorage _storage = const FlutterSecureStorage(aOptions: AndroidOptions(resetOnError: false));

/// Tests swap in a storage that fails on demand.
@visibleForTesting
set secretsStorage(FlutterSecureStorage s) => _storage = s;

/// A key that isn't stored yet (`read` gives null: first run) is made and
/// saved. If the read itself fails, the error is passed on: a key is never
/// made afresh over one that exists but can't be read right now.
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
  try {
    final v = await _storage.read(key: 'flag_$key');
    return v == null ? fallback : v == '1';
  } catch (_) {
    return fallback;
  }
}

Future<void> saveFlag(String key, bool on) => _storage.write(key: 'flag_$key', value: on ? '1' : '0');

/// Other settings, as text (null: not set).
Future<String?> loadSetting(String key) async {
  try {
    return await _storage.read(key: 'setting_$key');
  } catch (_) {
    return null;
  }
}

Future<void> saveSetting(String key, String? value) =>
    value == null ? _storage.delete(key: 'setting_$key') : _storage.write(key: 'setting_$key', value: value);

/// Panic wipe: destroys every key and setting. Without the store key the
/// database can never be decrypted, even if flash storage keeps old blocks.
Future<void> wipeSecrets() => _storage.deleteAll();

// ---- App PIN and decoy PIN --------------------------------------------

/// What an entered PIN unlocks.
enum PinMatch { real, decoy, wrong }

Future<bool> hasPin() async => await _storage.read(key: 'pin_real') != null;
Future<bool> hasDecoyPin() async => await _storage.read(key: 'pin_decoy') != null;

Future<Uint8List> _pinSalt() => _loadOrCreate(
    'pin_salt_v1',
    () async => Uint8List.fromList(
          List<int>.generate(16, (_) => _random.nextInt(256)),
        ));

final _random = Random.secure();

Future<String> _hashPin(String pin) async => base64Encode(await pinHash(pin: pin, salt: await _pinSalt()));

Future<void> setPin(String pin) => _hashPin(pin).then((h) => _storage.write(key: 'pin_real', value: h));

/// Removing the PIN removes the decoy too (it needs a PIN screen).
Future<void> removePin() async {
  await _storage.delete(key: 'pin_real');
  await removeDecoyPin();
}

Future<void> setDecoyPin(String pin) => _hashPin(pin).then((h) => _storage.write(key: 'pin_decoy', value: h));

Future<void> removeDecoyPin() => _storage.delete(key: 'pin_decoy');

Future<PinMatch> checkPin(String pin) async {
  final hash = await _hashPin(pin);
  if (hash == await _storage.read(key: 'pin_real')) return PinMatch.real;
  if (hash == await _storage.read(key: 'pin_decoy')) return PinMatch.decoy;
  return PinMatch.wrong;
}

/// The decoy profile's own identity and database key.
Future<Uint8List> loadOrCreateDecoyIdentitySecret() => _loadOrCreate('decoy_identity_secret_v1', identityNew);
Future<Uint8List> loadOrCreateDecoyStoreKey() => _loadOrCreate('decoy_store_key_v1', storeNewKey);
