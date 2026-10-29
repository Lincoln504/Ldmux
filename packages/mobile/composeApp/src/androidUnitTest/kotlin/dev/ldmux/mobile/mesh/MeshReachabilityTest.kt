package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.ConfigStore
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.LinkState
import dev.ldmux.mobile.MeshKeyMaterial
import dev.ldmux.mobile.Pairing
import dev.ldmux.mobile.initialLink
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The mesh route being *reachable*: two pairings that do not depend on each other, a gateway code
 * the phone can read, a picker that cannot save a route that will not run, and a scanner that does
 * not take half a pairing.
 */
class MeshReachabilityTest {
    private val tok32 = "0123456789abcdef0123456789abcdef"
    private val tailnet = LdmuxConfig(host = "100.100.0.1:7625", token = "0".repeat(64), deviceId = "d", deviceLabel = "Pixel")
    private val meshOnly = LdmuxConfig.DEFAULT.copy(route = MeshRoute.Mesh)
    private val full = MeshCredentialState(hasSessionKeys = true, hasToken = true, hasPeer = true)
    private val none = MeshCredentialState(false, false, false)

    private fun view(c: LdmuxConfig, s: MeshCredentialState, platform: MobilePlatform = MobilePlatform.Android, lib: Boolean = true) =
        meshSetupView(c, s, platform, sessionBuilt = lib, terminalBuilt = true)

    // ── the gateway code ───────────────────────────────────────────────────────────────

    @Test
    fun `a bare 32-hex token is the default endpoint, so an already printed code still works`() {
        val c = assertNotNull(parseGatewayCode(tok32))
        assertEquals(MeshGatewayEndpoint.DEFAULT, c.endpoint)
        assertContentEquals(unhex(tok32), c.token)
    }

    @Test
    fun `the structured payload carries a loopback endpoint and the token`() {
        val c = assertNotNull(parseGatewayCode("meshmore://gateway?v=1&host=127.0.0.1:41999&token=$tok32"))
        assertEquals(MeshGatewayEndpoint("127.0.0.1", 41999), c.endpoint)
        assertContentEquals(unhex(tok32), c.token)
        assertEquals(MeshGatewayEndpoint("127.0.0.1", 41731), parseGatewayCode("meshmore://gateway?v=1&token=$tok32")!!.endpoint)
        assertEquals(MeshGatewayEndpoint("localhost", 5), parseGatewayCode("MESHMORE://gateway?v=1&host=localhost:5&token=$tok32")!!.endpoint)
        assertEquals("[::1]:41731", parseGatewayCode("meshmore://gateway?v=1&host=[::1]:41731&token=$tok32")!!.endpoint.toString())
        // Unknown parameters are ignored, as in the ldmux link.
        assertNotNull(parseGatewayCode("meshmore://gateway?future=x&v=1&token=$tok32"))
    }

    @Test
    fun `anything malformed or off the device is refused whole, never half accepted`() {
        val bad = listOf(
            "meshmore://gateway?v=1&host=100.64.0.1:41731&token=$tok32", // the tailnet is not this phone
            "meshmore://gateway?v=1&host=192.168.1.5:41731&token=$tok32",
            "meshmore://gateway?v=1&host=example.com:41731&token=$tok32",
            "meshmore://gateway?v=1&host=127.0.0.1&token=$tok32", // no port
            "meshmore://gateway?v=1&host=127.0.0.1:0&token=$tok32",
            "meshmore://gateway?v=1&host=127.0.0.1:70000&token=$tok32",
            "meshmore://gateway?v=2&token=$tok32", // another version
            "meshmore://gateway?token=$tok32", // no version
            "meshmore://gateway?v=1", // no token
            "meshmore://gateway?v=1&token=${tok32.dropLast(2)}", // short
            "meshmore://gateway?v=1&token=${tok32.uppercase()}", // not the canonical form
            "meshmore://gateway", "meshmore://gatewayx?v=1&token=$tok32", "ldmux://pair?v=1&token=$tok32",
            tok32 + "00", tok32.uppercase(), "", "not a code",
        )
        for (b in bad) assertNull(parseGatewayCode(b), b)
    }

    @Test
    fun `a gateway code never prints its token`() {
        val shown = parseGatewayCode(tok32)!!.toString()
        assertFalse(tok32 in shown)
    }

    // ── two pairings, neither implying the other ──────────────────────────────────────

    @Test
    fun `tailnet only, mesh only, both and neither each resolve to the truth`() {
        // Tailnet only: a daemon, no mesh credentials.
        val a = view(tailnet, none)
        assertEquals(listOf(MeshRoute.Tailnet), a.choosable)
        assertEquals(MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NeedsSessionKeys), a.meshResolution)
        assertNotNull(tailnet.endpoint)

