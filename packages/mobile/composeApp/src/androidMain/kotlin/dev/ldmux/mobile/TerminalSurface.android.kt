package dev.ldmux.mobile


import android.content.Context
import android.os.SystemClock
import android.content.pm.ActivityInfo
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.input.pointer.PointerInputScope
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
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.material3.Text
import dev.ldmux.mobile.term.GridPlacement
import dev.ldmux.mobile.term.TerminalFrame
import dev.ldmux.mobile.term.TerminalPainter
import dev.ldmux.mobile.term.placeGrid
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.math.floor
import kotlin.math.roundToInt

/** Set from MainActivity so [AppPrefs] can reach SharedPreferences. */
object AndroidApp {
    lateinit var context: Context
}

/**
 * A random install id, generated once.
 *
 * `UUID.randomUUID` rather than a hardware identifier: this must not be a
 * tracking id, and it must not be spoofable into someone else's — it only has to
 * be unique among the handful of devices that share a daemon.
 */
actual fun newDeviceId(): String = java.util.UUID.randomUUID().toString()

/** The model name, e.g. "Pixel 8" — what "open on …" should call this phone. */
actual fun deviceLabel(): String = android.os.Build.MODEL.ifEmpty { "phone" }

actual object AppPrefs {
    private const val FILE = "ldmux"
    private const val KEY_HOST = "host"
    private const val KEY_TOKEN = "token"
    private const val KEY_SIDE = "side"
    private const val KEY_PREDICT = "predict"
    private const val KEY_FONT = "font_sp"
    private const val KEY_COMPACT = "compact"
    private const val KEY_DEVICE_ID = "device_id"
    private const val KEY_DEVICE_LABEL = "device_label"

    actual fun load(): LdmuxConfig {
        val sp = AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        // Generated on first load and written back immediately, so the identity
        // is stable from the first connection onwards — including the very first
        // one, which is the one that creates the claim this id is attributed to.
        val deviceId = sp.getString(KEY_DEVICE_ID, null)
            ?.takeIf { it.isNotEmpty() }
            ?: newDeviceId().also { sp.edit().putString(KEY_DEVICE_ID, it).apply() }
        val label = sp.getString(KEY_DEVICE_LABEL, null)
            ?.takeIf { it.isNotEmpty() }
            ?: deviceLabel().also { sp.edit().putString(KEY_DEVICE_LABEL, it).apply() }
        return LdmuxConfig(
            host = sp.getString(KEY_HOST, null) ?: LdmuxConfig.DEFAULT.host,
            token = sp.getString(KEY_TOKEN, null) ?: LdmuxConfig.DEFAULT.token,
            side = sp.getString(KEY_SIDE, null) ?: LdmuxConfig.DEFAULT.side,
            predict = sp.getBoolean(KEY_PREDICT, LdmuxConfig.DEFAULT.predict),
            fontSp = sp.getFloat(KEY_FONT, LdmuxConfig.DEFAULT_FONT_SP)
                .coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP),
            compact = sp.getBoolean(KEY_COMPACT, LdmuxConfig.DEFAULT.compact),
            deviceId = deviceId,
            deviceLabel = label,
        )
    }

    actual fun save(config: LdmuxConfig) {
        AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_HOST, config.host)
            .putString(KEY_TOKEN, config.token)
            .putString(KEY_SIDE, config.side)
            .putBoolean(KEY_PREDICT, config.predict)
            .putFloat(KEY_FONT, config.fontSp)
            .putBoolean(KEY_COMPACT, config.compact)
            .putString(KEY_DEVICE_ID, config.deviceId)
            .putString(KEY_DEVICE_LABEL, config.deviceLabel)
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
    active: Boolean,
    modifier: Modifier,
): TerminalHandle {
    val controller = remember(config, sessionId) { TerminalController(config, sessionId, active) }
    DisposableEffect(controller) { onDispose { controller.dispose() } }

    // Entering or leaving the foreground starts or drops the stream. The last
    // painted frame stays, so swiping back is instant rather than blank.
    LaunchedEffect(active) { controller.setActive(active) }

    val frame by controller.frame.collectAsState()
    val density = LocalDensity.current
    // Font size is a client setting, not a scaling factor: a bigger font means
    // fewer cells fit, and the daemon reflows the pane to that grid. Nothing is
    // ever shrunk to fit a fixed grid (ADR-0008), which is also what keeps the
    // touch mapping honest — there is no scale to divide by.
    var fontSp by remember { mutableStateOf(config.fontSp) }
    // A keyless `remember` never re-initialises, so a page opened before the
    // setting changed kept the old size and the Settings buttons looked broken.
    // The surface is the one composable that outlives a settings change.
    LaunchedEffect(config.fontSp) { fontSp = config.fontSp }
    val fontSizePx = with(density) { fontSp.sp.toPx() }
    val painter = remember(fontSizePx) { TerminalPainter(fontSizePx) }
    val scope = rememberCoroutineScope()

    val context = LocalContext.current

    // The shell drives the phone's clipboard, not the other way round. Android's
    // own clipboard is used rather than Compose's wrapper because a *clear* is a
    // real operation here (clearPrimaryClip) and the wrapper cannot express it,
    // only an empty string — which is a different thing on every platform that
    // distinguishes the two, and is what a program asking to clear the clipboard
    // actually meant.
    //
    // Assigned in an effect keyed on the controller rather than on every
    // recomposition, so a frame arriving mid-recomposition cannot find a
    // half-updated callback.
    val androidClipboard = remember(context) {
        context.getSystemService(android.content.ClipboardManager::class.java)
    }
    LaunchedEffect(controller, androidClipboard) {
        controller.onClipboardRequest = { request ->
            when (request) {
                ClipboardRequest.Clear -> androidClipboard?.clearPrimaryClip()
                is ClipboardRequest.Set -> androidClipboard?.setPrimaryClip(
                    android.content.ClipData.newPlainText("ldmux", request.text),
                )
            }
        }
    }
    val focusManager = LocalFocusManager.current
    val focusRequester = remember { FocusRequester() }
    // The surface's own focus target, which is what landscape types into. See the
    // LaunchedEffect below for why a terminal with no text field still needs it.
    val surfaceFocus = remember { FocusRequester() }
    var viewport by remember { mutableStateOf(IntSize.Zero) }
    /** Where the user last tapped, so the snap is visible before the server answers. */
    var tapAt by remember { mutableStateOf<Pair<Int, Int>?>(null) }
    /** A pinch is in flight; a tap must not be read out of it. */
    var pinching by remember { mutableStateOf(false) }
    /** The cursor blink phase. Its own canvas, so a blink repaints one cell. */
    var blinkOn by remember { mutableStateOf(true) }

    // The gesture detectors must read the *current* frame and metrics without
    // being restarted by them (restarting would cancel an in-flight gesture).
    val currentFrame by rememberUpdatedState(frame)
    val currentViewport by rememberUpdatedState(viewport)
    val currentPainter by rememberUpdatedState(painter)
    LaunchedEffect(tapAt) {
        if (tapAt != null) {
            delay(600)
            tapAt = null
        }
    }

    val metrics = painter.metrics
    // The render transform and the touch hit-test are the same geometry, so it
    // is computed once, here, and both read it (term/GridPlacement.kt).
    val placement = remember(frame?.cols, frame?.rows, metrics.width, metrics.height, viewport) {
        placeGrid(
            cellWidth = metrics.width,
            cellHeight = metrics.height,
            cols = frame?.cols ?: 0,
            rows = frame?.rows ?: 0,
            viewportWidth = viewport.width.toFloat(),
            viewportHeight = viewport.height.toFloat(),
        )
    }
    val currentPlacement by rememberUpdatedState(placement)

    // The blink follows the pane's idle time: any new cursor position restarts
    // the cycle with the cursor *shown*, so a cursor never looks "off" at the
    // moment the user is typing at it.
    val liveCursor = frame?.let { it.cursorVisible && it.viewportActive } == true
    val blinkKey = frame?.let { "${it.cursorX}:${it.cursorY}:${it.cursorVisible}:${it.viewportActive}" }
    LaunchedEffect(blinkKey) {
        blinkOn = true
        if (!liveCursor) return@LaunchedEffect
        while (true) {
            delay(BLINK_MS)
            blinkOn = !blinkOn
        }
    }
    // The pane follows the viewport, always. In portrait the keyboard is part of
    // that viewport (it is always up there), so the grid simply fits the space
    // that is actually visible; nothing is scaled down to a postage stamp.
    // The short delay debounces the two or three intermediate sizes a rotation
    // and an IME animation produce, so the daemon is told once, not thrice.
    // The first layout is the moment a frame that arrived early becomes
    // drawable. Without this the surface can sit permanently blank: the snapshot
    // was applied while the canvas had no size, and because the grid it computes
    // turns out to be the grid the pane already had, `setGrid` sends nothing and
    // no fresh snapshot follows.
    LaunchedEffect(viewport) {
        if (viewport.width > 0 && viewport.height > 0) controller.repaint()
    }

    LaunchedEffect(viewport, metrics.width, metrics.height) {
        if (viewport.width <= 0 || viewport.height <= 0) return@LaunchedEffect
        if (metrics.width <= 0f || metrics.height <= 0f) return@LaunchedEffect
        delay(250)
        val cols = floor(viewport.width / metrics.width).toInt().coerceAtLeast(20)
        val rows = floor(viewport.height / metrics.height).toInt().coerceAtLeast(5)
        controller.setGrid(cols, rows)
    }

    // Dictation is not this composable's business any more. It is one microphone for
    // the app, driven from the side panel, because this composable exists per *page*
    // and the pager keeps a neighbour composed for an instant swipe — so stopping
    // dictation on dispose stopped the dictation belonging to whichever page was
    // being swiped away from, and two composed pages could each draw the banner.

    // The orientation itself is not set here any more: `App` owns it, because
    // this composable exists per *page* while the screen has one orientation, and
    // the version that lived here was a second opinion that would have fought the
    // launcher's. What remains is the keyboard, which is this surface's business.
    LaunchedEffect(typing) {
        if (typing) {
            focusRequester.requestFocus()
        } else {
            // Landscape has no text field, and it still has to hold focus.
            // `onPreviewKeyEvent` is delivered from the root down to the focused
            // node, so with nothing focused the box that maps key codes to
            // terminal keys is never asked at all and every hardware key is
            // dropped silently. Clearing focus is how the keyboard is hidden; it
            // must not also be how the keyboard is muted.
            focusManager.clearFocus()
            surfaceFocus.requestFocus()
        }
    }

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
        // Characters. The table above maps terminal *keys*, and everything else
        // used to fall on the floor: in landscape there is no text field, so a
        // letter arrives as a key event and nothing ever turned it into input.
        // Named keys worked and letters did not, which is exactly what the phone
        // showed.
        //
        // Skipped while the soft keyboard is up, because there the text field owns
        // text: handling it here too would send every character a second time.
        // Skipped under Ctrl or Alt, which are keys in their own right — a ctrl
        // chord on a hardware keyboard must not become its letter.
        val native = event.nativeKeyEvent
        if (!typing && !native.isCtrlPressed && !native.isAltPressed && !native.isMetaPressed) {
            val cp = native.unicodeChar
            if (cp >= 0x20 && cp != 0x7F) {
                controller.sendText(cp.toChar().toString())
                return@onPreviewKeyEvent true
            }
        }
        if (key == null) return@onPreviewKeyEvent false
        controller.sendKey(key)
        true
    }.focusRequester(surfaceFocus).focusable()) {
        Canvas(
            Modifier
                .fillMaxSize()
                .onSizeChanged { viewport = it }
                .pointerInput(Unit) {
                    // A tap places the cursor. The hit-test is the exact inverse of
                    // the draw below — both read the same GridPlacement, which is
                    // the fix for the recorded failure mode (pointer coordinates
                    // that forgot to divide by a scale factor).
                    // Where the finger went down, so a wobble that never reached
                    // the drag threshold is not mistaken for a tap.
                    var downAt: Offset? = null
                    detectTapGestures(
                        onPress = { downAt = it },
                        onTap = { pos ->
                            if (pinching) return@detectTapGestures
                            // A finger that wandered is a scroll attempt, not a
                            // tap. `detectTapGestures` fires on ANY unconsumed
                            // up, however far the finger travelled, so without
                            // this a wobble shorter than the drag threshold would
                            // move the remote cursor — the same class of bug as a
                            // scroll that clicks.
                            val down = downAt
                            if (down != null && (pos - down).getDistance() > TAP_SLOP_PX) {
                                return@detectTapGestures
                            }
                            currentFrame ?: return@detectTapGestures
                            val p = currentPlacement
                            // cellAt answers with a row-major index, so the column
                            // comes back out of the same arithmetic that put it in.
                            val index = p.cellAt(pos.x, pos.y) ?: return@detectTapGestures
                            val row = index / p.cols
                            val col = index % p.cols
                            tapAt = col to row
                            controller.tapCell(col, row)
                        },
                    )
                }
                .pointerInput(Unit) {
                    // Selection. Long-press then drag is the gesture, on purpose:
                    //
                    //   * a long press with no movement is "this word", which is
                    //     the standard gesture on both platforms and keeps this
                    //     out of cross-platform seams (ADR-0013, ADR-0014 §6);
                    //   * the drag that follows extends it, so nothing has to
                    //     decide between "select" and "scroll" up front;
                    //   * release copies, which is the behaviour that needs no
                    //     button. The `copy` control stays for copying again.
                    //
                    // It cannot fight the tap handler above: a press that becomes
                    // a long press is not a tap, so only one of the two ever fires.
                    detectDragGesturesAfterLongPress(
                        onDragStart = { pos ->
                            val p = currentPlacement
                            val index = p.cellAt(pos.x, pos.y) ?: return@detectDragGesturesAfterLongPress
                            // The engine needs the cell size to reason about
                            // distance, and it is cheapest to keep it fresh here
                            // rather than to invalidate it from elsewhere.
                            controller.selectionGeometry(
                                p.cols,
                                p.cellWidth.toInt(),
                                p.cellHeight.toInt(),
                                p.dx.toInt(),
                                viewport.height,
                            )
                            val row = index / p.cols
                            val col = index % p.cols
                            tapAt = col to row
                            controller.selectWordAt(col, row, SystemClock.elapsedRealtime())
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            val p = currentPlacement
                            val index = p.cellAt(change.position.x, change.position.y)
                            if (index != null) {
                                controller.extendSelection(
                                    index % p.cols,
                                    index / p.cols,
                                    SystemClock.elapsedRealtime(),
                                )
                            }
                        },
                        onDragEnd = {
                            // Copy on release. The selection is already what the
                            // user can see, so this is the one moment where the
                            // text and the highlight provably agree.
                            val text = controller.selectionText()
                            if (!text.isNullOrEmpty()) {
                                // The same clipboard the shell writes to via OSC 52,
                                // deliberately: one slot, one owner, so "what did I
                                // just copy" has one answer.
                                androidClipboard?.setPrimaryClip(
                                    android.content.ClipData.newPlainText("ldmux", text),
                                )
                            }
                        },
                        onDragCancel = { controller.clearSelection() },
                    )
                }
                .pointerInput(Unit) {
                    // Two fingers change the font size, which changes how many
                    // cells fit, which resizes the shell. Narrow on purpose: it
                    // never consumes a single-finger event, so tap and scroll
                    // are untouched by it.
                    detectPinch(
                        onZoom = { factor ->
                            if (!pinching) pinching = true
                            fontSp = (fontSp * factor)
                                .coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP)
                        },
                        onEnd = {
                            // Persist through a fresh load, so a stale config
                            // capture cannot write back an old host or token.
                            val persisted = AppPrefs.load()
                            if (persisted.fontSp != fontSp) AppPrefs.save(persisted.copy(fontSp = fontSp))
                            // A pinch ends with one finger still down, and that
                            // finger's release must not then be read as a tap.
                            scope.launch {
                                delay(150)
                                pinching = false
                            }
                        },
                    )
                }
                .pointerInput(Unit) {
                    var carry = 0f
                    detectVerticalDragGestures(
                        onVerticalDrag = { change, dragAmount ->
                            change.consume()
                            val rowPx = currentPainter.metrics.height
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
                        // The remainder deliberately survives the end of a drag.
                        // Resetting it here threw away the sub-cell part of every
                        // gesture, so a short drag — under one cell, which is
                        // easy to do — consumed the gesture, produced no rows and
                        // then also cancelled the tap, i.e. nothing happened at
                        // all. Kept, a 20 px drag followed by another still adds
                        // up to a line.
                        onDragCancel = { carry = 0f },
                    )
                },
        ) {
            val f = frame ?: return@Canvas
            if (placement.cellWidth <= 0f || placement.gridWidth <= 0f) return@Canvas
            // 1:1 with the grid's top-left at the placement's origin. The grid is
            // sized to the viewport, so what is left over is under one cell, and
            // an oversized grid (a font change whose resize has not landed) is
            // clipped rather than squashed — the cells keep the size the user
            // chose.
            withTransform({ translate(placement.dx, placement.dy) }) {
                painter.paint(this, f)
            }
            drawScrollbar(f, placement, size.width, size.height)
            tapAt?.let { (col, row) ->
                val (x, y) = placement.cellOrigin(col, row)
                drawRect(
                    color = Accent,
                    topLeft = Offset(x, y),
                    size = androidx.compose.ui.geometry.Size(placement.cellWidth, placement.cellHeight),
                    style = androidx.compose.ui.graphics.drawscope.Stroke(width = 1.5f),
                )
            }
        }

        // The cursor gets its own one-cell canvas. Blinking it is the one thing
        // that must keep happening while nothing else changes, and repainting a
        // 5000-cell grid two or three times a second forever is a permanent tax
        // on a phone's battery for a picture nobody asked for.

        // The cursor, in its own one-cell canvas. Blinking is the one thing that
        // must keep happening while nothing else changes, and repainting a
        // whole grid two or three times a second forever is a permanent tax on a
        // phone's battery for a picture nobody asked for.
        val cursorFrame = frame
        if (cursorFrame != null && liveCursor && blinkOn && placement.gridWidth > 0f) {
            val (cx, cy) = placement.cellOrigin(cursorFrame.cursorX, cursorFrame.cursorY)
            Canvas(
                Modifier
                    .offset { IntOffset(cx.roundToInt(), cy.roundToInt()) }
                    .size(
                        with(density) { placement.cellWidth.toDp() },
                        with(density) { placement.cellHeight.toDp() },
                    ),
            ) {
                painter.paintCursor(this, cursorFrame)
            }
        }

        // Typing is opt-in, and the field is deliberately tiny: it only has to
        // be focusable for the IME, and covering the grid with it would eat the
        // scroll gesture.
        //
        // The field is the IME's own buffer and is treated as one: it is left
        // holding what the IME says it holds, and the difference between two values
        // is what gets sent. See `InputDelta.kt` for why — the short version is that
        // resetting it to a sentinel after every change lost half of an injected
        // burst, measured, because the write replaced the buffer the IME was midway
        // through producing into. Keyed on the session, because a different shell is
        // a different line, and cleared here rather than inside the callback, which
        // is the whole point.
        if (typing) {
            var field by remember(sessionId) { mutableStateOf("") }
            BasicTextField(
                value = field,
                onValueChange = { next ->
                    for (op in inputOpsFor(field, next)) {
                        when (op) {
                            is InputOp.Keys -> for (name in op.names) controller.sendKey(name)
                            is InputOp.Text -> controller.sendText(op.value)
                        }
                    }
                    // What the IME already reported, and not one character more.
                    field = next
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

    }
    return controller
}

/**
 * Two-finger pinch to font size.
 *
 * Written by hand rather than with `detectTransformGestures` for one reason:
 * that detector also claims single-finger "pan", which on this surface would
 * fight the scroll gesture and swallow taps. This one never consumes a
 * single-finger event at all, so it is invisible to everything else.
 *
 * The distance ratio between the two pointers *is* the zoom; it is compared
 * against [SLOP] because two fingers resting still still jitter, and a stream of
 * 1.001x steps would resize the shell continuously for no reason.
 */
private suspend fun PointerInputScope.detectPinch(
    slop: Float = PINCH_SLOP,
    onZoom: (Float) -> Unit,
    onEnd: () -> Unit,
) {
    awaitEachGesture {
        var previous = 0f
        var zoomed = false
        do {
            val event = awaitPointerEvent()
            val held = event.changes.filter { it.pressed }.take(2)
            if (held.size == 2) {
                val distance = (held[0].position - held[1].position).getDistance()
                if (previous > 0f && distance > 0f) {
                    val factor = distance / previous
                    if (factor < 1f - slop || factor > 1f + slop) {
                        zoomed = true
                        onZoom(factor)
                        held.forEach { it.consume() }
                    }
                }
                previous = distance
            } else {
                // Back to one finger: the next two-finger distance is a fresh
                // baseline, not a jump from wherever the last pinch left off.
                previous = 0f
            }
        } while (event.changes.any { it.pressed })
        if (zoomed) onEnd()
    }
}

private const val PINCH_SLOP = 0.03f

/**
 * How far a finger may wander and still count as a tap, in px.
 *
 * Slightly tighter than the system's touch slop on purpose. `detectTapGestures`
 * fires `onTap` on any unconsumed up, however far the finger travelled, so the
 * window between "too small to be a drag" and "big enough to be a tap" is where
 * a sloppy scroll would move the remote cursor — the same class of bug as a
 * scroll that clicks. Anything in that window now does nothing instead, which is
 * the safe failure.
 */
private const val TAP_SLOP_PX = 20f

/**
 * The cursor blink half-period, in ms. 530 is the classic rate (the VT220's),
 * and it is the one users recognise without noticing it.
 */
private const val BLINK_MS = 530L

/**
 * A thin scrollbar while the view is parked in history. The emulator polls its
 * own scrollbar geometry per frame (there is deliberately no change event), and
 * the frame carries it, so nothing extra is requested to draw this.
 */
private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawScrollbar(
    f: TerminalFrame.Frame,
    placement: GridPlacement,
    width: Float,
    height: Float,
) {
    if (f.viewportActive || f.scrollTotal <= 0 || f.scrollLen <= 0) return
    val frac = f.scrollLen.toFloat() / f.scrollTotal.toFloat()
    val barH = (height * frac).coerceAtLeast(24f)
    val maxOffset = (f.scrollTotal - f.scrollLen).coerceAtLeast(0).toFloat()
    val pos = if (maxOffset <= 0f) 0f else (f.scrollOffset.toFloat() / maxOffset).coerceIn(0f, 1f)
    val top = (height - barH) * pos
    val x = placement.dx + placement.gridWidth - 6f
    drawRect(
        color = Color(0x66FFFFFF),
        topLeft = Offset(x, top),
        size = androidx.compose.ui.geometry.Size(4f, barH),
    )
}
