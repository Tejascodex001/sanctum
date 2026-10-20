package io.sanctum.sanctum

import android.os.Bundle
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity

class MainActivity : FlutterActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Block screenshots and the recents preview by default. Per-chat
        // opt-out (after biometric unlock) will toggle this from Dart.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }
}
