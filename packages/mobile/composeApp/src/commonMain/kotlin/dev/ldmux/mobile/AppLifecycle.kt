package dev.ldmux.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Whether the app is on screen.
 *
 * A remote terminal has to survive being switched away from — that is the whole
 * point of the foreground service — but "survive" is not the same as "hold on
 * forever". A phone in a pocket with a shell open used to keep a socket, a
 * partial wake lock and a claim on somebody else's shell for as long as the
 * battery lasted, which is a cost with no reader: nobody is looking at a terminal
 * that is not on screen, and the shell is safe in tmux on the host regardless.
 *
 * So the app is on screen or it is not, and that fact is stated once, here, where
 * the platform can set it. What the app *does* about it is [App]'s business.
 */
object AppLifecycle {
    var foreground by mutableStateOf(true)
        private set

    /** Called by the platform. `onStart`/`onStop` are the honest pair: the app is
     *  on screen between them, and neither fires for a dialog or a keyboard. */
    fun set(value: Boolean) {
        foreground = value
    }
}
