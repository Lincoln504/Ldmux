package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame
import kotlin.math.abs

/**
 * What a swipe means on the alternate screen.
 *
 * The alternate screen has no scrollback, so the local view cannot move. A
 * desktop terminal turns the wheel into something the program understands, and
 * so does this:
 *
 *  - **mouse reporting on (SGR)** — wheel events (`CSI < 64 ; c ; r M` up,
 *    `65` down), which is what `less`, `vim` and `htop` get from a real wheel;
 *  - **otherwise** — Up/Down arrows, the alternate-scroll behaviour (DEC 1007)
 *    that terminals apply by default on the alternate screen.
 *
 * [rows] follows `scrollBy`: negative looks at older content (wheel up). Pure,
 * so the rule can be tested without a device.
 */
sealed interface AltScroll {
    /** Raw bytes for the `input` message. */
    data class Wheel(val bytes: String) : AltScroll

    /** Key names for the `key` message. */
    data class Keys(val keys: List<String>) : AltScroll

    data object None : AltScroll
}

/**
 * One swipe step never turns into more than this many events.
 *
 * This used to be 5, and that was the clunkiness: a fast swipe produces few
 * drag callbacks with a large row delta, so almost every flick of the finger
 * was truncated to five lines no matter how far it travelled. One event per row
 * moved (up to a screen and a half) is what a wheel does on a desktop, which is
 * the behaviour this mirrors.
 */
internal const val ALT_SCROLL_MAX_EVENTS = 40

fun planAltScroll(rows: Int, frame: TerminalFrame.Frame, mouseRefused: Boolean): AltScroll {
    if (rows == 0 || !frame.alternateOn || !frame.viewportActive || frame.passwordInput) return AltScroll.None
    val n = abs(rows).coerceAtMost(ALT_SCROLL_MAX_EVENTS)
    val up = rows < 0
    if (frame.mouseTracking && frame.mouseSgr && !mouseRefused) {
        val col = (frame.cols / 2).coerceAtLeast(0) + 1
        val row = (frame.rows / 2).coerceAtLeast(0) + 1
        val button = if (up) 64 else 65
        return AltScroll.Wheel("\u001b[<$button;$col;${row}M".repeat(n))
    }
    return AltScroll.Keys(List(n) { if (up) "Up" else "Down" })
}
