import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:sanctum/version.dart';

void main() {
  test('the version shown matches the one Android shows', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final version = RegExp(r'^version: ([^+\s]+)', multiLine: true).firstMatch(pubspec)!.group(1);
    final gradle = File('android/app/build.gradle.kts').readAsStringSync();
    final prefix = RegExp(r'versionName = "(.*)\$\{flutter.versionName\}"').firstMatch(gradle)!.group(1);
    expect(appVersion, '$prefix$version');
  });
}
