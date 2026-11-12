package io.ldmux.mobile.mesh

/**
 * The one call the app makes outside a client handle: a device keypair for a fresh pairing.
 *
 * **This interface used to be the whole v14 C ABI** — Noise handshake, the 6+2+16 record, the ARQ,
 * every poll and every queue. All of it is inside `ldmux_client_*` now
 * (`crates/session/src/client.rs`, `docs/RADIO-PLAN.md` §10a), and a second ABI for the same suite
 * was two implementations waiting to disagree. What is left is the call a pairing needs *before*
 * there is a session to put it in.
 */
interface SessionLib {
    fun keypair(priv: ByteArray, pub: ByteArray): Int

    /** A keypair coordinate, in bytes. */
    companion object {
        const val KEY_LEN = 32

        /** The ABI status for a call that worked. */
        const val OK = 0

        /** A null argument: the Kotlin side passed something the JNI shim could not hand on. */
        const val ERR_NULL = -1

        /** A buffer too small for the answer. */
        const val ERR_BUFFER = -2
    }
}

/**
 * The platform's [SessionLib], or null when the native library did not load.
 *
 * A null is the settings screen's "the library is missing" and the pairing's "cannot make a
 * keypair here", so it is a value rather than an exception.
 */
expect fun loadSessionLib(): SessionLib?
