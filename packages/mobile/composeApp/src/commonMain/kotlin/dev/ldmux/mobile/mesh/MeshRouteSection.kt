package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
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
import dev.ldmux.mobile.OptionButton
import dev.ldmux.mobile.Notice
import dev.ldmux.mobile.NoticeLine
import dev.ldmux.mobile.ScanResult
import dev.ldmux.mobile.SettingsNote
import dev.ldmux.mobile.SettingsRow
import dev.ldmux.mobile.SettingsValue
import kotlinx.coroutines.flow.MutableStateFlow
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
    /** The radio's channel is shared (a public tuple) or not. Persisted beside the pairing. */
    onRadioChannel: (Boolean) -> Unit,
    /** How typing leaves the phone: live, or held until sync. Persisted beside the pairing. */
    onInputMode: (MeshInputMode) -> Unit,
    onUnpairMesh: () -> Unit,
) {
    var credentials by remember { mutableStateOf(readCredentials()) }
    val view = meshSetupView(credentials)
    if (!view.meshExists) return // iOS: there is no mesh route to configure

    var note by remember { mutableStateOf<String?>(null) }
    var confirmForget by remember(current.connectionKey) { mutableStateOf(false) }
    var showPicker by remember { mutableStateOf(false) }
    var showPreset by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxWidth()) {
        // Healthy credentials are automatic. Only a missing one is worth a line, and it is a note
        // under the rows rather than a heading of its own: the rows are the settings, and a
        // missing credential is a fact about them.
        for (c in MeshCredential.entries) {
            if (c !in view.present) SettingsNote(meshCredentialLine(c), color = Warn)
        }
        (view.meshResolution as? MeshRouteResolution.Unusable)?.let {
            SettingsNote(meshProblemLabel(it.problem))
        }

        // The radio is the one new physical fact. Its name is shown because a MAC address is not
        // something a person recognises, and the address is kept because that is what BLE needs
        // after a reboot.
        //
        // **Two actions, both written down.** The name used to be tappable to open the preset and
        // the button beside it opened the radio picker, so one row carried a hidden action and a
        // visible one: tapping the name to "select the radio" changed its *operating point*
        // instead, which on a radio that then stops answering is indistinguishable from having
        // broken it.
        SettingsRow("radio") {
            SettingsValue(
                if (current.meshRadio.isEmpty()) {
                    "none chosen"
                } else if (current.meshRadioName.isEmpty()) {
                    current.meshRadio
                } else {
                    current.meshRadioName + "  " + current.meshRadio
                },
                Modifier.weight(1f),
                color = if (current.meshRadio.isEmpty()) Warn else Fg,
            )
            MiniTextButton(onClick = { showPicker = true }) {
                Text(if (current.meshRadio.isEmpty()) "choose" else "change", color = Accent, fontSize = 12.sp, maxLines = 1)
            }
        }

        if (current.meshRadio.isNotEmpty()) {
            // The radio's own operating point and its airtime, read from the radio rather than
            // remembered: the tuple is what the radio is actually on, and the airtime is the
            // duty-cycle figure the budget is spent against.
            MeshRadioFacts(address = current.meshRadio, onOpenPreset = { showPreset = true })

            // **Which kind of channel the radio is on**, beside the radio itself.
            //
            // It is not a setting: it is derived from the tuple the radio reports, and it decides
            // one thing — whether typing is held (ADR-0026). Saying it here is what stops "typing
            // does nothing until sync" from being a mystery.
            SettingsRow("channel") {
                SettingsValue(
                    if (current.meshPublicChannel) {
                        "public · every keystroke is a broadcast"
                    } else {
                        "private · mode is your choice"
                    },
                    Modifier.weight(1f),
                )
            }

            // **The mode is a property of the link, so it is set where the link is set.** It used
            // to be a `hold input` / `leave` pair in the terminal's own row, which made a
            // connection-level decision look like a per-screen toggle and put a "leave" button
            // next to the keys. `live` is unavailable on a public channel, and it is drawn dim
            // rather than removed so the row keeps its shape.
            SettingsRow("input") {
                OptionButton("live", selected = !current.meshHoldsInput, enabled = !current.meshPublicChannel) {
                    if (current.meshInputMode != MeshInputMode.Live) onInputMode(MeshInputMode.Live)
                }
                OptionButton("manual sync", selected = current.meshHoldsInput) {
                    if (current.meshInputMode != MeshInputMode.ManualSync) onInputMode(MeshInputMode.ManualSync)
                }
            }
            if (current.meshPublicChannel) {
                SettingsNote("a public channel forces manual sync: every keystroke there is a broadcast")
            }
        }

        // One verb, one target: the ldmux code carries the session keys and the host radio key.
        SettingsRow("code") {
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
            }
        }
        if (confirmForget) {
            SettingsNote("this forgets the keys, the radio and the saved route")
        }
        note?.let { SettingsNote(it, color = Danger) }
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
            onClassified = onRadioChannel,
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

