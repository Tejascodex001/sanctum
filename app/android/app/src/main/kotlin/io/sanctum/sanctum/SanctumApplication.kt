package io.sanctum.sanctum

import android.app.Application
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
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
    private val callAudio by lazy { CallAudio(this) }

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
                            publicText = call.argument<String>("redacted"),
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
                    // Calls (see CallAudio.kt; the sound goes to Rust directly).
                    "callCodecs" -> result.success(CallAudio.codecs())
                    "callAudioStart" -> result.success(
                        callAudio.start(
                            call.argument<String>("send")!!,
                            call.argument<String>("recv")!!,
                            call.argument<Boolean>("speaker") == true,
                        ),
                    )
                    "callAudioStop" -> {
                        callAudio.stop()
                        result.success(null)
                    }
                    "callMute" -> {
                        callAudio.muted = call.argument<Boolean>("on") == true
                        result.success(null)
                    }
                    "callSpeaker" -> {
                        callAudio.setSpeaker(call.argument<Boolean>("on") == true)
                        result.success(null)
                    }
                    "callRing" -> {
                        ring(call.argument<String>("title")!!)
                        result.success(null)
                    }
                    "callRingStop" -> {
                        NotificationManagerCompat.from(this).cancel(RING_ID)
                        result.success(null)
                    }
                    "callServiceStart" -> {
                        val intent = Intent(this, CallService::class.java)
                            .putExtra(CallService.EXTRA_TITLE, call.argument<String>("title") ?: "Sanctum")
                        result.success(runCatching { ContextCompat.startForegroundService(this, intent) }.isSuccess)
                    }
                    "callServiceStop" -> {
                        stopService(Intent(this, CallService::class.java))
                        result.success(null)
                    }
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
                NotificationChannel(CHANNEL_CALLS, "Calls", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "A friend calling you"
                    setSound(
                        RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
                        AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                            .build(),
                    )
                    enableVibration(true)
                    vibrationPattern = longArrayOf(0, 800, 600, 800)
                    lockscreenVisibility = Notification.VISIBILITY_PRIVATE
                },
                NotificationChannel(CHANNEL_CALL_ONGOING, "Call in progress", NotificationManager.IMPORTANCE_LOW)
                    .apply { setSound(null, null) },
            ),
        )
    }

    /** "Decline" or "Hang up" tapped on a call's notification. */
    fun callAction(action: String) {
        NotificationManagerCompat.from(this).cancel(RING_ID)
        channel.invokeMethod("callAction", action)
    }

    /**
     * A friend is calling: rings until answered, declined or given up. Over
     * the lock screen it says only "Incoming call". Full screen (opening
     * the app) where Android allows it.
     */
    @android.annotation.SuppressLint("MissingPermission") // checked by areNotificationsEnabled
    private fun ring(title: String) {
        val manager = NotificationManagerCompat.from(this)
        if (!manager.areNotificationsEnabled()) return
        val show = openApp(this, 9, "call")
        val answer = openApp(this, 8, "call:answer")
        val decline = PendingIntent.getBroadcast(
            this,
            10,
            Intent(this, CallActionReceiver::class.java).setAction(CallActionReceiver.DECLINE),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val fullScreen = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            getSystemService(NotificationManager::class.java).canUseFullScreenIntent()
        } else {
            true
        }
        fun builder(): NotificationCompat.Builder {
            val redacted = NotificationCompat.Builder(this, CHANNEL_CALLS)
                .setSmallIcon(R.drawable.ic_stat_sanctum)
                .setContentTitle("Sanctum")
                .setContentText("Incoming call")
                .build()
            val b = NotificationCompat.Builder(this, CHANNEL_CALLS)
                .setSmallIcon(R.drawable.ic_stat_sanctum)
                .setContentTitle(title)
                .setContentText("Incoming call")
                .setCategory(NotificationCompat.CATEGORY_CALL)
                .setPriority(NotificationCompat.PRIORITY_MAX)
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setPublicVersion(redacted)
                .setOngoing(true)
                .setAutoCancel(false)
                .setTimeoutAfter(RING_TIMEOUT_MS)
                .setContentIntent(show)
                .setSound(RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE))
            if (fullScreen) b.setFullScreenIntent(show, true)
            return b
        }
        // Android 12+: the system's call look, with Answer and Decline.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && fullScreen) {
            val styled = runCatching {
                val caller = Person.Builder().setName(title).setImportant(true).build()
                val n = builder().setStyle(NotificationCompat.CallStyle.forIncomingCall(caller, decline, answer)).build()
                n.flags = n.flags or Notification.FLAG_INSISTENT
                manager.notify(RING_ID, n)
            }
            if (styled.isSuccess) return
        }
        val n = builder()
            .addAction(0, "Decline", decline)
            .addAction(0, "Answer", answer)
            .build()
        n.flags = n.flags or Notification.FLAG_INSISTENT
        runCatching { manager.notify(RING_ID, n) }
    }

    @android.annotation.SuppressLint("MissingPermission") // checked by areNotificationsEnabled
    private fun notify(
        id: Int,
        channel: String,
        title: String,
        body: String,
        open: String?,
        count: Int,
        publicText: String? = null,
    ) {
        val manager = NotificationManagerCompat.from(this)
        if (!manager.areNotificationsEnabled()) return
        // On the lock screen: only that something arrived.
        val redacted = NotificationCompat.Builder(this, channel)
            .setSmallIcon(R.drawable.ic_stat_sanctum)
            .setContentTitle("Sanctum")
            .setContentText(publicText ?: if (channel == CHANNEL_REQUESTS) "Friend request" else "New message")
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
        const val CHANNEL_CALLS = "calls"
        const val CHANNEL_CALL_ONGOING = "call_ongoing"
        const val RING_ID = 3
        const val RING_TIMEOUT_MS = 60_000L
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
