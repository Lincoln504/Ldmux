package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * A handle to one terminal surface. The native sidebar drives the terminal
 * through this, so controls live in native chrome rather than inside a WebView.
 */
interface TerminalHandle {
    fun sendKey(name: String)
    fun sendText(text: String)

    /** Page the session's history into the emulator and show its oldest row. */
    fun loadHistory()

    /** Return to the live prompt. */
    fun jumpToBottom()

    /** Scroll the emulator's viewport by [rows] (negative is up). */
    fun scrollBy(rows: Int) {}

    /** Start dictation. The transcript is typed into the pane, keyboard or not. */
    fun startVoice() {}
}

/**
 * Create (or reuse) the native terminal surface for one session.
 *
 * [typing] is the app-wide typing mode: the surface focuses its input
 * connection (raising the keyboard) and the platform flips to portrait while it
 * is set.
 */
@Composable
expect fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    typing: Boolean,
    modifier: Modifier,
): TerminalHandle