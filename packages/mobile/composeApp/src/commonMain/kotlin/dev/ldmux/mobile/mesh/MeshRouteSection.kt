package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicTextField
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
 * Honest by construction, and **terse by decision**: Settings is a list of controls and their
 * state, not a manual. The rate limit and the "MeshMore must be running" fact are stated where
 * they bite — on the mesh screen, next to the input — not here, where they were noise that pushed
 * the controls off the page.
 *  - **Hidden where the route does not exist** (iOS), not shown and disabled.
 *  - A route that cannot run is **not offered**; what is missing is a short mark on the
 *    credential line, in the order the ceremony goes.
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
    var pasted by remember { mutableStateOf("") }
    var confirmForget by remember(current.connectionKey) { mutableStateOf(false) }
    Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("mesh route", color = Fg, fontSize = 13.sp)
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
        // Same-phone pairing cannot be scanned: MeshMore's code is displayed by the other app on
        // this very screen, so there is nothing for this camera to point at. Typing or pasting the
        // code is the only path for the daily case, and it is routed by its own scheme so a user
        // never has to know which box it belongs in.
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            BasicTextField(
                value = pasted,
                onValueChange = { pasted = it },
                singleLine = true,
                textStyle = androidx.compose.ui.text.TextStyle(color = Fg, fontSize = 12.sp),
                modifier = Modifier
                    .weight(1f)
                    .padding(vertical = 4.dp),
                decorationBox = { inner ->
                    if (pasted.isEmpty()) Text("paste a code", color = Dim, fontSize = 12.sp) else inner()
                },
            )
            MiniTextButton(onClick = {
                note = null
                val code = pasted.trim()
                if (code.startsWith("meshmore://") || code.matches(Regex("[0-9a-fA-F]{32}"))) {
                    when (val g = onGatewayCode(code)) {
                        is GatewayScan.Saved -> { onConfig(g.config); credentials = readCredentials(); pasted = "" }
                        is GatewayScan.Refused -> note = gatewayScanLine(g.problem)
                    }
                } else {
                    when (val o = onMeshPairing(code).also { credentials = readCredentials() }) {
                        is MeshPairingScan.Enrolled -> pasted = ""
                        is MeshPairingScan.Refused -> note = meshPairingLine(o.problem)
                    }
                }
            }) { Text("use code", color = Accent, fontSize = 12.sp) }
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
