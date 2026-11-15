package com.droidnode.brokers

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.util.Log

private const val TAG = "WifiLockBroker"

/**
 * Broker: keeps the Wi-Fi radio awake while the agent runs.
 *
 * Without it Android suspends Wi-Fi when the screen turns off, heartbeats stop,
 * the node flips to NotReady and k3s starts logging
 * `Software caused connection abort (os error 103)`.
 *
 * WIFI_MODE_FULL_HIGH_PERF is the mode that means "keep the radio on"; it is
 * deprecated since API 29 but still the correct choice for a background
 * service that must stay reachable. On API 29+ we additionally ask for
 * low-latency mode when available because some OEMs only honour that one.
 */
class WifiLockBroker(context: Context) {

    private val wifiManager =
        context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
    private var lock: WifiManager.WifiLock? = null

    @Suppress("DEPRECATION")
    fun acquire() {
        val wm = wifiManager ?: run {
            Log.w(TAG, "no WifiManager; skipping wifi lock")
            return
        }
        if (lock?.isHeld == true) return

        val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        val wl = wm.createWifiLock(mode, "DroidNode::WifiLock").apply {
            setReferenceCounted(false)
        }
        try {
            wl.acquire()
            lock = wl
            Log.i(TAG, "wifi lock acquired (mode=$mode)")
        } catch (e: Exception) {
            // Some OEM ROMs throw when Wi-Fi is off; the node keeps working on
            // mobile data, so this is not fatal.
            Log.w(TAG, "could not acquire wifi lock: ${e.message}")
        }
    }

    fun release() {
        lock?.let {
            if (it.isHeld) {
                it.release()
                Log.i(TAG, "wifi lock released")
            }
        }
        lock = null
    }

    fun isHeld(): Boolean = lock?.isHeld == true
}
