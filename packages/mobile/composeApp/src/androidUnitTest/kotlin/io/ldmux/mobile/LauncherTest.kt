package io.ldmux.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * What the launcher offers, and how it reads the daemon's answer.
 *
 * The two rules tested here are the ones that would be silently wrong otherwise:
 * a pi session that already has a shell must not be offered twice, and a JSON
 * field the client does not understand must not take the screen down.
 */
class LauncherTest {

    private fun shell(id: String, cwd: String, title: String = "") =
        Session(id = id, kind = "shell", title = title, cwd = cwd, status = "running", keeperName = "ldmux-$id")

    @Test
    fun `desk pi processes are not rows, only shells that can be opened are`() {
        // A pi outside ldmux cannot be opened, and a row for it used to start a new
        // phone shell instead (2026-09-20). A pi inside a desk window is that window's shell
        // row, and it is named by its title (the folder, when nothing has named it) — not by the
        // program in the pane.
        val rows = launcherEntries(
            listOf(shell("abc", "/home/u/proj").copy(process = "pi", title = "proj")),
        )
        assertEquals(listOf("abc"), rows.map { it.sessionId })
        assertEquals("proj", rows[0].label)
    }

    @Test
    fun `an open shell is named by its title and located by its folder`() {
        // The daemon's title is the conversation's name with the folder as its fallback, so the
        // row and `ldmux list` say the same word. The program in the pane is not the name: every
        // agent-run shell would read `pi`.
        val rows = launcherEntries(
            listOf(shell("abc", "/home/u/proj").copy(process = "vim", title = "Radio paths")),
        )
        assertEquals("Radio paths", rows[0].label)
        assertEquals("/home/u/proj", rows[0].cwd)
        assertEquals("abc", rows[0].sessionId)
    }

    @Test
    fun `a shell with no title still has a name`() {
        // No title and no folder: the daemon falls back to the id, and an empty one here still
        // reaches the row's last resort rather than a blank line.
        val rows = launcherEntries(listOf(shell("abc", "/x").copy(title = "")))
        assertEquals("", rows[0].label)
        assertEquals("x", shellRowTitle(rows[0].label, rows[0].cwd))
    }

    @Test
    fun `only a shell that lost its window says so`() {
        // A desk window is the ordinary state and the row itself says it, so the words are
        // blank. A phone shell says it with the icon. A shell that never had a window is
        // ordinary too: `ldmux new` makes one and it is waiting to be attached to.
        //
        // Only a shell whose window **went away** is news, and that is the one case the
        // daemon has to name (`hadWindow`). Before it did, every headless shell was drawn in
        // amber as a fault — three of them were on the phone, created from the CLI, with
        // nothing wrong (2026-11-05).
        val rows = launcherEntries(
            listOf(
                shell("desk", "/x").copy(liveWorkspace = "2", hadWindow = true),
                shell("unplaced", "/x").copy(liveWorkspace = "", hadWindow = true),
                shell("phone", "/x").copy(createdBy = "phone"),
                shell("headless", "/x"),
                shell("lost", "/x").copy(hadWindow = true),
            ),
        )
        assertEquals(listOf("", "", "", "", "no window"), rows.map { it.state })
    }

    @Test
    fun `the row's second line is always the folder, then the state`() {
        assertEquals("~/proj", rowSubtitle("/home/u/proj", ""))
        // **The folder is never dropped.** It used to be, when the title was the folder's own name,
        // and that left `ldmux`, `meshmore-sns`, `tmp` and `RCNT` with no place on the row at all
        // (measured on the device, 2026-11-07). Two rows may read the same; that is allowed.
        assertEquals("~/proj  \u00b7  on desktop 2", rowSubtitle("/home/u/proj", "on desktop 2"))
        assertEquals("~", rowSubtitle("~", ""))
        assertEquals("~  \u00b7  no window", rowSubtitle("~", "no window"))
    }

    @Test
    fun `a shell a phone is using is marked, and one the desk holds is not`() {
        // The shade says "connected"; the desk holding its own shell is the normal
        // state and says nothing.
        val rows = launcherEntries(
            listOf(
                shell("p", "/x").copy(heldElsewhere = true, ownerKind = "phone", ownerName = "Pixel 8"),
                shell("d", "/y").copy(heldElsewhere = true, ownerKind = "desktop", ownerName = "debian"),
            ),
        )
        assertEquals(listOf(true, false), rows.map { it.inUseOnPhone })
    }

    // ── reading the daemon's answer ────────────────────────────────────────────

    private fun parse(json: String) =
        parseDesktop(Json.parseToJsonElement(json).jsonObject)

