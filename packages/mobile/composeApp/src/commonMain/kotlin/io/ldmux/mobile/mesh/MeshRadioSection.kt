package io.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.ldmux.mobile.Accent
import io.ldmux.mobile.AccentBtn
import io.ldmux.mobile.Danger
import io.ldmux.mobile.Dim
import io.ldmux.mobile.Field
import io.ldmux.mobile.Fg
import io.ldmux.mobile.MiniTextButton
import io.ldmux.mobile.ModalActions
import io.ldmux.mobile.SettingsPanel
import io.ldmux.mobile.SettingsRow
import io.ldmux.mobile.SettingsChoice
import io.ldmux.mobile.Muted
import io.ldmux.mobile.Notice
import io.ldmux.mobile.NoticeLine
import io.ldmux.mobile.OptionButton
import io.ldmux.mobile.OutlinedBtn
import io.ldmux.mobile.Warn
import io.ldmux.mobile.MonoFont
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlin.random.Random

/** A bounded scan, shown as a small list. The scan stops itself; closing cancels it. */
@Composable
internal fun RadioPickerPanel(
    onDismiss: () -> Unit,
    onPick: (RadioDevice) -> Unit,
) {
    var token by remember { mutableStateOf(0) }
    var scanning by remember { mutableStateOf(true) }
    var result by remember { mutableStateOf<RadioScanResult?>(null) }
    LaunchedEffect(token) {
        scanning = true
        result = null
        RadioScanner.scan {
            result = it
            scanning = false
        }
    }
    DisposableEffect(Unit) { onDispose { RadioScanner.cancel() } }

    SettingsPanel("choose a LoRa radio", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (scanning) {
                Text("scanning…", color = Muted, fontSize = 12.sp)
            }
            val r = result
            if (r != null && !r.ok) {
                Text(radioProblemLine(r.problem), color = Warn, fontSize = 12.sp)
            }
            if (r != null && r.devices.isEmpty() && r.ok) {
                Text("no LoRa radio found", color = Muted, fontSize = 12.sp)
            }
            r?.devices?.forEach { d ->
                MiniTextButton(
                    onClick = { onPick(d); onDismiss() },
                ) {
                    Text(
                        "${d.name}  ${d.address}" + (d.rssi?.let { "  $it dBm" } ?: ""),
                        color = Fg,
                        fontSize = 12.sp,
                        fontFamily = MonoFont,
                        maxLines = 1,
                    )
                }
            }
        }
        ModalActions {
            OutlinedBtn("close", Modifier.weight(1f), onClick = onDismiss)
            // Disabled while a scan is already running. It used to read `scanning…` and stay
            // live, which is a button whose label is a state and whose tap restarts the thing
            // it is describing.
            MiniTextButton(
                enabled = !scanning,
                modifier = Modifier.weight(1f),
                onClick = { token += 1 },
            ) {
                Text("scan again", color = if (scanning) Dim else Accent, fontSize = 12.sp)
            }
        }
    }
}

/**
 * Read the selected radio, show its current operating point, and write the LDMUX
 * preset as one tuple. The write is not trusted: the dialog reconnects and reads
 * SELF_INFO/DEVICE_INFO back before it says it worked.
 */
