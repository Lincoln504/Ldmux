package io.ldmux.mobile

import android.content.Intent
import android.net.Uri

/**
 * The version Android installed, read from the package manager rather than baked in: the
 * Gradle `versionName` is the single source, so this cannot drift from what a store listing
 * says. An unreadable package reads as empty rather than crashing a settings screen.
 */
actual fun appVersionName(): String =
    runCatching {
        val pm = AndroidApp.context.packageManager
        pm.getPackageInfo(AndroidApp.context.packageName, 0).versionName.orEmpty()
    }.getOrDefault("")

actual fun openUrl(url: String): Boolean = runCatching {
    val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    }
    AndroidApp.context.startActivity(intent)
    true
}.getOrDefault(false)
