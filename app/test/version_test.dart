import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/version.dart';

void main() {
  test('the version shown matches the one Android shows', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final version = RegExp(r'^version: ([^+\s]+)', multiLine: true).firstMatch(pubspec)!.group(1);
    final gradle = File('android/app/build.gradle.kts').readAsStringSync();
    final name = RegExp(r'versionName = "(.*)\$\{flutter.versionName\}\.\$\{flutter.versionCode\}"').firstMatch(gradle);
    expect(appVersion, '${name!.group(1)}$version');
    expect(fullVersion(66), '$appVersion.66');
  });
}