@Composable
internal fun RadioPresetPanel(
    address: String,
    openAirtime: Boolean,
    onBudget: (RadioBudgetMode) -> Unit,
    /**
     * The radio is on a public channel or not, as read from it and as written by an apply.
     *
     * Reported rather than stored here: whether the channel is shared is a fact about the radio
     * that the app persists beside the pairing, and this dialog is the only place that can read
     * it (ADR-0026 — it decides whether public-channel mode is forced).
     */
    onClassified: (Boolean) -> Unit,
    onUnset: () -> Unit,
    onDismiss: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var link by remember(address) { mutableStateOf<RadioLink?>(null) }
    var note by remember { mutableStateOf<Notice?>(null) }
    var confirmUnset by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var region by remember { mutableStateOf(RadioRegion.UsCanada) }
    var seed by remember { mutableStateOf(0) }
    var selected by remember { mutableStateOf(privatePreset(RadioRegion.UsCanada, Random(0))) }
    var original by remember { mutableStateOf<RadioPreset?>(null) }
    var seeded by remember { mutableStateOf(false) }
    var showPublic by remember { mutableStateOf(false) }
    // The host's tuple cannot be discovered by this phone before the two radios are on the same
    // channel — that is the chicken and egg — so the app has to be able to be *told* it. The
    // region presets and "randomise" cover "pick a channel"; this covers "join the channel the host
    // is already on", which is the case a user actually hits: the host radio is configured once and
    // stays put, and the phone must land on it.
    var custom by remember { mutableStateOf(false) }
    var customText by remember { mutableStateOf("") }
    val fallbackState = remember { MutableStateFlow(MeshLinkState.NeedsRadio) }
    val fallbackInfo = remember { MutableStateFlow<RadioSelfInfo?>(null) }
    val fallbackDevice = remember { MutableStateFlow<RadioDeviceInfo?>(null) }
    val fallbackStatus = remember { MutableStateFlow<MeshStatus?>(null) }
    val state by (link?.state ?: fallbackState).collectAsState()
    val info by (link?.selfInfo ?: fallbackInfo).collectAsState()
    val device by (link?.deviceInfo ?: fallbackDevice).collectAsState()
    val status by (link?.status ?: fallbackStatus).collectAsState()
    val current = info?.let {
        RadioPreset(
            frequencyMhz = it.frequencyMhz,
            bandwidthKhz = it.bandwidthKhz,
            spreadingFactor = it.spreadingFactor,
            codingRate = it.codingRate,
            txPowerDbm = it.txPowerDbm,
            pathHashSize = (device?.pathHashMode ?: 0) + 1,
        )
    }

    // What the custom field currently parses to. Null when it does not parse, which is also what
    // disables "apply": applying the previous selection because the text was malformed would be a
    // silent surprise on a control that changes the radio's frequency.
    val typed = if (custom && customText.isNotBlank()) parseRadioTuple(customText, selected) else null

    LaunchedEffect(address) {
        if (address.isEmpty()) {
            note = Notice("choose a LoRa radio first", ok = false)
            return@LaunchedEffect
        }
        val l = meshRadioLink(address, ByteArray(0))
        if (l == null) {
            note = Notice("this build has no LoRa radio support", ok = false)
            return@LaunchedEffect
        }
        link = l
        l.setBudgetMode(if (openAirtime) RadioBudgetMode.Open else RadioBudgetMode.Metered)
        try {
            l.connect()
            l.readDeviceInfo()
        } catch (e: RadioLinkException) {
            note = Notice(radioLinkProblemLine(e.state), ok = false)
        } catch (e: Exception) {
            note = Notice(e.message ?: "the LoRa radio did not answer", ok = false)
        }
    }
    // Open on the tuple the radio is actually using, not on a fresh proposal.
    //
    // **Two flags, because one was doing two jobs.** `seeded` meant both "the radio's own tuple
    // has arrived" and "the user has touched this dialog", and `choose()` set it. So tapping a
    // region or "randomise" before the radio answered made the seeding effect return early for
    // good: `original` stayed null, "restore" was disabled forever, the "current …" comparison
    // never appeared, and "apply" became enabled with no idea what it was replacing. `seeded`
    // now means only "the radio's tuple has been read"; `userTouched` only "the proposal is the
    // user's, so the arrival must not overwrite it".
    var userTouched by remember { mutableStateOf(false) }
    // The radio's own tuple is the truth about which channel it is on, so it is reported every
    // time it arrives rather than only on the first read.
    LaunchedEffect(current) {
        val c = current ?: return@LaunchedEffect
        onClassified(isPublicChannel(c))
    }
    LaunchedEffect(current, device) {
        val c = current ?: return@LaunchedEffect
        if (seeded) return@LaunchedEffect
        original = c
        // The user's choice wins over the arrival: they may have started choosing while the
        // radio was still answering, and a dialog that silently undid that would be worse than
        // one that opened on a proposal.
        if (!userTouched) {
            region = regionOfPreset(c)
            selected = c
        }
        seeded = true
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

    fun choose(p: RadioPreset) {
        userTouched = true
        selected = p
    }

    fun applyNow(p: RadioPreset) {
        val l = link ?: return
        busy = true
        scope.launch {
            note = try {
                if (l.applyPreset(p)) {
                    // Written from what was asked for, not from a read-back: the radio may have
                    // taken it and the next `SELF_INFO` will correct this if it did not, and the
                    // mode that depends on it is the cautious one either way.
                    onClassified(isPublicChannel(p))
                    Notice("applied", ok = true)
                } else Notice("not confirmed", ok = false)
            } catch (e: RadioLinkException) {
                Notice(radioLinkProblemLine(e.state), ok = false)
            } catch (e: Exception) {
                Notice(e.message ?: "did not finish", ok = false)
            } finally {
                busy = false
            }
        }
    }

    SettingsPanel("LoRa radio", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    address.ifEmpty { "no LoRa radio" } + "  ·  " + meshLinkLabel(state),
                    color = Dim,
                    fontSize = 11.sp,
                    fontFamily = MonoFont,
                )
                if (current != null && current != selected) {
                    Text("current  " + presetLine(current), color = Dim, fontSize = 11.sp, fontFamily = MonoFont)
                }
                // **What the radio says about itself, before what it could be set to.** Every field
                // here was already read — `SELF_INFO` on the handshake, `DEVICE_INFO` on request,
                // `MeshStatus` for signal and battery — and none of it was shown: the dialog opened
                // straight onto "region", so the one screen about the radio answered "which channel"
                // without saying which radio, what firmware, or whether it was in range. A line only
                // appears when its source answered, so an unbound radio shows nothing rather than
                // zeros.
                RadioFacts(info, device, status)
                // Wrapped, not one row: the region names are wider than a phone's panel, and a
                // row that overflows loses the last one — the same bug the portrait key bar was
                // rebuilt to avoid.
                SettingsChoice("region", summary = "a private band inside the region's rules") {
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        for (r in RadioRegion.entries) {
                            OptionButton(r.label, selected = region == r) {
                                region = r
                                choose(privatePreset(r, Random(seed)))
                            }
                        }
                    }
                }
                SettingsRow("preset", summary = presetLine(selected))
                if (custom) {
                    // One line, four numbers, in the order the radio prints them: MHz, kHz, SF, CR.
                    // The host's own preset appears in the gateway's start-up line; typing it here is
                    // how the phone's board joins the channel the host is already on. Power and path
                    // width stay as they are, because they are not part of "which channel".
                    Field(
                        value = customText,
                        onValueChange = { text ->
                            customText = text
                            parseRadioTuple(text, selected)?.let { choose(it) }
                        },
                        placeholder = "910.525 500 7 5",
                    )
                    Text(
                        if (customText.isBlank()) {
                            "MHz kHz SF CR — the host's radio prints its preset when its gateway starts"
                        } else if (typed == null) {
                            "not a tuple: four numbers, 137-1020 MHz, 62.5-500 kHz, SF5-12, CR5-8"
                        } else {
                            "the tuple the host's LoRa radio is on"
                        },
                        color = if (customText.isNotBlank() && typed == null) Warn else Dim,
                        fontSize = 11.sp,
                    )
                }
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    MiniTextButton(onClick = {
                        seed += 1
                        choose(privatePreset(region, Random(seed)))
                    }) { Text("randomise", color = Accent, fontSize = 11.sp, maxLines = 1) }
                    MiniTextButton(
                        enabled = original != null && !busy && state == MeshLinkState.Bound,
                        onClick = {
                            val o = original ?: return@MiniTextButton
                            region = regionOfPreset(o)
                            choose(o)
                            applyNow(o)
                        },
                    ) { Text("restore", color = Accent, fontSize = 11.sp, maxLines = 1) }
                    // The way to join the host's channel instead of proposing a new one.
                    MiniTextButton(onClick = {
                        custom = !custom
                        if (custom) customText = radioTupleText(selected)
                    }) { Text(if (custom) "hide custom" else "custom", color = Accent, fontSize = 11.sp, maxLines = 1) }
                    // No ellipsis: it expands this card rather than opening another one,
                    // and `…` is the promise of a new surface everywhere else in the app.
                    MiniTextButton(onClick = { showPublic = !showPublic }) {
                        Text(if (showPublic) "hide presets" else "public presets", color = Accent, fontSize = 11.sp, maxLines = 1)
                    }
                }
                if (showPublic) {
                    // **Every public preset, not only this region's.** They are the community's
                    // operating points, and the one a user needs is often the one for where they
                    // are going rather than where they are. Each shows its tuple underneath, so
                    // two regions that share a frequency are still distinguishable.
                    SettingsRow("public presets", summary = "tap one to adopt its tuple")
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        for (c in PublicPresets) {
                            OptionButton(c.label, selected = c.preset == selected) {
                                choose(c.preset)
                                region = c.region ?: region
                            }
                        }
                    }
                }
                // `budget`, not `airtime`: airtime is the quantity, and the choice is whether
                // LDMUX applies the 36 s/hour meter of its own accord.
                SettingsChoice("budget") {
                    OptionButton("metered", selected = !openAirtime) {
                        onBudget(RadioBudgetMode.Metered)
                        link?.setBudgetMode(RadioBudgetMode.Metered)
                    }
                    OptionButton("unlimited", selected = openAirtime) {
                        onBudget(RadioBudgetMode.Open)
                        link?.setBudgetMode(RadioBudgetMode.Open)
                    }
                }
                NoticeLine(note)
            }
        // The row is left-aligned in three parts: the destructive action on its own, then
        // cancel and apply sharing the rest. `unset` used to fire on one tap while `unpair`
        // next door asked twice, in the same app, in the same screen.
        ModalActions {
            MiniTextButton(
                enabled = !busy,
                onClick = { if (confirmUnset) onUnset() else confirmUnset = true },
            ) {
                Text(if (confirmUnset) "unset?" else "unset", color = Danger, fontSize = 12.sp)
            }
            Spacer(Modifier.weight(1f))
            OutlinedBtn("close", Modifier.weight(1f), onClick = onDismiss)
            // Disabled exactly where a tap cannot work: the radio has not answered
            // `SELF_INFO` yet, or it is not bound. The tuple line above says which.
            // A custom tuple that does not parse must not leave "apply" live: it would commit the
            // previous selection, which is not what the text says.
            val customOk = !custom || customText.isBlank() || typed != null
            val canApply = link != null && !busy && seeded && state == MeshLinkState.Bound && customOk
            AccentBtn(
                label = if (busy) "applying…" else "apply",
                modifier = Modifier.weight(1f),
                enabled = canApply,
            ) { applyNow(selected) }
        }
    }
}

