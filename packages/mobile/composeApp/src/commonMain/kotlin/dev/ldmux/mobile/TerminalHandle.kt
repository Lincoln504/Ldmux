package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * How the link is behaving. The repaint cadence follows it, so showing it is how
 * a user (or a bug report) learns *why* a slow connection feels the way it does.
 */
data class LinkStats(
    /** Smoothed round trip in ms; an assumption until [measured]. */
    val rttMs: Long,
    /** Current repaint interval, in ms. */
    val frameIntervalMs: Long,
    /** True once a real round trip has been measured. */
    val measured: Boolean,
) {
    val fps: Int get() = (1000L / frameIntervalMs.coerceAtLeast(1L)).toInt()
}

/**
 * A handle to one terminal surface. The native sidebar drives the terminal
 * through this, so controls live in native chrome rather than inside a WebView.
 */interface TerminalHandle {
    fun sendKey(name: String)
    fun sendText(text: String)

    /** Page the session's history into the emulator and show its oldest row. */
    fun loadHistory()

    /** Return to the live prompt. */
    fun jumpToBottom()

    /** Scroll the emulator's viewport by [rows] (negative is up). */
    fun scrollBy(rows: Int) {}

    /**
     * The user tapped a cell. Whether that moves the shell's cursor, clicks for a
     * mouse-aware program, or does nothing, is decided by the pane's state.
     */
    fun tapCell(col: Int, row: Int) {}

    /** Start dictation. The transcript is typed into the pane, keyboard or not. */
    fun startVoice() {}

    /**
     * How the link is behaving, so the chrome can show it. The frame cadence is
     * derived from the round trip, and a number the user can see is the only way
     * to tell "this link is slow" from "this app is slow".
     */
    val link: StateFlow<LinkStats>
        get() = EMPTY_LINK

    companion object {
        /** For surfaces that do not report link state (the web view, tests). */
        val EMPTY_LINK: StateFlow<LinkStats> = MutableStateFlow(LinkStats(0, FramePacing.MIN_INTERVAL_MS, false))
    }
}

/**
 * Create (or reuse) the native terminal surface for one session.
 *
 * [typing] is the app-wide typing mode: the surface focuses its input
 * connection (raising the keyboard) and the platform flips to portrait while it
 * is set.
 *
 * [active] is whether this page is the one on screen. The pager keeps a
 * neighbour composed so a swipe is instant, but a composed page must not stay
 * *connected*: that would stream output for a terminal nobody is looking at and,
 * because the client owns the pane geometry, resize the panes of sessions the
 * user is not on.
 */
@Composable
expect fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    typing: Boolean,
    active: Boolean,
    modifier: Modifier,
): TerminalHandle