package io.ldmux.mobile


import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import android.content.Context
import android.view.KeyEvent
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
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
import androidx.compose.runtime.RememberObserver
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.Alignment
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
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
import io.ldmux.mobile.term.handleHit
import io.ldmux.mobile.term.selectionHandleAnchors
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.toArgb
import io.ldmux.mobile.term.SystemSelectionHandles
import io.ldmux.mobile.term.selectionHandleTopLeft
import io.ldmux.mobile.term.GridPlacement
import io.ldmux.mobile.term.TerminalFrame
import io.ldmux.mobile.term.TerminalPainter
import io.ldmux.mobile.term.placeGrid
import io.ldmux.mobile.term.rowAfterScroll
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.floor
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Fallback surface colour until the engine has told us its own background.
 *
 * The theme's shell colour rather than black: a light theme whose terminal had no frame yet opened
 * on a black rectangle, which is the flicker this constant exists to prevent.
 */
private val TermBg: Color get() = TerminalColors.background

/**
 * The native terminal surface: a Compose Canvas painting the libghostty-vt cell
 * grid.
 *
 * The grid is *not* scaled to fit a fixed pane any more. The surface measures
 * its own viewport, works out how many cells fit, and tells the daemon — which
 * resizes the session's pty to match. The shell is therefore always exactly the size
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
    keyboardRequest: Int,
): TerminalHandle {
    // Rebuilt only when the connection changes. It was keyed on the whole config, so
    // saving a font size reconnected every open page, and the first attach of a
    // controller takes its shell over (ADR-0010): a settings change took every shell
    // back from the desk.
    //
    // Held by a [ControllerHolder], which connects it when the composition keeps it and
    // disposes it when the composition forgets *or abandons* it. This used to be a plain
    // `remember` that built a controller whose constructor opened the socket, torn down
    // by a `DisposableEffect`: an effect only exists for a composition that is applied,
    // so a composition that was abandoned (an interrupted pager measure, a rotation
    // mid-compose) leaked a connected controller that held the shell with no owner.
    val holder = remember(config.connectionKey, sessionId) { ControllerHolder(config, sessionId, active) }
    val controller = holder.controller
    TerminalSurfaceFor(controller, sessionId, config, typing, active, modifier, keyboardRequest)
    return controller
}

/**
 * The surface itself, over any [TerminalSource]. The daemon-attached route and the mesh route
 * both draw through here, so gestures, selection, the cursor, scrolling and the IME path are
 * the same code for both and cannot drift apart again.
 */
