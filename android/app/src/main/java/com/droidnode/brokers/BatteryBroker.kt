package com.droidnode.brokers

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.util.Log

private const val TAG = "BatteryBroker"

data class BatteryState(
    val percent: Int,
    val charging: Boolean,
    /** Battery temperature in °C, when the device reports it. */
    val temperatureC: Float? = null,
)

/**
 * Broker: reads the current battery state from the Android BatteryManager.
 * Registers a sticky broadcast receiver — call [unregister] when done.
 * No thresholds, no logic — that belongs in ResourceGuardService.
 */
class BatteryBroker(private val context: Context) {

    /** Read the current battery state synchronously (sticky broadcast). */
    fun readState(): BatteryState {
        val intent: Intent? = context.registerReceiver(
            null,
            IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        )

        val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, 100) ?: 100
        val status = intent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1

        val percent = if (level >= 0 && scale > 0) (level * 100 / scale) else 100
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING ||
                status == BatteryManager.BATTERY_STATUS_FULL
        // EXTRA_TEMPERATURE is tenths of a degree Celsius; absent on some ROMs.
        val temperature = intent?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Int.MIN_VALUE)
            ?.takeIf { it != Int.MIN_VALUE && it != 0 }
            ?.let { it / 10f }

        Log.d(TAG, "battery: $percent% charging=$charging temp=$temperature")
        return BatteryState(percent = percent, charging = charging, temperatureC = temperature)
    }
}
