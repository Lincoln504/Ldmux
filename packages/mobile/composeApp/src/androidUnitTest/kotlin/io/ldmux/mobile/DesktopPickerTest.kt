package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * Which desktops the "send to desktop" dialog offers.
 *
 * The bug this pins: the list was workspaces that already had a window, so the one
 * place a person actually wants to put a window — wherever they are looking, which
 * on a fresh workspace is empty — was not offered, and the dialog read as doing
 * nothing.
 */
class DesktopPickerTest {
    private fun ws(
        name: String,
        focused: Boolean = false,
        visible: Boolean = false,
        count: Int = 0,
    ) = DesktopWorkspace(
        name = name,
        index = name.toIntOrNull(),
        focused = focused,
        visible = visible,
        windowCount = count,
    )

    private fun win(workspace: String) =
        DesktopWindow(id = "0x1", appClass = "x", title = "", workspace = workspace, pid = null)

    @Test
    fun `a focused empty workspace is offered`() {
        val out = choosableWorkspaces(listOf(ws("1"), ws("2", focused = true)), emptyList())
        assertEquals(listOf("2"), out.map { it.name })
    }

    @Test
    fun `occupied workspaces are offered even when nothing is focused`() {
        val out = choosableWorkspaces(
            listOf(ws("1", count = 2), ws("2"), ws("3", count = 1)),
            listOf(win("1"), win("1"), win("3")),
        )
        assertEquals(listOf("1", "3"), out.map { it.name })
    }

    @Test
    fun `an invisible, empty, unfocused workspace is not offered`() {
        val out = choosableWorkspaces(
            listOf(ws("1"), ws("2"), ws("3", count = 1)),
            listOf(win("3")),
        )
        assertEquals(listOf("3"), out.map { it.name })
    }

    @Test
    fun `a visible workspace on another output is offered`() {
        val out = choosableWorkspaces(listOf(ws("1"), ws("2", visible = true)), emptyList())
        assertEquals(listOf("2"), out.map { it.name })
    }
}
