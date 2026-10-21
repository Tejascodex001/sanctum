package io.sanctum.sanctum

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** "Decline" and "Hang up" from a call's notification, passed to Dart. */
class CallActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val action = when (intent.action) {
            DECLINE -> "decline"
            HANG_UP -> "hangup"
            else -> return
        }
        (context.applicationContext as? SanctumApplication)?.callAction(action)
    }

    companion object {
        const val DECLINE = "io.sanctum.sanctum.call.DECLINE"
        const val HANG_UP = "io.sanctum.sanctum.call.HANG_UP"
    }
}
