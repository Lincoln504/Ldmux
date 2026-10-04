package dev.ldmux.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The pairing link, from the app's side of the contract.
 *
 * The cases are not written here. They are read from the fixture the CLI's tests
 * read (`crates/protocol/test/fixtures/pairing-links.json`), so this parser and the
 * Rust one are held to the same list and cannot drift apart without one of the
 * two suites failing.
 */
class PairingTest {
    private val fixture: JsonObject by lazy {
        // Unit tests run with the module directory as the working directory.
        val file = File("../../crates/protocol/test/fixtures/pairing-links.json")
        assertTrue(file.exists(), "the shared fixture is missing: ${file.absolutePath}")
        Json.parseToJsonElement(file.readText()).jsonObject
    }

    @Test
    fun `every valid fixture link parses to its pairing`() {
        val valid = fixture["valid"]!!.jsonArray
        assertTrue(valid.size >= 5, "the fixture should carry its cases")
        for (c in valid) {
            val o = c.jsonObject
            val p = o["pairing"]!!.jsonObject
            val expected = Pairing(
                host = p["host"]!!.jsonPrimitive.content,
                token = p["token"]!!.jsonPrimitive.content,
                name = p["name"]!!.jsonPrimitive.content,
            )
            val link = o["link"]!!.jsonPrimitive.content
            assertEquals(expected, parsePairingLink(link), link)
        }
    }

    @Test
    fun `every invalid fixture link is refused`() {
        val invalid = fixture["invalid"]!!.jsonArray
        assertTrue(invalid.size >= 10)
        for (c in invalid) {
            val link = c.jsonPrimitive.content
            assertEquals(null, parsePairingLink(link), link)
        }
    }

    @Test
    fun `tailnet only, at the edges`() {
        for (a in listOf("100.64.0.0", "100.127.255.255", "127.0.0.1", "::1", "[::1]", "localhost", "fd7a:115c:a1e0::1", "::ffff:100.64.0.1")) {
            assertTrue(isTailnetOrLoopback(a), a)
        }
        for (a in listOf("100.63.255.255", "100.128.0.0", "192.168.1.5", "0.0.0.0", "::", "fd7a:115c:a1e1::1", "fe80::1%wlan0", "01.2.3.4", "debian.ts.net", "")) {
            assertFalse(isTailnetOrLoopback(a), a)
        }
    }
}
