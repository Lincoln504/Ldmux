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
    fun loadHistory()
    fun jumpToBottom()
}

/** Create (or reuse) the native terminal surface for one session. */
@Composable
expect fun rememberTerminalSurface(config: LdmuxConfig, sessionId: String, modifier: Modifier): TerminalHandle