package dev.devcat.keepalive

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.PowerManager
import android.provider.Settings
import androidx.core.content.ContextCompat
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/** Starts and stops [KeepAliveService]; called from the app's Rust side (src/lib.rs). */
@TauriPlugin
class KeepAlivePlugin(private val activity: Activity) : Plugin(activity) {
    private fun service() = Intent(activity, KeepAliveService::class.java)

    @Command
    fun start(invoke: Invoke) {
        try {
            ContextCompat.startForegroundService(activity, service())
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("Couldn't stay connected in the background: ${e.message}")
        }
    }

    @Command
    fun stop(invoke: Invoke) {
        activity.stopService(service())
        invoke.resolve()
    }

    @Command
    fun battery(invoke: Invoke) {
        val pm = activity.getSystemService(PowerManager::class.java)
        val ret = JSObject()
        ret.put("unrestricted", pm?.isIgnoringBatteryOptimizations(activity.packageName) == true)
        invoke.resolve(ret)
    }

    /** The list of apps and their battery optimization (needs no special permission). */
    @Command
    fun settings(invoke: Invoke) {
        try {
            activity.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        } catch (e: Exception) {
            activity.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${activity.packageName}")))
        }
        invoke.resolve()
    }
}
