/// The version people see. Sanctum is in beta until the owner says
/// otherwise; until then it stays "beta-1.0.0", shown with the build number
/// after it ("beta-1.0.0.66"). Releases are still tagged `build-N`, which
/// installed apps look for to update. Keep it matching `pubspec.yaml`'s
/// version and the "beta-" prefix in `android/app/build.gradle.kts`.
const appVersion = 'beta-1.0.0';

/// The full version people see, with the build number as its last part:
/// "beta-1.0.0.66" (Android shows the same).
String fullVersion(int build) => build > 0 ? '$appVersion.$build' : appVersion;
