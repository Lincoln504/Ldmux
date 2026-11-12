package io.ldmux.mobile.mesh

/**
 * The phone's keypair, from the library that owns the suite.
 *
 * **This is all that is left here.** The v14 session — Noise handshake, the 6+2+16 record, the ARQ —
 * lived in this file and was driven by the frame-loop driver. The v15 engine owns all of it in Rust
 * behind `ldmux_client_*` (`crates/session/src/client.rs`), so what the app still needs from the old
 * C ABI is one call: a device keypair for a fresh pairing (`docs/RADIO-PLAN.md` §7, `A2`).
 */
object MeshSession {
    fun generateKeypair(lib: SessionLib): Pair<ByteArray, ByteArray> {
        val priv = ByteArray(SessionLib.KEY_LEN)
        val pub = ByteArray(SessionLib.KEY_LEN)
        val rc = lib.keypair(priv, pub)
        if (rc != SessionLib.OK) throw sessionFailure(rc, "keypair")
        return priv to pub
        }
}

/**
 * The ABI's status code as the one error vocabulary the mesh package already has.
 *
 * Kept beside the keypair call because that is the only place left that reads a raw ABI code: the
 * engine maps its own inside Rust (`crates/session-ffi`).
 */
internal fun sessionFailure(code: Int, what: String): MeshSessionException =
    MeshSessionException(sessionStateFor(code), code, "mesh session: $what failed ($code)")

/**
 * Failure of the session layer, in the mesh package's one vocabulary. [state] is where the link ends
 * up; [code] is the ABI status for logs and tests. No session secret appears in [message].
 */
class MeshSessionException(val state: MeshLinkState, val code: Int, message: String) :
    Exception(message)

/**
 * Map an ABI status to the link state it means. A handshake the host rejected is the same situation
 * as a refused token and the same cure (re-pair), so it reuses [MeshLinkState.Refused]; every other
 * failure is [MeshLinkState.SessionFailed].
 */
internal fun sessionStateFor(code: Int): MeshLinkState = when (code) {
    SessionLib.ERR_BAD_FILTER,
    SessionLib.ERR_STALE,
    SessionLib.ERR_REPLAY,
    SessionLib.ERR_PIN_MISMATCH,
    -> MeshLinkState.Refused
    else -> MeshLinkState.SessionFailed
}
