package dev.ldmux.mobile

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

    private fun pi(pid: Int, cwd: String, label: String, ws: String? = "1") = DesktopPiSession(
        pid = pid,
        cwd = cwd,
        sessionId = "sid-$pid",
        startedAt = null,
        windowId = null,
        workspace = ws,
        label = label,
    )

    private fun shell(id: String, cwd: String, title: String = "") =
        Session(id = id, kind = "shell", title = title, cwd = cwd, status = "running", tmuxName = "ldmux-$id")

    @Test
    fun `a pi session with no shell becomes a row to open a shell in its directory`() {
        val rows = launcherEntries(listOf(pi(100, "/home/ldeen/proj", "proj")), emptyList())
        assertEquals(1, rows.size)
        assertEquals("new:/home/ldeen/proj", rows[0].key)
        assertEquals("pi", rows[0].process)
        assertEquals("/home/ldeen/proj", rows[0].cwd)
        assertNull(rows[0].sessionId)
    }

    @Test
    fun `an open shell is named by its process and located by its folder`() {
        // The row has to answer "which of these is my editor", and a directory
        // alone cannot: two shells in one folder would otherwise be the same row.
        val rows = launcherEntries(
            emptyList(),
            listOf(shell("abc", "/home/ldeen/proj").copy(process = "vim")),
        )
        assertEquals("vim", rows[0].label)
        assertEquals("/home/ldeen/proj", rows[0].cwd)
        assertEquals("abc", rows[0].sessionId)
    }

    @Test
    fun `a shell with an unknown process still has a name`() {
        val rows = launcherEntries(emptyList(), listOf(shell("abc", "/x")))
        assertEquals("shell", rows[0].label)
    }

    @Test
    fun `a held shell says who has it`() {
        val rows = launcherEntries(
            emptyList(),
            listOf(shell("abc", "/x").copy(heldElsewhere = true, ownerName = "Pixel 8")),
        )
        assertEquals("open on Pixel 8", rows[0].note)
    }

    @Test
    fun `a pi session that already has a shell is that shell, not a second thing`() {
        val rows = launcherEntries(
            listOf(pi(100, "/home/ldeen/proj", "proj")),
            listOf(shell("abc", "/home/ldeen/proj", "proj")),
        )
        // Exactly one entry: the existing shell. Offering both would show the same
        // work twice, and choosing wrong would open a duplicate shell.
        assertEquals(1, rows.size)
        assertEquals("abc", rows[0].key)
    }

    @Test
    fun `two pi sessions in one directory are one row, because the shell is per directory`() {
        val rows = launcherEntries(listOf(pi(100, "/a", "a"), pi(200, "/a", "a")), emptyList())
        assertEquals(1, rows.size)
        assertEquals("new:/a", rows[0].key)
    }

    @Test
    fun `pi rows come first, then shells, so the pager order is the screen order`() {
        val rows = launcherEntries(
            listOf(pi(100, "/new", "new")),
            listOf(shell("one", "/x"), shell("two", "/y")),
        )
        assertEquals(listOf("new:/new", "one", "two"), rows.map { it.key })
    }

    @Test
    fun `the keys of a new-shell row and a session id cannot collide`() {
        val rows = launcherEntries(
            listOf(pi(100, "/x", "x")),
            listOf(shell("/x", "/x")),
        )
        // A cwd can never equal a session id, but the keys still must not meet.
        assertEquals(1, rows.size)
        assertEquals("/x", rows[0].key)
    }

    // ── reading the daemon's answer ────────────────────────────────────────────

    private fun parse(json: String) =
        parseDesktop(Json.parseToJsonElement(json).jsonObject)

    @Test
    fun `a full inventory round-trips`() {
        val inv = parse(
            """
            {"wm":"i3",
             "workspaces":[{"name":"1","index":1,"focused":false,"visible":false,"windowCount":2}],
             "windows":[{"id":"0x4600005","appClass":"Alacritty","title":"π","workspace":"1","pid":9707}],
             "piSessions":[{"pid":100,"cwd":"/home/ldeen","sessionId":"01a0","startedAt":"2026-09-15T00:00:00Z",
                            "transcript":"/t.jsonl","windowId":"0x4600005","workspace":"1","label":"ldeen"}]}
            """.trimIndent(),
        )!!
        assertEquals("i3", inv.wm)
        assertEquals(2, inv.workspaces[0].windowCount)
        assertEquals(true, inv.workspaces[0].index == 1)
        assertEquals(9707, inv.windows[0].pid)
        assertEquals("ldeen", inv.piSessions[0].label)
        assertEquals("0x4600005", inv.piSessions[0].windowId)
    }

    @Test
    fun `null-valued fields read as absent rather than throwing`() {
        // The daemon sends null for values it could not resolve (no window, no
        // workspace, no session id). `jsonPrimitive` throws on those; this must not.
        val inv = parse(
            """
            {"wm":"ewmh","workspaces":[],"windows":[],
             "piSessions":[{"pid":1,"cwd":"/x","sessionId":null,"startedAt":null,
                            "transcript":null,"windowId":null,"workspace":null,"label":"x"}]}
            """.trimIndent(),
        )!!
        assertEquals(1, inv.piSessions.size)
        assertNull(inv.piSessions[0].workspace)
        assertNull(inv.piSessions[0].sessionId)
        assertNull(inv.piSessions[0].windowId)
    }

    @Test
    fun `a daemon that cannot enumerate windows still reports its pi sessions`() {
        val inv = parse(
            """{"wm":"unknown","detail":"no window manager could be enumerated",
                "workspaces":[],"windows":[],
                "piSessions":[{"pid":1,"cwd":"/x","label":"x"}]}""".trimIndent(),
        )!!
        assertEquals("unknown", inv.wm)
        assertTrue(inv.windows.isEmpty())
        assertEquals(1, inv.piSessions.size)
        assertEquals("no window manager could be enumerated", inv.detail)
    }

    @Test
    fun `an unknown field from a newer daemon is ignored`() {
        val inv = parse(
            """{"wm":"i3","somethingNew":{"a":1},"workspaces":[],"windows":[],"piSessions":[]}""".trimIndent(),
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
}
