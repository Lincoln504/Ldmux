package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * Scroll fidelity (SPEC §7.4) at a reconnect: the decision that used to be "rebuild
 * the engine, always, unless it is a resize".
 *
 * A rebuilt engine has no scrollback and sits at its bottom, so a user reading history
 * who lost the link for a moment came back to the live prompt with the history gone.
 * Only explicit user action may move the view. These pin the decision, because the
 * engine itself cannot run on the JVM.
 */
class SnapshotPlanTest {
    private fun plan(
        current: dev.ldmux.mobile.term.TerminalFrame.Frame?,
        cols: Int = 80,
        rows: Int = 24,
        alt: Boolean = false,
        history: Boolean = false,
    ) = planSnapshot(current, cols, rows, alt, history)

    @Test
    fun `the first snapshot rebuilds`() {
        assertEquals(SnapshotApply.Rebuild, plan(null))
    }

    @Test
    fun `a reconnect while following live output rebuilds for a clean parser state`() {
        assertEquals(SnapshotApply.Rebuild, plan(testFrame(viewportActive = true)))
    }

    @Test
    fun `a reconnect while the user is reading history replays in place`() {
        // The scenario of the audit: viewport anchored, link blinks, same grid.
        assertEquals(SnapshotApply.ReplayInPlace, plan(testFrame(viewportActive = false)))
    }

    @Test
    fun `a resync or mispredict recovery while anchored keeps the anchor too`() {
        // They arrive as the same message as a reconnect; the plan cannot tell them
        // apart and must not need to.
        assertEquals(SnapshotApply.ReplayInPlace, plan(testFrame(viewportActive = false), history = false))
    }

    @Test
    fun `a history page-in reply rebuilds even when anchored`() {
        // It replaces the scrollback with a deeper one, and the user asked for it by
        // reaching the top.
        assertEquals(SnapshotApply.Rebuild, plan(testFrame(viewportActive = false), history = true))
    }

    @Test
    fun `a history page-in that also changed the grid still rebuilds`() {
        // The bug this pins: the grid check used to run first, so a page-in whose
        // reply arrived after the pane was resized (another client, a rotation) was
        // applied in place — which re-establishes only the visible screen, so the
        // history rows above it were dropped and the user was left at the top of the
        // same shallow scrollback, unable to ask again.
        assertEquals(
            SnapshotApply.Rebuild,
            plan(testFrame(viewportActive = false), cols = 100, history = true),
        )
        assertEquals(
            SnapshotApply.Rebuild,
            plan(testFrame(viewportActive = true), rows = 30, history = true),
        )
    }

    @Test
    fun `a resize replays in place whether or not the view is anchored`() {
        assertEquals(SnapshotApply.ReplayInPlace, plan(testFrame(viewportActive = true), cols = 100))
        assertEquals(SnapshotApply.ReplayInPlace, plan(testFrame(viewportActive = false), rows = 30))
    }

    @Test
    fun `a change of screen always rebuilds, because 1049 would switch screens in a live engine`() {
        assertEquals(SnapshotApply.Rebuild, plan(testFrame(viewportActive = false, alternateOn = false), alt = true))
        assertEquals(SnapshotApply.Rebuild, plan(testFrame(viewportActive = false, alternateOn = true), alt = false))
        // ...even when the grid changed at the same time.
        assertEquals(SnapshotApply.Rebuild, plan(testFrame(alternateOn = false), cols = 100, alt = true))
    }

    @Test
    fun `the plan agrees with replayResize on every resize it takes`() {
        // The older predicate stays as the definition of "a resize the engine takes in
        // place"; the plan must be a superset of it, never contradict it.
        for (cols in listOf(80, 100)) for (rows in listOf(24, 30)) for (alt in listOf(false, true)) {
            val f = testFrame(alternateOn = false)
            if (replayResize(f, cols, rows, alt)) {
                assertEquals(SnapshotApply.ReplayInPlace, plan(f, cols, rows, alt))
            }
        }
    }
}
