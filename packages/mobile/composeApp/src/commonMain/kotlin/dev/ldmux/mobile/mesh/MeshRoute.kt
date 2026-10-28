package dev.ldmux.mobile.mesh

/**
 * How an endpoint is reached. A per-endpoint choice, never a global fallback chain.
 *
 * `Tailnet` is the existing WebSocket to the daemon. `Mesh` is the MeshMore loopback gateway
 * and, through it, a LoRa radio. They are different links with different rates, and a route
 * that silently turned into the other would hide why the app got slow or went quiet.
 */
enum class MeshRoute { Tailnet, Mesh }

/** The platforms the mobile app ships on. iOS is tailnet only: no MeshCore, no gateway. */
enum class MobilePlatform { Android, Ios }

/** Routes the user may pick on [platform]. The mesh option does not exist on iOS at all. */
fun availableRoutes(platform: MobilePlatform): List<MeshRoute> = when (platform) {
    MobilePlatform.Android -> listOf(MeshRoute.Tailnet, MeshRoute.Mesh)
    MobilePlatform.Ios -> listOf(MeshRoute.Tailnet)
}

/** Why a chosen route cannot be used. Each is shown; none is papered over with another route. */
enum class MeshRouteProblem {
    /** `Mesh` is saved, but this platform has no gateway (iOS). Fails visibly, never falls back. */
    NotOnThisPlatform,

    /** No pairing token for the MeshMore gateway yet. */
    NeedsToken,

    /** No host radio public key to bind. */
    NeedsPeer,
}

sealed interface MeshRouteResolution {
    data class Use(val route: MeshRoute) : MeshRouteResolution
    data class Unusable(val route: MeshRoute, val problem: MeshRouteProblem) : MeshRouteResolution
}

/**
 * Decide what to do with a saved route. A saved `Mesh` on a platform without it is
 * [MeshRouteResolution.Unusable], not a quiet switch to the tailnet: the user chose mesh,
 * and what they see must say why it is not happening.
 */
fun resolveRoute(
    saved: MeshRoute,
    platform: MobilePlatform,
    hasToken: Boolean,
    hasPeer: Boolean,
): MeshRouteResolution {
    if (saved == MeshRoute.Tailnet) return MeshRouteResolution.Use(MeshRoute.Tailnet)
    if (MeshRoute.Mesh !in availableRoutes(platform)) {
        return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NotOnThisPlatform)
    }
    if (!hasToken) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsToken)
    if (!hasPeer) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsPeer)
    return MeshRouteResolution.Use(MeshRoute.Mesh)
}

/** One line for the link, in the same register as `linkLabel`. Never says "delivered". */
fun meshLinkLabel(state: MeshLinkState): String = when (state) {
    MeshLinkState.NeedsToken -> "mesh · needs pairing token"
    MeshLinkState.Idle -> "mesh · idle"
    MeshLinkState.Connecting -> "mesh · connecting"
    MeshLinkState.Bound -> "mesh · bound"
    MeshLinkState.RadioUnavailable -> "mesh · radio unavailable"
    MeshLinkState.GatewayBusy -> "mesh · another app holds the gateway"
    MeshLinkState.ProtocolMismatch -> "mesh · gateway version mismatch"
    MeshLinkState.Unreachable -> "mesh · MeshMore not running"
    MeshLinkState.Refused -> "mesh · refused (re-pair, or add the host as a contact)"
    MeshLinkState.Closed -> "mesh · closed"
}

fun meshProblemLabel(p: MeshRouteProblem): String = when (p) {
    MeshRouteProblem.NotOnThisPlatform -> "mesh route is not available on this device"
    MeshRouteProblem.NeedsToken -> "mesh · needs pairing token"
    MeshRouteProblem.NeedsPeer -> "mesh · needs the host radio's public key"
}

/**
 * What the route can honestly offer. The mesh link is a low-rate command channel, not a
 * terminal (`docs/MESH-GATEWAY.md` section 6.3): no live cursor, no per-keystroke echo, no
 * streaming. The UI reads these flags instead of assuming the tailnet's capabilities.
 */
data class RouteCapabilities(
    val liveCursor: Boolean,
    val keystrokeEcho: Boolean,
    val streamingOutput: Boolean,
    /** Every reply carries a timestamp, because "minutes later" is normal. */
    val timestampReplies: Boolean,
    /** Show the budget as operations left this hour, not as seconds. */
    val budgetInOperations: Boolean,
)

fun capabilitiesOf(route: MeshRoute): RouteCapabilities = when (route) {
    MeshRoute.Tailnet -> RouteCapabilities(true, true, true, false, false)
    MeshRoute.Mesh -> RouteCapabilities(false, false, false, true, true)
}

/** The honest one-liner shown wherever the mesh route is offered or active. */
const val MESH_RATE_NOTICE: String =
    "Low-rate command channel over LoRa. Replies take minutes, not milliseconds, and a sent command is not a delivered one."

/*
 * Operations left this hour is NOT computed here. It arrives in `STATUS` as
 * [MeshStatus.opsLeftThisHour], derived by the gateway from the radio's real preset. A function
 * that took airtime-per-record would make this app carry radio knowledge, which the architecture
 * forbids (meshmore-sns-architecture.md, responsibility matrix).
 */
