package io.ldmux.mobile.mesh

import io.ldmux.mobile.LdmuxConfig
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.runBlocking
import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The standalone route's pure decisions: no token, a real radio, and one honest body per state. */
class MeshStandaloneTest {

    private fun keys(withRadio: Boolean = true) = MeshSessionKeys(
        ByteArray(32) { 1 },
        ByteArray(32) { 2 },
        ByteArray(32) { 3 },
        ByteArray(32) { 4 },
        if (withRadio) ByteArray(32) { 5 } else null,
    )

    @Test
    fun `credentials need session keys and a host radio, and nothing else`() {
        val store = InMemoryMeshSessionKeyStore().apply { save(keys()) }
        val (state, creds) = readMeshCredentials(store)
        assertTrue(state.complete)
        assertTrue(state.hasSessionKeys)
        assertTrue(state.hasPeer)
        assertNotNull(creds)
        assertEquals(5, creds.hostRadio[0].toInt())

        val noRadio = InMemoryMeshSessionKeyStore().apply { save(keys(withRadio = false)) }
        val (state2, creds2) = readMeshCredentials(noRadio)
        assertFalse(state2.complete)
        assertTrue(state2.hasSessionKeys)
        assertFalse(state2.hasPeer)
        assertNull(creds2)
    }

    @Test
    fun `a saved mesh route resolves without a MeshMore token`() {
        assertEquals(
            MeshRouteResolution.Use(MeshRoute.Mesh),
            resolveRoute(
                MeshRoute.Mesh,
                hasPeer = true, hasSessionKeys = true,
                sessionBuilt = true, terminalBuilt = true,
            ),
        )
        assertEquals(
            MeshRouteProblem.NeedsPeer,
            (resolveRoute(
                MeshRoute.Mesh,
                hasPeer = false, hasSessionKeys = true,
                sessionBuilt = true, terminalBuilt = true,
            ) as MeshRouteResolution.Unusable).problem,
        )
    }

    @Test
    fun `missing keys are named before missing radio`() {
        val r = resolveRoute(
            MeshRoute.Mesh,
            hasPeer = false, hasSessionKeys = false,
            sessionBuilt = true, terminalBuilt = true,
        )
        assertEquals(MeshRouteProblem.NeedsSessionKeys, (r as MeshRouteResolution.Unusable).problem)
    }

    @Test
    fun `the setup view offers the mesh route only when it can run`() {
        val view = meshSetupView(
            MeshCredentialState(hasSessionKeys = true, hasPeer = true),
            sessionBuilt = true,
            terminalBuilt = true,
        )
        assertEquals(setOf(MeshCredential.SessionKeys, MeshCredential.HostRadioKey), view.present)
        assertEquals(MeshRouteResolution.Use(MeshRoute.Mesh), view.meshResolution)
    }

    @Test
    fun `the setup view says which credential is missing`() {
        val view = meshSetupView(
            MeshCredentialState(hasSessionKeys = true, hasPeer = false),
            sessionBuilt = true,
            terminalBuilt = true,
        )
        assertEquals(
            MeshRouteProblem.NeedsPeer,
            (view.meshResolution as MeshRouteResolution.Unusable).problem,
        )
    }

    @Test
    fun `the radio is part of the connection key and a font change is not`() {
        val base = LdmuxConfig.DEFAULT.copy(route = MeshRoute.Mesh, meshRadio = "AA:BB", meshRadioName = "WIO")
        assertEquals(base.connectionKey, base.copy(fontSp = 14f, compact = false, side = "right").connectionKey)
        assertTrue(base.connectionKey != base.copy(meshRadio = "CC:DD").connectionKey)
        assertTrue(base.connectionKey != base.copy(meshRadioName = "other").connectionKey)
    }

    @Test
    fun `every link state has a sentence and the body never says token`() {
        for (link in MeshLinkState.entries) {
            val body = meshBodyFor(MeshTermView.LinkDown(link))
            assertTrue(body != null, link.name)
            assertTrue(body.line.isNotBlank(), link.name)
            assertFalse(body.line.contains("token", ignoreCase = true), link.name)
            assertTrue(meshLinkLabel(link).isNotBlank(), link.name)
        }
    }

    @Test
    fun `the radio gateway maps one send and exposes the record ceiling`() = runBlocking<Unit> {
        val radio = FakeRadio()
        val gateway = MeshRadioGateway(radio)
        assertEquals(172, gateway.uplinkRecordMax)
        assertEquals(MeshSendOutcome.SentDirect, gateway.send(ByteArray(4)).outcome)
        assertEquals(1, radio.sends)
    }

    @Test
    fun `the preset is one tuple with a path-hash width`() {
        assertEquals(910.525, RadioPreset.BENCH.frequencyMhz)
        assertEquals(500.0, RadioPreset.BENCH.bandwidthKhz)
        assertEquals(7, RadioPreset.BENCH.spreadingFactor)
        assertEquals(5, RadioPreset.BENCH.codingRate)
        assertEquals(22, RadioPreset.BENCH.txPowerDbm)
        // **Two bytes is ldmux's default, not the firmware's one.** One byte has 254 usable values
        // and collides at ≈20 co-channel repeaters; two has 65 536 and still allows 32 hops.
        assertEquals(2, RadioPreset.BENCH.pathHashSize)
        assertEquals(3, RadioPreset.CANADA.pathHashSize)
        assertEquals(2, RadioPreset.USA.pathHashSize)
    }

