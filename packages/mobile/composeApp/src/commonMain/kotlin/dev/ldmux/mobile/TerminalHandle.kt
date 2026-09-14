package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * A handle to one terminal surface. The native sidebar drives the terminal
 * through this, so controls live in native chrome rather than in the WebView.
 */
interface TerminalHandle {
    fun sendKey(name: String)
    fun sendText(text: String)
    fun loadHistory()
    fun jumpToBottom()
}

/** Create (or reuse) the terminal surface for a session URL. */
@Composable
expect fun rememberTerminalSurface(url: String, modifier: Modifier): TerminalHandle
