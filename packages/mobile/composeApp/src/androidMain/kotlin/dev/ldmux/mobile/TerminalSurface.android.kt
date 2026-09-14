package dev.ldmux.mobile

import android.content.Context
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.term.TerminalPainter

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

/**
 * The native terminal surface: a Compose Canvas painting the libghostty-vt cell
 * grid, scaled to fit (the daemon owns the pane size, as with the web client).
 */
@Composable
actual fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    modifier: Modifier,
): TerminalHandle {
    val controller = remember(config, sessionId) { TerminalController(config, sessionId) }
    DisposableEffect(controller) { onDispose { controller.dispose() } }

    val frame by controller.frame.collectAsState()
    val density = LocalDensity.current
    val fontSizePx = with(density) { 12.sp.toPx() }
    val painter = remember(fontSizePx) { TerminalPainter(fontSizePx) }

    Box(modifier.background(Bg)) {
        Canvas(Modifier.fillMaxSize()) {
            val f = frame ?: return@Canvas
            val m = painter.metrics
            val gridW = m.width * f.cols
            val gridH = m.height * f.rows
            if (gridW <= 0f || gridH <= 0f) return@Canvas
            val scale = minOf(size.width / gridW, size.height / gridH)
            withTransform({ scale(scale, scale) }) {
                painter.paint(this, f)
            }
        }
    }
    return controller
}