package io.sanctum.sanctum

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

/**
 * Keeps the process (and so Tor and the links to friends) alive while the
 * app is closed. Android requires a visible notification for this; it's
 * kept minimal and silent.
 */
class ConnectionService : Service() {
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val notification = NotificationCompat.Builder(this, SanctumApplication.CHANNEL_CONNECTION)
            .setSmallIcon(R.drawable.ic_stat_sanctum)
            .setContentTitle("Sanctum")
            .setContentText("Connected privately")
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setContentIntent(SanctumApplication.openApp(this, 0, null))
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING)
        } else {
            startForeground(ID, notification)
        }
        // Restarted by Android if it has to stop the process; the engine
        // (and Tor) start again with it.
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val ID = 1
    }
}
