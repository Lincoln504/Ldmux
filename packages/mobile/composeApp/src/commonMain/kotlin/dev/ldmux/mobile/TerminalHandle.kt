package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import dev.ldmux.mobile.term.TerminalFrame
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
    /**
     * The link is down.
     *
     * Not the same state as "no measurement yet": while the socket is gone the user
     * can still type, and every keystroke is dropped without a trace. The line this
     * feeds is the only thing on a terminal page that says anything at all about the
     * link, so it has to survive the link failing rather than vanish with it — a
     * disappearing diagnostic reads as the app being slow, not the link being absent.
     */
    val offline: Boolean = false,
) {
    val fps: Int get() = (1000L / frameIntervalMs.coerceAtLeast(1L)).toInt()
}

/**
 * What a program running in the shell asked the *system* clipboard to do.
 *
 * This is the shell driving the phone, not the phone driving the shell. A program
 * copies with OSC 52 (or OSC 1337, or Kitty's OSC 5522) and the value belongs in
 * the clipboard the user's other apps read. Until this existed the sequence
 * parsed, was discarded in silence, and the program was told nothing had gone
 * wrong — libghostty-vt's own header names the unset callback as the reason
 * ("Set to NULL to ignore clipboard writes").
 */
sealed interface ClipboardRequest {
    /** The program asked for the clipboard to be emptied. */
    data object Clear : ClipboardRequest

    /** The program copied [text]. */
    data class Set(val text: String) : ClipboardRequest
}

/**
 * A handle to one terminal surface. The shared chrome drives the terminal through
 * this, so controls live in the app's own chrome rather than inside the surface.
 */
interface TerminalHandle {
    fun sendKey(name: String)
    fun sendText(text: String)

    /**
     * Paste [text] the way the *program* expects to receive a paste.
     *
     * Not the same thing as [sendText]. A program that turned on bracketed paste
     * (mode 2004) has been promised the text will arrive wrapped in the markers,
     * and that promise is the whole point of the mode: unwrapped, every newline
     * in the paste is a keypress, so a paragraph pasted into a shell runs its
     * first line before the user has seen the rest. The default here is the plain
     * path, so an implementation that cannot know the program's mode is at least
     * never wrong about the bytes it is asked to send.
     *
     * Returns whether the text went anywhere. `false` means it was **refused** — the
     * link is down, or another device holds the shell — and the caller must not act as
     * if it had been pasted: a paste into nothing used to vanish without a trace.
     */
    fun paste(text: String): Boolean {
        sendText(text)
        return true
    }

    /**
     * Selection. Defaults make every one of these a no-op, so a handle that
     * cannot select (a platform without the engine, a test double) still composes
     * and still taps to move the cursor.
     */
    val hasSelection: Boolean get() = false

    fun selectionGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {}

    /**
     * Set the selection to an explicit inclusive range of viewport cells, for the
     * handles.
     */
    fun selectionSet(startCol: Int, startRow: Int, endCol: Int, endRow: Int): Boolean = false

    /** Where the selection is, or null when there is none. */
    fun selectionBounds(): IntArray? = null

    fun selectionText(): String? = null

    fun clearSelection() {}

    fun selectAll() {}

    /**
     * Send one key with the sticky modifiers applied (0x03 for Ctrl-C, ESC+x for
     * Alt-x). Separate from [sendKey] because the *bytes* differ, not just the
     * name: a modifier on a character is a control code, and the app has to be
     * the one that works it out.
     */
    fun sendModifiedKey(key: String, modifiers: Modifiers)

    /**
     * The sticky modifiers this session has armed, if any.
     *
     * The session owns the latch because only it sees every path that sends a
     * command — a bar key, a hardware key, a typed character, and dictation all
     * arrive here, where they used not to. The chrome reads this back to draw its
     * Ctrl and Alt keys as latched.
     */
    val modifiers: Modifiers get() = Modifiers()

    /**
     * Arm or clear one sticky modifier. It applies to the next command and is spent
     * by it, so there is no way to stack two of them.
     */
    fun toggleModifier(which: StickyKey) {}

