package io.ldmux.mobile.mesh

import io.ldmux.mobile.LdmuxConfig
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
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
        // A nonsense hop count is clamped to the firmware's own longest path.
        assertEquals(6_000L + 63 * 2_000L, answerDeadlineMs(6_000L, 1_000))
    }

    @Test
    fun the_resync_floor_scales_the_same_way() {
        assertEquals(4_000L, resyncFirstRetryMs(0))
        assertEquals(24_000L, resyncFirstRetryMs(10))
    }

    /// **The handshake retry must outlast one round trip, and a round trip is per hop.** The eight
    /// seconds that fixed a destroyed answer on a link next door is a fifth of one round trip over
    /// ten repeaters: the re-ask would abandon `msg2` while it was still travelling.
    @Test
    fun the_handshake_reask_covers_the_route() {
        assertEquals(8_000L, handshakeRetryFor(0))
        assertEquals(8_000L, handshakeRetryFor(null))
        assertEquals(28_000L, handshakeRetryFor(10))
        assertEquals(48_000L, handshakeRetryFor(20))
    }
}

/**
 * **Every attach that happens before a surface has measured anything states the remembered grid.**
 *
 * The connection's own resume attach runs inside `connect`, and a cold start's attach from the
 * launcher runs before the terminal page exists. Stating nothing there costs a repaint at the desk's
 * size — a 255x82 pane is twenty-odd records — before the phone's own arrives.
 */
class RememberedGridTest {
    private fun config(grid: String) = LdmuxConfig(host = "", token = "", meshLastGrid = grid)

    @Test
    fun a_remembered_grid_is_read_back_as_two_numbers() {
        assertEquals(46 to 17, rememberedGridOf(config("46,17")))
        assertEquals(79 to 18, rememberedGridOf(config(" 79, 18 ")))
    }

    @Test
    fun nothing_remembered_is_no_grid_at_all() {
        assertNull(rememberedGridOf(config("")))
        assertNull(rememberedGridOf(config("46")), "half a grid is not a grid")
        assertNull(rememberedGridOf(config("46,x")))
        assertNull(rememberedGridOf(config("0,17")), "a grid has at least one column")
        assertNull(rememberedGridOf(config("46,300")), "and fits the wire's byte")
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

    /**
     * **A repair must outlast the round trip it is repairing.** A flat twenty seconds is a fine
     * floor for a neighbour and a flood every twenty seconds on a twenty-hop route, where one
     * working round trip is 72 s.
     */
    /**
     * **The handshake's total wait must exceed the last re-ask it schedules.** A flat sixty seconds
     * did the opposite on any route of 26 hops or more: the timeout fired before the first re-ask,
     * so the mesh was given one attempt and a fresh ephemeral key was never sent.
     */
    @Test
    fun a_handshake_is_given_the_whole_ladder_it_schedules() {
        for (hops in listOf(1, 5, 14, 26, 28, 63)) {
            val retry = handshakeRetryFor(hops)
            // Each re-ask waits twice the one before — 1x, 2x and 4x — and the last one is then
            // given a full answer window, because a multi-hop reply can take over a minute
            // (`openhop_core`, reproducing the firmware's own arithmetic).
            val ladder = retry * 7
            assertEquals(
                ladder + maxOf(retry, HANDSHAKE_ANSWER_WINDOW_MS),
                handshakeWaitMs(hops),
                "at $hops hops the wait is the ladder's sum plus one answer window",
            )
            assertTrue(
                handshakeWaitMs(hops) >= ladder + 120_000L,
                "at $hops hops the last re-ask gets at least two minutes to be answered",
            )
        }
        // And it scales with the route rather than being a fixed minute: at 28 hops the route term
        // alone (7 x 64 s) is larger than the whole wait at a neighbour, answer window included.
        assertTrue(
            handshakeWaitMs(28) > handshakeWaitMs(1) + HANDSHAKE_ANSWER_WINDOW_MS,
            "the route term still dominates at 28 hops",
        )
    }

    @Test
    fun a_route_repair_waits_for_a_round_trip_that_scales() {
        assertEquals(20_000L, routeRepairMs(1), "a neighbour gets the floor")
        assertEquals(20_000L, routeRepairMs(null))
        assertEquals(36_000L, routeRepairMs(5))
        assertEquals(72_000L, routeRepairMs(10))
        assertEquals(144_000L, routeRepairMs(20))
        // The firmware's ceiling clamps it, as every route-dependent figure is clamped.
        assertEquals(2 * 3_600L * 63, routeRepairMs(1_000))
    }

    @Test
    fun every_hop_buys_room_for_its_round_trip() {
        // **3.6 s a hop is the session's own first retransmission** (six airtimes at the public
        // tuple), so the bound never sits below one round trip however long the route is.
        assertEquals(96_000L, silenceFailureMs(10))
        assertEquals(132_000L, silenceFailureMs(20))
        // The firmware's longest path is 63 one-byte hops; past that a number is a misread.
        assertEquals(60_000L + 63 * 3_600L, silenceFailureMs(63))
        assertEquals(60_000L + 63 * 3_600L, silenceFailureMs(1_000), "a nonsense hop count cannot hang the page")
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
