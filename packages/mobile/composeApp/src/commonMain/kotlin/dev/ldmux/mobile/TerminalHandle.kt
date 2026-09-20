package dev.ldmux.mobile

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
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
) {
    val fps: Int get() = (1000L / frameIntervalMs.coerceAtLeast(1L)).toInt()
}

/**
 * A handle to one terminal surface. The native sidebar drives the terminal
 * through this, so controls live in native chrome rather than inside a WebView.
 *//**
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
     */
    fun paste(text: String) = sendText(text)

    /**
     * Selection. Defaults make every one of these a no-op, so a handle that
     * cannot select (a platform without the engine, a test double) still composes
     * and still taps to move the cursor.
     */
    val hasSelection: Boolean get() = false

    fun selectionGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {}

    fun beginSelection(col: Int, row: Int, atMs: Long) {}

    fun selectWordAt(col: Int, row: Int, atMs: Long) {}

    fun extendSelection(col: Int, row: Int, atMs: Long) {}

    fun endSelection(col: Int, row: Int, valid: Boolean, atMs: Long): String? = null

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
     */
    fun scrollBy(rows: Int): Int = 0

    /**
     * The user tapped a cell. Whether that moves the shell's cursor, clicks for a
     * mouse-aware program, or does nothing, is decided by the pane's state.
     */
    fun tapCell(col: Int, row: Int) {}

    /**
     * Enter or leave tmux copy-mode.
     *
     * The only way to scroll a full-screen program: its output is on the
     * alternate screen, which has no scrollback, so what you scroll is tmux's own
     * view of the pane.
     */
    fun setCopyMode(on: Boolean) {}

    /**
     * Another device holds this shell, named when known (ADR-0010).
     *
     * A state rather than a one-shot event, because the screen it produces is
     * *the page*: a refusal that only took effect if something happened to be
     * listening at the right moment would leave a blank terminal and no reason.
     */
    val ownedBy: StateFlow<String?>
        get() = EMPTY_OWNED

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

    /** Whether the pane is in a tmux mode right now, as last reported. */
    val inCopyMode: Boolean get() = false

    /**
     * How the link is behaving, so the chrome can show it. The frame cadence is
     * derived from the round trip, and a number the user can see is the only way
     * to tell "this link is slow" from "this app is slow".
     */
    val link: StateFlow<LinkStats>
        get() = EMPTY_LINK

    companion object {
        /** For surfaces that do not report link state (the web view, tests). */
        val EMPTY_LINK: StateFlow<LinkStats> = MutableStateFlow(LinkStats(0, FramePacing.MIN_INTERVAL_MS, false))
        /** For surfaces that do not report ownership (the web view, tests). */
        val EMPTY_OWNED: StateFlow<String?> = MutableStateFlow(null)
    }
}

/**
 * Create (or reuse) the native terminal surface for one session.
 *
 * [typing] is the app-wide typing mode: the surface focuses its input
 * connection (raising the keyboard) and the platform flips to portrait while it
 * is set.
 *
 * [active] is whether this page is the one on screen. The pager keeps a
 * neighbour composed so a swipe is instant, but a composed page must not stay
 * *connected*: that would stream output for a terminal nobody is looking at and,
 * because the client owns the pane geometry, resize the panes of sessions the
 * user is not on.
 */
@Composable
expect fun rememberTerminalSurface(
    config: LdmuxConfig,
    sessionId: String,
    typing: Boolean,
    active: Boolean,
    modifier: Modifier,
): TerminalHandle