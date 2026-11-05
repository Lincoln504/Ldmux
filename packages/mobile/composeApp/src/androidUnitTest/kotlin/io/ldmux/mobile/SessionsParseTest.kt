package io.ldmux.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Reading the daemon's `sessions` message.
 *
 * The fixture below is the **real** payload, captured from the running daemon over
 * the tailnet with a WebSocket probe, not a hand-written approximation of it. That
 * distinction is the point of this test: the parser was suspected of dropping every
 * row and it was innocent, and a fixture invented to match the parser would have
 * confirmed the suspicion either way.
 *
 * The parse used `jsonPrimitive`, which **throws** on a value that is `JsonNull` or
 * an object, while every other parser in the file used the tolerant `text` /
 * `number` / `flag` helpers. The safe-call in front of it only guards an *absent*
 * key, so it looked defensive and was not — and the throw would have landed inside
 * a `mapNotNull` inside a receive loop inside `catch (_: Throwable)`, which is
 * where one unreadable field becomes a blank screen and no explanation.
 */
class SessionsParseTest {

    private fun parse(json: String, deviceId: String = "me") =
        parseSessions(Json.parseToJsonElement(json).jsonObject["sessions"], deviceId)

    /**
     * Shaped exactly like the daemon's output (captured 2026-09-20, and the session-layer name
     * field renamed `keeperName` on 2026-10-05), with **synthetic paths and names**: a fixture
     * that carries one machine's home directory and one user's project names is a fixture that
     * cannot be read on any other machine, and it says nothing the test needs.
     */
    private val real = """{"type":"sessions","sessions":[{"id":"e1b2f6f8f9","kind":"shell","title":"home","cwd":"/home/u","status":"dead","keeperName":"ldmux-e1b2f6f8f9","createdBy":"local","lastActivityAt":1789919010983},{"id":"9bbe2eaf62","kind":"shell","title":"alpha","cwd":"/home/u/src/alpha","status":"running","keeperName":"ldmux-9bbe2eaf62","createdBy":"phone","process":"bash","lastActivityAt":1789857207000},{"id":"0c4d7be073","kind":"shell","title":"beta","cwd":"/home/u/src/beta","status":"running","keeperName":"ldmux-0c4d7be073","createdBy":"phone","process":"bash","lastActivityAt":1789857210000},{"id":"09207486a3","kind":"shell","title":"home","cwd":"/home/u","status":"running","keeperName":"ldmux-09207486a3","createdBy":"local","process":"bash","lastActivityAt":1789831137000},{"id":"5dff7004ec","kind":"shell","title":"home","cwd":"/home/u","status":"running","keeperName":"ldmux-5dff7004ec","createdBy":"local","process":"pi","lastActivityAt":1789832638000},{"id":"f1ffeb84cd","kind":"shell","title":"home","cwd":"/home/u","status":"running","keeperName":"ldmux-f1ffeb84cd","createdBy":"local","process":"bash","lastActivityAt":1789826242000}]}"""

    @Test
    fun `the real payload parses every session`() {
        val sessions = parse(real)
        assertEquals(6, sessions.size, "the daemon sent six")
        assertTrue(sessions.all { it.id.isNotEmpty() }, "every row has an id")
        // The names the daemon chose, and the reason `list` now returns the
        // described view: these are folders, not the word "pi".
        assertEquals("alpha", sessions.first { it.cwd.endsWith("/alpha") }.title)
        assertEquals("beta", sessions.first { it.cwd.endsWith("/beta") }.title)
        // A shell whose pane is gone has no `process` key at all.
        assertTrue(sessions.count { it.process.isEmpty() } >= 1)
    }

    @Test
    fun `a null or an object in a field does not take the list down`() {
        // The hostile cases, in one message: `process`, `liveWorkspace` and `owner`
        // all null, an owner that is an object, an entry with no id, and an entry
        // that is not an object at all.
        val sessions = parse("""{"sessions":[{"id":"a","kind":"shell","title":"t","cwd":"/x","status":"running","keeperName":"m","createdBy":"local","process":null,"liveWorkspace":null,"owner":null},{"id":"b","kind":"shell","title":"t","cwd":"/y","status":"running","keeperName":"m2","createdBy":"local","owner":{"id":"other","name":"desk","kind":"desktop"}},{"id":"c","kind":"shell","title":"t","cwd":"/z","status":"running","keeperName":"m3","createdBy":"local","owner":{"id":"me","name":"this","kind":"phone"}},{"kind":"shell","title":"no id at all"},"not an object",{"id":"d","kind":"shell","title":"t","cwd":"/w","status":"running","keeperName":"m4","createdBy":"local"}]}""")
        assertEquals(4, sessions.size, "the unreadable entries are skipped, the rest survive")
        assertTrue(sessions.all { it.id.isNotEmpty() })
    }

    @Test
    fun `an owner that is another device means held elsewhere`() {
        val sessions = parse("""{"sessions":[{"id":"a","kind":"shell","title":"t","cwd":"/x","status":"running","keeperName":"m","createdBy":"local","process":null,"liveWorkspace":null,"owner":null},{"id":"b","kind":"shell","title":"t","cwd":"/y","status":"running","keeperName":"m2","createdBy":"local","owner":{"id":"other","name":"desk","kind":"desktop"}},{"id":"c","kind":"shell","title":"t","cwd":"/z","status":"running","keeperName":"m3","createdBy":"local","owner":{"id":"me","name":"this","kind":"phone"}},{"kind":"shell","title":"no id at all"},"not an object",{"id":"d","kind":"shell","title":"t","cwd":"/w","status":"running","keeperName":"m4","createdBy":"local"}]}""")
        val other = sessions.first { it.id == "b" }
        val mine = sessions.first { it.id == "c" }
        assertTrue(other.heldElsewhere, "someone else's owner id")
        assertEquals("desk", other.ownerName)
        assertEquals("desktop", other.ownerKind)
        assertTrue(!mine.heldElsewhere, "our own device id is not somebody else")
    }

    /**
     * The wire contract with the daemon (`SessionInfo` in `crates/protocol`, pinned
     * on the Rust side by `rpc.rs` and `remote_integration.rs`): the session-layer
     * name is `keeperName`. A row carrying only the retired key reads as having no
     * name, so it is not attachable, rather than being silently accepted.
     */
    @Test
    fun `the session layer name is keeperName on the wire`() {
        val sessions = parse("""{"sessions":[
            {"id":"a","kind":"shell","title":"t","cwd":"/x","status":"running","keeperName":"ldmux-a","createdBy":"local"},
            {"id":"b","kind":"shell","title":"t","cwd":"/y","status":"running","tmuxName":"ldmux-b","createdBy":"local"}
        ]}""")
        assertEquals("ldmux-a", sessions.first { it.id == "a" }.keeperName)
        assertTrue(sessions.first { it.id == "a" }.attachable)
        assertEquals("", sessions.first { it.id == "b" }.keeperName)
        assertTrue(!sessions.first { it.id == "b" }.attachable)
    }

    @Test
    fun `no sessions is an empty list, not a failure`() {
        assertEquals(0, parse("""{"sessions":[]}""").size)
        assertEquals(0, parse("""{"sessions":null}""").size)
        assertEquals(0, parse("""{"nope":1}""").size)
        assertEquals(0, parseSessions(null, "me").size)
    }
}
