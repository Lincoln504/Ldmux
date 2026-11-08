package io.ldmux.mobile.mesh

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
import io.ldmux.mobile.Accent
import io.ldmux.mobile.CodeScanner
import io.ldmux.mobile.Danger
import io.ldmux.mobile.Dim
import io.ldmux.mobile.Fg
import io.ldmux.mobile.LdmuxConfig
import io.ldmux.mobile.MiniTextButton
import io.ldmux.mobile.Muted
import io.ldmux.mobile.OptionButton
import io.ldmux.mobile.Notice
import io.ldmux.mobile.NoticeLine
import io.ldmux.mobile.ScanResult
import io.ldmux.mobile.SettingsChoice
import io.ldmux.mobile.SettingsNote
import io.ldmux.mobile.SettingsRow
import io.ldmux.mobile.SettingsToggle
import io.ldmux.mobile.SettingsValue
import kotlinx.coroutines.flow.MutableStateFlow
import io.ldmux.mobile.Warn
import io.ldmux.mobile.SettingsInline
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import io.ldmux.mobile.parseHostPreset

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
    /**
     * Bumped by every pairing in the row above.
     *
     * Secure storage is not part of the config, so nothing here recomposes when a scan stores the
     * keys: without this the block kept the reading it captured at first composition and went on
     * saying "scan a pairing code made with `ldmux code --mesh`" directly under a row that had
     * just said "· radio". The pair succeeded and the screen contradicted it.
     */
    credentialsVersion: Int = 0,
    /** Read secure storage now. Called again after every action, because the config alone may not change. */
    readCredentials: () -> MeshCredentialState,
    /** Persist a changed config now (route, radio) without closing Settings. */
    onConfig: (LdmuxConfig) -> Unit,
    /** The radio's channel is shared (a public tuple) or not. Persisted beside the pairing. */
    onRadioChannel: (Boolean) -> Unit,
    /** How typing leaves the phone: live, or held until sync. Persisted beside the pairing. */
    onInputMode: (MeshInputMode) -> Unit,
) {
    var credentials by remember(credentialsVersion) { mutableStateOf(readCredentials()) }
    val view = meshSetupView(credentials)

    var showPicker by remember { mutableStateOf(false) }
    var showPreset by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxWidth()) {
        // **The pairing is not here any more.** It is one row in the `pairing` section, because a
        // scanned code carries the tailnet's address and token and this route's keys together and
        // `ConfigStore.pair` applies whichever halves it holds. This route's own row asked the same
        // question twice and left the reader guessing which one to scan under.
        (view.meshResolution as? MeshRouteResolution.Unusable)?.let {
            SettingsNote(meshProblemLabel(it.problem))
        }
        // **The success state, where the scan happened.** A code that carries only the radio's
        // keys changes no tailnet fact, so with nothing said here a completed pairing and a
        // refused one looked the same. The row above says `· radio`; this says what is stored.
        if (view.present.size == MeshCredential.entries.size) {
            SettingsNote("keys stored · host radio key pinned")
        } else if (view.present.isNotEmpty()) {
            SettingsNote(
                "stored: " + view.present.joinToString { meshCredentialName(it) } +
                    " · missing: " + MeshCredential.entries.filterNot { it in view.present }
                        .joinToString { meshCredentialName(it) },
            )
        }

        // The radio is the one new physical fact. Its name is shown because a MAC address is not
        // something a person recognises, and the address is kept because that is what BLE needs
        // after a reboot.
        SettingsRow(
            // Which radio THIS PHONE drives. The host's radio is on the host's own USB.
            "phone's radio",
            // **The channel belongs here, not in a row of its own.** It is derived from the
            // preset the panel writes — nothing on this page can change it — so a separate
            // `channel` row said the same thing twice and made the page look like it had two
            // radio settings. The address moved into the panel for the same reason: it is the
            // same fact as the name, and two long strings do not fit one summary line.
            summary = when {
                current.meshRadio.isEmpty() -> "none chosen"
                current.meshPublicChannel -> (current.meshRadioName.ifEmpty { current.meshRadio }) + " · public channel"
                else -> (current.meshRadioName.ifEmpty { current.meshRadio }) + " · private channel"
            },
            trailing = {
                MiniTextButton(onClick = { showPicker = !showPicker; showPreset = false }) {
                    Text(
                        if (current.meshRadio.isEmpty()) "choose" else "change",
                        color = Accent,
                        fontSize = 12.sp,
                        maxLines = 1,
                    )
                }
            },
        )
        // The picker opens under the row that named the radio, not at the end of the list and not
        // over the screen: it is an edit to this row.
        if (showPicker) {
            RadioPickerPanel(
                onDismiss = { showPicker = false },
                onPick = { d ->
                    onConfig(current.copy(meshRadio = d.address, meshRadioName = d.name))
                    credentials = readCredentials()
                    // One flow: choosing a radio moves straight to its operating point, so a
                    // first run does not stop at an address and leave the radio deaf.
                    showPicker = false
                    showPreset = true
                },
            )
        }

        if (current.meshRadio.isNotEmpty()) {
            // The radio's own operating point and its airtime, read from the radio rather than
            // remembered.
            MeshRadioFacts(
                // **What the desktop's radio is on, from the pairing code.** The phone had no way to
                // learn this: a mismatch was silent and looked exactly like being out of range.
                hostPreset = parseHostPreset(current.meshHostPreset),
                address = current.meshRadio,
                openAirtime = current.meshOpenAirtime,
                onBudget = { onConfig(current.copy(meshOpenAirtime = it == RadioBudgetMode.Open)) },
                onOpenPreset = { showPreset = !showPreset; showPicker = false },
                afterPreset = {
                    if (showPreset) {
                        RadioPresetPanel(
                            address = current.meshRadio,
                            openAirtime = current.meshOpenAirtime,
                            onBudget = { onConfig(current.copy(meshOpenAirtime = it == RadioBudgetMode.Open)) },
                            onClassified = onRadioChannel,
                            onUnset = {
                                onConfig(current.copy(meshRadio = "", meshRadioName = ""))
                                showPreset = false
                            },
                            onDismiss = { showPreset = false },
                            // What the desktop's radio said when this phone was paired: the one fact
                            // that turns "apply" into a link-down without the operator knowing.
                            hostTuple = parseHostPreset(current.meshHostPreset),
                        )
                    }
                    // **`input` before `airtime limit`.** The mode decides what the meter is
                    // measuring (batch holds text, live does not), so the reader meets the mode
                    // first. It is drawn here rather than after the facts block because the facts
                    // block ends with the airtime row.
                    SettingsInline("input") {
                        OptionButton(
                            "live",
                            selected = !current.meshHoldsInput,
                            enabled = !current.meshPublicChannel,
                        ) {
                            if (current.meshInputMode != MeshInputMode.Live) {
                                onInputMode(MeshInputMode.Live)
                            }
                        }
                        OptionButton("batch", selected = current.meshHoldsInput) {
                            if (current.meshInputMode != MeshInputMode.ManualSync) {
                                onInputMode(MeshInputMode.ManualSync)
                            }
                        }
                    }
                    if (current.meshPublicChannel) {
                        // **What batching is for, not what a keystroke is.** The old note said every keystroke on a
                        // public channel is a broadcast, which is exactly what batch mode prevents: the
                        // keystrokes are collected and leave together. The reason the mode is forced is
                        // that a shared channel is somebody else's airtime, so the app reduces how many
                        // transmissions it spends.
                        SettingsNote("public channel forces batch: keystrokes leave together")
                    }
                },
            )

        }

    }

}