        // Mesh only: NO daemon host or token, complete mesh credentials.
        val b = view(meshOnly, full)
        assertEquals(MeshRouteResolution.Use(MeshRoute.Mesh), b.meshResolution)
        assertEquals(listOf(MeshRoute.Mesh), b.choosable, "a mesh-only device has no tailnet route to offer")
        assertFalse(meshOnly.isPaired)

        // Both: either may be chosen.
        val c = view(tailnet, full)
        assertEquals(listOf(MeshRoute.Tailnet, MeshRoute.Mesh), c.choosable)

        // Neither: nothing may be chosen.
        val d = view(LdmuxConfig.DEFAULT, none)
        assertEquals(emptyList(), d.choosable)
    }

    @Test
    fun `a mesh-only config has no endpoint, so it can never open a tailnet connection`() {
        assertNull(meshOnly.endpoint)
        // Even with a daemon saved, a mesh route opens nothing on the tailnet.
        assertNull(tailnet.copy(route = MeshRoute.Mesh).endpoint)
        assertNull(meshOnly.routeProblem(hasToken = true, hasPeer = true, hasSessionKeys = true))
        // Without credentials the reason is the mesh route's own, not "unpaired".
        assertEquals(MeshRouteProblem.NeedsSessionKeys, meshOnly.routeProblem())
        assertEquals(LinkState.RouteUnavailable, initialLink(meshOnly))
        assertEquals(LinkState.Unpaired, initialLink(LdmuxConfig.DEFAULT))
    }

    @Test
    fun `the tailnet path is byte-identical`() {
        assertEquals("100.100.0.1:7625|${"0".repeat(64)}|d|Pixel", tailnet.connectionKey)
        assertEquals(tailnet.connectionKey, tailnet.copy(meshGateway = "127.0.0.1:1").connectionKey, "a gateway setting does not rebuild a tailnet page")
        assertEquals(LinkState.Connecting, initialLink(tailnet))
        assertTrue(tailnet.copy(route = MeshRoute.Mesh).connectionKey != tailnet.connectionKey)
    }

    @Test
    fun `every missing credential is a different state with a different line`() {
        val keysOnly = MeshCredentialState(hasSessionKeys = true, hasToken = false, hasPeer = false)
        val noRadio = MeshCredentialState(hasSessionKeys = true, hasToken = true, hasPeer = false)
        val tokenOnly = MeshCredentialState(hasSessionKeys = false, hasToken = true, hasPeer = false)
        val resolutions = listOf(none, keysOnly, noRadio, tokenOnly).map { view(meshOnly, it).meshResolution }
        val problems = resolutions.map { (it as MeshRouteResolution.Unusable).problem }
        assertEquals(
            listOf(MeshRouteProblem.NeedsSessionKeys, MeshRouteProblem.NeedsToken, MeshRouteProblem.NeedsPeer, MeshRouteProblem.NeedsSessionKeys),
            problems,
        )
        assertEquals(3, problems.toSet().size)
        assertEquals(3, problems.toSet().map(::meshProblemLabel).toSet().size)
        // What the settings screen lists as missing, in the order of the ceremony.
        assertEquals(
            listOf(MeshCredential.SessionKeys, MeshCredential.HostRadioKey, MeshCredential.GatewayToken),
            view(meshOnly, none).missing,
        )
        assertEquals(listOf(MeshCredential.GatewayToken), view(meshOnly, keysOnly.copy(hasPeer = true)).missing)
        assertEquals(emptyList(), view(meshOnly, full).missing)
    }

    @Test
    fun `iOS has no mesh route to pick, and a library that did not load blocks it visibly`() {
        val ios = view(tailnet, full, platform = MobilePlatform.Ios)
        assertFalse(ios.meshExists)
        assertEquals(listOf(MeshRoute.Tailnet), ios.choosable)
        assertEquals(MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NotOnThisPlatform), ios.meshResolution)
        val noLib = view(tailnet, full, lib = false)
        assertEquals(MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.SessionNotBuilt), noLib.meshResolution)
        assertEquals(listOf(MeshRoute.Tailnet), noLib.choosable)
    }

    // ── the picker ──────────────────────────────────────────────────────────────────────

    @Test
    fun `the picker cannot save a route that will not run`() {
        assertNull(chooseRoute(tailnet, MeshRoute.Mesh, view(tailnet, none)), "mesh without credentials")
        assertNull(chooseRoute(tailnet, MeshRoute.Mesh, view(tailnet, full, MobilePlatform.Ios)), "mesh on iOS")
        assertNull(chooseRoute(meshOnly, MeshRoute.Tailnet, view(meshOnly, full)), "tailnet with no daemon")
        assertEquals(MeshRoute.Mesh, chooseRoute(tailnet, MeshRoute.Mesh, view(tailnet, full))!!.route)
        assertEquals(MeshRoute.Tailnet, chooseRoute(tailnet.copy(route = MeshRoute.Mesh), MeshRoute.Tailnet, view(tailnet, full))!!.route)
    }

    // ── the scanners ────────────────────────────────────────────────────────────────────

    private val keys = MeshSessionKeys(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(32) { 3 }, ByteArray(32) { 4 })

    @Test
    fun `the gateway scanner refuses a token without a session pairing and stores nothing`() {
        val tokens = InMemoryMeshTokenStore()
        val r = applyGatewayScan(tok32, tailnet, InMemoryMeshSessionKeyStore(), tokens)
        assertEquals(GatewayScan.Refused(GatewayScanProblem.NeedsSessionPairing), r)
        assertNull(tokens.load(), "a refused scan must leave no half-pairing")
    }

    @Test
    fun `the gateway scanner stores the token, and the endpoint only when it is not the default`() {
        val store = InMemoryMeshSessionKeyStore().also { it.save(keys) }
        val tokens = InMemoryMeshTokenStore()
        val a = assertIs<GatewayScan.Saved>(applyGatewayScan(tok32, meshOnly, store, tokens))
        assertEquals("", a.config.meshGateway)
        assertContentEquals(unhex(tok32), tokens.load())
        val b = assertIs<GatewayScan.Saved>(applyGatewayScan("meshmore://gateway?v=1&host=127.0.0.1:41999&token=$tok32", meshOnly, store, tokens))
        assertEquals("127.0.0.1:41999", b.config.meshGateway)
        assertEquals(MeshGatewayEndpoint("127.0.0.1", 41999), b.config.meshGatewayEndpoint)
        // Scanning the plain code again returns to the default.
        assertEquals("", (applyGatewayScan(tok32, b.config, store, tokens) as GatewayScan.Saved).config.meshGateway)
    }

    @Test
    fun `the gateway scanner refuses what is not a gateway code, and a failing store`() {
        val store = InMemoryMeshSessionKeyStore().also { it.save(keys) }
        val tokens = InMemoryMeshTokenStore()
        assertEquals(GatewayScan.Refused(GatewayScanProblem.NotAGatewayCode), applyGatewayScan("ldmux://pair?v=1", meshOnly, store, tokens))
        assertNull(tokens.load())
        val broken = object : MeshTokenStore {
            override fun load(): ByteArray? = null
            override fun save(token: ByteArray) = error("keystore")
            override fun clear() {}
        }
        assertEquals(GatewayScan.Refused(GatewayScanProblem.StoreFailed), applyGatewayScan(tok32, meshOnly, store, broken))
    }

    private fun configStore(initial: LdmuxConfig, keyStore: InMemoryMeshSessionKeyStore): Pair<ConfigStore, () -> LdmuxConfig> {
        var saved = initial
        val s = ConfigStore(initial, MeshEnrollment(keyStore) { ByteArray(32) { 9 } to ByteArray(32) { 8 } }) { saved = it }
        return s to { saved }
    }

    private val meshMaterial = MeshKeyMaterial(ByteArray(32) { 5 }, ByteArray(32) { 6 }, ByteArray(32) { 7 })
    private val meshOnlyLink =
        "ldmux://pair?v=1&name=debian&mv=1&mk=${"05".repeat(32)}&psk=${"06".repeat(32)}&mr=${"07".repeat(32)}"

    @Test
    fun `a mesh-only code enrols the keys and leaves the tailnet pairing exactly as it was`() {
        val keyStore = InMemoryMeshSessionKeyStore()
        val (store, saved) = configStore(tailnet.copy(pairName = "work"), keyStore)
        assertEquals(MeshPairingScan.Enrolled(meshOnly = true), applyMeshPairingScan(meshOnlyLink, store))
        val k = assertNotNull(keyStore.load())
        assertContentEquals(ByteArray(32) { 5 }, k.hostPublic)
        assertContentEquals(ByteArray(32) { 7 }, k.hostRadio)
        assertEquals(tailnet.copy(pairName = "work"), store.config.value, "a mesh-only code must not touch the daemon pairing")
        assertEquals(tailnet.copy(pairName = "work"), saved())
    }

    @Test
    fun `on a device with no tailnet pairing a mesh-only code gives it a mesh pairing and still no daemon`() {
        val keyStore = InMemoryMeshSessionKeyStore()
        val (store, _) = configStore(LdmuxConfig.DEFAULT, keyStore)
        assertEquals(MeshPairingScan.Enrolled(true), applyMeshPairingScan(meshOnlyLink, store))
        assertFalse(store.config.value.isPaired)
        assertEquals("", store.config.value.host)
        assertEquals("debian", store.config.value.pairName)
        assertTrue(keyStore.hasPeer)
    }

    @Test
    fun `the mesh scanner refuses a code with no mesh keys, and what is not a code`() {
        val keyStore = InMemoryMeshSessionKeyStore()
        val (store, saved) = configStore(LdmuxConfig.DEFAULT, keyStore)
        val plain = "ldmux://pair?v=1&host=100.100.0.1:7625&token=${"0".repeat(64)}&name=x"
        assertEquals(MeshPairingScan.Refused(MeshPairingProblem.NoMeshKeys), applyMeshPairingScan(plain, store))
        assertEquals(LdmuxConfig.DEFAULT, store.config.value, "the mesh entry point must not pair the tailnet by accident")
        assertEquals(MeshPairingScan.Refused(MeshPairingProblem.NotAnLdmuxCode), applyMeshPairingScan("nope", store))
        assertEquals(MeshPairingScan.Refused(MeshPairingProblem.NotAnLdmuxCode), applyMeshPairingScan("ldmux://pair?v=1&token=${"0".repeat(64)}&mv=1&mk=${"05".repeat(32)}&psk=${"06".repeat(32)}", store))
        assertNull(keyStore.load())
        assertEquals(LdmuxConfig.DEFAULT, saved())
    }

    @Test
    fun `a full code still pairs the tailnet exactly as before, with the mesh keys alongside`() {
        val keyStore = InMemoryMeshSessionKeyStore()
        val (store, _) = configStore(LdmuxConfig.DEFAULT, keyStore)
        val p = Pairing("100.100.0.1:7625", "0".repeat(64), "debian", meshMaterial)
        assertEquals(MeshEnrollOutcome.Enrolled, store.pair(p))
        assertTrue(store.config.value.isPaired)
        assertEquals("100.100.0.1:7625", store.config.value.host)
        assertTrue(keyStore.hasPeer)
    }

    @Test
    fun `unpairing the mesh forgets the keys, the token and the endpoint, and leaves the tailnet alone`() {
        val keyStore = InMemoryMeshSessionKeyStore().also { it.save(keys) }
        val tokens = InMemoryMeshTokenStore(unhex(tok32))
        var saved = tailnet
        val store = ConfigStore(
            tailnet.copy(route = MeshRoute.Mesh, meshGateway = "127.0.0.1:41999"),
            MeshEnrollment(keyStore, tokens) { null },
        ) { saved = it }
        store.unpairMesh()
        assertNull(keyStore.load()); assertNull(tokens.load())
        assertEquals(tailnet, saved, "tailnet pairing intact, route back on the tailnet, endpoint forgotten")
    }

    @Test
    fun `unpairing the machine also forgets the gateway token, which used to be left behind`() {
        val keyStore = InMemoryMeshSessionKeyStore().also { it.save(keys) }
        val tokens = InMemoryMeshTokenStore(unhex(tok32))
        val store = ConfigStore(tailnet, MeshEnrollment(keyStore, tokens) { null }) { }
        store.unpair()
        assertNull(keyStore.load())
        assertNull(tokens.load(), "a token for a gateway whose keys are gone must not survive")
        assertFalse(store.config.value.isPaired)
    }

    @Test
    fun `the screen plan needs no tailnet pairing, and a gateway endpoint travels with it`() {
        val lib = assertNotNull(loadSessionLib(), "the ABI tests load the real library, so this must too")
        val keyStore = InMemoryMeshSessionKeyStore().also { it.save(keys.withRadio(ByteArray(32) { 7 })) }
        val tokens = InMemoryMeshTokenStore(unhex(tok32))
        val cfg = meshOnly.copy(meshGateway = "127.0.0.1:41999")
        val plan = assertNotNull(meshScreenPlan(cfg, keyStore, tokens, lib))
        assertEquals(MeshGatewayEndpoint("127.0.0.1", 41999), plan.endpoint)
        // The same config with the route on the tailnet shows no mesh screen.
        assertNull(meshScreenPlan(cfg.copy(route = MeshRoute.Tailnet), keyStore, tokens, lib))
        // Missing token: no plan, and the reason is the route's own.
        assertNull(meshScreenPlan(cfg, keyStore, InMemoryMeshTokenStore(), lib))
        // iOS: no plan even with everything present.
        assertNull(meshScreenPlan(cfg, keyStore, tokens, lib, platform = MobilePlatform.Ios))
    }

    private fun MeshSessionKeys.withRadio(r: ByteArray) = MeshSessionKeys(localPrivate, localPublic, hostPublic, psk, r)
    private fun unhex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
