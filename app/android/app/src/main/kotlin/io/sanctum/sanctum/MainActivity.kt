package io.sanctum.sanctum

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.view.WindowManager
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.FileProvider
import java.io.File
import com.google.zxing.client.android.Intents
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import io.flutter.embedding.android.FlutterFragmentActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterFragmentActivity() {
    private var pendingScan: MethodChannel.Result? = null
    private var pendingPermission: MethodChannel.Result? = null
    private var appChannel: MethodChannel? = null

    /** A chat to open, from a tapped notification, until Dart takes it. */
    private var pendingOpen: String? = null

    // ZXing's own scanner: pure Java, on-device, no Google services.
    private val scanLauncher = registerForActivityResult(ScanContract()) { result ->
        pendingScan?.success(result.contents)
        pendingScan = null
    }

    private val permissionLauncher = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        pendingPermission?.success(granted)
        pendingPermission = null
    }

    // The engine lives in SanctumApplication and outlives this activity.
    override fun getCachedEngineId(): String = SanctumApplication.ENGINE

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Block screenshots and the recents preview by default. Per-chat
        // opt-out (after biometric unlock) will toggle this from Dart.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        takeOpen(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        takeOpen(intent)
    }

    private fun takeOpen(intent: Intent?) {
        if (intent == null) return
        val open = intent.getStringExtra(SanctumApplication.EXTRA_OPEN) ?: return
        intent.removeExtra(SanctumApplication.EXTRA_OPEN)
        pendingOpen = open
        // Dart may already be listening; if not, it asks on resume.
        appChannel?.invokeMethod("open", open, object : MethodChannel.Result {
            override fun success(result: Any?) {
                if (pendingOpen == open) pendingOpen = null
            }

            override fun error(code: String, message: String?, details: Any?) {}
            override fun notImplemented() {}
        })
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val messenger = flutterEngine.dartExecutor.binaryMessenger
        MethodChannel(messenger, "sanctum/scanner")
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
        appChannel = MethodChannel(messenger, "sanctum/app").apply {
            setMethodCallHandler { call, result ->
                when (call.method) {
                    "takeOpen" -> {
                        result.success(pendingOpen)
                        pendingOpen = null
                    }
                    "requestNotifications" -> {
                        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                            result.success(true)
                        } else {
                            pendingPermission?.success(false)
                            pendingPermission = result
                            permissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                        }
                    }
                    "setSecure" -> {
                        // Screenshots and the recents preview: blocked unless
                        // the open chat allows them.
                        if (call.argument<Boolean>("secure") != false) {
                            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        } else {
                            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
                        }
                        result.success(null)
                    }
                    "installApk" -> result.success(installApk(call.argument<String>("path")!!))
                    "backgroundBrand" -> result.success(
                        mapOf("brand" to BackgroundSettings.brand(), "aggressive" to BackgroundSettings.aggressive()),
                    )
                    "openBackgroundSettings" -> result.success(BackgroundSettings.open(this@MainActivity))
                    "requestUnrestricted" -> {
                        // Lets the connection survive Doze and aggressive
                        // battery savers. The user confirms in a system dialog.
                        @android.annotation.SuppressLint("BatteryLife")
                        val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
                            .setData(Uri.parse("package:$packageName"))
                        runCatching { startActivity(intent) }
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        }
    }

    /**
     * Hands a downloaded update to Android's installer, which asks the user
     * and refuses it unless it's signed with this app's key. The first time,
     * the user must allow Sanctum to install apps; returns "permission" then.
     */
    private fun installApk(path: String): String {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !packageManager.canRequestPackageInstalls()) {
            val settings = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName"))
            runCatching { startActivity(settings) }
            return "permission"
        }
        val uri = FileProvider.getUriForFile(this, "$packageName.updates", File(path))
        val install = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        return if (runCatching { startActivity(install) }.isSuccess) "started" else "failed"
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        // The engine stays; stop routing its calls to this (dying) activity.
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "sanctum/scanner").setMethodCallHandler(null)
        appChannel?.setMethodCallHandler(null)
        appChannel = null
        super.cleanUpFlutterEngine(flutterEngine)
    }
}