/**
 * The operating point as one line: frequency, bandwidth, spreading factor, coding rate,
 * power, and the path-hash width in bytes.
 *
 * `SF8` and `CR5` are the radio's own vocabulary and are left as the radio's numbers. The
 * path width gets a word, because `1B` alone reads as a unit with no quantity.
 */
internal fun presetLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}  ${p.txPowerDbm} dBm  path ${p.pathHashSize}B"

private fun radioProblemLine(problem: RadioScanProblem?): String = when (problem) {
    RadioScanProblem.PermissionDenied -> "Bluetooth permission is off. Grant it, then scan again."
    RadioScanProblem.BluetoothOff -> "Bluetooth is off."
    RadioScanProblem.Unsupported -> "this device has no Bluetooth LE support."
    RadioScanProblem.Failed -> "the scan failed. Try again."
    null -> ""
}

private fun radioLinkProblemLine(state: MeshLinkState): String = when (state) {
    MeshLinkState.NeedsRadio -> "choose a LoRa radio first"
    MeshLinkState.NeedsPermission -> "Bluetooth permission is off"
    MeshLinkState.RadioUnavailable -> "the LoRa radio is not answering"
    MeshLinkState.NoPath -> "the host path does not fit the terminal"
    else -> meshLinkLabel(state)
}

