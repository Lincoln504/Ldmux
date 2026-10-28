package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.ConfigStore
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.MeshKeyMaterial
import dev.ldmux.mobile.Pairing
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class MeshEnrollmentTest {
    private val tok = "0123456789abcdef".repeat(4)
    private fun mesh(b: Int) = MeshKeyMaterial(ByteArray(32) { b.toByte() }, ByteArray(32) { (b + 1).toByte() })
    private fun pairing(host: String = "100.100.0.1:7625", mesh: MeshKeyMaterial? = null) =
        Pairing(host, tok, "debian", mesh)

    private var counter = 0
    private val store = InMemoryMeshSessionKeyStore()
    private val enroll = MeshEnrollment(store) { counter++; ByteArray(32) { counter.toByte() } to ByteArray(32) { (counter + 100).toByte() } }

    @Test
    fun `a link with keys stores a fresh phone keypair, the host key and the secret`() {
        assertEquals(MeshEnrollOutcome.Enrolled, enroll.onPaired("", pairing(mesh = mesh(5))))
        val k = assertNotNull(store.load())
        assertContentEquals(ByteArray(32) { 1 }, k.localPrivate)
        assertContentEquals(ByteArray(32) { 5 }, k.hostPublic)
        assertContentEquals(ByteArray(32) { 6 }, k.psk)
        assertTrue(store.hasPeer)
    }

    @Test
    fun `the host radio key rides along when the link had it, and without it the blob is the old v1 shape`() {
        enroll.onPaired("", pairing(mesh = MeshKeyMaterial(ByteArray(32) { 5 }, ByteArray(32) { 6 }, ByteArray(32) { 7 })))
        val k = assertNotNull(store.load())
        assertContentEquals(ByteArray(32) { 7 }, k.hostRadio)
        assertEquals(MeshSessionKeys.ENCODED_WITH_RADIO, k.encode().size)
        enroll.onPaired("100.100.0.1:7625", pairing(mesh = mesh(5)))
        val v1 = assertNotNull(store.load())
        assertNull(v1.hostRadio)
        assertEquals(MeshSessionKeys.ENCODED, v1.encode().size)
    }

    @Test
    fun `a re-scan replaces the pairing and issues a new phone key`() {
        enroll.onPaired("", pairing(mesh = mesh(5)))
        val first = store.load()!!
        enroll.onPaired("100.100.0.1:7625", pairing(mesh = mesh(9)))
        val second = store.load()!!
        assertContentEquals(ByteArray(32) { 9 }, second.hostPublic)
        assertContentEquals(ByteArray(32) { 10 }, second.psk)
        assertFalse(first.localPrivate.contentEquals(second.localPrivate), "re-pairing is a new phone static key")
    }

    @Test
    fun `a v1 link to the same machine leaves the mesh pairing alone`() {
        enroll.onPaired("", pairing(mesh = mesh(5)))
        assertEquals(MeshEnrollOutcome.NoMeshKeys, enroll.onPaired("100.100.0.1:7625", pairing()))
        assertTrue(store.hasPeer)
    }

    @Test
    fun `a v1 link to a different machine forgets the old machine's keys`() {
        enroll.onPaired("", pairing(mesh = mesh(5)))
        enroll.onPaired("100.100.0.1:7625", pairing(host = "100.100.0.2:7625"))
        assertFalse(store.hasPeer)
    }

    @Test
    fun `no library means nothing is stored and it says so`() {
        val e = MeshEnrollment(store) { null }
        assertEquals(MeshEnrollOutcome.LibraryUnavailable, e.onPaired("", pairing(mesh = mesh(5))))
        assertFalse(store.hasPeer)
    }

    @Test
    fun `a failing secure store does not take the pairing down`() {
        val broken = object : MeshSessionKeyStore {
            override fun load(): MeshSessionKeys? = null
            override fun save(keys: MeshSessionKeys) = error("keystore")
            override fun clear() = error("keystore")
        }
        val e = MeshEnrollment(broken) { ByteArray(32) to ByteArray(32) }
        assertEquals(MeshEnrollOutcome.StoreFailed, e.onPaired("", pairing(mesh = mesh(5))))
        e.onUnpaired()
        e.onPaired("old", pairing())
    }

    @Test
    fun `the config store pairs, enrols, and forgets the keys on unpair`() {
        val saved = ArrayList<LdmuxConfig>()
        val cs = ConfigStore(LdmuxConfig.DEFAULT, enroll) { saved += it }
        cs.pair(pairing(mesh = mesh(5)))
        assertEquals("100.100.0.1:7625", cs.config.value.host)
        assertTrue(store.hasPeer)
        cs.unpair()
        assertFalse(store.hasPeer)
        assertEquals("", cs.config.value.host)
        // The keys are not part of the persisted config.
        assertTrue(saved.none { "psk" in it.toString() })
    }

    @Test
    fun `a config store built without enrolment behaves exactly as before`() {
        val cs = ConfigStore(LdmuxConfig.DEFAULT) {}
        cs.pair(pairing(mesh = mesh(5)))
        assertTrue(cs.config.value.isPaired)
        cs.unpair()
        assertNull(cs.config.value.host.takeIf { it.isNotEmpty() })
    }
}
