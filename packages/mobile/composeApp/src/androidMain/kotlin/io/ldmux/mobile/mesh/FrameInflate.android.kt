package io.ldmux.mobile.mesh

import java.util.zip.DataFormatException
import java.util.zip.Inflater

/**
 * Raw-deflate, no wrapper — the host writes `DeflateEncoder` with `nowrap` semantics and a length
 * field of its own, so the zlib header must not be expected here.
 */
internal actual fun inflateFrame(bytes: ByteArray, expected: Int): ByteArray? {
    if (expected <= 0 || expected > MeshTerm.FRAME_MAX_INFLATED) return null
    val out = ByteArray(expected)
    val inflater = Inflater(true)
    return try {
        inflater.setInput(bytes)
        var filled = 0
        while (filled < expected) {
            val n = inflater.inflate(out, filled, expected - filled)
            if (n == 0) {
                // No progress: either the input is exhausted (which is fine only at the exact
                // size) or it is truncated/corrupt.
                if (inflater.finished() || inflater.needsInput() || inflater.needsDictionary()) break
            }
            filled += n
        }
        if (filled != expected) null else out
    } catch (_: DataFormatException) {
        null
    } finally {
        inflater.end()
    }
}