/**
 * The radio's own description, as lines of facts rather than as a form.
 *
 * Three lines at most, and each is dropped when its source did not answer:
 *
 *  - **name and firmware** — the name is how neighbours see this node, and the firmware version is
 *    the only thing that says which command set the radio has. The vendor pin (`companion-v1.15.0`)
 *    is what the code was written against; the boards in the field ran 1.17.1, so this is the line
 *    that tells a user which one they are on.
 *  - **signal and battery** — the sentinels the radio uses for "not measured" (-128 dBm, 255 %) are
 *    rendered as words, as `ldmux mesh status` does, because a figure that is really "unknown" read
 *    as a measurement is worse than a blank.
 *  - **power and path width** — the ceiling is regulatory as well as physical, so the dialog states
 *    the radio's own maximum beside the power it is using; the path width in bytes is what neighbours
 *    read in the trace of a message this radio sends.
 *
 * Deliberately **not** editable. Changing the name or the channel slots is MeshMore's job on these
 * boards, and two apps writing the same field is how a radio ends up named something neither user
 * chose. What this screen configures is the operating point — the tuple — and it says so by only
 * offering that as a control.
 */
@Composable
private fun RadioFacts(info: RadioSelfInfo?, device: RadioDeviceInfo?, status: MeshStatus?) {
    if (info == null && device == null && status == null) return
    if (info != null) {
        FactLine("name", info.name + (device?.let { "  ·  " + it.firmwareVersion } ?: ""))
    } else if (device != null) {
        FactLine("firmware", device.firmwareVersion)
    }
    val rssi = status?.rssiDbm
    val volts = device?.batteryMillivolts?.let { "%.2f V".format(it / 1000.0) }
    val battery = status?.batteryPercent?.let { "$it%" }
    if (rssi != null || volts != null || battery != null) {
        FactLine(
            "signal",
            listOfNotNull(rssi?.let { "$it dBm" }, volts ?: battery).joinToString("  ·  "),
        )
    }
    if (info != null) {
        FactLine(
            "power",
            buildList {
                add("${info.txPowerDbm} of ${info.maxTxPowerDbm} dBm")
                device?.let { add("path ${it.pathHashSize} B") }
                device?.let { add("${it.maxContacts} contacts") }
            }.joinToString("  ·  "),
        )
    }
}

/** One labelled fact, in the app's dim-label/value pairing. */
@Composable
private fun FactLine(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, color = Dim, fontSize = 11.sp, fontFamily = MonoFont)
        Text(value, color = Fg, fontSize = 11.sp, fontFamily = MonoFont, maxLines = 1)
    }
}
