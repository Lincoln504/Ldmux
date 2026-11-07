package io.ldmux.mobile

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