    /** Forget any armed modifier. Used when the user moves to a different shell. */
    fun clearModifiers() {}

    /**
     * Scroll the emulator's viewport by [rows] (negative is up), and answer with the
     * rows it *actually* moved.
     *
     * Zero means the engine refused: either end of the history, or the alternate
     * screen, which keeps no scrollback to scroll. It answers rather than just
     * acting because a caller dragging a selection has to move its anchored end by
     * what happened rather than by what it asked for, and a `Unit` return left it no
     * way to tell those two apart.
     *
     * [pageInHistory] is false when the caller is a live selection gesture. A
     * page-in replaces the engine, so asking for one mid-drag throws away the
     * selection the drag is extending — ADR-0014 §4.4 requires the request to be
     * suppressed while a gesture is live, and the caller is the only thing that
     * knows. It is a per-call answer rather than a flag the gesture has to
     * remember to clear, so a gesture that is cancelled cannot leave paging off
     * for the rest of the session.
     */
    fun scrollBy(rows: Int, pageInHistory: Boolean = true): Int = 0

    /**
     * Park the viewport on the live bottom, whatever history it was reading.
     *
     * The bottom is where a shell happens — the prompt, and the output the user
     * has not seen. A handle that cannot scroll (a test double) does nothing,
     * like the scroll default above.
     */
    fun scrollToBottom() {}

    /**
     * The user tapped a cell. Whether that moves the shell's cursor, clicks for a
     * mouse-aware program, or does nothing, is decided by the pane's state.
     *
     * Returns whether it did anything. The surface draws its tap outline on a yes
     * and nothing on a no, because the outline is feedback for an action: drawn
     * unconditionally, a tap the policy refused — a password prompt, history, a
     * program that hides its cursor and so reports no position — flashed a box over
     * the cell and moved nothing, which reads as the app having missed the tap.
     */
    fun tapCell(col: Int, row: Int): Boolean = false

    /**
     * Another device holds this shell, named when known (ADR-0010).
     *
     * A state rather than a one-shot event, because the screen it produces is
     * *the page*: a refusal that only took effect if something happened to be
     * listening at the right moment would leave a blank terminal and no reason.
     */
    val ownedBy: StateFlow<String?>
        get() = EMPTY_OWNED

    /**
     * The shell is gone (ended, or unknown to the daemon), so the page is over.
     *
     * A state rather than an event, for the same reason as [ownedBy]: a page that
     * only reacted if somebody happened to be listening would sit there showing a
     * frozen frame with nothing to read.
     */
    val ended: StateFlow<Boolean>
        get() = EMPTY_ENDED

    /**
     * The daemon refused this device's token: the pairing is stale (rotated) and
     * retrying cannot fix it. A state, for the same reason as [ownedBy].
     */
    val rejected: StateFlow<Boolean>
        get() = EMPTY_ENDED

    /**
     * The last keystroke the pane refused, or null.
     *
     * The daemon answers `input_failed` when the write into a pane fails: the shell died under
     * the key, or its pty is gone. The tap was accepted here and then lost, and nothing else on
     * screen would say so — so it is said, briefly, and cleared. A state rather than an event so
     * a notice that arrives while the page is being composed is still drawn.
     */
    val inputRefused: StateFlow<String?>
        get() = EMPTY_NOTICE

    /** The user confirmed: take the shell from whoever has it. */
    fun takeOver() {}

    /**
     * Repaint from what the engine already holds.
     *
     * For the one case that would otherwise be invisible: a snapshot that arrived
     * before this surface had been laid out, so it was rendered into nothing and
     * never re-rendered — because nothing about the pane changed afterwards, so
     * no new snapshot was ever sent.
     */
    fun repaint() {}

    /**
     * How the link is behaving, so the chrome can show it. The frame cadence is
     * derived from the round trip, and a number the user can see is the only way
     * to tell "this link is slow" from "this app is slow".
     */
    val link: StateFlow<LinkStats>
        get() = EMPTY_LINK