/**
 * The radio's own operating point and its airtime, read from the radio.
 *
 * **Read, not remembered.** The tuple is what the radio is actually on, and the airtime is the
 * duty-cycle figure the budget is spent against — both are facts about the hardware, so a copy in
 * the config would be a second truth that drifts. The link is the app's shared one
 * (`RadioLinks`), so opening Settings while the terminal is live does not start a second GATT
 * client; `connect` returns at once when it is already bound.
 *
 * The airtime line is the one the user asked for: "the app can track in settings, like duty cycle
 * use". It is the radio's own estimate for the current hour, and the records line is what is left
 * of the hourly budget at the current record size. Both are display only — the budget is enforced
 * where records are sent, not here.
 */
@Composable
private fun MeshRadioFacts(address: String, onOpenPreset: () -> Unit) {
    var link by remember(address) { mutableStateOf<RadioLink?>(null) }
    val fallbackInfo = remember { MutableStateFlow<RadioSelfInfo?>(null) }
    val fallbackDevice = remember { MutableStateFlow<RadioDeviceInfo?>(null) }
    val fallbackStatus = remember { MutableStateFlow<MeshStatus?>(null) }
    val info by (link?.selfInfo ?: fallbackInfo).collectAsState()
    val device by (link?.deviceInfo ?: fallbackDevice).collectAsState()
    val status by (link?.status ?: fallbackStatus).collectAsState()
    LaunchedEffect(address) {
        if (address.isEmpty()) return@LaunchedEffect
        val l = meshRadioLink(address, ByteArray(0)) ?: return@LaunchedEffect
        link = l
        // A read, not a session: the radio answers SELF_INFO/DEVICE_INFO without a peer, and a
        // failure here is not worth a line — the rows below say "not read" instead.
        runCatching { l.connect() }
    }
    // **Capture the link, do not read the state in `onDispose`.** `link` is a delegated state
    // property, so a closure that reads it inside `onDispose` reads the value *at dispose time* —
    // which is the new link when the key changed from the old one. That released the instance the
    // effect had just been created for, taking the refcount to zero and closing a live BLE link
    // the moment it was acquired (measured on the device: the GATT opened and was torn down again
    // within eight seconds, and the radio's facts stayed "not read").
    DisposableEffect(link) {
        val held = link
        onDispose { held?.release() }
    }

    val preset = info?.let {
        RadioPreset(
            frequencyMhz = it.frequencyMhz,
            bandwidthKhz = it.bandwidthKhz,
            spreadingFactor = it.spreadingFactor,
            codingRate = it.codingRate,
            txPowerDbm = it.txPowerDbm,
            pathHashSize = (device?.pathHashMode ?: 0) + 1,
        )
    }
    SettingsRow("preset") {
        SettingsValue(
            preset?.let { presetLine(it) } ?: "not read",
            Modifier.weight(1f),
            color = if (preset == null) Muted else Fg,
            // Two lines: the tuple is five facts and the row has a button beside it, so one line
            // cut it at `SF8 …` on the device.
            maxLines = 2,
        )
        MiniTextButton(onClick = onOpenPreset) {
            Text("preset", color = Accent, fontSize = 12.sp, maxLines = 1)
        }
    }
    SettingsRow("airtime") {
        SettingsValue(airtimeLine(status), Modifier.weight(1f), maxLines = 2)
    }
}

/**
 * The hour's airtime and what is left of the budget, in the radio's own units.
 *
 * `txAirtimeTenthsThisHour` is tenths of a second, and the app's own budget is
 * [RadioAirtime.BUDGET_MS_PER_HOUR] (36 s, the 1 % duty cycle). The records figure is null when
 * the budget is open, which is a different fact from "zero left" and is said as such.
 */
internal fun airtimeLine(status: MeshStatus?): String {
    if (status == null) return "not read"
    val used = status.txAirtimeTenthsThisHour / 10.0
    val budget = RadioAirtime.BUDGET_MS_PER_HOUR / 1000.0
    val left = status.opsLeftThisHour
    val records = when {
        left == null -> "no hourly cap"
        left >= 255 -> "255 or more records left"
        else -> "$left records left"
    }
    return "%.1f s of %.0f s this hour · %s".format(used, budget, records)
}
