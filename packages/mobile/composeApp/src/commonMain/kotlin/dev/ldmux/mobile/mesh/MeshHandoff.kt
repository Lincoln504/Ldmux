package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.LdmuxConfig

/**
 * The same-phone handoff of MeshMore's gateway token (`meshmore-sns-gateway-handoff.md`).
 *
 * Only the **L2 grant** moves this way: the token, and the port the gateway actually bound. The
 * session keys and the host radio key still arrive by the `ldmux code` ceremony, because they are
 * the other half of a two-ceremony pairing and MeshMore does not hold them.
 */
const val MESH_HANDOFF_VERSION = 1

/** What MeshMore's handoff service answered. [token] is null unless [state] is `running`. */
class MeshHandoffOffer(
    val version: Int,
    val state: String,
    val port: Int,
    val token: ByteArray?,
    /** The phone's own radio key, for display. LDMUX's `HostRadioKey` is the *host's* radio, not this. */
    val radioKey: String?,
    val peerName: String?,
)

/** Why there is no offer. Each reads differently to the user because each is cured differently. */
enum class MeshHandoffFailure {
    NotInstalled,

    /** Installed, but this build has no handoff service (an older MeshMore). */
    ServiceMissing,

    /** Installed, but signed with another certificate: the platform refused the bind. */
    PermissionDenied,
    Timeout,

    /** The service answered with something that is not a version-1 offer. */
    BadReply,
}

sealed interface MeshHandoffResult {
    class Offer(val offer: MeshHandoffOffer) : MeshHandoffResult
    class Failed(val why: MeshHandoffFailure) : MeshHandoffResult
}

/** Ask MeshMore on this phone for its gateway's endpoint and token. Never throws; no UI of its own. */
expect suspend fun requestMeshHandoff(): MeshHandoffResult

/**
 * Bring MeshMore to the foreground, if this phone has it.
 *
 * The handoff can only read what MeshMore's running process published, and a process that was
 * killed cannot start its Flutter side from a Binder bind alone: the bind creates the service, not
 * the engine that runs the gateway. Opening the app does start it (the gateway service is eager),
 * so "not serving" has one cure the user can recognise. Fire-and-forget: the screen retries the
 * handoff when it comes back, and there is nothing here to await.
 */
expect fun openMeshMore(): Boolean

/** What applying an offer did. The caller stores nothing itself. */
sealed interface MeshHandoffApplied {
    /** The token (and endpoint) are now what MeshMore offered. [config] carries the endpoint. */
    class Linked(val config: LdmuxConfig) : MeshHandoffApplied

    /** A different token is already stored and was not asked to be replaced. Nothing changed. */
    data object KeptExisting : MeshHandoffApplied
    class NotServing(val state: String) : MeshHandoffApplied
    class Unavailable(val why: MeshHandoffFailure) : MeshHandoffApplied

    /** Same refusal as scanning MeshMore's code: a token alone is a half-pairing. */
    data object NeedsSessionPairing : MeshHandoffApplied
    data object StoreFailed : MeshHandoffApplied
}

/**
 * Merge [result] into what is stored. Pure apart from the two stores it is handed.
 *
 * **The binding rule: a handoff never silently overwrites a working pairing.** It fills a missing
 * token, and it replaces a stored one only when the caller says that one has just failed
 * ([replaceStale], set by the retry after a refused or unreachable link). Otherwise a different
 * token is left alone and the user is told, because a token that is working is the one fact the
 * app must not lose to a convenience.
 *
 * The endpoint follows the token: MeshMore's port is whatever it managed to bind, so it is taken
 * together with a token that is stored or already current, and never by itself.
 */
