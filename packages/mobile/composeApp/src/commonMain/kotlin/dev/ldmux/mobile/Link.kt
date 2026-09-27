package dev.ldmux.mobile

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The state of the app's link to its daemon, as one value.
 *
 * There used to be a `connected` boolean and nothing else, so "the token was
 * refused", "the machine is off" and "not paired at all" all drew as the same empty
 * list under the same host name, and the only sentence anywhere was "not connected
 * to the host", shown after trying to open a shell.
 */
enum class LinkState {
    /** No pairing: nothing to connect to. */
    Unpaired,
    /** Paired, and the first attempt since pairing (or launch) is under way. */
    Connecting,
    Online,
    /** Paired, and the machine cannot be reached. Retried with backoff. */
    Unreachable,
    /**
     * The daemon answered and refused the token: the pairing is stale (the token
     * was rotated). Not retried on a timer — nothing about waiting will fix it —
     * only on refresh or a new pairing.
     */
    Rejected,
}

/**
 * What a failed connection means, from the daemon's answer to a plain `GET /ws`.
 *
 * The WebSocket handshake failing says nothing about why; its exception is a
 * library's sentence about a status code. The daemon answers the same path without
 * upgrading, with the same checks, so one request tells the cases apart by status
 * alone: 401 is a refused token. 400 means the token was accepted (and the upgrade
 * failed for some other reason, which is transient), and no answer at all is a
 * machine that is not there.
 */
fun linkStateFromProbe(status: Int?): LinkState = when (status) {
    401 -> LinkState.Rejected
    else -> LinkState.Unreachable
}

/** The launcher's line under the title: what it is paired with, and how that is going. */
fun linkLabel(state: LinkState, config: LdmuxConfig): String = when (state) {
    LinkState.Unpaired -> "unpaired"
    LinkState.Online -> config.pairedName
    LinkState.Connecting -> "${config.pairedName} · connecting"
    LinkState.Unreachable -> "${config.pairedName} · unreachable"
    LinkState.Rejected -> "${config.pairedName} · rejected"
}

/**
 * The one owner of the config: load, save, and tell everyone.
 *
 * Three places used to write it — `App()` held it, and two copies of the settings
 * dialog's `onSave` each saved it and reconnected the client — so a change had to be
 * made correctly in all three. This is where pairing and unpairing happen too, and
 * what the rest of the app observes.
 */
class ConfigStore(initial: LdmuxConfig, private val persist: (LdmuxConfig) -> Unit) {
    private val _config = MutableStateFlow(initial)
    val config: StateFlow<LdmuxConfig> = _config

    fun update(next: LdmuxConfig) {
        if (next == _config.value) return
        persist(next)
        _config.value = next
    }

    fun pair(p: Pairing) = update(_config.value.paired(p))

    fun unpair() = update(_config.value.unpaired())
}

/** How long the "why did that fail" request may take before the answer is "unreachable". */
internal const val PROBE_TIMEOUT_MS = 3_000L
