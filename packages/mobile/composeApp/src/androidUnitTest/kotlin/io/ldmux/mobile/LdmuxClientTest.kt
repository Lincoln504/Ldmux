package io.ldmux.mobile

import io.ldmux.mobile.mesh.ShellMark
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject

/**
 * The launcher socket's message handling against hostile and future payloads.
 *
 * `onMessage` used `jsonPrimitive` / `jsonObject`, which throw on a `null` or an
 * object where a string was expected, inside the receive loop's catch-all — so one odd
 * field was a dropped socket. These are the payloads that did it.
 */
class LdmuxClientTest {
    private fun client() = LdmuxClient(testConfig())

    @Test
    fun `null fields do not throw`() {
        val c = client()
        c.onMessage("""{"type":"error","code":null,"message":null}""")
        assertEquals(LdmuxClient.ServerError("error", "daemon refused that"), c.serverError.value)
        c.onMessage("""{"type":"created","sessionId":null}""")
        assertTrue(c.created.value.isEmpty())
        c.onMessage("""{"type":"live","sessionId":null,"ok":null,"detail":null}""")
        assertEquals(LiveResult("", false, null), c.liveResult.value)
        // `owned` is ignored by this client (it attaches no stream) and must stay safe to
        // receive: an older daemon can still send it.
        c.onMessage("""{"type":"owned","sessionId":null,"owner":null}""")
    }

    @Test
    fun `objects where strings belong do not throw`() {
        val c = client()
        c.onMessage("""{"type":{"x":1}}""")
        c.onMessage("""{"type":"error","code":{"a":1},"message":[1]}""")
        c.onMessage("""{"type":"owned","sessionId":{"x":1},"owner":{"name":{"y":2}}}""")
        c.onMessage("""{"type":"desktop","inventory":"not an object"}""")
        c.onMessage("""{"type":"sessions","sessions":{"not":"a list"}}""")
        assertTrue(c.sessions.value.isEmpty())
    }

    @Test
    fun `created ids accumulate and an id-less created still ticks`() {
        val c = client()
        c.onMessage("""{"type":"created","sessionId":"a"}""")
        c.onMessage("""{"type":"created","sessionId":"b"}""")
        c.onMessage("""{"type":"created"}""")
        assertEquals(listOf("a", "b"), c.created.value)
        assertEquals(3, c.createdTick.value)
        c.clearCreated()
        assertTrue(c.created.value.isEmpty())
    }

    @Test
    fun `unknown message types and unknown fields are ignored`() {
        val c = client()
        c.onMessage("""{"type":"session_ended","sessionId":"a","exitCode":0}""")
        c.onMessage("""{"type":"sessions","sessions":[{"id":"a","keeperName":"k","status":"running","futureField":[1]}],"epoch":9}""")
        assertEquals(1, c.sessions.value.size)
    }

    @Test
    fun `garbage is ignored`() {
        val c = client()
        for (t in listOf("", "{", "[]", "null", "42", "\"str\"")) c.onMessage(t)
        assertTrue(c.sessions.value.isEmpty())
        assertNull(c.serverError.value)
    }

    @Test
    fun `an error reply is surfaced and ticks, so two in a row are both seen`() {
        val c = client()
        c.onMessage("""{"type":"error","code":"bad_path","message":"not a directory"}""")
        c.onMessage("""{"type":"error","code":"bad_path","message":"not a directory"}""")
        assertEquals(LdmuxClient.ServerError("bad_path", "not a directory"), c.serverError.value)
        assertEquals(2, c.errorTick.value)
    }

    @Test
    fun `creating a shell with no socket is refused, reported and not pretended`() {
        val c = client()
        assertFalse(c.create("/tmp"))
        assertEquals("offline", c.serverError.value?.code)
        assertEquals(1, c.errorTick.value)
    }

    @Test
    fun `reads asked for offline are kept and writes are not`() {
        val c = client()
        // A queued read reports success (it will be sent on connect); a close is refused.
        c.refreshSessions()
        c.close("a")
        c.kill("a")
        // No exception, and nothing observable beyond the link staying where it was.
        assertEquals(LinkState.Connecting, c.link.value)
    }

    @Test
    fun `a mark parses to a shell and a word, and an unknown word is not a mark`() {
        // The daemon's payload: `{"session": "...", "kind": "done"|"attention", "note", "at"}`.
        val done = parseMark(
            Json.parseToJsonElement(
                """{"session":"ab12","kind":"done","note":"","at":1}""",
            ).jsonObject,
        )
        assertEquals("ab12" to ShellMark.Done, done)
        val attention = parseMark(
            Json.parseToJsonElement("""{"session":"cd34","kind":"attention"}""").jsonObject,
        )
        assertEquals("cd34" to ShellMark.Attention, attention)
        // A word this app has not learned reads as nothing rather than as a broken list.
        assertNull(
            parseMark(Json.parseToJsonElement("""{"session":"ab12","kind":"later"}""").jsonObject),
        )
        assertNull(parseMark(Json.parseToJsonElement("""{"kind":"done"}""").jsonObject))
        assertNull(parseMark(null))
    }

    @Test
    fun `hello is one message in one shape for both sockets`() {
        val cfg = testConfig()
        val hello = helloMessage(cfg)
        assertEquals(
            """{"type":"hello","device":{"id":"dev-1","name":"Test phone","kind":"phone"}}""",
            hello,
        )
    }
}