    @Test
    fun `a full inventory round-trips`() {
        // `piSessions` is what a daemon older than this build sends. It is gone from the
        // contract, and a field nobody reads must not make the inventory unreadable.
        val inv = parse(
            """
            {"wm":"i3",
             "workspaces":[{"name":"1","index":1,"focused":false,"visible":false,"windowCount":2}],
             "windows":[{"id":"0x4600005","appClass":"Alacritty","title":"π","workspace":"1"}],
             "piSessions":[{"pid":100,"cwd":"/home/u","label":"ldeen"}]}
            """.trimIndent(),
        )!!
        assertEquals("i3", inv.wm)
        assertEquals(2, inv.workspaces[0].windowCount)
        assertEquals(true, inv.workspaces[0].index == 1)
        assertEquals("Alacritty", inv.windows[0].appClass)
        assertEquals("1", inv.windows[0].workspace)
    }

    @Test
    fun `null-valued fields read as absent rather than throwing`() {
        // The daemon sends null for values it could not resolve (no index on a named
        // workspace, no workspace on a window). `jsonPrimitive` throws on those; this
        // must not.
        val inv = parse(
            """
            {"wm":"ewmh",
             "workspaces":[{"name":"web","index":null,"focused":false,"visible":true,"windowCount":1}],
             "windows":[{"id":"0x1","appClass":"firefox","title":"t","workspace":null}]}
            """.trimIndent(),
        )!!
        assertNull(inv.workspaces[0].index)
        assertEquals("", inv.windows[0].workspace)
    }

    @Test
    fun `a daemon that cannot enumerate windows says why`() {
        val inv = parse(
            """{"wm":"unknown","detail":"no window manager could be enumerated",
                "workspaces":[],"windows":[]}""".trimIndent(),
        )!!
        assertEquals("unknown", inv.wm)
        assertTrue(inv.windows.isEmpty())
        assertEquals("no window manager could be enumerated", inv.detail)
    }

    @Test
    fun `an unknown field from a newer daemon is ignored`() {
        val inv = parse(
            """{"wm":"i3","somethingNew":{"a":1},"workspaces":[],"windows":[]}""".trimIndent(),
        )!!
        assertEquals("i3", inv.wm)
        assertTrue(inv.isEmpty)
    }

    @Test
    fun `missing arrays are empty, not fatal`() {
        val inv = parse("""{"wm":"i3"}""")!!
        assertTrue(inv.isEmpty)
    }

    @Test
    fun `a missing inventory is null rather than an empty screen with no explanation`() {
        assertNull(parseDesktop(null))
    }

    @Test
    fun `the shell view names the folder, not the path to it`() {
        // A title is not a location. The whole route from the root is worth the
        // room when you are choosing a directory and worth none of it when you are
        // already in one, and the shell view is the already-in-one case.
        assertEquals("ldmux", folderName("/home/u/src/ldmux"))
        assertEquals("shelltest", folderName("/tmp/shelltest"))
        assertEquals("src", folderName("/home/u/proj/src/"))
        // The home directory has no last segment worth showing, and the root has
        // none at all.
        assertEquals("~", folderName("/home/u"))
        assertEquals("~", folderName("/home/u/"))
        assertEquals("/", folderName("/"))
        // Someone else's home is not "~" in principle, but the client has no way
        // to know the local user, so any `/home/<name>` reads as one. That is the
        // same convention `tildify` already uses, and it is only ever wrong about
        // a directory that is not yours.
        assertEquals("~", folderName("/home/bob"))
        assertEquals("work", folderName("/home/bob/work"))
    }

    // ── the button's verb ───────────────────────────────────────────────────
    //
    // The launcher said `open` over a row that could not be opened. A desktop
    // `pi` with no keeper session behind it is a *directory intention* — `sessionId` is
    // null for exactly those rows — and tapping `open` on one started a
    // phone-owned shell while reading as "go to my session on the computer".
    // The word now follows what was actually chosen.

    private fun row(key: String, session: String) =
        LauncherEntry(
            key = key,
            sessionId = session,
            process = "pi",
            // A named conversation: the row is called that, not after the program in the pane.
            title = "Radio paths",
            cwd = "/tmp/$key",
        )

    @Test
    fun `an empty selection says open`() {
        assertEquals("open", openButtonLabel(emptyList()))
    }

    @Test
    fun `a selection of sessions says open`() {
        assertEquals("open (2)", openButtonLabel(listOf(row("a", "s1"), row("b", "s2"))))
    }

    @Test
    fun `a blank live workspace is not a workspace`() {
        // The daemon records an empty string when a window opened but i3 did not place it —
        // the window exists and must be tracked, because it is writable and a claim has to
        // follow it. `!= null` then rendered a dangling `on desktop ` and, worse, matched
        // the branch above the two that say something useful, so a row lost "in use on
        // desktop" to a label with no noun in it.
        assertNull(liveWorkspaceOf(null))
        assertNull(liveWorkspaceOf(""))
        assertNull(liveWorkspaceOf("   "))
        // A real one passes through untouched, including a named workspace.
        assertEquals("9", liveWorkspaceOf("9"))
        assertEquals("web", liveWorkspaceOf("web"))
    }

}
