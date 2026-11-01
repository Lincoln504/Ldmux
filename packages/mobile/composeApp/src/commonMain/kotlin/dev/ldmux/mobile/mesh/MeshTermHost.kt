package dev.ldmux.mobile.mesh

import kotlin.time.Clock
import kotlin.time.TimeSource
import kotlinx.coroutines.CoroutineScope

/** The platform's socket to the MeshMore gateway on loopback. */
expect fun meshSocketFactory(): MeshSocketFactory

/** The credentials a mesh attach needs, all read from secure storage. */
class MeshCredentials(val keys: MeshSessionKeys, val token: ByteArray, val hostRadio: ByteArray)

/** Why there are no [MeshCredentials]: what resolveRoute should be told. */
data class MeshCredentialState(val hasSessionKeys: Boolean, val hasToken: Boolean, val hasPeer: Boolean) {
    val complete: Boolean get() = hasSessionKeys && hasToken && hasPeer
}

/** What is stored, without ever throwing: a broken Keystore reads as "needs pairing". */
fun readMeshCredentials(keys: MeshSessionKeyStore, tokens: MeshTokenStore): Pair<MeshCredentialState, MeshCredentials?> {
    val k = runCatching { keys.load() }.getOrNull()
    val t = runCatching { tokens.load() }.getOrNull()?.takeIf { it.size == MeshGateway.PAIRING_TOKEN_BYTES }
    val state = MeshCredentialState(hasSessionKeys = k != null, hasToken = t != null, hasPeer = k?.hostRadio != null)
    val creds = if (k != null && t != null && k.hostRadio != null) MeshCredentials(k, t, k.hostRadio) else null
    return state to creds
}

/**
 * The whole mesh attach stack for one screen: the gateway client, the session handshake and the
 * driver. Nothing here is decided by a radio fact: it carries credentials and bytes.
 *
 * A connection is single use, like [MeshGatewayClient]: leaving the screen closes it, entering
 * again makes a new one.
 */
class MeshTermConnection(
    sockets: MeshSocketFactory,
    scope: CoroutineScope,
    private val creds: MeshCredentials,
    private val lib: SessionLib,
    /** MeshMore's gateway on this phone (the saved one, or the default). */
    endpoint: MeshGatewayEndpoint = MeshGatewayEndpoint.DEFAULT,
    private val wallMs: () -> Long = { Clock.System.now().toEpochMilliseconds() },
    selector: String = "",
) {
    private val gateway = MeshGatewayClient(sockets, scope, creds.token, creds.hostRadio, host = endpoint.host, port = endpoint.port)
    private val origin = TimeSource.Monotonic.markNow()
    private val clock: () -> Long = { origin.elapsedNow().inWholeMilliseconds }

    val driver = MeshTermDriver(
        gateway = gateway,
        begin = {
            MeshSession.begin(
                lib, creds.keys.localPrivate, creds.keys.localPublic, creds.keys.hostPublic, creds.keys.psk, wallMs(),
            )
        },
        selector = selector,
        clock = clock,
    )

    /** Connect to the gateway, then drive the terminal until cancelled. A refused connect is a visible link-down. */
    suspend fun run() {
        try {
            gateway.connect()
        } catch (e: MeshGatewayException) {
            driver.onGatewayState(e.state)
            return
        }
        try {
            driver.run()
        } finally {
            gateway.close()
        }
    }

    fun close() = gateway.close()
}

/**
 * One line for each thing the mesh terminal can be doing. The three "not working" cases read
 * differently on purpose: the cure for each is different.
 */
/**
 * The header's status line, or empty when the view has nothing to report.
 *
 * A header that says "choose a shell" above a list of shells is noise: the screen already says
 * it, and the one line costs a row that the terminal will want. Only a state the user would not
 * otherwise know - securing, waiting, failed, closed - earns the line.
 */
fun meshHeaderStatus(view: MeshTermView, endpoint: MeshGatewayEndpoint? = null): String = when (view) {
    // Routine: the body already says what to do.
    is MeshTermView.Choose, MeshTermView.Idle, MeshTermView.Listing -> ""
    is MeshTermView.Live -> if (view.resyncing || !view.painted || view.stalled) {
        meshTermLabel(view, endpoint)
    } else {
        ""
    }
    else -> meshTermLabel(view, endpoint)
}

