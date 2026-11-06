package io.ldmux.mobile.mesh

/**
 * How an endpoint is reached. A per-endpoint choice, never a global fallback chain.
 *
 * `Tailnet` is the existing WebSocket to the daemon. `Mesh` is the phone-owned BLE radio
 * and, through it, a LoRa link to the host. They are different links with different rates,
 * and a route that silently turned into the other would hide why the app got slow or went quiet.
 */
enum class MeshRoute { Tailnet, Mesh }

/**
 * How typing leaves the phone on the mesh route.
 *
 * The names are the user's words, not the implementation's: `live` is "it goes as I type" and
 * `batch` is "my typing leaves when I send it". The old name for the second one was
 * "public-channel mode", which described *why* it exists rather than what it does, and it was
 * forced on a public channel — so a user on a private channel had a mode named after a condition
 * that was not true for them.
 */
enum class MeshInputMode { Live, ManualSync }

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
 * [resolveRoute] still reports every missing credential and the library not loading, and
 * nothing is opened for a route that resolves to a problem. The route is chosen in the app header
 * (`availableRoutes`), and Settings carries the BLE radio selector (`MeshRouteSection`,
 * `meshSetupView`). The standalone path has a two-radio hardware run as of 2026-11-02;
 * `docs/ARCHITECTURE.md` sections 2 and 3 are the current record.
 */
const val MESH_TERMINAL_BUILT: Boolean = true

/** Whether the native session library loaded on this device. */
fun sessionLibAvailable(): Boolean = loadSessionLib() != null

/**
 * Every route this build can take. One list, because there is one target: `composeApp`
 * declares only `androidTarget`, so a per-platform answer had a branch nothing could reach.
 * A second client (`packages/mobile` would need an iOS target first) adds its own answer here.
 */
fun availableRoutes(): List<MeshRoute> = listOf(MeshRoute.Tailnet, MeshRoute.Mesh)

/** Parse a persisted route. Unknown text is the tailnet, which is what an old install is. */
fun meshRouteFromStored(text: String?): MeshRoute =
    MeshRoute.entries.firstOrNull { it.name.equals(text, ignoreCase = true) } ?: MeshRoute.Tailnet

/** Why a chosen route cannot be used. Each is shown; none is papered over with another route. */
enum class MeshRouteProblem {
    /** The session library is not in this build, or did not load on this device. */
    SessionNotBuilt,

    /** Kept for a build that ships the session without the terminal profile; the shipped flag is true. */
    TerminalNotBuilt,

    /** No session keys: the pairing code was not scanned with `ldmux code --mesh`, or the keys could not be stored. */
    NeedsSessionKeys,

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
    hasPeer: Boolean,
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
    /** The phone holds the L3 session keys (`MeshSessionKeyStore.hasPeer`). */
    hasSessionKeys: Boolean = true,
): MeshRouteResolution {
    if (saved == MeshRoute.Tailnet) return MeshRouteResolution.Use(MeshRoute.Tailnet)
    // Checked before the peer: asking the user to pair a route that cannot work yet is a
    // trap, so the first thing they are told is the real reason.
    if (!sessionBuilt) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.SessionNotBuilt)
    if (!terminalBuilt) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.TerminalNotBuilt)
    if (!hasSessionKeys) return MeshRouteResolution.Unusable(saved, MeshRouteProblem.NeedsSessionKeys)
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
    MeshLinkState.NeedsRadio -> "mesh · choose a LoRa radio"
    MeshLinkState.NeedsPermission -> "mesh · Bluetooth permission is off"
    MeshLinkState.NoPath -> "mesh · the host's LoRa radio has no usable direct path"
    MeshLinkState.Idle -> "mesh · idle"
    MeshLinkState.Connecting -> "mesh · connecting"
    MeshLinkState.Bound -> "mesh · bound"
    MeshLinkState.RadioUnavailable -> "mesh · LoRa radio unavailable"
    MeshLinkState.Refused -> "mesh · the LoRa radio refused that record, retrying"
    MeshLinkState.SessionFailed -> "mesh · link dropped, reconnecting"
    MeshLinkState.Closed -> "mesh · closed"
}

fun meshProblemLabel(p: MeshRouteProblem): String = when (p) {
    MeshRouteProblem.SessionNotBuilt -> "mesh · session library unavailable on this device"
    MeshRouteProblem.TerminalNotBuilt -> "mesh · the terminal is unavailable on this device"
    MeshRouteProblem.NeedsSessionKeys -> "mesh · scan a pairing code made with ldmux code --mesh"
    MeshRouteProblem.NeedsPeer -> "mesh · needs the host's LoRa radio public key"
}

/*
 * Operations left this hour is NOT computed here. It arrives in `STATUS` as
 * [MeshStatus.opsLeftThisHour], derived by the radio adapter from the live preset. A function
 * that took airtime-per-record would make the UI carry radio knowledge.
 */
