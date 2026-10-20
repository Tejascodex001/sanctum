package io.sanctum.sanctum

import android.os.Bundle
import android.view.WindowManager
import com.google.zxing.client.android.Intents
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterFragmentActivity() {
    private var pendingScan: MethodChannel.Result? = null

    // ZXing's own scanner: pure Java, on-device, no Google services.
    private val scanLauncher = registerForActivityResult(ScanContract()) { result ->
        pendingScan?.success(result.contents)
        pendingScan = null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Block screenshots and the recents preview by default. Per-chat
        // opt-out (after biometric unlock) will toggle this from Dart.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "sanctum/scanner")
            .setMethodCallHandler { call, result ->
                if (call.method != "scan") return@setMethodCallHandler result.notImplemented()
                // Only one scan at a time; a second request cancels the first.
                pendingScan?.success(null)
                pendingScan = result
                val options = ScanOptions()
                    .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                    .setPrompt(call.argument<String>("prompt") ?: "")
                    .setBeepEnabled(false)
                    .setOrientationLocked(true)
                    .setCaptureActivity(PortraitCaptureActivity::class.java)
                    .addExtra(Intents.Scan.SCAN_TYPE, Intents.Scan.MIXED_SCAN)
                scanLauncher.launch(options)
            }
    }
}