/** What the Settings dialog calls into for the mesh route. One place wires it to the real stores. */
class MeshRouteActions(
    /** Read secure storage, never throwing (a broken Keystore reads as "nothing stored"). */
    val credentials: () -> MeshCredentialState,
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
private fun MeshRadioFacts(
    address: String,
    /** The desktop's tuple, when the pairing code carried one. */
    hostPreset: RadioPreset? = null,
    openAirtime: Boolean,
    onBudget: (RadioBudgetMode) -> Unit,
    onOpenPreset: () -> Unit,
    /**
     * Drawn **directly under the preset row**, before anything else this block emits.
     *
     * The preset panel used to be emitted by the caller after the whole block, so it landed under
     * whatever the block ended with — which is now the airtime toggle and its readout. Tapping
     * `preset` opened a panel two rows below the row that opened it, and the user reported exactly
     * that (measured 2026-11-05: "the box that comes up is beneath the air time limit"). A slot
     * keeps the panel tied to its own row by construction, not by emission order in another file.
     */
    afterPreset: @Composable () -> Unit = {},
) {
    var link by remember(address) { mutableStateOf<RadioLink?>(null) }
    val scope = rememberCoroutineScope()
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
            Text("edit", color = Accent, fontSize = 12.sp, maxLines = 1)
        }
    }
    afterPreset()
    // **The desktop's radio, when it is on something else.** One row, one press: the alternative was
    // comparing two screens by eye and guessing which of five numbers was wrong.
    val mine = preset
    var matchProblem by remember(address) { mutableStateOf<String?>(null) }
    if (hostPreset != null && mine != null && !sameTuple(mine, hostPreset)) {
        SettingsRow("desktop's radio") {
            // Only the four numbers that have to match. The row used to print the host's power and
            // path width too, which are not part of the channel and are not applied.
            SettingsValue(channelLine(hostPreset), Modifier.weight(1f), color = Dim, maxLines = 2)
            MiniTextButton(onClick = {
                // **A press that cannot work says so**, the same as the panel's apply: a null link
                // used to return in silence and the row sat there unchanged.
                val l = link
                if (l == null) {
                    matchProblem = "radio not ready · reopen this panel"
                    return@MiniTextButton
                }
                matchProblem = null
                scope.launch {
                    // Only the channel: this radio keeps its own power and path width.
                    val ok = runCatching { l.applyPreset(hostPreset.channelOf(mine)) }.getOrElse { false }
                    // A refused write used to vanish: the row stayed on screen with no sentence,
                    // which reads as the press having done nothing.
                    matchProblem = if (ok) null else "radio did not take desktop's tuple · still on the one above"
                }
            }) {
                Text("match", color = Accent, fontSize = 12.sp, maxLines = 1)
            }
        }
        matchProblem?.let { SettingsNote(it, color = Warn) }
    }
    // **The limit and its readout, together, on the page.** The meter existed only inside the
    // preset panel, next to the tuple it does not belong to, and the airtime it spends was a
    // separate row in the facts above with no control near it. One setting and one line: the
    // box says whether the meter is applied, and the line says what it has spent — which is the
    // question a reader asks the moment they see the box.
    val mustMeter = preset?.let { dutyCycleRequired(it) } == true
    if (mustMeter) {
        // **Not a setting in this band.** 869.4-869.65 MHz carries a 10 % duty cycle as a condition
        // of use, so a toggle here could only ever be switched to a wrong answer — and the row said
        // so ("off · band requires 10 % duty cycle") while still offering the press. It is applied,
        // not offered, and a stored `open` from another band is corrected rather than shown.
        LaunchedEffect(mustMeter, openAirtime) {
            if (openAirtime) onBudget(RadioBudgetMode.Metered)
        }
        SettingsRow("airtime limit", summary = "on · band requires 10 % duty cycle")
    } else {
        SettingsToggle(
            "airtime limit",
            checked = !openAirtime,
            // **The allowance is the band's**, so the sentence is too: 36 s inside the European
            // sub-band, 360 s outside it, where there is no duty cycle to comply with.
            summary = if (openAirtime) "off · no hourly limit" else "on · ${allowanceSeconds(preset)} s per hour",
            onChange = { metered -> onBudget(if (metered) RadioBudgetMode.Metered else RadioBudgetMode.Open) },
        )
    }
    SettingsNote(airtimeLine(status, preset))
}

