package io.ldmux.mobile.mesh

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.double
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * The public preset table, held to the record it was ported from.
 *
 * [PublicPresets] decides the frequency, bandwidth, spreading factor, coding rate **and transmit
 * power** the phone actually transmits on. A drifted row is not a display bug: the EU rows are
 * 14 dBm and the Japanese 13, and the fallback for a row that lost its power is 22 — an operating
 * point outside the band's limit. The table is a hand-port of MeshMore's `Regions.kt`, so nothing
 * in Kotlin can prove it right; this file is the record, and the test fails when the two disagree.
 *
 * The fixture is `fixtures/public-presets.json`, beside this class. It is not generated: it is the
 * reviewed copy, and changing a row means changing both, deliberately.
 */
class PublicPresetsTest {
    private val fixture: JsonObject by lazy {
        val f = File("../../../packages/mobile/composeApp/src/androidUnitTest/fixtures/public-presets.json")
        assertTrue(f.exists(), "the preset fixture is missing: ${f.absolutePath}")
        Json.parseToJsonElement(f.readText()).jsonObject
    }

    @Test
    fun `every row in the table is the row on record`() {
        val rows = fixture["presets"]!!.jsonArray.map { it.jsonObject }
        assertEquals(rows.size, PublicPresets.size, "the table and the fixture have different lengths")
        assertEquals(
            rows.map { it["id"]!!.jsonPrimitive.content },
            PublicPresets.map { it.id },
            "the table's rows are the fixture's rows, in the same order",
        )
        for ((want, got) in rows.zip(PublicPresets)) {
            val id = want["id"]!!.jsonPrimitive.content
            assertEquals(want["label"]!!.jsonPrimitive.content, got.label, "$id: label")
            val region = want["region"]?.takeIf { it !is JsonNull }?.jsonPrimitive?.content
            assertEquals(region, got.region?.name, "$id: region")
            assertEquals(want["frequencyMhz"]!!.jsonPrimitive.double, got.preset.frequencyMhz, 1e-9, "$id: frequency")
            assertEquals(want["bandwidthKhz"]!!.jsonPrimitive.double, got.preset.bandwidthKhz, 1e-9, "$id: bandwidth")
            assertEquals(want["spreadingFactor"]!!.jsonPrimitive.int, got.preset.spreadingFactor, "$id: spreading factor")
            assertEquals(want["codingRate"]!!.jsonPrimitive.int, got.preset.codingRate, "$id: coding rate")
            assertEquals(want["txPowerDbm"]!!.jsonPrimitive.int, got.preset.txPowerDbm, "$id: transmit power")
            assertEquals(want["pathHashSize"]!!.jsonPrimitive.int, got.preset.pathHashSize, "$id: path width")
        }
    }

    @Test
    fun `ids are unique, because a row is chosen by its id`() {
        val ids = PublicPresets.map { it.id }
        assertEquals(ids.size, ids.toSet().size, "duplicate preset id in $ids")
    }

    @Test
    fun `every row says which region it belongs to, or that it has none`() {
        // A null region means ldmux offers no private band for it; that is a decision, not a
        // forgotten field, so the fixture records the key explicitly and this test fails if a
        // row loses it. Unknown text (a renamed enum) fails too: the name is the contract.
        val rows = fixture["presets"]!!.jsonArray.map { it.jsonObject }
        for (row in rows) {
            val id = row["id"]!!.jsonPrimitive.content
            val region = assertNotNull(row["region"], "$id: no region key at all")
            if (region is JsonNull) continue
            val name = region.jsonPrimitive.content
            assertTrue(
                RadioRegion.entries.any { it.name == name },
                "$id: region $name is not a RadioRegion",
            )
        }
    }
}
