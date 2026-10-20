import 'package:flutter/services.dart';

import '../security/app_lock.dart';

const _channel = MethodChannel('sanctum/scanner');

/// Opens ZXing's full-screen scanner (native, on-device). Returns the
/// scanned text, or null if the user backed out.
Future<String?> scanQr({required String prompt}) =>
    AppLock.whileAway(() => _channel.invokeMethod<String>('scan', {'prompt': prompt}));
