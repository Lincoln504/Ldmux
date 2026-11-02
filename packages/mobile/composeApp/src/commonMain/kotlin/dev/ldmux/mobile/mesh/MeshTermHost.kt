package dev.ldmux.mobile.mesh

/**
 * The credentials a mesh attach needs, all read from secure storage.
 *
 * The MeshMore gateway token is gone. The radio route needs the L3 session keys
 * and the host radio public key; the phone owns the BLE link directly.
 */
class MeshCredentials(val keys: MeshSessionKeys, val hostRadio: ByteArray)

/** Why there are no [MeshCredentials]: what resolveRoute should be told. */
data class MeshCredentialState(val hasSessionKeys: Boolean, val hasPeer: Boolean) {
    val complete: Boolean get() = hasSessionKeys && hasPeer
}

/** What is stored, without ever throwing: a broken Keystore reads as "needs pairing". */
fun readMeshCredentials(keys: MeshSessionKeyStore): Pair<MeshCredentialState, MeshCredentials?> {
    val k = runCatching { keys.load() }.getOrNull()
    val state = MeshCredentialState(hasSessionKeys = k != null, hasPeer = k?.hostRadio != null)
    val creds = if (k != null && k.hostRadio != null) MeshCredentials(k, k.hostRadio) else null
    return state to creds
}

/**
 * The header's status line, or empty when the view has nothing to report.
 *
 * A header that says "choose a shell" above a list of shells is noise: the screen already says
 * it, and the one line costs a row that the terminal will want. Only a state the user would not
 * otherwise know - securing, waiting, failed, closed - earns the line.
 */
fun meshHeaderStatus(view: MeshTermView): String = when (view) {
    // Routine: the body already says what to do.
    is MeshTermView.Choose, MeshTermView.Idle, MeshTermView.Listing -> ""
    is MeshTermView.Live -> if (view.resyncing || !view.painted || view.stalled) {
        meshTermLabel(view)
    } else {
        ""
    }
    else -> meshTermLabel(view)
}

fun meshTermLabel(view: MeshTermView): String = when (view) {
    MeshTermView.Idle -> "mesh · not attached"
    is MeshTermView.LinkDown -> meshLinkLabel(view.link)
    // "Can take minutes" was the old truth (a single msg1 and a five-minute timeout). A handshake
    // is three small records and is now retried every six seconds, so the honest label is the
    // state, not a warning about how long it might take.
    MeshTermView.Handshaking -> "mesh · securing the link"
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

/**
 * What is wrong with a saved mesh route right now, read from secure storage; null when it can
 * run. Never throws. The launcher uses it so the line under the title names the real next step
 * (scan a code, choose a radio) and not a guess.
 */
fun currentMeshResolution(route: MeshRoute): MeshRouteResolution {
    val (state, _) = readMeshCredentials(meshSessionKeyStore())
    return resolveRoute(
        route, currentPlatform,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
    )
}

/** Everything the mesh screen needs: the credentials, the library, and the selected radio. */
class MeshScreenPlan(
    val creds: MeshCredentials,
    val lib: SessionLib,
    /** The BLE address selected for this install; empty means no radio has been chosen. */
    val radioAddress: String = "",
)

/**
 * What to show the mesh terminal with, or null when the saved route is not mesh or cannot run (in
 * which case the caller shows the normal app and its stated reason). **Not gated on the tailnet
 * pairing**: a mesh-only device has no daemon host or token and is still a complete mesh pairing.
 * Tailnet configs return null without touching secure storage.
 */
fun meshScreenFor(config: dev.ldmux.mobile.LdmuxConfig): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    return meshScreenPlan(config, meshSessionKeyStore(), loadSessionLib())
}

/** [meshScreenFor] with its inputs passed in, so the rule is testable without a Keystore. */
fun meshScreenPlan(
    config: dev.ldmux.mobile.LdmuxConfig,
    keys: MeshSessionKeyStore,
    lib: SessionLib?,
    platform: MobilePlatform = currentPlatform,
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    val (state, creds) = readMeshCredentials(keys)
    if (lib == null) return null
    val resolution = resolveRoute(
        config.route, platform,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = MESH_SESSION_BUILT, terminalBuilt = terminalBuilt,
    )
    return if (resolution is MeshRouteResolution.Use && creds != null) {
        MeshScreenPlan(creds, lib, config.meshRadio)
    } else null
}

/** One picker line: the label when the host gave one, then the id the choice will send. */
fun meshShellLine(e: SessionEntry): String =
    if (e.label.isEmpty()) e.id else "${e.label}  (${e.id.take(8)})"
