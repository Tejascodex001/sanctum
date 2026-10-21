package io.sanctum.sanctum

import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

/**
 * Runs while a call is ringing out or going on, so Android keeps the
 * microphone and the process going when the screen is off or another app
 * is open. Its notification says a call is on and offers to hang up.
 */
class CallService : Service() {
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val title = intent?.getStringExtra(EXTRA_TITLE) ?: "Sanctum"
        val hangUp = PendingIntent.getBroadcast(
            this,
            7,
            Intent(this, CallActionReceiver::class.java).setAction(CallActionReceiver.HANG_UP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(this, SanctumApplication.CHANNEL_CALL_ONGOING)
            .setSmallIcon(R.drawable.ic_stat_sanctum)
            .setContentTitle(title)
            .setContentText("Call in progress")
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setContentIntent(SanctumApplication.openApp(this, 6, "call"))
            .addAction(0, "Hang up", hangUp)
            .build()
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                startForeground(ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
            } else {
                startForeground(ID, notification)
            }
        } catch (e: Exception) {
            // Not allowed right now (e.g. started from the background):
            // the call goes on while the app is open.
            stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val ID = 4
        const val EXTRA_TITLE = "title"
    }
}
