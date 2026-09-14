package dev.ldmux.mobile

import android.annotation.SuppressLint
import android.content.Context
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import org.json.JSONObject

/** Set from MainActivity so [AppPrefs] can reach SharedPreferences. */
object AndroidApp {
    lateinit var context: Context
}

actual object AppPrefs {
    private const val FILE = "ldmux"
    private const val KEY_HOST = "host"
    private const val KEY_TOKEN = "token"
    private const val KEY_SIDE = "side"

    actual fun load(): LdmuxConfig {
        val sp = AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        return LdmuxConfig(
            host = sp.getString(KEY_HOST, null) ?: LdmuxConfig.DEFAULT.host,
            token = sp.getString(KEY_TOKEN, null) ?: LdmuxConfig.DEFAULT.token,
            side = sp.getString(KEY_SIDE, null) ?: LdmuxConfig.DEFAULT.side,
        )
    }

    actual fun save(config: LdmuxConfig) {
        AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_HOST, config.host)
            .putString(KEY_TOKEN, config.token)
            .putString(KEY_SIDE, config.side)
            .apply()
    }
}

private class AndroidTerminalHandle : TerminalHandle {
    var view: WebView? = null

    private fun js(code: String) {
        view?.evaluateJavascript(code, null)
    }

    override fun sendKey(name: String) = js("window.ldmuxSendKey(${JSONObject.quote(name)})")
    override fun sendText(text: String) = js("window.ldmuxSendText(${JSONObject.quote(text)})")
    override fun loadHistory() = js("window.ldmuxLoadHistory()")
    override fun jumpToBottom() = js("window.ldmuxJumpToBottom()")
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
actual fun rememberTerminalSurface(url: String, modifier: Modifier): TerminalHandle {
    val handle = remember(url) { AndroidTerminalHandle() }
    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            WebView(ctx).apply {
                settings.javaScriptEnabled = true
                settings.domStorageEnabled = true
                settings.useWideViewPort = true
                settings.loadWithOverviewMode = false
                settings.mediaPlaybackRequiresUserGesture = false
                isScrollbarFadingEnabled = false
                webViewClient = WebViewClient()
                handle.view = this
                loadUrl(url)
            }
        },
    )
    return handle
}