    @Test
    fun `a fresh config is a private open channel`() {
        assertTrue(LdmuxConfig.DEFAULT.meshOpenAirtime)
        assertEquals("", LdmuxConfig.DEFAULT.meshRadio)
    }

    @Test
    fun `a private tuple stays inside its region envelope`() {
        repeat(40) {
            val us = privatePreset(RadioRegion.UsCanada)
            assertEquals(500.0, us.bandwidthKhz)
            assertTrue(us.frequencyMhz - 0.25 >= 902.0)
            assertTrue(us.frequencyMhz + 0.25 <= 928.0)

            val eu = privatePreset(RadioRegion.EuUk)
            assertEquals(125.0, eu.bandwidthKhz)
            assertTrue(eu.frequencyMhz - 0.0625 >= 869.4)
            assertTrue(eu.frequencyMhz + 0.0625 <= 869.65)

            val au = privatePreset(RadioRegion.Australia)
            assertTrue(au.frequencyMhz - 0.0625 >= 915.0)
            assertTrue(au.frequencyMhz + 0.0625 <= 928.0)
        }
    }

    @Test
    fun `the preset dialog infers the region from the applied tuple`() {
        assertEquals(RadioRegion.UsCanada, regionOfPreset(RadioPreset.BENCH))
        assertEquals(RadioRegion.UsCanada, regionOfPreset(RadioPreset.USA))
        assertEquals(RadioRegion.EuUk, regionOfPreset(publicPresets(RadioRegion.EuUk)[0].preset))
        assertEquals(RadioRegion.Australia, regionOfPreset(privatePreset(RadioRegion.Australia, Random(0))))
        assertEquals(RadioRegion.NewZealand, regionOfPreset(publicPresets(RadioRegion.NewZealand)[0].preset))
    }

    @Test
    fun `public presets exist for every region and carry a path width`() {
        for (r in RadioRegion.entries) {
            val choices = publicPresets(r)
            assertTrue(choices.isNotEmpty(), r.name)
            for (c in choices) {
                assertTrue(c.preset.pathHashSize in 1..3, c.label)
                assertTrue(c.preset.frequencyMhz > 400.0, c.label)
            }
        }
    }

    @Test
    fun `airtime is positive and a wider channel costs less`() {
        val wide = RadioPreset.BENCH
        val narrow = RadioPreset.USA
        val wideMs = RadioAirtime.timeOnAirMs(wide, 172)
        val narrowMs = RadioAirtime.timeOnAirMs(narrow, 172)
        assertTrue(wideMs > 0.0)
        assertTrue(narrowMs > wideMs, "62.5 kHz should cost more airtime")
        assertTrue(RadioAirtime.opsPerHour(wide, 172) > RadioAirtime.opsPerHour(narrow, 172))
    }

    @Test
    fun `a public channel is the tuple other radios share, and a different bandwidth is not it`() {
        // The four fields that decide who shares a frequency, and nothing else. Transmit power and
        // path-hash width do not change who hears whom, so they are excluded.
        assertTrue(isPublicChannel(RadioPreset.USA))
        assertTrue(isPublicChannel(RadioPreset.CANADA))
        assertTrue(isPublicChannel(RadioPreset.USA.copy(txPowerDbm = 14, pathHashSize = 2)))
        // The LDMUX private tuple is the public centre at a *different bandwidth*, which is a
        // different channel: 500 kHz and 62.5 kHz do not hear each other.
        assertFalse(isPublicChannel(RadioPreset.BENCH))
        assertFalse(isPublicChannel(RadioPreset.USA.copy(bandwidthKhz = 250.0)))
        assertFalse(isPublicChannel(RadioPreset.USA.copy(spreadingFactor = 9)))
        assertFalse(isPublicChannel(RadioPreset.USA.copy(codingRate = 7)))
    }

    private class FakeRadio : RadioLink {
        private val _state = MutableStateFlow(MeshLinkState.Bound)
        override val state: StateFlow<MeshLinkState> = _state.asStateFlow()
        override val status: StateFlow<MeshStatus?> = MutableStateFlow(null)
        override val selfInfo: StateFlow<RadioSelfInfo?> = MutableStateFlow(null)
        override val deviceInfo: StateFlow<RadioDeviceInfo?> = MutableStateFlow(null)
        override val received: Flow<ByteArray> = emptyFlow()
        override val uplinkRecordMax: Int = 172
        override val lastProblem: StateFlow<String?> = MutableStateFlow(null)
        var sends = 0
        override suspend fun connect() = Unit
        override suspend fun send(record: ByteArray): RadioSendResult {
            sends++
            return RadioSendResult.Accepted
        }
        override suspend fun readDeviceInfo(): RadioDeviceInfo? = null
        override suspend fun applyPreset(preset: RadioPreset): Boolean = true
        override fun close() {}
    }
}
