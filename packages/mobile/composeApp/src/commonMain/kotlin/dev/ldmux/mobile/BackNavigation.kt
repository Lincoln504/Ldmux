package dev.ldmux.mobile

import androidx.compose.runtime.Composable

/**
 * The platform's "go back" gesture, if it has one.
 *
 * Android has a system back gesture and it was unhandled, so back fell through to
 * finishing the activity: a user in a shell who pressed back left the app instead
 * of returning to the list of shells. iOS has no back button, so its actual is
 * empty — but the *intent* is shared, because "back" on a screen showing one shell
 * means the same thing on both platforms, and one of them spells it as an edge
 * swipe.
 *
 * Enabled only while a terminal is open, so back at the launcher still means what
 * the platform says it means (leaving).
 */
@Composable
expect fun BackToLauncher(enabled: Boolean, onBack: () -> Unit)
