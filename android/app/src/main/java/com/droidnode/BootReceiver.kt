package com.droidnode

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import com.droidnode.exposers.ForegroundServiceExposer
import com.droidnode.ui.v2.LocalConfig

private const val TAG = "BootReceiver"

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        if (!LocalConfig.autostart(context)) {
            Log.i(TAG, "boot completed — autostart disabled, not starting the agent")
            return
        }
        Log.i(TAG, "boot completed — starting DroidNode agent")
        val svcIntent = Intent(context, ForegroundServiceExposer::class.java)
        context.startForegroundService(svcIntent)
    }
}