@Composable
actual fun TerminalSurfaceFor(
    source: TerminalSource,
    key: String,
    config: LdmuxConfig,
    typing: Boolean,
    active: Boolean,
    modifier: Modifier,
    keyboardRequest: Int,
    scrollGestures: Boolean,
) {
    val controller = source
    val sessionId = key
    // Entering or leaving the foreground starts or drops the stream. The last
    // painted frame stays, so swiping back is instant rather than blank. Keyed on
    // the controller as well as the flag: a page that adopts a new controller while
    // it stays active (a fresh connection) must start *that* stream, and
    // `setActive(true)` on the old one would leave the new one unopened.
    LaunchedEffect(controller, active) { controller.setActive(active) }

    val frame by controller.frame.collectAsState()
    val density = LocalDensity.current
    // Font size is a client setting, not a scaling factor: a bigger font means
    // fewer cells fit, and the daemon reflows the pane to that grid. Nothing is
    // ever shrunk to fit a fixed grid (ADR-0008), which is also what keeps the
    // touch mapping honest — there is no scale to divide by.
    // The zoom is this shell's, and only while it is open: it starts at the
    // saved default, follows the pinch, and lives in the composition — never
    // written back to the saved settings, and never copied into another shell.
    // It used to be saved on every pinch, which made it one global setting:
    // zooming one shell re-sized every other open one on the next
    // recomposition, and the size outlived the shell it was chosen for.
    var fontSp by remember { mutableStateOf(config.fontSp) }
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
        // What the program may ask for is decided in common code
        // (`admitClipboardRequest`: capped, cleaned); this only carries it out, and a
        // refusal by the system (a transaction too large, a locked-down profile) is
        // caught in the controller instead of reaching the receive loop.
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
    // How far a finger may be from a handle's ball and still have grabbed it. The
    // app's one minimum touch size, so a handle is as reachable as a button is.
    val minTouchPx = with(density) { LocalTouch.current.min.toPx() }
    var grabOffset by remember { mutableStateOf(Offset.Zero) }
    // The platform's own selection handles, resolved from the theme once. Their shape
    // is the system's; their colour is this app's (see SystemSelectionHandles).
    // The framework's own design height; the width follows the asset's aspect ratio.
    val handleH = with(density) { 24.dp.roundToPx() }
    val systemHandles = remember(context, handleH) {
        SystemSelectionHandles.load(context, Fg.toArgb(), handleH)
    }
    // The platform handles' own size, or a sensible box while the drawables resolve.
    val handlesPx = systemHandles?.sizePx ?: IntSize(96, 48)
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
    /**
     * The momentum coroutine started when a scroll drag is released with speed.
     * Held outside the gesture detector (which is recreated only per `Unit` key,
     * but the job itself must be reachable from the "back to now" pill and from
     * the next drag's start) so either can cancel it — a fling that is still
     * pushing the viewport up the scrollback when the user grabs the screen
     * again, or taps back to live, must stop rather than fight the new input.
     */
    var flingJob by remember { mutableStateOf<Job?>(null) }

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
    //
    // `cursorKnown` rather than `viewportActive`: what the draw needs is that the
    // coordinates are real (byte 40), which the emulator reports separately from
    // whether the viewport is following live output. A cursor we were never given
    // a position for must not be painted at (0,0).
    val liveCursor = frame?.let { it.cursorVisible && it.cursorKnown } == true
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
    LaunchedEffect(controller, viewport) {
        if (viewport.width > 0 && viewport.height > 0) controller.repaint()
    }

    LaunchedEffect(controller, viewport, metrics.width, metrics.height) {
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
    // being swiped away from, and two composed pages could each draw the notice.

    // The orientation itself is not set here any more: `App` owns it, because
    // this composable exists per *page* while the screen has one orientation, and
    // the version that lived here was a second opinion that would have fought the
    // launcher's. What remains is the keyboard, which is this surface's business.
    val softKeyboard = LocalSoftwareKeyboardController.current
    // Whether the IME's own field currently has focus. Used only to decide whether the
    // show request below needs repeating; the platform can refuse a show issued while the
    // window is still settling.
    var fieldFocused by remember { mutableStateOf(false) }
    // **The keyboard is asked for after the frame, and asked again if it did not take.**
    //
    // A cold start in portrait composed the field and requested focus in the same frame the
    // window was still being attached. The focus arrived, the IME did not: `typing` was already
    // true and never changed, so the effect that asks for the keyboard never ran again and the
    // keypad did not appear until the user toggled it twice. Asking on a frame boundary and
    // repeating the request after a beat is what makes "portrait means typing" true on the
    // first try, on both routes.
    LaunchedEffect(typing, active, keyboardRequest) {
        if (typing && active) {
            withFrameNanos { }
            focusRequester.requestFocus()
            softKeyboard?.show()
            delay(IME_SETTLE_MS)
            // Two more asks, a settle apart, and only while the field has not taken focus. A
            // focused field with no keyboard does happen (the show is refused while the window
            // is still settling), so one retry is not always enough; stopping at "focused"
            // also means the retries end as soon as the platform accepts, rather than fighting
            // a keyboard the user then closes.
            repeat(2) {
                if (fieldFocused) return@LaunchedEffect
                focusRequester.requestFocus()
                softKeyboard?.show()
                delay(IME_SETTLE_MS)
            }
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

    // **The shell's ground is the theme's, and it changes with the theme.** A frame's own default
    // background is the *host's* terminal colour (black on the daemon's default config), so painting
    // it verbatim put a black shell under a white page; the painter maps default cells to the
    // theme, and this maps the surface the grid is drawn on to the same value.
    val termBg = TerminalColors.background

    Box(modifier.background(termBg).onPreviewKeyEvent { event ->
        // A hardware keyboard (and some IMEs' "send key" buttons) produce key
        // events rather than committed text. Anything that is a *terminal key*
        // must bypass the text field entirely, so it can never be mangled by the
        // IME — and Backspace here is the reliable path that the sentinel above
        // is the fallback for.
        if (event.type != KeyEventType.KeyDown) return@onPreviewKeyEvent false
        val named = when (event.key) {
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
            Key.Insert -> "IC"
            Key.F1 -> "F1"; Key.F2 -> "F2"; Key.F3 -> "F3"; Key.F4 -> "F4"
            Key.F5 -> "F5"; Key.F6 -> "F6"; Key.F7 -> "F7"; Key.F8 -> "F8"
            Key.F9 -> "F9"; Key.F10 -> "F10"; Key.F11 -> "F11"; Key.F12 -> "F12"
            else -> null
        }
        // Everything that *decides* — AltGr versus an Alt chord, modifiers on a named
        // key, whether the soft keyboard owns the text — is `mapHardwareKey`, in common
        // code. This only describes the event to it.
        val native = event.nativeKeyEvent
        val meta = native.metaState
        val ctrlBits = KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON or KeyEvent.META_CTRL_RIGHT_ON
        val altBits = KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON or KeyEvent.META_ALT_RIGHT_ON
        val action = mapHardwareKey(
            HardwareKey(
                named = named,
                shift = native.isShiftPressed,
                ctrl = native.isCtrlPressed,
                alt = native.isAltPressed,
                rightAlt = meta and KeyEvent.META_ALT_RIGHT_ON != 0,
                meta = native.isMetaPressed,
                charWithAlt = native.getUnicodeChar(meta and ctrlBits.inv()),
                charWithoutAlt = native.getUnicodeChar(meta and (ctrlBits or altBits).inv()),
                softKeyboardUp = typing,
            ),
            shiftKeys = controller.shiftKeys,
        )
        when (action) {
            HardwareAction.Pass -> false
            is HardwareAction.Type -> {
                controller.sendText(action.text)
                true
            }
            is HardwareAction.Press -> {
                controller.sendKey(action.name)
                true
            }
        }
    }.focusRequester(surfaceFocus).focusable()) {
        Canvas(
            Modifier
                .fillMaxSize()
                .onSizeChanged {
                    appLog("surface $sessionId viewport ${it.width}x${it.height}")
                    viewport = it
                }
                .pointerInput(controller) {
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
                            // **A tap in a selection ends the selection.**
                            //
                            // Before anything else, and before the cursor moves: the
                            // user's thumb is on the highlight they just made and a
                            // tap is how they let go of it. Moving the remote cursor
                            // here would be a command they did not give, in a shell
                            // they were not editing. The one exception is a tap on a
                            // handle, which is how a handle is grabbed, so that must
                            // not be read as letting go.
                            val liveBounds = currentFrame?.selection
                            if (liveBounds != null) {
                                val onHandle = handleHit(pos, liveBounds, p, handlesPx, minTouchPx)
                                if (onHandle == null) {
                                    controller.clearSelection()
                                    return@detectTapGestures
                                }
                            }
                            // cellAt answers with a row-major index, so the column
                            // comes back out of the same arithmetic that put it in.
                            val index = p.cellAt(pos.x, pos.y) ?: return@detectTapGestures
                            val row = index / p.cols
                            val col = index % p.cols
                            // The outline marks a tap that did something. Set
                            // unconditionally it flashed over taps the policy
                            // refused — a password prompt, history, a program with
                            // no cursor position — which reads as a dropped tap.
                            tapAt = if (controller.tapCell(col, row)) col to row else null
                        },
                    )
                }
                .pointerInput(controller) {
                    // **Selection: a long press chooses, a drag extends, a handle
                    // moves its own end.**
                    //
                    // One detector for all three, because a finger does one thing at a
                    // time. A long press that lands on a handle grabs it; anywhere
                    // else it chooses the word under it. The drag that follows always
                    // ends up as two coordinates — never as "the engine's drag", whose
                    // semantics are whatever the gesture decided — which is what lets
                    // a selection grow past the word it started on.
                    var fixedStart = false
                    var fixedCol = 0
                    var fixedRow = 0

                    detectDragGesturesAfterLongPress(
                        onDragStart = { pos ->
                            val p = currentPlacement
                            // The engine needs the cell size to reason about distance,
                            // and it is cheapest to keep it fresh here.
                            controller.selectionGeometry(
                                p.cols,
                                p.cellWidth.toInt(),
                                p.cellHeight.toInt(),
                                p.dx.toInt(),
                                viewport.height,
                            )
                            // Read from the frame that is *drawn*, not asked of the
                            // engine again: the handles the user can see are the ones
                            // the frame describes, and a fresh query could disagree
                            // with them by a frame.
                            val bounds = currentFrame?.selection
                            val onHandle = bounds?.let {
                                handleHit(pos, it, p, handlesPx, minTouchPx)
                            }
                            if (bounds != null && onHandle != null) {
                                // Grabbing an end. Remember where inside the handle the
                                // finger landed, so the end does not jump to the finger
                                // the moment the drag starts.
                                if (onHandle.start) {
                                    fixedCol = bounds.endCol
                                    fixedRow = bounds.endRow
                                    fixedStart = false
                                } else {
                                    fixedCol = bounds.startCol
                                    fixedRow = bounds.startRow
                                    fixedStart = true
                                }
                                grabOffset = onHandle.offset
                                tapAt = null
                            } else {
                                // Picking an end. The finger chooses a cell and the
                                // drag grows a range from it, which is the mechanic the
                                // user asked for: hold, then slide, and the two ends
                                // separate as you go.
                                //
                                // Not the engine's word gesture, and that is a
                                // measured decision rather than a preference: its deep
                                // press produces a word in the host tests and produces
                                // nothing at all on the phone (`bounds=null
                                // frameSel=null text=null` after one), and a gesture
                                // that silently does nothing is worse than one that does
                                // exactly what it says. Everything here goes through
                                // `selectionSet`, which is proven on the device.
                                val index = p.cellAt(pos.x, pos.y)
                                    ?: return@detectDragGesturesAfterLongPress
                                val row = index / p.cols
                                val col = index % p.cols
                                tapAt = col to row
                                controller.selectionSet(col, row, col, row)
                                fixedStart = true
                                fixedCol = col
                                fixedRow = row
                            }
                        },
                        onDrag = { change, _ ->
                            change.consume()
                            val p = currentPlacement
                            val at = change.position + grabOffset
                            // A finger past an edge means two things at once, and the
                            // grid settles it: the viewport follows by one row, and
                            // the dragged end goes to the cell on the edge it passed.
                            // Both used to be conditional on the finger being inside
                            // the grid, so a drag into the scrollback stopped
                            // updating the highlight entirely.
                            val step = p.scrollFor(at.y)
                            if (step != 0) {
                                // Shift the anchor by what the viewport ACTUALLY did,
                                // not by what it was asked. On the alternate screen
                                // there is no scrollback, so the engine declines and
                                // the answer is zero — while `step` is not, and the
                                // anchor used to walk a row per drag event away from
                                // its text while the content stood still. In vim, in
                                // htop, in `less`. That is the "skipped on the
                                // alternate screen" rule, expressed as a measurement
                                // instead of a flag, so it cannot disagree with the
                                // viewport it is describing.
                                val moved = controller.scrollBy(step, pageInHistory = false)
                                // The end that is not being dragged is anchored to
                                // *text*, and scrolling up moves the text down, so its
                                // viewport row rises. This added the delta instead of
                                // subtracting it, which slid the anchor by two rows
                                // for every row scrolled.
                                if (moved != 0) fixedRow = rowAfterScroll(fixedRow, moved)
                            }
                            val index = p.dragCell(at.x, at.y)
                            if (index != null) {
                                val col = index % p.cols
                                val row = index / p.cols
                                if (fixedStart) {
                                    controller.selectionSet(fixedCol, fixedRow, col, row)
                                } else {
                                    controller.selectionSet(col, row, fixedCol, fixedRow)
                                }
                            }
                        },
                        onDragEnd = {
                            // Copy on release. The selection is what the user can see,
                            // so this is the one moment where the text and the
                            // highlight provably agree.
                            val text = controller.selectionText()
                            if (!text.isNullOrEmpty()) copyToClipboard(androidClipboard, text)
                            grabOffset = Offset.Zero
                        },
                        onDragCancel = {
                            controller.clearSelection()
                            grabOffset = Offset.Zero
                        },
                    )
                }
                .pointerInput(controller) {
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
                            // Deliberately not saved here: the zoom belongs to this
                            // shell for as long as it is open, and saving it made
                            // it a setting every other shell inherited.
                            // A pinch ends with one finger still down, and that
                            // finger's release must not then be read as a tap.
                            scope.launch {
                                delay(150)
                                pinching = false
                            }
                        },
                    )
                }
                .pointerInput(controller, scrollGestures) {
                    if (!scrollGestures) return@pointerInput
                    var carry = 0f
                    // Tracks the raw pointer motion of the drag in flight so that,
                    // if the finger is still moving when it lifts, the release
                    // speed can carry the scroll on rather than stopping it dead —
                    // the actual complaint this fixes.
                    val tracker = VelocityTracker()
                    detectVerticalDragGestures(
                        onDragStart = {
                            // A fresh drag overrides whatever momentum was still
                            // running: the finger is back on the glass, and it — not
                            // last gesture's decaying velocity — owns the scroll now.
                            flingJob?.cancel()
                            flingJob = null
                            tracker.resetTracking()
                        },
                        onVerticalDrag = { change, dragAmount ->
                            change.consume()
                            tracker.addPosition(change.uptimeMillis, change.position)
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
                        onDragEnd = {
                            val rowPx = currentPainter.metrics.height
                            // Positive is the finger (and so the content) moving
                            // down, matching `dragAmount`'s sign above — same
                            // reason the fling below can reuse `-rows` unchanged.
                            val releaseVelocity = tracker.calculateVelocity().y
                            if (rowPx <= 0f || abs(releaseVelocity) < FLING_MIN_VELOCITY_PX_S) return@detectVerticalDragGestures
                            flingJob = scope.launch {
                                var velocity = releaseVelocity
                                var lastFrameNanos = -1L
                                // `isActive` is how a cancel (new drag, or jump-to-
                                // bottom) stops this without a flag threaded through
                                // three call sites.
                                while (isActive && abs(velocity) >= FLING_MIN_VELOCITY_PX_S) {
                                    val nowNanos = withFrameNanos { it }
                                    if (lastFrameNanos < 0L) {
                                        lastFrameNanos = nowNanos
                                        continue
                                    }
                                    // Clamped, and the first frame is not the only
                                    // one that can be far away: frames stop arriving
                                    // when the screen sleeps or the app is
                                    // backgrounded, so a fling in flight across that
                                    // gap came back to one enormous delta — which is
                                    // one `scrollBy` to the end of the scrollback,
                                    // not a fling at all. The cap is what a 30 fps
                                    // frame would be, so ordinary slow frames are
                                    // unaffected and a suspended app cannot jump.
                                    val dtSeconds = min(
                                        (nowNanos - lastFrameNanos) / 1_000_000_000f,
                                        FLING_MAX_FRAME_SECONDS,
                                    )
                                    lastFrameNanos = nowNanos
                                    // Same carry the drag used: the fling is that
                                    // same finger motion, continued past the release,
                                    // so it inherits whatever sub-row remainder the
                                    // drag left rather than starting a new one.
                                    carry += velocity * dtSeconds
                                    val rows = (carry / rowPx).toInt()
                                    if (rows != 0) {
                                        carry -= rows * rowPx
                                        // Self-clamping: 0 back means the scrollback
                                        // end (or the alternate screen) refused the
                                        // move, which is the honest place to stop —
                                        // decaying to zero against a wall a screen
                                        // away just wastes frames.
                                        if (controller.scrollBy(-rows) == 0) break
                                    }
                                    // Exponential decay, framerate-independent: a
                                    // dropped frame (a slow device) still lands the
                                    // same *speed* at the same *time*, not the same
                                    // speed after fewer, bigger steps.
                                    velocity *= exp(-FLING_FRICTION_PER_SEC * dtSeconds)
                                }
                                flingJob = null
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
                }
                // **Handles are dragged immediately, with no hold first.**
                //
                // A handle is a grab affordance: the finger lands on it and moves. It
                // required a long press before, because one detector was serving both
                // "choose a range" and "adjust one", and the practical result was
                // handles that could not be dragged at all.
                //
                // This is innermost in the chain on purpose. Compose delivers the
                // pointer *main* pass from the inside out, so being last means seeing
                // the event first and being able to consume it before the tap and
                // scroll gestures below — and it consumes *only* when the touch begins
                // on a handle, so a finger that misses one falls straight through and
                // behaves exactly as it did before.
                .pointerInput(controller) {
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        val bounds = currentFrame?.selection ?: return@awaitEachGesture
                        val p = currentPlacement
                        val grab = handleHit(down.position, bounds, p, handlesPx, minTouchPx)
                            ?: return@awaitEachGesture
                        down.consume()
                        // The end that is not being dragged stays where it is, and it is
                        // taken from the frame the user is looking at rather than
                        // remembered from an earlier gesture.
                        val fixedStart = !grab.start
                        var fixedCol = if (grab.start) bounds.endCol else bounds.startCol
                        var fixedRow = if (grab.start) bounds.endRow else bounds.startRow
                        while (true) {
                            val event = awaitPointerEvent()
                            val change = event.changes.firstOrNull { it.id == down.id } ?: break
                            change.consume()
                            if (!change.pressed) break
                            // The finger is on the handle's ball, and the cell the user
                            // means is under the handle's *tip*: the grab offset carries
                            // the difference, so the end does not jump on the first move.
                            val at = change.position + grab.offset
                            // Dragging a *handle* past an edge now reaches into the
                            // scrollback exactly as dragging the selection does. It did
                            // not, and that was the asymmetry: the body gesture could
                            // extend a selection into history and the handles could
                            // not, so the same intent worked or did nothing depending
                            // on whether the finger had landed on a handle. One rule,
                            // one helper, two callers.
                            val step = p.scrollFor(at.y)
                            if (step != 0) {
                                // The same rule as the body gesture above: shift by
                                // what the viewport did, so a refused scroll — the
                                // alternate screen, or an end of the history — moves
                                // the anchor by nothing at all.
                                val moved = controller.scrollBy(step, pageInHistory = false)
                                if (moved != 0) fixedRow = rowAfterScroll(fixedRow, moved)
                            }
                            val index = p.dragCell(at.x, at.y)
                            if (index != null) {
                                val col = index % p.cols
                                val row = index / p.cols
                                if (fixedStart) {
                                    controller.selectionSet(fixedCol, fixedRow, col, row)
                                } else {
                                    controller.selectionSet(col, row, fixedCol, fixedRow)
                                }
                            }
                        }
                        // Letting go copies, exactly as letting go of a selection does.
                        val text = controller.selectionText()
                        if (!text.isNullOrEmpty()) {
                            copyToClipboard(androidClipboard, text)
                        }
                    }
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
            // **The two handles.** Drawn by us on both platforms (ADR-0014), because
            // a Compose canvas owns every pixel and the platform's own handles come
            // with a TextView this app does not have. Positioned from the geometry
            // the hit test uses, so what is drawn and what is grabbable are one
            // thing. They sit *below* the range's corners and splay outward, which is
            // what makes it obvious which end is which without a label.
            f.selection?.let { bounds ->
                systemHandles?.let { handles ->
                    val (startAnchor, endAnchor) = selectionHandleAnchors(bounds, placement)
                    drawSelectionHandle(
                        handles.start,
                        startAnchor,
                        handles.sizePx,
                        start = true,
                        mirror = handles.mirrorStart,
                    )
                    drawSelectionHandle(
                        handles.end,
                        endAnchor,
                        handles.sizePx,
                        start = false,
                        mirror = false,
                    )
                }
            }
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
                    .focusRequester(focusRequester)
                    .onFocusChanged { fieldFocused = it.isFocused },
                cursorBrush = SolidColor(Color.Transparent),
                keyboardOptions = KeyboardOptions(
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                ),
            )
        }

    }
}

/**
 * Owns a [TerminalController] for exactly as long as the composition *keeps* it.
 *
 * [onRemembered] is the first moment the composition has actually adopted the
 * controller, so that is when it connects; [onForgotten] and [onAbandoned] both end
 * it, and `dispose` is idempotent. Constructing the controller allocates (an engine
 * and a scope) but opens nothing.
 */
private class ControllerHolder(config: LdmuxConfig, sessionId: String, active: Boolean) : RememberObserver {
    val controller = TerminalController(config, sessionId, active)

    override fun onRemembered() = controller.begin()

    override fun onForgotten() = controller.dispose()

    override fun onAbandoned() = controller.dispose()
}

/** A copy the user asked for; a system that refuses it must not take the gesture down. */
private fun copyToClipboard(clipboard: android.content.ClipboardManager?, text: String) {
    if (clipboard == null) return
    try {
        clipboard.setPrimaryClip(android.content.ClipData.newPlainText("ldmux", text))
    } catch (e: Exception) {
        appLog("copy failed: ${e::class.simpleName}")
    }
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
        // **The end runs even when the gesture is cancelled.** `onEnd` clears the
        // `pinching` flag that disables taps, and it used to be called only on the
        // normal path. A `pointerInput` restart (a controller change) cancels the
        // gesture mid-pinch, and the flag then stayed set for the life of the page:
        // every tap on that shell was swallowed as "probably a pinch". A `finally`
        // makes the flag's lifetime the gesture's, whatever ends it.
        try {
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
        } finally {
            if (zoomed) onEnd()
        }
    }
}

private const val PINCH_SLOP = 0.03f

/**
 * Draw one of the platform's handles at its anchor.
 *
 * The anchor is the handle's *tip* — the corner of the selected cell — and the
 * framework's hotspots put the tip at three quarters of the left handle's width and a
 * quarter of the right handle's, so each teardrop leans inwards at the text it belongs
 * to. That convention lives in `selectionHandleTopLeft`, which the hit test uses too,
 * so what is drawn and what is grabbable cannot drift apart.
 */
private fun DrawScope.drawSelectionHandle(
    image: ImageBitmap,
    anchor: Offset,
    size: IntSize,
    start: Boolean,
    mirror: Boolean,
) {
    val topLeft = selectionHandleTopLeft(anchor, size, start)
    if (!mirror) {
        drawImage(image = image, topLeft = topLeft)
        return
    }
    // A theme with one teardrop instead of a left/right pair: the start handle is the
    // same art flipped, which is exactly what the framework's own Editor does. The flip
    // is about the *handle's box*, not the anchor, so the hotspot stays where the hit
    // test expects it.
    withTransform({
        scale(-1f, 1f, pivot = Offset(topLeft.x + size.width / 2f, topLeft.y + size.height / 2f))
    }) {
        drawImage(image = image, topLeft = topLeft)
    }
}

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
 * How long the IME gets to act on a show request before it is asked again.
 *
 * One frame is enough on a warm window, but a cold start can drop the first ask, and a
 * keyboard that silently does not appear is indistinguishable from an app that cannot type.
 */
private const val IME_SETTLE_MS = 250L

/**
 * Below this release speed (px/s), a drag's momentum is not worth animating —
 * a slow lift is a deliberate stop, not a fling, and animating it anyway would
 * read as the scroll refusing to settle.
 */
private const val FLING_MIN_VELOCITY_PX_S = 50f

/**
 * Per-second exponential decay rate for the fling. Chosen so a typical fast
 * flick (≈4000–6000 px/s off `VelocityTracker`, measured against Android's
 * own fling defaults) decays below [FLING_MIN_VELOCITY_PX_S] in a little under a
 * second — present, but not a scroll that hunts for the bottom of the shell for
 * three seconds after one flick.
 */
private const val FLING_FRICTION_PER_SEC = 4.5f

/**
 * The longest frame the fling will believe in, in seconds.
 *
 * `withFrameNanos` stops firing when nothing is being drawn — the screen sleeps, or
 * the app goes to the background — and resumes with one delta covering the whole
 * gap. Multiplied by a live release velocity that is a scroll of thousands of rows:
 * the viewport lands at the end of the scrollback from a flick that was meant to
 * move a screen. Nothing here is measured in frames, so the fix is a bound rather
 * than a count: a third of a second is slower than any frame a device draws and far
 * shorter than any suspension. */
private const val FLING_MAX_FRAME_SECONDS = 0.033f

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
    // **A control boundary, and the floor for one is 3:1.** This was `Dim` at 45 %, which composites
    // to 2.15:1 on the light themes and 2.7-2.8:1 on the dark ones; `TerminalScrollbar` is the
    // theme's foreground at half — 3.2:1 on the light themes, 3.9-4.8:1 on the dark ones — and
    // `ThemesTest` holds every theme to it.
    drawRect(
        color = TerminalScrollbar,
        topLeft = Offset(x, top),
        size = androidx.compose.ui.geometry.Size(4f, barH),
    )
}