fun applyMeshHandoff(
    result: MeshHandoffResult,
    config: LdmuxConfig,
    keys: MeshSessionKeyStore,
    tokens: MeshTokenStore,
    replaceStale: Boolean = false,
): MeshHandoffApplied {
    val offer = when (result) {
        is MeshHandoffResult.Failed -> return MeshHandoffApplied.Unavailable(result.why)
        is MeshHandoffResult.Offer -> result.offer
    }
    if (offer.version != MESH_HANDOFF_VERSION) return MeshHandoffApplied.Unavailable(MeshHandoffFailure.BadReply)
    if (offer.state != "running") return MeshHandoffApplied.NotServing(offer.state)
    val token = offer.token?.takeIf { it.size == MeshGateway.PAIRING_TOKEN_BYTES }
        ?: return MeshHandoffApplied.Unavailable(MeshHandoffFailure.BadReply)
    if (offer.port !in 1..65535) return MeshHandoffApplied.Unavailable(MeshHandoffFailure.BadReply)
    if (runCatching { keys.load() }.getOrNull() == null) return MeshHandoffApplied.NeedsSessionPairing

    val stored = runCatching { tokens.load() }.getOrNull()?.takeIf { it.size == MeshGateway.PAIRING_TOKEN_BYTES }
    val same = stored != null && stored.contentEquals(token)
    if (stored != null && !same && !replaceStale) return MeshHandoffApplied.KeptExisting
    if (!same) {
        try {
            tokens.save(token)
        } catch (_: Exception) {
            return MeshHandoffApplied.StoreFailed
        }
    }
    val endpoint = MeshGatewayEndpoint("127.0.0.1", offer.port)
    val saved = if (endpoint == MeshGatewayEndpoint.DEFAULT) "" else endpoint.toString()
    return MeshHandoffApplied.Linked(config.copy(meshGateway = saved))
}

/** Plain words for a handoff that did not link, or null when it did. Never blank. */
fun meshHandoffLine(applied: MeshHandoffApplied): String? = when (applied) {
    is MeshHandoffApplied.Linked -> null
    MeshHandoffApplied.KeptExisting -> "MeshMore offered a different token: yours was kept"
    is MeshHandoffApplied.NotServing -> when (applied.state) {
        "waitingForRadio" -> "MeshMore is waiting for its radio"
        "failed" -> "MeshMore's gateway could not start"
        // The switch can be on and the app still not listening: the gateway is created with
        // MeshMore's Flutter side, so a process started only to answer this bind has none. Opening
        // the app is the cure, and the screen's button does that.
        else -> "MeshMore is not serving: open it, or turn on Connected apps there"
    }
    is MeshHandoffApplied.Unavailable -> when (applied.why) {
        MeshHandoffFailure.NotInstalled -> "MeshMore is not installed"
        MeshHandoffFailure.ServiceMissing -> "this MeshMore has no link: use its code"
        MeshHandoffFailure.PermissionDenied -> "MeshMore is signed differently: use its code"
        MeshHandoffFailure.Timeout -> "MeshMore did not answer"
        MeshHandoffFailure.BadReply -> "MeshMore sent something unreadable"
    }
    MeshHandoffApplied.NeedsSessionPairing -> gatewayScanLine(GatewayScanProblem.NeedsSessionPairing)
    MeshHandoffApplied.StoreFailed -> gatewayScanLine(GatewayScanProblem.StoreFailed)
}

/**
 * Whether asking MeshMore on its own would help: keys are there and the token is not. This is the
 * "first time the mesh route is wanted" condition for the automatic attempt.
 */
fun meshHandoffWanted(state: MeshCredentialState): Boolean = state.hasSessionKeys && !state.hasToken

/** Whether a link problem is the kind a fresh token cures (refused, or nothing on the saved port). */
fun meshLinkWantsHandoff(link: MeshLinkState): Boolean =
    link == MeshLinkState.Refused || link == MeshLinkState.Unreachable || link == MeshLinkState.NeedsToken

/**
 * The offer in the fields a platform read off the wire, or why it is not one. Here and not in the
 * Android actual so the rule for a half-formed reply is testable without a Binder.
 */
fun meshHandoffOf(
    version: Int,
    state: String?,
    port: Int,
    token: ByteArray?,
    radioKey: String?,
    peerName: String?,
): MeshHandoffResult =
    if (state == null) MeshHandoffResult.Failed(MeshHandoffFailure.BadReply)
    else MeshHandoffResult.Offer(MeshHandoffOffer(version, state, port, token, radioKey, peerName))