    companion object {
        /** For surfaces that do not report link state (tests). */
        val EMPTY_LINK: StateFlow<LinkStats> = MutableStateFlow(LinkStats(0, FramePacing.MIN_INTERVAL_MS, false))
        /** For surfaces that do not report ownership (tests). */
        val EMPTY_OWNED: StateFlow<String?> = MutableStateFlow(null)
        /** For surfaces whose shell cannot end (tests). */
        val EMPTY_ENDED: StateFlow<Boolean> = MutableStateFlow(false)
        val EMPTY_NOTICE: StateFlow<String?> = MutableStateFlow(null)
    }
}

/**
 * What the terminal *surface* needs from whatever feeds it, beyond what the chrome drives
 * ([TerminalHandle]): a frame to paint, a grid to report, a clipboard to hand a program's copy to.
 *
 * The surface (gestures, painter, selection, IME, scrollbar) is one composable and must stay one:
 * a second route that drew its own rows is how the mesh terminal became a different app. The
 * daemon-attached [TerminalController] and the mesh route's source both implement this, so the
 * only thing that differs between routes is where the bytes come from.
 */
interface TerminalSource : TerminalHandle {
    /** The painted screen, or null before there is one. */
    val frame: StateFlow<TerminalFrame.Frame?>

    /** Whether the host understands `S-` on named keys (the hardware-key mapper asks). */
    val shiftKeys: Boolean

    /** Where a program's clipboard write goes, if anyone can take it. */
    var onClipboardRequest: ((ClipboardRequest) -> Unit)?

    /** Start or stop streaming; a source with no stream ignores it. */
    fun setActive(value: Boolean)

    /** The grid the surface can paint. A source whose grid is fixed by its host ignores it. */
    fun setGrid(cols: Int, rows: Int)
}

/**
 * Whether a page should be connected: it is the one on screen, and the app is.
 *
 * The second half was missing and it was visible from the other end: backgrounding
 * the app left the page attached for the whole [BackgroundSession.releaseGraceMs]
 * grace, so a desk window went on saying "This shell is open on another device.
 * Press r to take it back" — with no page in front of anyone, and nothing but an
 * app in the background using the shell (2026-10-05).
 *
 * Nothing is lost by letting go: the shell is safe in the keeper on the host, the
 * page keeps its last frame, and coming back to the app re-attaches — which takes
 * the shell over, because arriving *is* the consent (ADR-0010).
 */
internal fun pageIsConnected(shown: Boolean, appOnScreen: Boolean): Boolean =
    shown && appOnScreen

/**
 * Create (or reuse) the native terminal surface for one session.
 *
 * [typing] is the app-wide typing mode: the surface focuses its input
 * connection (raising the keyboard) and the platform flips to portrait while it
 * is set.
 *
 * [active] is whether this page is the one on screen **and the app is on screen**:
 * the pager keeps a neighbour composed so a swipe is instant, but a composed page
 * must not stay *connected*. That would stream output for a terminal nobody is
 * looking at and, because the client owns the pane geometry, resize the panes of
 * sessions the user is not on — and it would hold the claim, which is the same
 * cost to somebody else's desk window: it sits on "This shell is open on another
 * device. Press r to take it back" while nothing is using the shell.
 *
 * [pageIsConnected] is the rule. The second half of it was missing, and it was
 * visible from the other end of the wire (2026-10-05).
 *
 * [keyboardRequest] changes each time the user asks for the keyboard. The page on
 * screen answers by taking focus and showing it, because a keyboard the system
 * dismissed (the back gesture) also took focus with it, and "show" on nothing
 * focused shows nothing.
 */
@Composable
expect fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    typing: Boolean,
    active: Boolean,
    modifier: Modifier,
    keyboardRequest: Int = 0,
): TerminalHandle

/**
 * The terminal surface for an existing [source]: the one composable both routes draw.
 *
 * [rememberTerminalSurface] builds the daemon-attached source and calls this; the mesh route
 * calls it with its own. [key] identifies the line being typed on, so a different shell starts a
 * fresh input buffer.
 */
@Composable
expect fun TerminalSurfaceFor(
    source: TerminalSource,
    key: String,
    config: LdmuxConfig,
    typing: Boolean,
    active: Boolean,
    modifier: Modifier,
    keyboardRequest: Int = 0,
)
