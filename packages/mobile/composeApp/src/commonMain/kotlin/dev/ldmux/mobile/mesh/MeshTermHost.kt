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
fun meshTermLabel(view: MeshTermView): String = when (view) {
    MeshTermView.Idle -> "mesh · not attached"
    is MeshTermView.LinkDown -> meshLinkLabel(view.link)
    MeshTermView.Handshaking -> "mesh · securing the link (can take minutes)"
    MeshTermView.ProfileMissing -> "mesh · linked, waiting for the host to describe the terminal"
    MeshTermView.Listing -> "mesh · asking the host which shells are running"
    is MeshTermView.Choose -> "mesh · choose a shell"
    MeshTermView.NoShells -> "mesh · the host is running no shell"
    is MeshTermView.Live -> when {
        view.resyncing -> "mesh · resyncing the screen"
        !view.painted -> "mesh · attached, no screen yet"
        view.stalled -> "mesh · waiting for send budget"
        else -> "mesh · live (reduced)"
    }
    is MeshTermView.HostClosed -> when (view.reason) {
        null -> "mesh · the host ended the session"
        MeshTerm.Closed.NOT_FOUND -> "mesh · no such shell on the host"
        MeshTerm.Closed.AMBIGUOUS -> "mesh · more than one shell on the host"
        MeshTerm.Closed.EXITED -> "mesh · the shell exited"
        MeshTerm.Closed.UNAVAILABLE -> "mesh · the host could not read the screen"
        MeshTerm.Closed.VERSION -> "mesh · the host speaks another terminal version"
        else -> "mesh · the host closed the terminal (${view.reason})"
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
