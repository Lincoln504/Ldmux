package dev.ldmux.mobile

import android.content.Context
import android.content.pm.ActivityInfo
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.material3.Text
import dev.ldmux.mobile.term.TerminalPainter
import dev.ldmux.mobile.voice.VoiceDictation
import dev.ldmux.mobile.voice.VoiceState
import dev.ldmux.mobile.voice.findActivity
import kotlinx.coroutines.delay
import kotlin.math.floor

/** Set from MainActivity so [AppPrefs] can reach SharedPreferences. */
object AndroidApp {
    lateinit var context: Context
}

actual object AppPrefs {
    private const val FILE = "ldmux"
    private const val KEY_HOST = "host"
    private const val KEY_TOKEN = "token"
    private const val KEY_SIDE = "side"
    private const val KEY_PREDICT = "predict"

    actual fun load(): LdmuxConfig {
        val sp = AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        return LdmuxConfig(
            host = sp.getString(KEY_HOST, null) ?: LdmuxConfig.DEFAULT.host,
            token = sp.getString(KEY_TOKEN, null) ?: LdmuxConfig.DEFAULT.token,
            side = sp.getString(KEY_SIDE, null) ?: LdmuxConfig.DEFAULT.side,
            predict = sp.getBoolean(KEY_PREDICT, LdmuxConfig.DEFAULT.predict),
        )
    }

    actual fun save(config: LdmuxConfig) {
        AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_HOST, config.host)
            .putString(KEY_TOKEN, config.token)
            .putString(KEY_SIDE, config.side)
            .putBoolean(KEY_PREDICT, config.predict)
            .apply()
    }
}

/** Fallback surface colour until the engine has told us its own background. */
private val TermBg = Color(0xFF000000)

/**
 * The native terminal surface: a Compose Canvas painting the libghostty-vt cell
 * grid.
 *
 * The grid is *not* scaled to fit a fixed pane any more. The surface measures
 * its own viewport, works out how many cells fit, and tells the daemon — which
 * resizes the tmux pane to match. The shell is therefore always exactly the size
 * of the screen showing it, and the canvas draws at 1:1 with only the leftover
 * sliver centred.
 */
