package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

class LinkTest {
    private val paired = LdmuxConfig(host = "100.100.0.1:7625", token = TOKEN, pairName = "debian", deviceId = "d1")

    @Test
    fun `a refused token is rejected, and everything else is unreachable`() {
        assertEquals(LinkState.Rejected, linkStateFromProbe(401))
        // 400 is the daemon saying "that was not an upgrade" to an accepted token.
        for (s in listOf(400, 403, 404, 500, null)) assertEquals(LinkState.Unreachable, linkStateFromProbe(s))
    }

    @Test
    fun `the launcher says what it is paired with and how the link is, in a few words`() {
        assertEquals("unpaired", linkLabel(LinkState.Unpaired, LdmuxConfig.DEFAULT))
        assertEquals("debian", linkLabel(LinkState.Online, paired))
        assertEquals("debian · unreachable", linkLabel(LinkState.Unreachable, paired))
        assertEquals("debian · rejected", linkLabel(LinkState.Rejected, paired))
        // A pairing made before names existed is called by its host.
        assertEquals("100.100.0.1:7625", linkLabel(LinkState.Online, paired.copy(pairName = "")))
    }

    @Test
    fun `the token is never in the URL, so it is never in a log`() {
        val e = paired.endpoint!!
        assertEquals("ws://100.100.0.1:7625/ws", e.wsUrl)
        assertFalse(TOKEN in e.wsUrl)
        assertFalse(TOKEN in e.probeUrl)
        assertFalse(TOKEN in e.toString())
        assertEquals("Bearer $TOKEN", e.authorization)
    }

    @Test
    fun `unpaired has no endpoint, and pairing then unpairing keeps every other setting`() {
        assertNull(LdmuxConfig.DEFAULT.endpoint)
        assertFalse(LdmuxConfig.DEFAULT.isPaired)
        val base = LdmuxConfig.DEFAULT.copy(fontSp = 15f, side = "right", deviceId = "d1", deviceLabel = "Moto")
        val p = base.paired(Pairing("100.100.0.1:7625", TOKEN, "debian"))
        assertTrue(p.isPaired)
        assertEquals("debian", p.pairedName)
        assertEquals(base, p.unpaired())
    }

    @Test
    fun `only the connection fields decide whether a page reconnects`() {
        // A display setting must not rebuild a page: a rebuild reconnects, and the
        // first attach takes the shell over (ADR-0010).
        assertEquals(paired.connectionKey, paired.copy(fontSp = 20f, side = "right", compact = false).connectionKey)
        assertTrue(paired.connectionKey != paired.copy(token = "f".repeat(64)).connectionKey)
        assertTrue(paired.connectionKey != paired.copy(host = "100.64.0.1:7625").connectionKey)
        assertTrue(paired.connectionKey != paired.unpaired().connectionKey)
    }

    @Test
    fun `the store saves only real changes, and pairs and unpairs`() {
        val saved = mutableListOf<LdmuxConfig>()
        val store = ConfigStore(LdmuxConfig.DEFAULT) { saved += it }
        store.update(LdmuxConfig.DEFAULT)
        assertEquals(0, saved.size)
        store.pair(Pairing("100.100.0.1:7625", TOKEN, "debian"))
        assertTrue(store.config.value.isPaired)
        store.unpair()
        assertFalse(store.config.value.isPaired)
        assertEquals(2, saved.size)
    }

    @Test
    fun `a host saved by an older version becomes the pairing shape`() {
        assertEquals("100.100.0.1:7625", storedHost("100.100.0.1:7625"))
        assertEquals("100.100.0.1:7625", storedHost("http://100.100.0.1:7625/"))
        assertEquals("", storedHost("localhost:7623"))
        assertEquals("", storedHost(null))
    }

    @Test
    fun `a link from outside is asked about, unless it is the pairing already held`() {
        val link = "ldmux://pair?v=1&host=100.100.0.1:7625&token=$TOKEN&name=debian"
        assertIs<InboundPairing.Ask>(inboundPairing(link, LdmuxConfig.DEFAULT))
        assertIs<InboundPairing.Same>(inboundPairing(link, paired))
        assertIs<InboundPairing.Ask>(inboundPairing(link, paired.copy(token = "f".repeat(64))))
        assertIs<InboundPairing.Invalid>(inboundPairing("https://example.com", paired))
    }

    @Test
    fun `a scan pairs, says what was wrong, or says nothing when backed out of`() {
        val link = "ldmux://pair?v=1&host=100.100.0.1:7625&token=$TOKEN"
        assertIs<PairOutcome.Paired>(scanOutcome(ScanResult.Code(link)))
        assertEquals(PairOutcome.Problem("not an ldmux code"), scanOutcome(ScanResult.Code("https://example.com")))
        assertEquals(PairOutcome.Problem("scanner unavailable"), scanOutcome(ScanResult.Unavailable))
        assertEquals(PairOutcome.Nothing, scanOutcome(ScanResult.Cancelled))
    }
}
