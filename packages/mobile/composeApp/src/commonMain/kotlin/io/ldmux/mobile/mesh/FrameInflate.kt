package io.ldmux.mobile.mesh

/**
 * Inflate one compressed frame's body, or null when it is not valid deflate of the declared size.
 *
 * **A platform seam because it is a platform library.** The mesh decoder is common code, but raw
 * deflate lives in `java.util.zip` on Android and in a different module elsewhere; a pure-Kotlin
 * inflate would be a second implementation of a solved problem for the one place it is used.
 *
 * Returning null rather than throwing is deliberate: a frame that will not inflate is one lost
 * frame — the screen it carried is asked for again — and must never be a stream error that kills
 * the session.
 */
internal expect fun inflateFrame(bytes: ByteArray, expected: Int, dictionary: Boolean = false): ByteArray?
