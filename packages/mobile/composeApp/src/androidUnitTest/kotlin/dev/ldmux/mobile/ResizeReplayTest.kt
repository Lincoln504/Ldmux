package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The in-place resize, and the one property that makes it safe.
 *
 * A rotation answers with a snapshot whose grid is not the one the engine holds.
 * Rebuilding the engine for it replayed the visible screen into a fresh terminal
 * and everything above it — the client's whole accumulated scrollback — was gone,
 * which is why a rotation used to empty the history and park the view on a screen
 * with nothing under it. So a resize is replayed *into* the live engine instead.
 *
 * The host suite proves the engine honors that byte sequence, but it types its own
 * copy of it, so it cannot constrain this builder: a regression that made
 * [resizeReplayBytes] emit `?1049` would pass every test there, because on a *fresh*
 * engine `?1049` establishes the screen and the replay still lands. On a *live*
 * engine it **switches** screens instead, orphaning the scrollback this path exists
 * to keep. That is the property pinned here, along with the decision to take the
 * path at all — which must refuse whenever it cannot be sure it is looking at a
 * bare resize.
 */
class ResizeReplayTest {

    private fun frame(
        cols: Int = 80,
        rows: Int = 24,
        alternateOn: Boolean = false,
    ) = TerminalFrame.Frame(
        cols = cols,
        rows = rows,
        cursorX = 0,
        cursorY = 0,
        cursorVisible = true,
        cursorStyle = 1,
        defaultFg = 0,
        defaultBg = 0,
        passwordInput = false,
        viewportActive = true,
        mouseTracking = false,
        mouseSgr = false,
        alternateOn = alternateOn,
        selection = null,
        scrollTotal = 0,
        scrollOffset = 0,
        scrollLen = 0,
        cells = emptyList(),
    )

    @Test
    fun `the first snapshot is never a resize`() {
        // There is no frame and no engine yet: nothing to preserve, and the replay
        // would write into an engine that has not established its screen.
        assertFalse(replayResize(null, cols = 80, rows = 24, alternateOn = false))
    }

    @Test
    fun `the same grid is a resync, not a resize`() {
        // A snapshot whose size the engine already has is asking for a clean parser
        // state — a mispredict's recovery, an explicit resync — and only a fresh
        // engine delivers that. Replaying in place would leave the wrong state up.
        val f = frame(cols = 80, rows = 24)
        assertFalse(replayResize(f, cols = 80, rows = 24, alternateOn = false))
    }

    @Test
    fun `a changed grid is a resize`() {
        val f = frame(cols = 80, rows = 24)
        assertTrue(replayResize(f, cols = 40, rows = 24, alternateOn = false))
        assertTrue(replayResize(f, cols = 80, rows = 12, alternateOn = false))
    }

    @Test
    fun `a change of screen falls back to the rebuild`() {
        // The screen is the one thing the replay cannot state for itself: on a live
        // engine `?1049` switches rather than establishes (see the class doc), so if
        // the program moved between the primary and alternate screens since the last
        // frame, the replay would land on the wrong one. Refuse, and let the rebuild
        // — which does send the switch, on a fresh engine where it means "establish"
        // — handle it.
        val primary = frame(cols = 80, rows = 24, alternateOn = false)
        val alternate = frame(cols = 80, rows = 24, alternateOn = true)
        assertFalse(replayResize(primary, cols = 40, rows = 24, alternateOn = true))
        assertFalse(replayResize(alternate, cols = 40, rows = 24, alternateOn = false))
    }

    @Test
    fun `the replay never switches screens`() {
        // The whole reason this builder exists separately from `snapshotBytes`.
        val bytes = resizeReplayBytes(
            screen = "hello\r\n",
            cursorX = 2,
            cursorY = 1,
            cols = 80,
            rows = 24,
            modes = TerminalModes(mouse = true),
        ).decodeToString()
        assertFalse(bytes.contains("\u001b[?1049"), "the replay would switch screens and orphan the scrollback")
    }

    @Test
    fun `the replay clears the visible screen and nothing else`() {
        // `2J` is the visible screen (the scrollback is what this path protects), and
        // the state that would make the text land in the wrong cells is reset before
        // it: scroll region, origin mode, insert mode. The modes come first, as they
        // do in `snapshotBytes` — state, then content.
        val bytes = resizeReplayBytes(
            screen = "hello",
            cursorX = 2,
            cursorY = 1,
            cols = 80,
            rows = 24,
        ).decodeToString()
        val reset = "\u001b[r\u001b[?6l\u001b[4l\u001b[H\u001b[2J"
        assertTrue(bytes.contains(reset), "the reset prefix is missing ($bytes)")
        assertTrue(bytes.indexOf(reset) < bytes.indexOf("hello"), "the screen is written before the reset")
        assertTrue(bytes.indexOf("hello") < bytes.indexOf("\u001b[2;3H"), "the cursor is placed before the text")
        assertTrue(bytes.endsWith("\u001b[2;3H"), "the cursor is not placed last ($bytes)")
    }

    @Test
    fun `an empty capture is still a reset and a cursor`() {
        // A blank pane is a real state, and skipping it would leave the previous
        // screen up.
        val bytes = resizeReplayBytes(screen = "", cursorX = 0, cursorY = 0, cols = 80, rows = 24)
            .decodeToString()
        assertTrue(bytes.contains("\u001b[2J"))
        assertTrue(bytes.endsWith("\u001b[1;1H"))
    }

    @Test
    fun `the modes are re-asserted, and off is stated as plainly as on`() {
        // A live engine already holds the modes, so the `h`/`l` forms here are
        // idempotent — but they must still be sent, or a mode the program cleared
        // since the last frame would survive the replay.
        val on = resizeReplayBytes(
            screen = "",
            cursorX = 0,
            cursorY = 0,
            cols = 80,
            rows = 24,
            modes = TerminalModes(mouse = true, mouseSgr = true, appCursor = true, bracketedPaste = true),
        ).decodeToString()
        assertTrue(on.contains("\u001b[?1000h"))
        assertTrue(on.contains("\u001b[?1006h"))
        assertTrue(on.contains("\u001b[?1h"))
        assertTrue(on.contains("\u001b[?2004h"))
        // Before the state reset, like the screen switch in `snapshotBytes`: state
        // first, then content.
        assertTrue(on.indexOf("\u001b[?1000h") < on.indexOf("\u001b[H\u001b[2J"), "modes are set after the reset")

        val off = resizeReplayBytes(screen = "", cursorX = 0, cursorY = 0, cols = 80, rows = 24)
            .decodeToString()
        assertTrue(off.contains("\u001b[?1000l"))
        assertTrue(off.contains("\u001b[?1006l"))
    }

    @Test
    fun `the cursor is clamped to the grid`() {
        assertEquals(
            "\u001b[24;80H",
            cursorSequence(col = 999, row = 999, cols = 80, rows = 24),
        )
        val bytes = resizeReplayBytes(screen = "", cursorX = 999, cursorY = 999, cols = 80, rows = 24)
            .decodeToString()
        assertTrue(bytes.endsWith("\u001b[24;80H"), bytes)
    }
}
