import 'dart:convert';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/identity/secrets.dart';
import 'package:sanctum/src/rust/frb_generated.dart';

/// A storage in memory whose reads can be made to fail, like a phone whose
/// key store can't decrypt.
class _FakeStorage extends FlutterSecureStorage {
  final values = <String, String>{};
  bool failReads = false;
  int writes = 0;

  @override
  Future<String?> read({
    required String key,
    Object? iOptions,
    Object? aOptions,
    Object? lOptions,
    Object? webOptions,
    Object? mOptions,
    Object? wOptions,
  }) async {
    if (failReads) throw Exception('key store could not decrypt');
    return values[key];
  }

  @override
  Future<void> write({
    required String key,
    required String? value,
    Object? iOptions,
    Object? aOptions,
    Object? lOptions,
    Object? webOptions,
    Object? mOptions,
    Object? wOptions,
  }) async {
    writes++;
    if (value == null) {
      values.remove(key);
    } else {
      values[key] = value;
    }
  }
}

void main() {
  setUpAll(() async {
    await RustLib.init(externalLibrary: ExternalLibrary.open('../target/debug/librust_lib_sanctum.so'));
  });

  late _FakeStorage storage;
  setUp(() {
    storage = _FakeStorage();
    secretsStorage = storage;
  });

  test('first run makes the keys once, then keeps them', () async {
    final secret = await loadOrCreateIdentitySecret();
    final key = await loadOrCreateStoreKey();
    expect(secret.length, 32);
    expect(key.length, 32);
    expect(storage.writes, 2);
    expect(await loadOrCreateIdentitySecret(), secret);
    expect(await loadOrCreateStoreKey(), key);
    expect(storage.writes, 2, reason: 'no new keys on later runs');
  });

  test('a key that exists is used as it is, nothing is written', () async {
    final stored = List<int>.generate(32, (i) => i);
    storage.values['identity_secret_v1'] = base64Encode(stored);
    expect(await loadOrCreateIdentitySecret(), stored);
    expect(storage.writes, 0);
  });

  test('a failing read is an error, never a new key', () async {
    final stored = List<int>.generate(32, (i) => 255 - i);
    storage.values['identity_secret_v1'] = base64Encode(stored);
    storage.values['store_key_v1'] = base64Encode(stored);
    storage.failReads = true;
    await expectLater(loadOrCreateIdentitySecret(), throwsException);
    await expectLater(loadOrCreateStoreKey(), throwsException);
    await expectLater(checkPin('1234'), throwsException);
    expect(storage.writes, 0, reason: 'nothing may be remade over keys that exist');
    // Once the phone can read again, the very same keys come back.
    storage.failReads = false;
    expect(await loadOrCreateIdentitySecret(), stored);
    expect(await loadOrCreateStoreKey(), stored);
    expect(storage.writes, 0);
  });

  test('settings that fail to read fall back instead of failing', () async {
    storage.failReads = true;
    expect(await loadFlag('x', fallback: true), isTrue);
    expect(await loadSetting('x'), isNull);
  });
}
