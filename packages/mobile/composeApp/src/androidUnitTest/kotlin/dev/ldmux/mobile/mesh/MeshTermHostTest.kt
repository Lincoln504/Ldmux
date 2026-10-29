package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import dev.ldmux.mobile.LdmuxConfig

class MeshTermHostTest {
    private fun keys(radio: ByteArray?) =
        MeshSessionKeys(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(32) { 3 }, ByteArray(32) { 4 }, radio)

    @Test
    fun `the three not-working states read differently, and so does every host close`() {
        val labels = listOf(
            meshTermLabel(MeshTermView.LinkDown(MeshLinkState.Unreachable)),
            meshTermLabel(MeshTermView.ProfileMissing),
            meshTermLabel(MeshTermView.HostClosed(MeshTerm.Closed.EXITED)),
            meshTermLabel(MeshTermView.Handshaking),
        )
        assertEquals(labels.size, labels.toSet().size, labels.toString())
        assertTrue(labels.all { it.startsWith("mesh · ") })
        val closes = listOf(null, 1, 2, 3, 4, 5).map { meshTermLabel(MeshTermView.HostClosed(it)) }
        assertEquals(closes.size, closes.toSet().size)
    }

    @Test
    fun `the absent features are named, and a full list is named whole`() {
        assertEquals("", meshAbsentLine(emptyList()))
        val line = meshAbsentLine(MeshTermFeature.entries)
        for (f in MeshTermFeature.entries) assertTrue(f.name.lowercase() in line, line)
    }

    @Test
    fun `the budget line is in records and says when it is not reported`() {
        assertEquals("send budget: not reported", meshBudgetLine(null))
        assertEquals("send budget: none left this hour", meshBudgetLine(0))
        assertTrue("58 records" in meshBudgetLine(58))
        assertTrue("255 or more" in meshBudgetLine(255))
        // No seconds, no preset: the app must not do radio arithmetic.
        assertFalse(listOf(null, 0, 58, 255).any { "second" in meshBudgetLine(it) || "ms" in meshBudgetLine(it) })
    }

    @Test
    fun `credentials need all three, and a radio key is part of what is needed`() {
        val tok = ByteArray(16) { 9 }
        val full = InMemoryMeshSessionKeyStore().apply { save(keys(ByteArray(32) { 7 })) }
        val (s1, c1) = readMeshCredentials(full, InMemoryMeshTokenStore(tok))
        assertTrue(s1.complete); assertNotNull(c1)
        val (s2, c2) = readMeshCredentials(InMemoryMeshSessionKeyStore().apply { save(keys(null)) }, InMemoryMeshTokenStore(tok))
        assertEquals(MeshCredentialState(hasSessionKeys = true, hasToken = true, hasPeer = false), s2); assertNull(c2)
        val (s3, _) = readMeshCredentials(full, InMemoryMeshTokenStore(null))
        assertEquals(MeshCredentialState(true, false, true), s3)
        val (s4, _) = readMeshCredentials(InMemoryMeshSessionKeyStore(), InMemoryMeshTokenStore(ByteArray(15)))
        assertEquals(MeshCredentialState(false, false, false), s4, "a wrong-length token is no token")
    }

    @Test
    fun `a broken secure store reads as needs pairing and never throws`() {
        val broken = object : MeshSessionKeyStore {
            override fun load(): MeshSessionKeys? = error("keystore")
            override fun save(keys: MeshSessionKeys) = error("keystore")
            override fun clear() = error("keystore")
        }
        val brokenTokens = object : MeshTokenStore {
            override fun load(): ByteArray? = error("keystore")
            override fun save(token: ByteArray) = error("keystore")
            override fun clear() = error("keystore")
        }
        val (state, creds) = readMeshCredentials(broken, brokenTokens)
        assertEquals(MeshCredentialState(false, false, false), state); assertNull(creds)
    }

    @Test
    fun `a tailnet config never touches secure storage`() {
        // The stores are Android-only; on the JVM, reaching them would throw. Returning null proves
        // the tailnet path does not read them.
        val tailnet = LdmuxConfig(host = "100.100.0.1:7625", token = "0".repeat(64))
        assertNull(meshScreenFor(tailnet))
    }
}
