package dev.ldmux.mobile

import android.content.Context
import android.content.pm.ActivityInfo
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.runtime.rememberUpdatedState
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
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.material3.Text
import dev.ldmux.mobile.term.TerminalFrame
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
 * A zero-width character kept in the hidden input field so the IME always has
 * something to delete. See the field's comment: without it, backspace is
 * unreliable because the IME and the app disagree about the field's contents.
 */
private const val SENTINEL = "\u200b"

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
    /** Where the user last tapped, so the snap is visible before the server answers. */
    var tapAt by remember { mutableStateOf<Pair<Int, Int>?>(null) }

    // The tap detector must read the *current* frame without being restarted by
    // it (restarting would cancel an in-flight gesture).
    val currentFrame by rememberUpdatedState(frame)
    val currentViewport by rememberUpdatedState(viewport)
    LaunchedEffect(tapAt) {
        if (tapAt != null) {
            delay(600)
            tapAt = null
        }
    }

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

    Box(modifier.background(termBg).onPreviewKeyEvent { event ->
        // A hardware keyboard (and some IMEs' "send key" buttons) produce key
        // events rather than committed text. Anything that is a *terminal key*
        // must bypass the text field entirely, so it can never be mangled by the
        // IME — and Backspace here is the reliable path that the sentinel above
        // is the fallback for.
        if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
        val key = when (event.key) {
            Key.Backspace -> "BSpace"
            Key.Enter, Key.NumPadEnter -> "Enter"
            Key.Tab -> "Tab"
            Key.Escape -> "Escape"
            Key.DirectionUp -> "Up"
            Key.DirectionDown -> "Down"
            Key.DirectionLeft -> "Left"
            Key.DirectionRight -> "Right"
            Key.MoveHome -> "Home"
            Key.MoveEnd -> "End"
            Key.PageUp -> "PageUp"
            Key.PageDown -> "PageDown"
            Key.Delete -> "DC"
            else -> null
        }
        if (key == null) return@onPreviewKeyEvent false
        controller.sendKey(key)
        true
    }) {
        Canvas(
            Modifier
                .fillMaxSize()
                .onSizeChanged { viewport = it }
                .pointerInput(Unit) {
                    // A tap places the cursor. The hit-test is the exact inverse of
                    // the draw transform below; keeping them adjacent is the point
                    // (the recorded failure mode of scale-to-fit is pointer
                    // coordinates that forgot to divide by the scale).
                    detectTapGestures { pos ->
                        val f = currentFrame ?: return@detectTapGestures
                        val cell = cellAt(pos, currentViewport, f, painter.metrics) ?: return@detectTapGestures
                        tapAt = cell
                        controller.tapCell(cell.first, cell.second)
                    }
                }
                .pointerInput(Unit) {
                    var carry = 0f
                    val rowPx = painter.metrics.height
                    detectVerticalDragGestures(
                        onVerticalDrag = { change, dragAmount ->
                            change.consume()
                            if (rowPx <= 0f) return@detectVerticalDragGestures
                            carry += dragAmount
                            // Dragging down moves the content down, i.e. back
                            // into history: the emulator's delta is negative up.
                            val rows = (carry / rowPx).toInt()
                            if (rows != 0) {
                                carry -= rows * rowPx
                                controller.scrollBy(-rows)
                            }
                        },
                        onDragEnd = { carry = 0f },
                        onDragCancel = { carry = 0f },
                    )
                },
        ) {
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
            drawScrollbar(f, scale, dx, size.width, size.height)
            tapAt?.let { (col, row) ->
                val m2 = painter.metrics
                drawRect(
                    color = Accent,
                    topLeft = androidx.compose.ui.geometry.Offset(
                        dx + col * m2.width * scale,
                        dy + row * m2.height * scale,
                    ),
                    size = androidx.compose.ui.geometry.Size(m2.width * scale, m2.height * scale),
                    style = androidx.compose.ui.graphics.drawscope.Stroke(width = 1.5f * scale),
                )
            }
        }

        // Typing is opt-in, and the field is deliberately tiny: it only has to
        // be focusable for the IME, and covering the grid with it would eat the
        // scroll gesture.
        //
        // The field's value is a **sentinel**: a single zero-width character,
        // restored after every change. That is what makes deletion reliable.
        // With an accumulating buffer the IME's own idea of the text (composing
        // regions, autocorrect, suggestions) drifts from ours, and a backspace
        // can arrive as a change we cannot interpret — which is exactly the
        // "delete randomly stops working" bug. With a sentinel there is always
        // one character to delete, so the IME always reports the deletion.
        if (typing) {
            var field by remember { mutableStateOf(SENTINEL) }
            val sentinelLen = SENTINEL.length
            BasicTextField(
                value = field,
                onValueChange = { next ->
                    // Reset first: no state may accumulate across changes.
                    field = SENTINEL
                    val content = next.replace(SENTINEL, "")
                    if (content.isEmpty()) {
                        // The only thing removed was the sentinel: a backspace
                        // with nothing left on our side of the line.
                        if (next.length < sentinelLen) controller.sendKey("BSpace")
                        return@BasicTextField
                    }
                    val pending = StringBuilder()
                    for (ch in content) {
                        when (ch) {
                            '\n', '\r' -> {
                                if (pending.isNotEmpty()) {
                                    controller.sendText(pending.toString())
                                    pending.clear()
                                }
                                controller.sendKey("Enter")
                            }
                            // Tab is a named key, not text: it must reach tmux as
                            // Tab so completion and tmux's own bindings see a
                            // real TAB, and so it can never be predicted.
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
                },
                modifier = Modifier
                    .size(1.dp)
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

/**
 * The inverse of the draw transform: a touch point to a terminal cell, or null
 * when the point is in the letterbox beside the grid.
 */
private fun cellAt(
    pos: androidx.compose.ui.geometry.Offset,
    viewport: IntSize,
    f: TerminalFrame.Frame,
    m: dev.ldmux.mobile.term.CellMetrics,
): Pair<Int, Int>? {
    if (viewport.width <= 0 || viewport.height <= 0) return null
    val gridW = m.width * f.cols
    val gridH = m.height * f.rows
    if (gridW <= 0f || gridH <= 0f) return null
    val scale = minOf(viewport.width / gridW, viewport.height / gridH)
    val dx = (viewport.width - gridW * scale) / 2f
    val dy = (viewport.height - gridH * scale) / 2f
    val col = floor((pos.x - dx) / (m.width * scale)).toInt()
    val row = floor((pos.y - dy) / (m.height * scale)).toInt()
    if (col < 0 || row < 0 || col >= f.cols || row >= f.rows) return null
    return col to row
}

/**
 * A thin scrollbar while the view is parked in history. The emulator polls its
 * own scrollbar geometry per frame (there is deliberately no change event), and
 * the frame carries it, so nothing extra is requested to draw this.
 */
private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawScrollbar(
    f: TerminalFrame.Frame,
    scale: Float,
    dx: Float,
    width: Float,
    height: Float,
) {
    if (f.viewportActive || f.scrollTotal <= 0 || f.scrollLen <= 0) return
    val trackTop = 0f
    val trackH = height
    val frac = f.scrollLen.toFloat() / f.scrollTotal.toFloat()
    val barH = (trackH * frac).coerceAtLeast(24f)
    val maxOffset = (f.scrollTotal - f.scrollLen).coerceAtLeast(0).toFloat()
    val pos = if (maxOffset <= 0f) 0f else (f.scrollOffset.toFloat() / maxOffset).coerceIn(0f, 1f)
    val top = trackTop + (trackH - barH) * pos
    val x = dx + width - 6f * scale
    drawRect(
        color = Color(0x66FFFFFF),
        topLeft = androidx.compose.ui.geometry.Offset(x, top),
        size = androidx.compose.ui.geometry.Size(4f * scale, barH),
    )
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