/** The hourly allowance for a preset, in whole seconds, as the settings row says it. */
internal fun allowanceSeconds(preset: RadioPreset?): Int =
    ((preset?.let { RadioAirtime.budgetMsPerHour(it) } ?: RadioAirtime.BUDGET_MS_PER_HOUR) / 1000.0).toInt()

/**
 * The hour's airtime and what is left of the budget, in the radio's own units.
 *
 * `txAirtimeTenthsThisHour` is tenths of a second, and the app's own budget is the band's
 * ([RadioAirtime.budgetMsPerHour]): 36 s where a 10 % duty cycle is a condition of use, 360 s where
 * there is none. The records figure is null when the budget is open, which is a different fact from
 * "zero left" and is said as such.
 */
internal fun airtimeLine(status: MeshStatus?, preset: RadioPreset?): String {
    if (status == null) return "not read"
    val used = status.txAirtimeTenthsThisHour / 10.0
    val budget = (preset?.let { RadioAirtime.budgetMsPerHour(it) } ?: RadioAirtime.BUDGET_MS_PER_HOUR) / 1000.0
    val left = status.opsLeftThisHour
    val records = when {
        left == null -> "no hourly cap"
        left >= 255 -> "255+ records left"
        else -> "$left records left"
    }
    return "%.1f/%.0f s this hour · %s".format(used, budget, records)
}
