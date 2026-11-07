package io.ldmux.mobile

/**
 * The Android half of [probeFailureIsRefused].
 *
 * `ConnectException` is what the host's `ECONNREFUSED` becomes: the machine is up, the port is
 * closed, so the daemon is not running. A timeout and an unreachable network are different
 * failures with a different sentence, and they are deliberately **not** this one. The cause chain
 * is walked because Ktor wraps the engine's exception, and on this engine the wrapper is not the
 * one that knows.
 *
 * **The type is not enough on its own.** A route that does not exist (`ENETUNREACH`, the tailnet
 * being down, the phone on the wrong network) reaches this as a `ConnectException` too, and it
 * must not read as "daemon not running" — that sends the person to the one machine that is fine.
 * Its message says "unreachable" or "no route", so a connect exception carrying either word falls
 * through to the caller's generic sentence.
 */
internal actual fun probeFailureIsRefused(e: Throwable): Boolean {
    var cause: Throwable? = e
    var hops = 0
    while (cause != null && hops < 8) {
        // `ConnectException`, not `SocketException`: a closed port raises the former, and the
        // latter also covers "network is unreachable", which is not this answer.
        if (cause is java.net.ConnectException && cause !is java.net.NoRouteToHostException) {
            val text = (cause.message ?: "").lowercase()
            if (!text.contains("unreachable") && !text.contains("no route")) return true
        }
        cause = cause.cause
        hops++
    }
    return false
}
