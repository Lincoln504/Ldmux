package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.LdmuxConfig
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The reconnect ladder and the mode's one rule, as values rather than as behaviour buried in an
 * effect. Both are the kind of policy a later edit inverts without noticing: a ladder that stops
 * climbing strands a user on a dead link, and a mode that leaves live armed on a public channel
 * puts every keystroke on the air.
 */
class MeshReconnectTest {
    @Test
    fun `the rebuild ladder climbs to its cap and stays there`() {
        assertEquals(2_000L, rebuildCapMs(0))
        assertEquals(4_000L, rebuildCapMs(1))
        assertEquals(8_000L, rebuildCapMs(2))
        assertEquals(16_000L, rebuildCapMs(3))
        assertEquals(32_000L, rebuildCapMs(4))
        assertEquals(64_000L, rebuildCapMs(5))
        assertEquals(128_000L, rebuildCapMs(6))
        assertEquals(256_000L, rebuildCapMs(7))
        // Never unbounded, and never stops: 256 s is the slowest it ever gets, so a link that
        // comes back is picked up without the user pressing anything.
        assertEquals(256_000L, rebuildCapMs(8))
        assertEquals(256_000L, rebuildCapMs(50))
        // A negative index cannot make `shl` throw or go backwards.
        assertEquals(2_000L, rebuildCapMs(-3))
    }

    @Test
    fun `a public channel holds typing whatever the user chose`() {
        val public = LdmuxConfig.DEFAULT.copy(meshPublicChannel = true)
        assertTrue(public.copy(meshInputMode = MeshInputMode.Live).meshHoldsInput)
        assertTrue(public.copy(meshInputMode = MeshInputMode.ManualSync).meshHoldsInput)
    }

    @Test
    fun `a private channel does what the user chose`() {
        val private = LdmuxConfig.DEFAULT.copy(meshPublicChannel = false)
        assertFalse(private.copy(meshInputMode = MeshInputMode.Live).meshHoldsInput)
        assertTrue(private.copy(meshInputMode = MeshInputMode.ManualSync).meshHoldsInput)
    }

    @Test
    fun `the default is live typing on a private channel`() {
        // The default config has not classified its radio, which reads as public (the safe
        // reading), so the *default* holds; what must stay live is the choice on a private one.
        assertEquals(MeshInputMode.Live, LdmuxConfig.DEFAULT.meshInputMode)
        assertFalse(LdmuxConfig.DEFAULT.copy(meshPublicChannel = false).meshHoldsInput)
    }
}
