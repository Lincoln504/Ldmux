package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.CodeScanner
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.ScanResult
import dev.ldmux.mobile.Warn

/**
 * The mesh route's setup, in Settings. It draws [meshSetupView] and decides nothing: which
 * credentials exist, what a mesh route would do, and which routes may be chosen are all computed
 * there and tested there.
 *
 * Honest by construction:
 *  - **Hidden where the route does not exist** (iOS), not shown and disabled.
 *  - A route that cannot run is **not offered**; what is missing is listed instead, in the order
 *    the ceremony goes (ldmux code, then MeshMore's code).
 *  - It says the one real constraint: the mesh route only works **while MeshMore is running on this
 *    phone** with its gateway on. Its Connected apps screen shows that; when it is not running this
 *    app shows "mesh · MeshMore not running" and nothing else happens.
 */
@Composable
internal fun MeshRouteSection(
    current: LdmuxConfig,
    /** Read secure storage now. Called again after every action, because the config alone may not change. */
    readCredentials: () -> MeshCredentialState,
    /** Apply a scanned ldmux code to the mesh route ([applyMeshPairingScan]). */
    onMeshPairing: (String) -> MeshPairingScan,
    /** Take a scanned MeshMore gateway code; returns the result (the caller stores the token). */
    onGatewayCode: (String) -> GatewayScan,
    /** Persist a changed config now (route, gateway) without closing Settings. */
    onConfig: (LdmuxConfig) -> Unit,
    onUnpairMesh: () -> Unit,
) {
    var credentials by remember { mutableStateOf(readCredentials()) }
    val view = meshSetupView(current, credentials)
    if (!view.meshExists) return // iOS: there is no mesh route to configure

    var note by remember { mutableStateOf<String?>(null) }
    var confirmForget by remember(current.connectionKey) { mutableStateOf(false) }
    Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("mesh route", color = Fg, fontSize = 13.sp)
        Text(MESH_RATE_NOTICE, color = Dim, fontSize = 11.sp)
        Text(
            "Works only while MeshMore is running on this phone with its gateway on (MeshMore → Connected apps). " +
                "If it is not, this app says \"mesh · MeshMore not running\" and opens nothing else.",
            color = Dim, fontSize = 11.sp,
        )
        for (c in MeshCredential.entries) {
            val ok = c in view.present
            Text(meshCredentialLine(c, ok), color = if (ok) Fg else Warn, fontSize = 12.sp)
        }
        (view.meshResolution as? MeshRouteResolution.Unusable)?.let {
            Text(meshProblemLabel(it.problem), color = Dim, fontSize = 11.sp)
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            MiniTextButton(onClick = {
                note = null
                CodeScanner.scan { r ->
                    note = when (r) {
                        is ScanResult.Code -> when (val o = onMeshPairing(r.text).also { credentials = readCredentials() }) {
                            is MeshPairingScan.Enrolled -> null
                            is MeshPairingScan.Refused -> meshPairingLine(o.problem)
                        }
                        ScanResult.Cancelled -> null
                        ScanResult.Unavailable -> "scanner unavailable"
                    }
                }
            }) { Text("scan ldmux code", color = Accent, fontSize = 12.sp) }
            MiniTextButton(onClick = {
                note = null
                CodeScanner.scan { r ->
                    note = when (r) {
                        is ScanResult.Code -> when (val g = onGatewayCode(r.text)) {
                            is GatewayScan.Saved -> { onConfig(g.config); credentials = readCredentials(); null }
                            is GatewayScan.Refused -> gatewayScanLine(g.problem)
                        }
                        ScanResult.Cancelled -> null
                        ScanResult.Unavailable -> "scanner unavailable"
                    }
                }
            }) { Text("scan MeshMore code", color = Accent, fontSize = 12.sp) }
        }
        // The picker: only routes that can run are offered, so it cannot save one that will not.
        if (view.choosable.isNotEmpty()) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("route", color = Dim, fontSize = 12.sp)
                for (route in view.choosable) {
                    MiniTextButton(onClick = { chooseRoute(current, route, view)?.let(onConfig) }) {
                        Text(route.name.lowercase(), color = if (current.route == route) Accent else Dim)
                    }
                }
            }
        } else {
            Text("no route can run yet: pair the tailnet or finish the mesh pairing", color = Dim, fontSize = 11.sp)
        }
        if (view.present.isNotEmpty()) {
            MiniTextButton(onClick = { if (confirmForget) { onUnpairMesh(); credentials = readCredentials() } else confirmForget = true }) {
                Text(if (confirmForget) "forget mesh pairing?" else "forget mesh pairing", color = if (confirmForget) Accent else Dim, fontSize = 12.sp)
            }
        }
        note?.let { Text(it, color = Warn, fontSize = 11.sp) }
    }
}

/** What the Settings dialog calls into for the mesh route. One place wires it to the real stores. */
class MeshRouteActions(
    /** Read secure storage, never throwing (a broken Keystore reads as "nothing stored"). */
    val credentials: () -> MeshCredentialState,
    val onPairing: (String) -> MeshPairingScan,
    val onGatewayCode: (String) -> GatewayScan,
    val onConfig: (LdmuxConfig) -> Unit,
    val onUnpair: () -> Unit,
)
