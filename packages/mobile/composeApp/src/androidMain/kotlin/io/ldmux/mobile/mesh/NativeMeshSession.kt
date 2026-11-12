package io.ldmux.mobile.mesh

/**
 * The Android half of [SessionLib]: one keypair call, delegated to the library that owns the suite.
 *
 * It used to declare the whole v14 ABI as `external` methods. That ABI is gone — the engine owns
 * the session — so what is left is a delegation, and [available] still asks the same question the
 * settings screen asks: is the native library here?
 */
object NativeMeshSession : SessionLib {
    /** True if the native library loaded. Never throws. */
    val available: Boolean = NativeRadioClient.available

    override fun keypair(priv: ByteArray, pub: ByteArray): Int =
        NativeRadioClient.keypair(priv, pub)
}

/** The Android [loadSessionLib]: the object above, when the library loaded. */
actual fun loadSessionLib(): SessionLib? = if (NativeMeshSession.available) NativeMeshSession else null
