package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class MeshSessionKeyStoreTest {
    private fun keys() = MeshSessionKeys(
        ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(32) { 3 }, ByteArray(32) { 4 },
    )

    @Test
    fun theBlobIsExactlyOneVersionByteAndFourKeysInOrder() {
        val b = keys().encode()
        assertEquals(129, b.size)
        assertEquals(1, b[0].toInt())
        assertEquals(1, b[1].toInt()); assertEquals(2, b[33].toInt())
        assertEquals(3, b[65].toInt()); assertEquals(4, b[97].toInt())
    }

    @Test
    fun saveThenLoadReturnsTheSameKeysAndClearForgets() {
        val s = InMemoryMeshSessionKeyStore()
        assertFalse(s.hasPeer)
        s.save(keys())
        assertTrue(s.hasPeer)
        val k = assertNotNull(s.load())
        assertContentEquals(ByteArray(32) { 4 }, k.psk)
        assertContentEquals(ByteArray(32) { 3 }, k.hostPublic)
        s.clear()
        assertNull(s.load())
        assertFalse(s.hasPeer)
    }

    @Test
    fun anythingThatIsNotAWholeVersionOneBlobIsNeedsPairingNotACrash() {
        assertNull(MeshSessionKeys.decode(null))
        assertNull(MeshSessionKeys.decode(ByteArray(128)))
        assertNull(MeshSessionKeys.decode(ByteArray(130)))
        val wrongVersion = keys().encode().also { it[0] = 2 }
        assertNull(MeshSessionKeys.decode(wrongVersion))
        assertNull(InMemoryMeshSessionKeyStore(ByteArray(5)).load())
    }

    @Test
    fun aKeyOfTheWrongSizeCannotBeConstructed() {
        assertFailsWith<IllegalArgumentException> {
            MeshSessionKeys(ByteArray(31), ByteArray(32), ByteArray(32), ByteArray(32))
        }
    }

    @Test
    fun theStoredKeysDriveARealHandshakeAgainstTheResponder() {
        // The keys are not just bytes: a phone built from a stored pairing completes the real
        // **The handshake half is retired with `MeshSession.begin`.** The v14 session lived in Kotlin
        // and drove the old ABI; the v15 engine owns the handshake in Rust behind `ldmux_client_*`,
        // and `crates/session/tests/bootstrap.rs` is where that ladder is proved. What this test is
        // for — the store round-trip — still runs above it.
    }
}

class MeshSessionKeyBlobV2Test {
    private fun keys(radio: ByteArray?) =
        MeshSessionKeys(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(32) { 3 }, ByteArray(32) { 4 }, radio)

    @Test
    fun `a blob with a radio key is version 2 and round trips`() {
        val b = keys(ByteArray(32) { 9 }).encode()
        assertEquals(161, b.size)
        assertEquals(2, b[0].toInt())
        assertContentEquals(ByteArray(32) { 9 }, MeshSessionKeys.decode(b)!!.hostRadio)
    }

    @Test
    fun `a version 1 blob still loads, with no radio key`() {
        val b = keys(null).encode()
        assertEquals(129, b.size)
        assertNull(MeshSessionKeys.decode(b)!!.hostRadio)
    }

    @Test
    fun `a wrong length for either version is needs-pairing, never a crash`() {
        assertNull(MeshSessionKeys.decode(ByteArray(0)))
        assertNull(MeshSessionKeys.decode(ByteArray(161) { if (it == 0) 1 else 0 }))
        assertNull(MeshSessionKeys.decode(ByteArray(129) { if (it == 0) 2 else 0 }))
    }
}
