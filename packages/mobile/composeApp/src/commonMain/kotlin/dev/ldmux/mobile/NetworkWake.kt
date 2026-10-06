package dev.ldmux.mobile

import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull

/**
 * "A usable network just appeared" — one signal for every socket in the app.
 *
 * Only the launcher socket used to hear it, so after a Wi-Fi rejoin or a
 * Tailscale flap the list came back at once and every open terminal sat out the
 * rest of its backoff, up to ten seconds of a frozen screen on a link that was
 * already fine.
 */
object NetworkWake {
    private val events = MutableSharedFlow<Unit>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)
    val wakes: SharedFlow<Unit> = events

    fun fire() {
        events.tryEmit(Unit)
    }

    /** Wait [ms], or less if the network comes back first. True when woken. */
    suspend fun sleep(ms: Long): Boolean = withTimeoutOrNull(ms) { events.first() } != null
}

/**
 * The user asked, from outside the app (the notification's Disconnect), to let go
 * of every open shell. The app answers it the way it answers the launcher button.
 *
 * Replayed to a collector that arrives late. The service stops itself on Disconnect,
 * so a request made while nothing was collecting (the activity was between instances)
 * was simply lost; a replay of one means the next collector hears it. [handled] empties
 * the replay, so a request is answered once and a later collector is not woken by it.
 */
object DisconnectRequests {
    private val events = MutableSharedFlow<Unit>(
        replay = 1,
        extraBufferCapacity = 1,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    val requests: SharedFlow<Unit> = events

    fun request() {
        events.tryEmit(Unit)
    }

    /** The request has been acted on; forget it. */
    fun handled() {
        events.resetReplayCache()
    }
}
