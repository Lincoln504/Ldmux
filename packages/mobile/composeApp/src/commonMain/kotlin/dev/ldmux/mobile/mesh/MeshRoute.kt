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

/**
 * The platform this build runs on. Only `androidTarget` exists in `composeApp/build.gradle.kts`,
 * so [MobilePlatform.Ios] is a spec value with no build variant yet: when an iOS target is
 * added the compiler will demand its `actual` here, which is the point of declaring it.
 */
expect val currentPlatform: MobilePlatform

/**
 * Whether this build contains the phone's end of the end-to-end session (ADR-0020): `MeshSession`
 * over the Rust library. It does. Whether the library *loaded on this device* is a separate fact,
 * asked at resolution time ([sessionLibAvailable]), so a device whose ABI has no build reports
 * [MeshRouteProblem.SessionNotBuilt] instead of crashing.
 */
const val MESH_SESSION_BUILT: Boolean = true

/**
 * Whether a shell can be carried over the session. It can, as of 2026-10-28 (ADR-0025): the Kotlin
 * port of the row encoding (`MeshTerm`, held to the Rust encoder's bytes), the attach driver
 * (`MeshTermDriver`), the reduced-terminal screen (`MeshTerminalScreen`) and the key delivery
 * (`ldmux code --mesh`, `MeshEnrollment`) exist and are tested. **True does not mean usable**:
 * [resolveRoute] still reports every missing credential, the library not loading, and iOS, and
 * nothing is opened for a route that resolves to a problem. Since 2026-10-29 Settings has a route
 * picker and a scanner for MeshMore's gateway code (`MeshRouteSection`, `meshSetupView`), so the
 * route is reachable; what has still not happened is any run on a device. `docs/STATE.md` has the list.
 */
const val MESH_TERMINAL_BUILT: Boolean = true

/** Whether the native session library loaded on this device. */
fun sessionLibAvailable(): Boolean = loadSessionLib() != null

/** Routes that exist on [platform]. The mesh option does not exist on iOS at all. */
fun availableRoutes(platform: MobilePlatform): List<MeshRoute> = when (platform) {
    MobilePlatform.Android -> listOf(MeshRoute.Tailnet, MeshRoute.Mesh)
    MobilePlatform.Ios -> listOf(MeshRoute.Tailnet)
}

/**
 * Routes the user may *pick*: those that exist here and can get past "not built". A route that
 * can only ever resolve to a not-built problem is not offered, so it cannot be selected where it
 * cannot work; a saved one still resolves visibly (never to another route).
 */
fun selectableRoutes(
    platform: MobilePlatform,
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): List<MeshRoute> = availableRoutes(platform).filter {
    it == MeshRoute.Tailnet || (sessionBuilt && terminalBuilt)
}

/** Parse a persisted route. Unknown text is the tailnet, which is what an old install is. */
fun meshRouteFromStored(text: String?): MeshRoute =
    MeshRoute.entries.firstOrNull { it.name.equals(text, ignoreCase = true) } ?: MeshRoute.Tailnet

/** Why a chosen route cannot be used. Each is shown; none is papered over with another route. */
enum class MeshRouteProblem {
    /** `Mesh` is saved, but this platform has no gateway (iOS). Fails visibly, never falls back. */
    NotOnThisPlatform,

    /** The session library is not in this build, or did not load on this device. */
    SessionNotBuilt,

    /** Kept for a build that ships the session without the terminal profile; the shipped flag is true. */
    TerminalNotBuilt,

    /** No session keys: the pairing code was not scanned with `ldmux code --mesh`, or the keys could not be stored. */
    NeedsSessionKeys,

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
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
    /** The phone holds the L3 session keys (`MeshSessionKeyStore.hasPeer`). */
    hasSessionKeys: Boolean = true,
): MeshRouteResolution {
    if (saved == MeshRoute.Tailnet) return MeshRouteResolution.Use(MeshRoute.Tailnet)
    if (MeshRoute.Mesh !in availableRoutes(platform)) {
        return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NotOnThisPlatform)
    }
    // Checked before the token: asking the user to pair a route that cannot work yet is a
    // trap, so the first thing they are told is the real reason.
    if (!sessionBuilt) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.SessionNotBuilt)
    if (!terminalBuilt) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.TerminalNotBuilt)
    if (!hasSessionKeys) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsSessionKeys)
    if (!hasToken) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsToken)
    if (!hasPeer) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsPeer)
    return MeshRouteResolution.Use(MeshRoute.Mesh)
}

/**
 * One line for the link, in the same register as `linkLabel`. Never says "delivered".
 *
 * Short on purpose. An earlier version named the endpoint and the cure here, and on a phone that
 * wrapped to five lines inside a status bar. The address and the remedy are in [meshBodyFor],
 * which has the room.
 */
fun meshLinkLabel(state: MeshLinkState): String = when (state) {
    MeshLinkState.NeedsToken -> "meshmore · needs pairing token"
    MeshLinkState.Idle -> "meshmore · idle"
    MeshLinkState.Connecting -> "meshmore · connecting"
    MeshLinkState.Bound -> "meshmore · bound"
    MeshLinkState.RadioUnavailable -> "meshmore · radio unavailable"
    MeshLinkState.GatewayBusy -> "meshmore · another app holds the gateway"
    MeshLinkState.ProtocolMismatch -> "meshmore · gateway version mismatch"
    // The header is a status bar, not a diagnosis. Naming the address and the cure here made one
    // line wrap to five on a phone, at the moment the user most needs to see the screen. The
    // address and what to do about it live in the body, which has room and already says both.
    MeshLinkState.Unreachable -> "meshmore · unreachable"
    MeshLinkState.Refused -> "meshmore · refused"
    MeshLinkState.SessionFailed -> "meshmore · host not answering"
    MeshLinkState.Closed -> "meshmore · closed"
}

fun meshProblemLabel(p: MeshRouteProblem): String = when (p) {
    MeshRouteProblem.NotOnThisPlatform -> "meshmore route is not available on this device"
    MeshRouteProblem.SessionNotBuilt -> "meshmore · session library unavailable on this device"
    MeshRouteProblem.TerminalNotBuilt -> "meshmore · the terminal is unavailable on this device"
    MeshRouteProblem.NeedsSessionKeys -> "meshmore · scan a pairing code made with ldmux code --mesh"
    MeshRouteProblem.NeedsToken -> "meshmore · needs pairing token"
    MeshRouteProblem.NeedsPeer -> "meshmore · needs the host radio's public key"
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
