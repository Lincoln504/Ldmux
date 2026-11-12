package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * A screen split across chunks is one screen.
 *
 * A first full paint of a wide grid deflates past what one transaction carries, so the host may
 * send several `Snapshot` messages sharing a `snapshotId` (`docs/RADIO-PLAN.md` §12.2). These tests
 * pin the two properties that make that safe: **nothing is painted until the last chunk arrives**,
 * and the assembled screen is a legal one-message snapshot of exactly the same content.
 */
class SnapshotAssemblyTest {
    private fun chunk(
        idx: Int,
        count: Int,
        snapshotId: Int = 7,
        patches: List<TermPatch> = emptyList(),
        cursorY: Int = 18,
        flags: Int = MeshTerm.SnapshotFlag.FULL or MeshTerm.SnapshotFlag.SETTLED,
    ) = TermMessage.Snapshot(
        flags, idx, count, snapshotId, 0, 0, MeshTerm.Applied.NONE,
        46, 19, 0, cursorY, 0, "ab12cd34", patches,
    )

    private fun patch(y: Int, text: String) = TermPatch(y, false, text)

    private fun ready(joined: Joined): TermMessage.Snapshot =
        (joined as? Joined.Ready)?.screen ?: error("expected a whole screen, got $joined")

    @Test
    fun a_screen_split_in_two_paints_once_and_keeps_the_last_chunks_header() {
        val assembly = SnapshotAssembly()
        assertEquals(
            Joined.Pending,
            assembly.accept(chunk(0, 2, patches = listOf(patch(0, "one")), cursorY = 0)),
            "half a screen was painted",
        )
        assertTrue(assembly.isOpen)
        val whole = ready(
            assembly.accept(chunk(1, 2, patches = listOf(patch(1, "two")), cursorY = 1)),
        )
        assertEquals(2, whole.patches.size, "the screen lost a chunk: ${whole.patches}")
        assertEquals(listOf("one", "two"), whole.patches.map { it.text })
        assertEquals(1, whole.cursorY, "the header came from the first chunk, not the last")
        assertEquals(0, whole.chunkIdx)
        assertEquals(1, whole.chunkCount, "the assembled form is one message")
        assertFalse(assembly.isOpen, "the assembly did not reset")
    }

    @Test
    fun a_retransmitted_chunk_is_a_repair_and_is_painted_once() {
        val assembly = SnapshotAssembly()
        val zero = chunk(0, 2, patches = listOf(patch(0, "one")))
        assembly.accept(zero)
        // The host repeated chunk 0 because its acknowledgement was lost.
        assertEquals(Joined.Pending, assembly.accept(zero))
        val whole = ready(assembly.accept(chunk(1, 2, patches = listOf(patch(1, "two")))))
        assertEquals(2, whole.patches.size, "the repeat was joined as a second patch: ${whole.patches}")
    }

    @Test
    fun a_screen_that_never_began_is_refused() {
        val assembly = SnapshotAssembly()
        assertEquals(
            Joined.Refused(ChunkRefusal.WrongCommand),
            assembly.accept(chunk(1, 2, patches = listOf(patch(1, "two")))),
            "a screen with no first chunk was accepted",
        )
        assertFalse(assembly.isOpen, "a refused first chunk left an assembly open")
    }

    @Test
    fun a_chunk_of_another_screen_is_refused_and_the_first_is_kept() {
        val assembly = SnapshotAssembly()
        assembly.accept(chunk(0, 2, snapshotId = 7, patches = listOf(patch(0, "one"))))
        assertEquals(
            Joined.Refused(ChunkRefusal.WrongCommand),
            assembly.accept(chunk(1, 2, snapshotId = 8, patches = listOf(patch(1, "two")))),
            "two screens were joined into one",
        )
        assertTrue(assembly.isOpen, "the refusal lost the first screen")
        val whole = ready(assembly.accept(chunk(1, 2, snapshotId = 7, patches = listOf(patch(1, "two")))))
        assertEquals(listOf("one", "two"), whole.patches.map { it.text })
    }

    @Test
    fun a_chunk_count_that_disagrees_with_the_first_chunk_is_refused() {
        val assembly = SnapshotAssembly()
        assembly.accept(chunk(0, 2, patches = listOf(patch(0, "one"))))
        assertEquals(
            Joined.Refused(ChunkRefusal.WrongCount),
            assembly.accept(chunk(1, 3, patches = listOf(patch(1, "two")))),
            "two lengths for one screen was accepted",
        )
    }

    @Test
    fun a_screen_past_the_total_is_refused_rather_than_truncated() {
        val assembly = SnapshotAssembly()
        val half = MeshTerm.MAX_SNAPSHOT_TOTAL / 2 / 3 * 3
        assembly.accept(chunk(0, 2, patches = listOf(patch(0, "x".repeat(half - 3)))))
        assertEquals(
            Joined.Refused(ChunkRefusal.TooLarge),
            assembly.accept(chunk(1, 2, patches = listOf(patch(1, "y".repeat(half))))),
            "a screen past the cap was assembled",
        )
    }

    @Test
    fun the_assembled_screen_re_encodes_as_one_legal_message() {
        val assembly = SnapshotAssembly()
        assembly.accept(chunk(0, 2, patches = listOf(patch(0, "one"))))
        val whole = ready(assembly.accept(chunk(1, 2, patches = listOf(patch(1, "two")))))
        val round = TermDecoder().push(whole.encode())
        assertEquals(1, round.size, "the assembled screen did not encode to one message: $round")
        val back = (round.single() as TermItem.Message).message as TermMessage.Snapshot
        assertEquals(1, back.chunkCount)
        assertEquals(listOf("one", "two"), back.patches.map { it.text })
    }

    @Test
    fun a_chunk_pair_no_receiver_can_place_is_refused_by_the_encoder() {
        for ((idx, count) in listOf(1 to 1, 0 to 0, 5 to 3)) {
            assertFailsWith<TermEncodeException>("$idx of $count was encoded") {
                chunk(idx, count, patches = listOf(patch(0, "one"))).encode()
            }
        }
    }

    @Test
    fun a_chunk_past_the_end_of_its_own_screen_does_not_decode() {
        // The bytes a hand-built message would carry: `Snapshot` with `chunkIdx` 3 of 2.
        val good = chunk(1, 2, patches = listOf(patch(0, "one"))).encode()
        val body = good.copyOfRange(2, good.size)
        body[1] = 3
        val framed = ByteArray(2 + body.size)
        framed[0] = MeshTerm.T_SNAPSHOT.toByte()
        framed[1] = body.size.toByte()
        body.copyInto(framed, 2)
        assertTrue(
            TermDecoder().push(framed).none { it is TermItem.Message },
            "a chunk past the end of its own screen was decoded",
        )
    }
}
