package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect

/**
 * Which way a screen may turn (ADR-0011, amended 2026-09-27).
 *
 * The shell view follows the device. The launcher — the list, and every dialog
 * opened from it (new shell, settings, the desktop picker) — stays portrait: it is
 * a list you read and tap, and turning it sideways only makes it shorter.
 *
 * A seam because the policy is the OS's to apply (an Activity's requested
 * orientation on Android, the scene's supported orientations on iOS). The decision
 * of *which screen* holds which policy is made here and nowhere else.
 */
enum class OrientationPolicy { Portrait, FollowDevice }

expect object ScreenOrientation {
    fun apply(policy: OrientationPolicy)
}

/**
 * Hold [policy] while this is composed, and give the device back when it leaves.
 *
 * One holder at a time by construction: only the launcher branch composes this, and
 * leaving it restores [OrientationPolicy.FollowDevice] — so there is never a second
 * writer to disagree with, which is what ADR-0011 removed the old lock for.
 */
@Composable
fun HoldOrientation(policy: OrientationPolicy) {
    DisposableEffect(policy) {
        ScreenOrientation.apply(policy)
        onDispose { ScreenOrientation.apply(OrientationPolicy.FollowDevice) }
    }
}
