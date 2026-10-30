package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.LdmuxConfig
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The rules for taking an offer from MeshMore on this phone, with no Binder and no Keystore. */
class MeshHandoffTest {
    private val config = LdmuxConfig(host = "", token = "")
    private val keys = InMemoryMeshSessionKeyStore().also {
        it.save(MeshSessionKeys(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(32) { 3 }, ByteArray(32) { 4 }, ByteArray(32) { 5 }))
    }
    private fun token(b: Int) = ByteArray(MeshGateway.PAIRING_TOKEN_BYTES) { b.toByte() }
    private fun offer(
        state: String = "running", port: Int = 41731, token: ByteArray? = token(7), version: Int = MESH_HANDOFF_VERSION,
    ) = MeshHandoffResult.Offer(MeshHandoffOffer(version, state, port, token, null, null))

    @Test fun `a missing token is filled`() {
        val tokens = InMemoryMeshTokenStore()
        val applied = applyMeshHandoff(offer(), config, keys, tokens)
        assertIs<MeshHandoffApplied.Linked>(applied)
        assertContentEquals(token(7), tokens.load())
        assertNull(meshHandoffLine(applied))
    }

    @Test fun `the default port is stored as the default, another port as itself`() {
        val a = applyMeshHandoff(offer(port = 41731), config, keys, InMemoryMeshTokenStore()) as MeshHandoffApplied.Linked
        assertEquals("", a.config.meshGateway)
        // MeshMore falls back to an ephemeral port when its preferred one is reserved: that port is the endpoint.
        val b = applyMeshHandoff(offer(port = 38211), config, keys, InMemoryMeshTokenStore()) as MeshHandoffApplied.Linked
        assertEquals("127.0.0.1:38211", b.config.meshGateway)
    }

    @Test fun `a different token never overwrites a stored one by itself`() {
        val tokens = InMemoryMeshTokenStore(token(9))
        val applied = applyMeshHandoff(offer(token = token(7)), config, keys, tokens)
        assertEquals(MeshHandoffApplied.KeptExisting, applied)
        assertContentEquals(token(9), tokens.load())
        assertNotNull(meshHandoffLine(applied))
    }

    @Test fun `a stored token that has just failed is replaced when asked`() {
        val tokens = InMemoryMeshTokenStore(token(9))
        val applied = applyMeshHandoff(offer(token = token(7)), config, keys, tokens, replaceStale = true)
        assertIs<MeshHandoffApplied.Linked>(applied)
        assertContentEquals(token(7), tokens.load())
    }

    @Test fun `the same token refreshes the endpoint and writes nothing`() {
        val failing = object : MeshTokenStore {
            override fun load() = token(7)
            override fun save(token: ByteArray) = error("must not write an unchanged token")
            override fun clear() {}
        }
        val applied = applyMeshHandoff(offer(port = 40000), config, keys, failing)
        assertEquals("127.0.0.1:40000", (applied as MeshHandoffApplied.Linked).config.meshGateway)
    }

    @Test fun `no token is offered unless the gateway is running`() {
        for (state in listOf("off", "waitingForRadio", "failed", "anything else")) {
            val tokens = InMemoryMeshTokenStore()
            // Even a token wrongly attached to a non-running state must not be taken.
            val applied = applyMeshHandoff(offer(state = state, token = token(7)), config, keys, tokens)
            assertIs<MeshHandoffApplied.NotServing>(applied)
            assertNull(tokens.load(), state)
            assertTrue(meshHandoffLine(applied)!!.startsWith("MeshMore"), state)
        }
    }

    @Test fun `a running offer with no usable token is unreadable, not a link`() {
        for (bad in listOf<ByteArray?>(null, ByteArray(15), ByteArray(32))) {
            val tokens = InMemoryMeshTokenStore()
            val applied = applyMeshHandoff(offer(token = bad), config, keys, tokens)
            assertEquals(MeshHandoffApplied.Unavailable(MeshHandoffFailure.BadReply).why, (applied as MeshHandoffApplied.Unavailable).why)
            assertNull(tokens.load())
        }
    }

    @Test fun `a port outside the range and another contract version are unreadable`() {
        assertIs<MeshHandoffApplied.Unavailable>(applyMeshHandoff(offer(port = 0), config, keys, InMemoryMeshTokenStore()))
        assertIs<MeshHandoffApplied.Unavailable>(applyMeshHandoff(offer(port = 70000), config, keys, InMemoryMeshTokenStore()))
        assertIs<MeshHandoffApplied.Unavailable>(applyMeshHandoff(offer(version = 2), config, keys, InMemoryMeshTokenStore()))
    }

    @Test fun `a token with no session keys is refused as a half pairing, as a scanned code is`() {
        val tokens = InMemoryMeshTokenStore()
        val applied = applyMeshHandoff(offer(), config, InMemoryMeshSessionKeyStore(), tokens)
        assertEquals(MeshHandoffApplied.NeedsSessionPairing, applied)
        assertNull(tokens.load())
    }

    @Test fun `a store that refuses the write leaves nothing and says so`() {
        val broken = object : MeshTokenStore {
            override fun load(): ByteArray? = null
            override fun save(token: ByteArray) = error("keystore")
            override fun clear() {}
        }
        assertEquals(MeshHandoffApplied.StoreFailed, applyMeshHandoff(offer(), config, keys, broken))
    }

    @Test fun `every failure has a line in plain words`() {
        for (why in MeshHandoffFailure.entries) {
            val line = meshHandoffLine(MeshHandoffApplied.Unavailable(why))
            assertTrue(!line.isNullOrBlank(), why.name)
        }
        assertTrue(meshHandoffLine(MeshHandoffApplied.NeedsSessionPairing)!!.isNotBlank())
        assertTrue(meshHandoffLine(MeshHandoffApplied.StoreFailed)!!.isNotBlank())
    }

    @Test fun `a reply with no state is unreadable`() {
        val r = meshHandoffOf(1, null, 1, null, null, null)
        assertEquals(MeshHandoffFailure.BadReply, (r as MeshHandoffResult.Failed).why)
        assertIs<MeshHandoffResult.Offer>(meshHandoffOf(1, "off", 0, null, null, null))
    }

    @Test fun `the automatic attempt is wanted only when the keys are in and the token is not`() {
        assertTrue(meshHandoffWanted(MeshCredentialState(hasSessionKeys = true, hasToken = false, hasPeer = true)))
        assertTrue(!meshHandoffWanted(MeshCredentialState(hasSessionKeys = true, hasToken = true, hasPeer = true)))
        assertTrue(!meshHandoffWanted(MeshCredentialState(hasSessionKeys = false, hasToken = false, hasPeer = false)))
    }
}
