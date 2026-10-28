package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.MeshKeyMaterial
import dev.ldmux.mobile.Pairing

/** What a pairing did to the phone's mesh keys. */
enum class MeshEnrollOutcome {
    /** The link carried no mesh keys. Any existing mesh pairing was left alone (same machine) or cleared (a different one). */
    NoMeshKeys,

    /** New keys stored: a fresh phone keypair, the host's key and the secret. */
    Enrolled,

    /** The link carried keys but this device cannot run the session library, so nothing was stored. */
    LibraryUnavailable,

    /** The secure store refused the write (Keystore failure). The tailnet pairing is unaffected; the mesh route stays "needs keys". */
    StoreFailed,
}

/**
 * Turns a scanned pairing into the keys the L3 session needs, and keeps them honest across
 * re-pairing (ADR-0020 "Revocation"): a re-scan issues a **new phone static key** and replaces
 * the old record, and pairing with a different machine forgets the previous machine's keys.
 *
 * Does not design the ceremony: it stores and clears. The PSK comes only from a link the user
 * scanned or opened and confirmed, and is never logged.
 *
 * @param newKeypair `(private, public)` for this phone, or null when the library did not load.
 */
class MeshEnrollment(
    private val store: MeshSessionKeyStore,
    private val newKeypair: () -> Pair<ByteArray, ByteArray>?,
) {
    /**
     * Apply [p]. [previousHost] is the tailnet host this config was paired with before, or empty:
     * keys for a different machine must not survive pairing with this one.
     */
    fun onPaired(previousHost: String, p: Pairing): MeshEnrollOutcome {
        val mesh = p.mesh
        if (mesh == null) {
            if (previousHost.isNotEmpty() && previousHost != p.host) runCatching { store.clear() }
            return MeshEnrollOutcome.NoMeshKeys
        }
        val (priv, pub) = newKeypair() ?: return MeshEnrollOutcome.LibraryUnavailable
        return try {
            store.save(MeshSessionKeys(priv, pub, mesh.hostKey, mesh.psk, mesh.hostRadio))
            MeshEnrollOutcome.Enrolled
        } catch (_: Exception) {
            // A broken Keystore must not take the tailnet pairing down with it.
            MeshEnrollOutcome.StoreFailed
        }
    }

    /** Unpairing forgets the mesh keys with it. */
    fun onUnpaired() { runCatching { store.clear() } }
}

/** The platform's secure store for the session keys (Android Keystore). */
expect fun meshSessionKeyStore(): MeshSessionKeyStore

/** The platform's secure store for the MeshMore gateway token. */
expect fun meshTokenStore(): MeshTokenStore

/** A device keypair from the real library, or null when it did not load. */
fun libraryKeypair(): Pair<ByteArray, ByteArray>? =
    loadSessionLib()?.let { MeshSession.generateKeypair(it) }
