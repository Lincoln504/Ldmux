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
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.CodeScanner
import dev.ldmux.mobile.Danger
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.Muted
import dev.ldmux.mobile.Notice
import dev.ldmux.mobile.NoticeLine
import dev.ldmux.mobile.ScanResult
import dev.ldmux.mobile.Warn

/**
 * The mesh route's setup, in Settings. It draws [meshSetupView] and decides nothing: which
 * credentials exist, what a mesh route would do, and which routes may be chosen are all computed
 * there and tested there.
 *
 * The phone owns the radio. There is no MeshMore token, no Binder handoff and no inter-app gateway
 * endpoint here: one scan selects the radio, one preset action writes the operating point, and
 * the ordinary ldmux pairing code supplies the L3 keys and the host radio key.
 */
@Composable
internal fun MeshRouteSection(
    current: LdmuxConfig,
    /** Read secure storage now. Called again after every action, because the config alone may not change. */
    readCredentials: () -> MeshCredentialState,
    /** Apply a scanned ldmux code to the mesh route ([applyMeshPairingScan]). */
    onMeshPairing: (String) -> MeshPairingScan,
    /** Persist a changed config now (route, radio) without closing Settings. */
    onConfig: (LdmuxConfig) -> Unit,
    onUnpairMesh: () -> Unit,
) {
    var credentials by remember { mutableStateOf(readCredentials()) }
    val view = meshSetupView(credentials)
    if (!view.meshExists) return // iOS: there is no mesh route to configure

    var note by remember { mutableStateOf<String?>(null) }
    var confirmForget by remember(current.connectionKey) { mutableStateOf(false) }
    var showPicker by remember { mutableStateOf(false) }
    var showPreset by remember { mutableStateOf(false) }

    Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        // The same label shape as the `tailnet` row, because this is the other half of the
        // same question. It was a 13sp heading in `Fg`, which made the two pairings look
        // like a section and a row rather than two rows.
        Text("mesh", color = Dim, fontSize = 12.sp)
        // Healthy credentials are automatic. Only a missing one is worth a line.
        for (c in MeshCredential.entries) {
            if (c !in view.present) {
                Text(meshCredentialLine(c), color = Warn, fontSize = 12.sp)
            }
        }
        (view.meshResolution as? MeshRouteResolution.Unusable)?.let {
            Text(meshProblemLabel(it.problem), color = Muted, fontSize = 11.sp)
        }

        // The radio is the one new physical fact. Its name is shown because a MAC address is
        // not something a person recognises, and the address is kept because that is what BLE
        // needs after a reboot.
        // **Two actions, both written down.** The name used to be tappable to open the preset
        // and the button beside it opened the radio picker, so one row carried a hidden action
        // and a visible one: tapping the name to "select the radio" changed its *operating
        // point* instead, which on a radio that then stops answering is indistinguishable from
        // having broken it.
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text("radio", color = Dim, fontSize = 12.sp)
            Text(
                if (current.meshRadio.isEmpty()) {
                    "none chosen"
                } else if (current.meshRadioName.isEmpty()) {
                    current.meshRadio
                } else {
                    current.meshRadioName + "  " + current.meshRadio
                },
                color = if (current.meshRadio.isEmpty()) Warn else Fg,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (current.meshRadio.isNotEmpty()) {
                MiniTextButton(onClick = { showPreset = true }) {
                    Text("preset", color = Accent, fontSize = 12.sp, maxLines = 1)
                }
            }
            MiniTextButton(onClick = { showPicker = true }) {
                Text(if (current.meshRadio.isEmpty()) "choose" else "change", color = Accent, fontSize = 12.sp, maxLines = 1)
            }
        }

        // One verb, one target: the ldmux code carries the session keys and the host radio key.
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text("code", color = Dim, fontSize = 12.sp)
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
            }) { Text("scan", color = Accent, fontSize = 12.sp, maxLines = 1) }
        }
        if (view.present.isNotEmpty()) {
            MiniTextButton(onClick = {
                if (confirmForget) {
                    onUnpairMesh()
                    credentials = readCredentials()
                } else {
                    confirmForget = true
                }
            }) {
                // `unpair`, the word the tailnet row uses, because it is the same act on the
                // other pairing. It also drops the chosen radio and the saved route, which is
                // why it asks first and says so.
                Text(if (confirmForget) "unpair?" else "unpair", color = if (confirmForget) Danger else Dim, fontSize = 12.sp)
            }
            if (confirmForget) {
                Text("this forgets the keys, the radio and the saved route", color = Muted, fontSize = 11.sp)
            }
        }
        note?.let { Text(it, color = Danger, fontSize = 11.sp) }
    }

    if (showPicker) {
        RadioPickerDialog(
            onDismiss = { showPicker = false },
            onPick = { d ->
                onConfig(current.copy(meshRadio = d.address, meshRadioName = d.name))
                credentials = readCredentials()
                // One flow: choosing a radio moves straight to its operating point,
                // so a first run does not stop at an address and leave the radio deaf.
                showPicker = false
                showPreset = true
            },
        )
    }
    if (showPreset) {
        RadioPresetDialog(
            address = current.meshRadio,
            openAirtime = current.meshOpenAirtime,
            onBudget = { onConfig(current.copy(meshOpenAirtime = it == RadioBudgetMode.Open)) },
            onUnset = {
                onConfig(current.copy(meshRadio = "", meshRadioName = ""))
                showPreset = false
            },
            onDismiss = { showPreset = false },
        )
    }
}

/** What the Settings dialog calls into for the mesh route. One place wires it to the real stores. */
class MeshRouteActions(
    /** Read secure storage, never throwing (a broken Keystore reads as "nothing stored"). */
    val credentials: () -> MeshCredentialState,
    val onPairing: (String) -> MeshPairingScan,
    val onConfig: (LdmuxConfig) -> Unit,
    val onUnpair: () -> Unit,
)
