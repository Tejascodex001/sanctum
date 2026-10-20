package io.sanctum.sanctum

import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings

/**
 * Phone makers that stop background apps despite a foreground service each
 * hide the "let it run" switch on their own settings page. Opens the right
 * one directly, else the app's own settings. (Pages from dontkillmyapp.com
 * and the AutoStarter project; they move between OS versions, so several
 * are tried.)
 */
object BackgroundSettings {
    private val pages: Map<String, List<ComponentName>> = mapOf(
        "xiaomi" to listOf(
            ComponentName("com.miui.securitycenter", "com.miui.permcenter.autostart.AutoStartManagementActivity"),
            ComponentName("com.miui.powerkeeper", "com.miui.powerkeeper.ui.HiddenAppsConfigActivity"),
        ),
        "oppo" to listOf(
            ComponentName("com.coloros.safecenter", "com.coloros.safecenter.permission.startup.StartupAppListActivity"),
            ComponentName("com.coloros.safecenter", "com.coloros.safecenter.startupapp.StartupAppListActivity"),
            ComponentName("com.oppo.safe", "com.oppo.safe.permission.startup.StartupAppListActivity"),
        ),
        "vivo" to listOf(
            ComponentName("com.vivo.permissionmanager", "com.vivo.permissionmanager.activity.BgStartUpManagerActivity"),
            ComponentName("com.iqoo.secure", "com.iqoo.secure.ui.phoneoptimize.AddWhiteListActivity"),
            ComponentName("com.iqoo.secure", "com.iqoo.secure.ui.phoneoptimize.BgStartUpManager"),
        ),
        "huawei" to listOf(
            ComponentName("com.huawei.systemmanager", "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity"),
            ComponentName("com.huawei.systemmanager", "com.huawei.systemmanager.optimize.process.ProtectActivity"),
        ),
        "samsung" to listOf(
            ComponentName("com.samsung.android.lool", "com.samsung.android.sm.battery.ui.BatteryActivity"),
            ComponentName("com.samsung.android.lool", "com.samsung.android.sm.ui.battery.BatteryActivity"),
            ComponentName("com.samsung.android.sm", "com.samsung.android.sm.ui.battery.BatteryActivity"),
        ),
        "oneplus" to listOf(
            ComponentName("com.oneplus.security", "com.oneplus.security.chainlaunch.view.ChainLaunchAppListActivity"),
        ),
    )

    /** Brands whose battery savers are known to kill background apps. */
    private val aliases = mapOf("redmi" to "xiaomi", "poco" to "xiaomi", "realme" to "oppo", "iqoo" to "vivo", "honor" to "huawei")

    fun brand(): String {
        val maker = Build.MANUFACTURER.lowercase()
        return aliases[maker] ?: maker
    }

    fun aggressive(): Boolean = pages.containsKey(brand())

    /** Opens the best page; returns which kind was opened. */
    fun open(activity: Activity): String {
        for (page in pages[brand()].orEmpty()) {
            val intent = Intent().setComponent(page).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            if (runCatching { activity.startActivity(intent) }.isSuccess) return "maker"
        }
        val details = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${activity.packageName}"))
        return if (runCatching { activity.startActivity(details) }.isSuccess) "app" else "none"
    }
}
