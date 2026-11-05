package io.ldmux.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Whether the app is on screen, and what to do when it leaves.
 *
 * A remote terminal has to survive being switched away from — that is the whole point
 * of the foreground service — but "survive" is not the same as "hold on forever". A
 * phone in a pocket with a shell open used to keep a socket, a partial wake lock and a
 * claim on somebody else's shell for as long as the battery lasted, which is a cost
 * with no reader: nobody is looking at a terminal that is not on screen, and the shell
 * is safe in the keeper on the host regardless.
 *
 * So the app is on screen or it is not, and that fact is stated once, here, where the
 * platform sets it ([set], from `onStart`/`onStop` on Android and from
 * `didBecomeActive`/`didEnterBackground` on iOS). What the app *does* about it is
 * registered with [setReleaseHandler] and run by [BackgroundRelease] **from that
 * platform call**, not from a Compose effect: an effect needs a live composition and a
 * frame clock, and a suspended iOS process offers neither.
 */
object AppLifecycle {
    var foreground by mutableStateOf(true)
        private set

    private var release: BackgroundRelease? = null
    private val backgroundHandlers = mutableListOf<() -> Unit>()
    private val foregroundHandlers = mutableListOf<() -> Unit>()

    /**
     * Run [handler] when the app leaves the screen, immediately (no grace) — for
     * things that must stop the moment nobody is looking, such as the microphone.
     * Returns a function that removes it.
     */
    fun onBackground(handler: () -> Unit): () -> Unit {
        backgroundHandlers += handler
        return { backgroundHandlers -= handler }
    }

    /**
     * Run [handler] when the app comes back on screen, before anything is drawn again.
     *
     * A socket that spent minutes in the background is not alive because it was never told it
     * died: the phone's radio slept, the far end timed the flow out, and the first sign either
     * end gets is a write that goes nowhere. Waiting for the heartbeat to notice cost about
     * twenty-six seconds of "cannot reach the desktop" after every long absence (measured
     * 2026-11-03); the handler asks now, with a short deadline, and reconnects if the answer does
     * not come.
     */
    fun onForeground(handler: () -> Unit): () -> Unit {
        foregroundHandlers += handler
        return { foregroundHandlers -= handler }
    }

    /**
     * Run [handler] once the app has been off screen for the platform's grace
     * ([BackgroundSession.releaseGraceMs]) — synchronously when that is 0. Replaces any
     * earlier handler. Returns a function that removes it.
     */
    fun setReleaseHandler(
        graceMs: Long = BackgroundSession.releaseGraceMs,
        timer: ReleaseTimer = CoroutineReleaseTimer,
        handler: () -> Unit,
    ): () -> Unit {
        val mine = BackgroundRelease(graceMs, timer, handler)
        release = mine
        return {
            mine.onForeground()
            if (release === mine) release = null
        }
    }

    /** Called by the platform. `onStart`/`onStop` are the honest pair: the app is
     *  on screen between them, and neither fires for a dialog or a keyboard. */
    fun set(value: Boolean) {
        if (value == foreground) return
        foreground = value
        if (value) {
            release?.onForeground()
            for (h in foregroundHandlers.toList()) runCatching { h() }
        } else {
            for (h in backgroundHandlers.toList()) runCatching { h() }
            runCatching { release?.onBackground() }
        }
    }
}
