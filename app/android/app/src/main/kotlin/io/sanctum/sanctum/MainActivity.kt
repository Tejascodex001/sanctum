package io.sanctum.sanctum

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.net.Uri
import android.os.CancellationSignal
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import android.provider.Settings
import android.view.WindowManager
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContract
import androidx.activity.result.contract.ActivityResultContracts
import android.os.Handler
import android.os.Looper
import kotlin.concurrent.thread
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

    private val voice by lazy { VoiceRecorder(applicationContext) }

    private var pendingPick: MethodChannel.Result? = null
    private val pickVisual = registerForActivityResult(ActivityResultContracts.PickVisualMedia()) { finishPick(it) }
    private val pickFile = registerForActivityResult(ActivityResultContracts.OpenDocument()) { finishPick(it) }

    /**
     * A photo taken now with the camera app (daily moments). It's written
     * to Sanctum's cache through a link granted just for this, prepared
     * like a picked photo, then deleted.
     */
    private var pendingCapture: MethodChannel.Result? = null
    private var captureFile: File? = null
    private val takePicture = registerForActivityResult(ActivityResultContracts.TakePicture()) { ok ->
        val result = pendingCapture ?: return@registerForActivityResult
        pendingCapture = null
        val file = captureFile
        captureFile = null
        if (!ok || file == null || file.length() == 0L) {
            file?.delete()
            return@registerForActivityResult result.success(null)
        }
        val main = Handler(Looper.getMainLooper())
        thread(isDaemon = true) {
            val uri = FileProvider.getUriForFile(this@MainActivity, "$packageName.updates", file)
            val prepared = runCatching { MediaPrep.prepare(applicationContext, uri) }.getOrNull()
            file.delete()
            main.post { result.success(prepared) }
        }
    }

    private fun capturePhoto(result: MethodChannel.Result) {
        pendingCapture?.success(null)
        pendingCapture = result
        val dir = File(cacheDir, "open").apply { mkdirs() }
        val file = File(dir, "moment-${System.nanoTime()}.jpg")
        captureFile = file
        val uri = FileProvider.getUriForFile(this, "$packageName.updates", file)
        if (runCatching { takePicture.launch(uri) }.isFailure) {
            captureFile = null
            pendingCapture = null
            result.success(null)
        }
    }

    /**
     * "Save to phone" on Android 9 and older: the system's own "save as"
     * screen (no storage permission needed). Newer phones save straight
     * into the Pictures, Movies, Music or Downloads folder instead.
     */
    private class CreateFile : ActivityResultContract<Pair<String, String>, Uri?>() {
        override fun createIntent(context: Context, input: Pair<String, String>): Intent =
            Intent(Intent.ACTION_CREATE_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .setType(input.first)
                .putExtra(Intent.EXTRA_TITLE, input.second)

        override fun parseResult(resultCode: Int, intent: Intent?): Uri? =
            if (resultCode == Activity.RESULT_OK) intent?.data else null
    }

    private var pendingSave: MethodChannel.Result? = null
    private var pendingSaveFile: File? = null
    private val createDocument = registerForActivityResult(CreateFile()) { finishSave(it) }

    private fun finishSave(uri: Uri?) {
        val result = pendingSave ?: return
        val file = pendingSaveFile
        pendingSave = null
        pendingSaveFile = null
        if (uri == null || file == null) return result.success("cancelled")
        val main = Handler(Looper.getMainLooper())
        thread(isDaemon = true) {
            val ok = runCatching {
                contentResolver.openOutputStream(uri)!!.use { out -> file.inputStream().use { it.copyTo(out) } }
            }.isSuccess
            main.post { result.success(if (ok) "saved" else "failed") }
        }
    }

    /**
     * Copies a decrypted file to where the user's other apps can see it, only
     * when they ask. Answers "saved", "cancelled", "missing" or "failed".
     */
    private fun saveToPhone(path: String, name: String, mime: String?, result: MethodChannel.Result) {
        val file = File(path)
        if (!file.isFile) return result.success("missing")
        val type = MediaPrep.guessMime(mime, name)
        val saveName = MediaPrep.saveName(name, type)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val main = Handler(Looper.getMainLooper())
            thread(isDaemon = true) {
                val ok = runCatching { saveToMediaStore(file, saveName, type) }.getOrDefault(false)
                main.post { result.success(if (ok) "saved" else "failed") }
            }
        } else {
            pendingSave?.success("cancelled")
            pendingSave = result
            pendingSaveFile = file
            if (runCatching { createDocument.launch(type to saveName) }.isFailure) {
                pendingSave = null
                pendingSaveFile = null
                result.success("failed")
            }
        }
    }

    /** Android 10+: a "Sanctum" folder inside Pictures, Movies, Music or Downloads. */
    private fun saveToMediaStore(file: File, name: String, mime: String): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return false
        val volume = MediaStore.VOLUME_EXTERNAL_PRIMARY
        val (collection, folder) = when {
            mime.startsWith("image/") -> MediaStore.Images.Media.getContentUri(volume) to Environment.DIRECTORY_PICTURES
            mime.startsWith("video/") -> MediaStore.Video.Media.getContentUri(volume) to Environment.DIRECTORY_MOVIES
            mime.startsWith("audio/") -> MediaStore.Audio.Media.getContentUri(volume) to Environment.DIRECTORY_MUSIC
            else -> MediaStore.Downloads.getContentUri(volume) to Environment.DIRECTORY_DOWNLOADS
        }
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, "$folder/Sanctum")
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val resolver = contentResolver
        val uri = resolver.insert(collection, values) ?: return false
        return try {
            resolver.openOutputStream(uri)!!.use { out -> file.inputStream().use { it.copyTo(out) } }
            resolver.update(uri, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
            true
        } catch (e: Exception) {
            runCatching { resolver.delete(uri, null, null) }
            false
        }
    }

    /**
     * Opens a decrypted copy in another app, through a read-only link
     * granted just for this. Answers "opened", "noapp" (nothing on the phone
     * can show this kind of file), "missing" or "failed".
     */
    private fun hasLocationPermission(): Boolean =
        checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
            checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

    /// One fix from the phone's own location service (never Google's): the
    /// first answer from GPS or the network, or after 20 seconds the freshest
    /// one the phone already knew. Null if location is off or denied.
    @android.annotation.SuppressLint("MissingPermission")
    private fun currentLocation(result: MethodChannel.Result) {
        val manager = getSystemService(Context.LOCATION_SERVICE) as LocationManager
        val providers = listOf(LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER)
            .filter { runCatching { manager.isProviderEnabled(it) }.getOrDefault(false) }
        if (!hasLocationPermission() || providers.isEmpty()) {
            result.success(null)
            return
        }
        val main = Handler(Looper.getMainLooper())
        val listeners = mutableListOf<LocationListener>()
        val cancel = CancellationSignal()
        var done = false
        fun finish(location: Location?) {
            if (done) return
            done = true
            cancel.cancel()
            listeners.forEach { runCatching { manager.removeUpdates(it) } }
            result.success(location?.let { mapOf("lat" to it.latitude, "lon" to it.longitude) })
        }
        for (provider in providers) {
            runCatching {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    manager.getCurrentLocation(provider, cancel, mainExecutor) { it?.let(::finish) }
                } else {
                    val listener = object : LocationListener {
                        override fun onLocationChanged(location: Location) = finish(location)
                        @Deprecated("Needed before Android 10")
                        override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {}
                        override fun onProviderEnabled(provider: String) {}
                        override fun onProviderDisabled(provider: String) {}
                    }
                    listeners.add(listener)
                    @Suppress("DEPRECATION")
                    manager.requestSingleUpdate(provider, listener, Looper.getMainLooper())
                }
            }
        }
        main.postDelayed({
            val known = providers
                .mapNotNull { runCatching { manager.getLastKnownLocation(it) }.getOrNull() }
                .maxByOrNull { it.time }
            finish(known)
        }, 20_000)
    }

    private fun openFile(path: String, name: String, mime: String?): String {
        val file = File(path)
        if (!file.isFile) return "missing"
        val uri = try {
            FileProvider.getUriForFile(this, "$packageName.updates", file)
        } catch (e: IllegalArgumentException) {
            return "failed"
        }
        val view = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, MediaPrep.guessMime(mime, name.ifEmpty { file.name }))
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        // Some phones only pass the permission on to the app that opens the
        // file when the link is also attached as clip data.
        view.clipData = ClipData.newRawUri("", uri)
        return try {
            // No chooser of our own: Android asks which app when there are
            // several, and says so when there is none.
            startActivity(view)
            "opened"
        } catch (e: ActivityNotFoundException) {
            "noapp"
        } catch (e: Exception) {
            "failed"
        }
    }

    /** Several photos and videos at once (for albums), prepared in order. */
    private var pendingPickMany: MethodChannel.Result? = null
    private val pickVisualMany =
        registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(30)) { uris ->
            val result = pendingPickMany ?: return@registerForActivityResult
            pendingPickMany = null
            val main = Handler(Looper.getMainLooper())
            thread(isDaemon = true) {
                val prepared = uris.mapNotNull { runCatching { MediaPrep.prepare(applicationContext, it) }.getOrNull() }
                main.post { result.success(prepared) }
            }
        }

    /** Copies and prepares the pick off the main thread (photos are shrunk). */
    private fun finishPick(uri: Uri?) {
        val result = pendingPick ?: return
        pendingPick = null
        if (uri == null) return result.success(null)
        val main = Handler(Looper.getMainLooper())
        thread(isDaemon = true) {
            val prepared = runCatching { MediaPrep.prepare(applicationContext, uri) }.getOrNull()
            main.post { result.success(prepared) }
        }
    }

    private var pendingPermissions: MethodChannel.Result? = null
    private val permissionsLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
            pendingPermissions?.success(granted.values.all { it })
            pendingPermissions = null
        }

    // Sharing a place: precise or approximate is fine.
    private var pendingLocationPermission: MethodChannel.Result? = null
    private val locationPermissionLauncher =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
            pendingLocationPermission?.success(granted.values.any { it })
            pendingLocationPermission = null
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
        // A call ringing: answerable from the lock screen.
        if (open.startsWith("call")) showOverLock(true)
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
                    "requestMicrophone" -> {
                        pendingPermission?.success(false)
                        pendingPermission = result
                        permissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                    }
                    "recordStart" -> result.success(voice.start())
                    "recordLevel" -> result.success(voice.level())
                    "recordStop" -> result.success(voice.stop())
                    "recordCancel" -> {
                        voice.cancel()
                        result.success(null)
                    }
                    "pickMedia" -> {
                        pendingPick?.success(null)
                        pendingPick = result
                        when (call.argument<String>("kind")) {
                            "file" -> pickFile.launch(arrayOf("*/*"))
                            "image" -> pickVisual.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                            else -> pickVisual.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo),
                            )
                        }
                    }
                    "capturePhoto" -> capturePhoto(result)
                    "requestCamera" -> {
                        pendingPermissions?.success(false)
                        pendingPermissions = result
                        permissionsLauncher.launch(arrayOf(Manifest.permission.CAMERA))
                    }
                    "pickMediaMany" -> {
                        pendingPickMany?.success(emptyList<Any>())
                        pendingPickMany = result
                        pickVisualMany.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
                    }
                    "requestLocation" -> {
                        if (hasLocationPermission()) {
                            result.success(true)
                        } else {
                            pendingLocationPermission?.success(false)
                            pendingLocationPermission = result
                            locationPermissionLauncher.launch(
                                arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION),
                            )
                        }
                    }
                    "currentLocation" -> currentLocation(result)
                    "openGeo" -> {
                        val lat = call.argument<Double>("lat") ?: 0.0
                        val lon = call.argument<Double>("lon") ?: 0.0
                        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("geo:$lat,$lon?q=$lat,$lon"))
                        result.success(runCatching { startActivity(intent) }.isSuccess)
                    }
                    "openFile" -> result.success(
                        openFile(
                            call.argument<String>("path")!!,
                            call.argument<String>("name") ?: "",
                            call.argument<String>("mime"),
                        ),
                    )
                    "saveToPhone" -> saveToPhone(
                        call.argument<String>("path")!!,
                        call.argument<String>("name") ?: "file",
                        call.argument<String>("mime"),
                        result,
                    )
                    "requestBluetooth" -> {
                        // Nearby devices (Android 12+); before that, BLE
                        // scanning needs location access.
                        val needed = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                            arrayOf(
                                Manifest.permission.BLUETOOTH_SCAN,
                                Manifest.permission.BLUETOOTH_ADVERTISE,
                                Manifest.permission.BLUETOOTH_CONNECT,
                            )
                        } else {
                            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)
                        }
                        pendingPermissions?.success(false)
                        pendingPermissions = result
                        permissionsLauncher.launch(needed)
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
                    "callShowOverLock" -> {
                        showOverLock(call.argument<Boolean>("on") == true)
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
                    "openNotificationSettings" -> {
                        val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                            Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                                .putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
                        } else {
                            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
                                .setData(Uri.parse("package:$packageName"))
                        }
                        runCatching { startActivity(intent) }
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        }
    }

    /** While a call rings or goes on: shown over the lock screen, waking it. */
    private fun showOverLock(on: Boolean) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(on)
            setTurnScreenOn(on)
        } else {
            @Suppress("DEPRECATION")
            val flags = WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
            if (on) window.addFlags(flags) else window.clearFlags(flags)
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