@Composable
actual fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    typing: Boolean,
    modifier: Modifier,
): TerminalHandle {
    val controller = remember(config, sessionId) { TerminalController(config, sessionId) }
    DisposableEffect(controller) { onDispose { controller.dispose() } }

    val frame by controller.frame.collectAsState()
    val density = LocalDensity.current
    val fontSizePx = with(density) { 12.sp.toPx() }
    val painter = remember(fontSizePx) { TerminalPainter(fontSizePx) }

    val context = LocalContext.current
    val activity = remember(context) { context.findActivity() }
    val focusManager = LocalFocusManager.current
    val focusRequester = remember { FocusRequester() }
    var voice by remember { mutableStateOf<VoiceState>(VoiceState.Idle) }
    var viewport by remember { mutableStateOf(IntSize.Zero) }

    val metrics = painter.metrics
    // The pane follows the viewport, always. In portrait the keyboard is part of
    // that viewport (it is always up there), so the grid simply fits the space
    // that is actually visible; nothing is scaled down to a postage stamp.
    // The short delay debounces the two or three intermediate sizes a rotation
    // and an IME animation produce, so the daemon is told once, not thrice.
    LaunchedEffect(viewport, metrics.width, metrics.height) {
        if (viewport.width <= 0 || viewport.height <= 0) return@LaunchedEffect
        if (metrics.width <= 0f || metrics.height <= 0f) return@LaunchedEffect
        delay(250)
        val cols = floor(viewport.width / metrics.width).toInt().coerceAtLeast(20)
        val rows = floor(viewport.height / metrics.height).toInt().coerceAtLeast(5)
        controller.setGrid(cols, rows)
    }

    SideEffect {
        controller.onStartVoice = {
            val act = activity
            if (act == null) {
                voice = VoiceState.Unavailable
            } else {
                VoiceDictation.toggle(
                    act,
                    onText = { controller.sendText(it) },
                    onState = { voice = it },
                )
            }
        }
    }

    // Portrait typing flips the activity to portrait, landscape typing back.
    // Locked orientations, not sensor: the app never rotates itself.
    LaunchedEffect(typing) {
        activity?.requestedOrientation = if (typing) {
            ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        } else {
            ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        }
        if (typing) {
            focusRequester.requestFocus()
        } else {
            focusManager.clearFocus()
        }
    }
    DisposableEffect(Unit) { onDispose { VoiceDictation.stop() } }

    val termBg = frame?.defaultBg?.let { Color(0xFF000000.toInt() or (it and 0xFFFFFF)) } ?: TermBg

    Box(modifier.background(termBg)) {
        Canvas(Modifier.fillMaxSize().onSizeChanged { viewport = it }) {
            val f = frame ?: return@Canvas
            val m = painter.metrics
            val gridW = m.width * f.cols
            val gridH = m.height * f.rows
            if (gridW <= 0f || gridH <= 0f) return@Canvas
            // 1:1 after a resize; the scale only absorbs rounding (or the
            // keyboard's inset while typing), and the leftover is split evenly
            // so the grid sits centred rather than jammed into a corner.
            val scale = minOf(size.width / gridW, size.height / gridH)
            val dx = (size.width - gridW * scale) / 2f
            val dy = (size.height - gridH * scale) / 2f
            withTransform({
                translate(dx, dy)
                scale(scale, scale, pivot = androidx.compose.ui.geometry.Offset.Zero)
            }) {
                painter.paint(this, f)
            }
        }

        // Typing is opt-in: this field only exists while the keyboard is up, so
        // a tap on the grid can never raise the IME.
        if (typing) {
            var field by remember { mutableStateOf("") }
            BasicTextField(
                value = field,
                onValueChange = { next ->
                    val old = field
                    when {
                        next.length > old.length -> {
                            val added = next.substring(old.length)
                            val pending = StringBuilder()
                            for (ch in added) {
                                when (ch) {
                                    '\n', '\r' -> {
                                        if (pending.isNotEmpty()) {
                                            controller.sendText(pending.toString())
                                            pending.clear()
                                        }
                                        controller.sendKey("Enter")
                                    }
                                    // Tab is a named key, not text: it must reach
                                    // tmux as Tab so the shell's completion and
                                    // tmux's own bindings see a real TAB, and so
                                    // it can never be a prediction candidate.
                                    '\t' -> {
                                        if (pending.isNotEmpty()) {
                                            controller.sendText(pending.toString())
                                            pending.clear()
                                        }
                                        controller.sendKey("Tab")
                                    }
                                    else -> pending.append(ch)
                                }
                            }
                            if (pending.isNotEmpty()) controller.sendText(pending.toString())
                        }
                        next.length < old.length -> repeat(old.length - next.length) {
                            controller.sendKey("BSpace")
                        }
                    }
                    field = next
                },
                modifier = Modifier
                    .fillMaxSize()
                    .alpha(0f)
                    .focusRequester(focusRequester),
                cursorBrush = SolidColor(Color.Transparent),
                keyboardOptions = KeyboardOptions(
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                ),
            )
        }

        VoiceBanner(voice, Modifier.align(Alignment.TopStart))
    }
    return controller
}

/** A small line of feedback while dictation is running. */
@Composable
private fun VoiceBanner(state: VoiceState, modifier: Modifier) {
    val text = when (state) {
        VoiceState.Idle -> return
        VoiceState.RequestingPermission -> "microphone permission…"
        VoiceState.Listening -> "listening…"
        VoiceState.Denied -> "microphone permission denied"
        VoiceState.Unavailable -> "no speech recognition on this device"
        is VoiceState.Partial -> state.text
        is VoiceState.Failed -> "voice: ${state.reason}"
    }
    val accent = when (state) {
        is VoiceState.Failed, VoiceState.Denied, VoiceState.Unavailable -> Color(0xFFFF6B6B)
        else -> Accent
    }
    Box(
        modifier
            .padding(8.dp)
            .background(Surface)
            .padding(horizontal = 10.dp, vertical = 4.dp),
    ) {
        Text(text, color = accent, fontSize = 12.sp, fontFamily = FontFamily.Monospace)
    }
}