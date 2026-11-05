package io.ldmux.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The mesh-keys part of the pairing link, from the app's side. Cases come from
 * `crates/protocol/test/fixtures/pairing-links-mesh.json`, which the Rust parser and serialiser
 * read too, so the two cannot disagree without a suite failing.
 */
class PairingMeshTest {
    private val fixture: JsonObject by lazy {
        val f = File("../../../crates/protocol/test/fixtures/pairing-links-mesh.json")
        assertTrue(f.exists(), "the shared fixture is missing: ${f.absolutePath}")
        Json.parseToJsonElement(f.readText()).jsonObject
    }

    private fun unhex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

    @Test
    fun `every valid link parses to its pairing and its keys`() {
        val valid = fixture["valid"]!!.jsonArray
        assertTrue(valid.size >= 4)
        for (c in valid) {
            val o = c.jsonObject
            val p = parsePairingLink(o["link"]!!.jsonPrimitive.content)
            assertNotNull(p, o["link"].toString())
            val want = o["pairing"]!!.jsonObject
            assertEquals(want["host"]!!.jsonPrimitive.content, p.host)
            assertEquals(want["token"]!!.jsonPrimitive.content, p.token)
            assertEquals(want["name"]!!.jsonPrimitive.content, p.name)
            val m = o["mesh"]
            if (m == null || m.toString() == "null") {
                assertNull(p.mesh)
            } else {
                val mo = m.jsonObject
                assertEquals(
                    MeshKeyMaterial(
                        unhex(mo["hostKey"]!!.jsonPrimitive.content),
                        unhex(mo["psk"]!!.jsonPrimitive.content),
                        mo["hostRadio"]?.jsonPrimitive?.content?.let(::unhex),
                    ),
                    p.mesh,
                )
            }
        }
    }

    @Test
    fun `every invalid link is refused whole, not downgraded to the tailnet`() {
        val invalid = fixture["invalid"]!!.jsonArray
        assertTrue(invalid.size >= 13)
        for (c in invalid) {
            val o = c.jsonObject
            assertNull(parsePairingLink(o["link"]!!.jsonPrimitive.content), o["why"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun `mesh-only links parse to keys and a name, carry no daemon credential, and are not tailnet pairings`() {
        val only = fixture["meshOnly"]!!.jsonArray
        assertTrue(only.size >= 2)
        for (c in only) {
            val o = c.jsonObject
            val link = o["link"]!!.jsonPrimitive.content
            val p = parseScannedPairing(link)
            assertNotNull(p, link)
            assertTrue(p.isMeshOnly)
            assertEquals("", p.host); assertEquals("", p.token)
            assertEquals(o["name"]!!.jsonPrimitive.content, p.name)
            val mo = o["mesh"]!!.jsonObject
            assertEquals(
                MeshKeyMaterial(
                    unhex(mo["hostKey"]!!.jsonPrimitive.content), unhex(mo["psk"]!!.jsonPrimitive.content),
                    mo["hostRadio"]?.jsonPrimitive?.content?.let(::unhex),
                ),
                p.mesh,
            )
            // The tailnet reader must not take it for a tailnet pairing.
            assertNull(parsePairingLink(link))
        }
    }

    @Test
    fun `half a tailnet pairing, or a broken mesh-only link, is refused whole`() {
        val bad = fixture["meshOnlyInvalid"]!!.jsonArray
        assertTrue(bad.size >= 6)
        for (c in bad) {
            val o = c.jsonObject
            assertNull(parseScannedPairing(o["link"]!!.jsonPrimitive.content), o["why"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun `a v1 link is the pairing it always was`() {
        val tok = "0123456789abcdef".repeat(4)
        val p = parsePairingLink("ldmux://pair?v=1&host=100.100.0.1:7625&token=$tok&name=debian")
        assertEquals(Pairing("100.100.0.1:7625", tok, "debian"), p)
        assertNull(p!!.mesh)
    }

    @Test
    fun `keys never appear in a printed pairing`() {
        val p = parsePairingLink(fixture["valid"]!!.jsonArray[1].jsonObject["link"]!!.jsonPrimitive.content)!!
        val shown = p.toString()
        val psk = fixture["valid"]!!.jsonArray[1].jsonObject["mesh"]!!.jsonObject["psk"]!!.jsonPrimitive.content
        assertTrue(psk !in shown && "a0a1a2" !in shown, shown)
    }
}