fun meshTermLabel(view: MeshTermView, endpoint: MeshGatewayEndpoint? = null): String = when (view) {
    MeshTermView.Idle -> "meshmore · not attached"
    is MeshTermView.LinkDown -> meshLinkLabel(view.link)
    // "Can take minutes" was the old truth (a single msg1 and a five-minute timeout). A handshake
    // is three small records and is now retried every six seconds, so the honest label is the
    // state, not a warning about how long it might take.
    MeshTermView.Handshaking -> "meshmore · securing the link"
    MeshTermView.ProfileMissing -> "meshmore · linked, waiting for the host to describe the terminal"
    MeshTermView.Listing -> "meshmore · asking the host which shells are running"
    is MeshTermView.Choose -> "meshmore · choose a shell"
    MeshTermView.NoShells -> "meshmore · the host is running no shell"
    is MeshTermView.Live -> when {
        view.resyncing -> "meshmore · resyncing the screen"
        !view.painted -> "meshmore · attached, no screen yet"
        view.stalled -> "meshmore · waiting for send budget"
        else -> "meshmore · live (reduced)"
    }
    is MeshTermView.HostClosed -> when (view.reason) {
        null -> "meshmore · the host ended the session"
        MeshTerm.Closed.NOT_FOUND -> "meshmore · no such shell on the host"
        MeshTerm.Closed.AMBIGUOUS -> "meshmore · more than one shell on the host"
        MeshTerm.Closed.EXITED -> "meshmore · the shell exited"
        MeshTerm.Closed.UNAVAILABLE -> "meshmore · the host could not read the screen"
        MeshTerm.Closed.VERSION -> "meshmore · the host speaks another terminal version"
        else -> "meshmore · the host closed the terminal (${view.reason})"
    }
}

/** What this link cannot do, in words. Empty when the host reports nothing absent. */
fun meshAbsentLine(absent: List<MeshTermFeature>): String =
    if (absent.isEmpty()) "" else "Not on this link: " + absent.joinToString(", ") { it.name.lowercase() }

/** The budget line, never in seconds (the architecture forbids radio arithmetic here). */
fun meshBudgetLine(left: Int?): String = when (left) {
    null -> "send budget: not reported"
    0 -> "send budget: none left this hour"
    255 -> "send budget: 255 or more records left this hour"
    else -> "send budget: $left records left this hour"
}

/**
 * What is wrong with a saved mesh route right now, read from secure storage; null when it can
 * run. Never throws. The launcher uses it so the line under the title names the real next step
 * (scan a code, pair the gateway, add the host radio) and not a guess.
 */
fun currentMeshResolution(route: MeshRoute): MeshRouteResolution {
    val (state, _) = readMeshCredentials(meshSessionKeyStore(), meshTokenStore())
    return resolveRoute(
        route, currentPlatform,
        hasToken = state.hasToken, hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
    )
}

/** Everything the mesh screen needs: the credentials, the library, and where the gateway is. */
class MeshScreenPlan(val creds: MeshCredentials, val lib: SessionLib, val endpoint: MeshGatewayEndpoint)

/**
 * What to show the mesh terminal with, or null when the saved route is not mesh or cannot run (in
 * which case the caller shows the normal app and its stated reason). **Not gated on the tailnet
 * pairing**: a mesh-only device has no daemon host or token and is still a complete mesh pairing.
 * Tailnet configs return null without touching secure storage.
 */
fun meshScreenFor(config: dev.ldmux.mobile.LdmuxConfig): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    return meshScreenPlan(config, meshSessionKeyStore(), meshTokenStore(), loadSessionLib())
}

/** [meshScreenFor] with its inputs passed in, so the rule is testable without a Keystore. */
fun meshScreenPlan(
    config: dev.ldmux.mobile.LdmuxConfig,
    keys: MeshSessionKeyStore,
    tokens: MeshTokenStore,
    lib: SessionLib?,
    platform: MobilePlatform = currentPlatform,
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    val (state, creds) = readMeshCredentials(keys, tokens)
    if (lib == null) return null
    val resolution = resolveRoute(
        config.route, platform,
        hasToken = state.hasToken, hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = MESH_SESSION_BUILT, terminalBuilt = terminalBuilt,
    )
    return if (resolution is MeshRouteResolution.Use && creds != null) MeshScreenPlan(creds, lib, config.meshGatewayEndpoint) else null
}

/** One picker line: the label when the host gave one, then the id the choice will send. */
fun meshShellLine(e: SessionEntry): String =
    if (e.label.isEmpty()) e.id else "${e.label}  (${e.id.take(8)})"
