package io.ldmux.mobile.mesh

import io.ldmux.mobile.LdmuxConfig
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

/**
 * The silence bound is a function of distance, not a constant.
 *
 * Twenty seconds is right for a direct link and wrong for this mesh, whose own floods were
 * measured at 18-29 hops: a record's round trip through twenty repeaters is twenty transmissions
 * each way at about 0.6 s apiece, so the old constant declared a working long-range session dead
 * and rebuilt it - a handshake is a dozen records - while the answer was still on the air.
 */
/**
 * **A deadline for a link next door is not a deadline for a mesh.** MeshCore's forward delay is up
 * to two airtimes per hop, so an answer over ten repeaters is twenty-odd seconds before anything is
 * late; a flat six-second attach or list deadline re-asked while the answer was still on the air and
 * spent the records it exists to save.
 */
class MeshAnswerDeadlineTest {
    @Test
    fun a_neighbour_keeps_the_flat_deadline() {
        // `routeHops` counts *repeaters*: 0 is a direct neighbour.
        assertEquals(6_000L, answerDeadlineMs(6_000L, 0))
        assertEquals(6_000L, answerDeadlineMs(6_000L, null), "no route yet is a neighbour")
        assertEquals(8_000L, answerDeadlineMs(6_000L, 1), "one repeater adds its forward delay")
    }

    @Test
    fun every_hop_buys_two_seconds_of_forward_delay() {
        assertEquals(26_000L, answerDeadlineMs(6_000L, 10))
        assertEquals(46_000L, answerDeadlineMs(6_000L, 20))
        // A nonsense hop count cannot make the page wait for ever.
        assertEquals(86_000L, answerDeadlineMs(6_000L, 1_000))
    }

    @Test
    fun the_resync_floor_scales_the_same_way() {
        assertEquals(4_000L, resyncFirstRetryMs(0))
        assertEquals(24_000L, resyncFirstRetryMs(10))
    }
}

class MeshSilenceBoundTest {
    @Test
    fun a_direct_link_still_has_room_for_a_full_repaint() {
        // **Sixty seconds, not twenty.** One full screen is six or seven records plus their
        // acknowledgements, which is more than twenty seconds at 62.5 kHz — the old bound declared a
        // link that was painting perfectly well to be dead, mid-paint.
        assertEquals(60_000L, silenceFailureMs(0))
        assertEquals(60_000L, silenceFailureMs(null), "no route yet is treated as direct")
    }

    @Test
    fun every_hop_buys_room_for_its_round_trip() {
        assertEquals(75_000L, silenceFailureMs(10))
        assertEquals(90_000L, silenceFailureMs(20))
        // The ceiling keeps a genuinely dead link from hanging for more than two minutes.
        assertEquals(120_000L, silenceFailureMs(64))
        assertEquals(120_000L, silenceFailureMs(1_000), "a nonsense hop count cannot hang the page")
    }
}

/**
 * A record's size is a property of the path, and the session is sealed at the handshake.
 *
 * Walking out of direct range onto a repeater chain shrinks what the link can carry while the
 * session keeps sealing at the old size, so every record is refused by the packet builder and the
 * session retries the same oversized record until the silence bound ends it. The rule is a value a
 * test can hold: a smaller link needs a re-seal, a larger one does not, and no answer is no reason
 * to handshake again.
 */
class MeshSessionResizeTest {
    @Test
    fun a_shrinking_route_needs_a_new_session() {
        assertTrue(needsResize(171, 148))
        assertTrue(needsResize(171, 122))
    }

    @Test
    fun a_growing_route_does_not() {
        // A record sealed for a longer path still fits a shorter one, so nothing is re-sealed.
        assertFalse(needsResize(122, 171))
        assertFalse(needsResize(171, 171))
    }

    @Test
    fun no_answer_is_not_a_reason_to_handshake() {
        assertFalse(needsResize(171, 0))
        assertFalse(needsResize(0, 171), "a session that was never sealed has nothing to re-seal")
    }
}
