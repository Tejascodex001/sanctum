/// The version people see. Sanctum is in beta until the owner says
/// otherwise; until then it stays "beta-1.0.0" and every release is told
/// apart by its build number (releases are still tagged `build-N`, which
/// installed apps look for to update). Keep it matching `pubspec.yaml`'s
/// version and the "beta-" prefix in `android/app/build.gradle.kts`.
const appVersion = 'beta-1.0.0';
