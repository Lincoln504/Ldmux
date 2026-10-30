package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertNull

/** The phone's name-to-bytes table, pinned to the daemon's (`key_name_to_bytes`, `stream.rs`). */
class MeshKeysTest {
    private fun b(vararg x: Int) = ByteArray(x.size) { x[it].toByte() }
    private fun esc(s: String) = s.replace("ESC", "\u001b").encodeToByteArray()

    @Test
    fun `the names the shared key bars send have the daemon's bytes`() {
        for ((name, want) in listOf(
            "Enter" to b(0x0D), "Tab" to b(0x09), "Escape" to b(0x1B), "Space" to b(0x20), "BSpace" to b(0x7F),
            "Up" to esc("ESC[A"), "Down" to esc("ESC[B"), "Right" to esc("ESC[C"), "Left" to esc("ESC[D"),
            "Home" to esc("ESC[H"), "End" to esc("ESC[F"),
            "PageUp" to esc("ESC[5~"), "PageDown" to esc("ESC[6~"), "Delete" to esc("ESC[3~"),
            "F5" to esc("ESC[15~"), "F12" to esc("ESC[24~"), "C-c" to b(0x03), "C-r" to b(0x12), "a" to "a".encodeToByteArray(),
        )) assertContentEquals(want, meshKeyBytes(name), name)
    }

    @Test
    fun `modifiers are in the bytes, as xterm writes them`() {
        assertContentEquals(esc("ESCb"), meshKeyBytes("M-b"))
        assertContentEquals(b(0), meshKeyBytes("C-@"))
        assertContentEquals(esc("ESC[1;5D"), meshKeyBytes("C-Left"))
        assertContentEquals(esc("ESC[1;7D"), meshKeyBytes("M-C-Left"))
        assertContentEquals(esc("ESC[Z"), meshKeyBytes("S-Tab"))
        assertContentEquals(esc("ESC[1;2A"), meshKeyBytes("S-Up"))
    }

    @Test
    fun `a name the table does not know is refused, never typed`() {
        for (bad in listOf("", "NotAKey", "C-A", "C-|", "BSpace2")) assertNull(meshKeyBytes(bad), bad)
    }

    @Test
    fun `text outside ascii is text and carries no modifier`() {
        assertContentEquals("é".encodeToByteArray(), meshKeyBytes("é"))
        assertNull(meshKeyBytes("C-é"))
    }
}
