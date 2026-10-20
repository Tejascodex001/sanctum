package io.sanctum.sanctum

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.content.pm.PackageInfoCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.FlutterEngineCache
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel

/**
 * Owns the one Flutter engine for the life of the process, so Dart (and the
 * Rust core with its Tor links) keeps running after the screen closes; the
 * activity only attaches to it. Also posts notifications: natively, on the
 * phone, nothing goes through Google.
 */
class SanctumApplication : Application() {
    private lateinit var channel: MethodChannel
    private var bluetooth: BluetoothNearby? = null

    private val multicast: WifiManager.MulticastLock by lazy {
        (applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager)
            .createMulticastLock("sanctum-nearby")
            .apply { setReferenceCounted(false) }
    }

    override fun onCreate() {
        super.onCreate()
        createChannels()
        val engine = FlutterEngine(this)
        engine.dartExecutor.executeDartEntrypoint(DartExecutor.DartEntrypoint.createDefault())
        FlutterEngineCache.getInstance().put(ENGINE, engine)
        channel = MethodChannel(engine.dartExecutor.binaryMessenger, "sanctum/background")
        channel.setMethodCallHandler { call, result ->
                when (call.method) {
                    "btStart" -> result.success(startBluetooth(call.argument<Int>("rustPort")!!))
                    "btAdvertise" -> {
                        bluetooth?.advertise(call.argument<ByteArray>("id")!!)
                        result.success(null)
                    }
                    "btStop" -> {
                        bluetooth?.stop()
                        bluetooth = null
                        result.success(null)
                    }
                    "start" -> {
                        // Refused while in the background on Android 12+;
                        // Dart asks again whenever the app is on screen.
                        runCatching {
                            ContextCompat.startForegroundService(this, Intent(this, ConnectionService::class.java))
                        }
                        result.success(null)
                    }
                    "stop" -> {
                        stopService(Intent(this, ConnectionService::class.java))
                        result.success(null)
                    }
                    "notify" -> {
                        notify(
                            id = call.argument<Int>("id")!!,
                            channel = call.argument<String>("channel")!!,
                            title = call.argument<String>("title")!!,
                            body = call.argument<String>("body")!!,
                            open = call.argument<String>("open"),
                            count = call.argument<Int>("count") ?: 0,
                        )
                        result.success(null)
                    }
                    "buildNumber" -> result.success(
                        PackageInfoCompat.getLongVersionCode(packageManager.getPackageInfo(packageName, 0)),
                    )
                    "nearbyLock" -> {
                        // Some phones drop local broadcasts (the nearby
                        // beacons) unless an app asks to hear them.
                        if (call.argument<Boolean>("on") == true) multicast.acquire() else if (multicast.isHeld) multicast.release()
                        result.success(null)
                    }
                    "cancelAll" -> {
                        NotificationManagerCompat.from(this).cancelAll()
                        result.success(null)
                    }
                    "cancel" -> {
                        NotificationManagerCompat.from(this).cancel(call.argument<Int>("id")!!)
                        result.success(null)
                    }
                    "canNotify" -> result.success(NotificationManagerCompat.from(this).areNotificationsEnabled())
                    "batteryUnrestricted" -> {
                        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
                        result.success(power.isIgnoringBatteryOptimizations(packageName))
                    }
                    else -> result.notImplemented()
                }
            }
    }

    /** Bluetooth nearby (Android 10+); returns the relay port, or -1. */
    private fun startBluetooth(rustPort: Int): Int {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return -1
        bluetooth?.stop()
        val main = Handler(Looper.getMainLooper())
        val nearby = BluetoothNearby(this) { id, handle ->
            main.post { channel.invokeMethod("btSeen", mapOf("id" to id, "handle" to handle)) }
        }
        bluetooth = nearby
        return runCatching { nearby.start(rustPort) }.getOrDefault(-1)
    }

    private fun createChannels() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(CHANNEL_MESSAGES, "Messages", NotificationManager.IMPORTANCE_HIGH),
                NotificationChannel(CHANNEL_REQUESTS, "Friend requests", NotificationManager.IMPORTANCE_DEFAULT),
                NotificationChannel(CHANNEL_CONNECTION, "Background connection", NotificationManager.IMPORTANCE_MIN)
                    .apply { description = "Keeps Sanctum reachable by your friends over Tor" },
            ),
        )
    }

    @android.annotation.SuppressLint("MissingPermission") // checked by areNotificationsEnabled
    private fun notify(id: Int, channel: String, title: String, body: String, open: String?, count: Int) {
        val manager = NotificationManagerCompat.from(this)
        if (!manager.areNotificationsEnabled()) return
        // On the lock screen: only that something arrived.
        val redacted = NotificationCompat.Builder(this, channel)
            .setSmallIcon(R.drawable.ic_stat_sanctum)
            .setContentTitle("Sanctum")
            .setContentText(if (channel == CHANNEL_REQUESTS) "Friend request" else "New message")
            .build()
        val notification = NotificationCompat.Builder(this, channel)
            .setSmallIcon(R.drawable.ic_stat_sanctum)
            .setContentTitle(title)
            .setContentText(body)
            .setNumber(count)
            .setCategory(if (channel == CHANNEL_REQUESTS) NotificationCompat.CATEGORY_SOCIAL else NotificationCompat.CATEGORY_MESSAGE)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(redacted)
            .setAutoCancel(true)
            .setContentIntent(openApp(this, id, open))
            .build()
        manager.notify(id, notification)
    }

    companion object {
        const val ENGINE = "main"
        const val CHANNEL_MESSAGES = "messages"
        const val CHANNEL_REQUESTS = "requests"
        const val CHANNEL_CONNECTION = "connection"
        const val EXTRA_OPEN = "open"

        /** Opens the app; [open] names a chat to show ("c:12", "t:3"). */
        fun openApp(context: Context, requestCode: Int, open: String?): PendingIntent {
            val intent = Intent(context, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            if (open != null) intent.putExtra(EXTRA_OPEN, open)
            return PendingIntent.getActivity(
                context,
                requestCode,
                intent,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        }
    }
}
