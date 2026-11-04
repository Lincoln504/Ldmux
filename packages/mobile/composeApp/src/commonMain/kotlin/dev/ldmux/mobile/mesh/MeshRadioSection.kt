package dev.ldmux.mobile.mesh

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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.AccentBtn
import dev.ldmux.mobile.Danger
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.ModalActions
import dev.ldmux.mobile.ModalCard
import dev.ldmux.mobile.Muted
import dev.ldmux.mobile.Notice
import dev.ldmux.mobile.NoticeLine
import dev.ldmux.mobile.OptionButton
import dev.ldmux.mobile.OutlinedBtn
import dev.ldmux.mobile.Warn
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlin.random.Random

/** A bounded scan, shown as a small list. The scan stops itself; closing cancels it. */
@Composable
internal fun RadioPickerDialog(
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

    ModalCard("choose a radio", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (scanning) {
                Text("scanning…", color = Muted, fontSize = 12.sp)
            }
            val r = result
            if (r != null && !r.ok) {
                Text(radioProblemLine(r.problem), color = Warn, fontSize = 12.sp)
            }
            if (r != null && r.devices.isEmpty() && r.ok) {
                Text("no radio found", color = Muted, fontSize = 12.sp)
            }
            r?.devices?.forEach { d ->
                MiniTextButton(
                    onClick = { onPick(d); onDismiss() },
                ) {
                    Text(
                        "${d.name}  ${d.address}" + (d.rssi?.let { "  $it dBm" } ?: ""),
                        color = Fg,
                        fontSize = 12.sp,
                        fontFamily = FontFamily.Monospace,
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
internal fun RadioPresetDialog(
    address: String,
    openAirtime: Boolean,
    onBudget: (RadioBudgetMode) -> Unit,
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
    val fallbackState = remember { MutableStateFlow(MeshLinkState.NeedsRadio) }
    val fallbackInfo = remember { MutableStateFlow<RadioSelfInfo?>(null) }
    val fallbackDevice = remember { MutableStateFlow<RadioDeviceInfo?>(null) }
    val state by (link?.state ?: fallbackState).collectAsState()
    val info by (link?.selfInfo ?: fallbackInfo).collectAsState()
    val device by (link?.deviceInfo ?: fallbackDevice).collectAsState()
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

    LaunchedEffect(address) {
        if (address.isEmpty()) {
            note = Notice("choose a radio first", ok = false)
            return@LaunchedEffect
        }
        val l = meshRadioLink(address, ByteArray(0))
        if (l == null) {
            note = Notice("this build has no radio support", ok = false)
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
            note = Notice(e.message ?: "radio did not answer", ok = false)
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
    DisposableEffect(link) { onDispose { link?.release() } }

    fun choose(p: RadioPreset) {
        userTouched = true
        selected = p
    }

    fun applyNow(p: RadioPreset) {
        val l = link ?: return
        busy = true
        scope.launch {
            note = try {
                if (l.applyPreset(p)) Notice("applied", ok = true) else Notice("not confirmed", ok = false)
            } catch (e: RadioLinkException) {
                Notice(radioLinkProblemLine(e.state), ok = false)
            } catch (e: Exception) {
                Notice(e.message ?: "did not finish", ok = false)
            } finally {
                busy = false
            }
        }
    }

    ModalCard("radio", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    address.ifEmpty { "no radio" } + "  ·  " + meshLinkLabel(state),
                    color = Dim,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                )
                if (current != null && current != selected) {
                    Text("current  " + presetLine(current), color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
                }
                Text("region", color = Dim, fontSize = 11.sp)
                // Wrapped, not one row: four region names ("USA / Canada" is the shortest
                // two) are wider than a phone's modal, and a row that overflows loses the
                // last one — the same bug the portrait key bar was rebuilt to avoid.
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    for (r in RadioRegion.entries) {
                        OptionButton(r.label, selected = region == r) {
                            region = r
                            choose(privatePreset(r, Random(seed)))
                        }
                    }
                }
                Text("preset", color = Dim, fontSize = 11.sp)
                Text(
                    presetLine(selected),
                    color = Fg,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                )
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
                    // No ellipsis: it expands this card rather than opening another one,
                    // and `…` is the promise of a new surface everywhere else in the app.
                    MiniTextButton(onClick = { showPublic = !showPublic }) {
                        Text(if (showPublic) "hide presets" else "public presets", color = Accent, fontSize = 11.sp, maxLines = 1)
                    }
                }
                if (showPublic) {
                    for (c in publicPresets(region)) {
                        MiniTextButton(onClick = { choose(c.preset) }) {
                            Text(c.label, color = Dim, fontSize = 11.sp, maxLines = 1)
                        }
                    }
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    // `budget`, not `airtime`: airtime is the quantity, and the choice is
                    // whether LDMUX applies the 36 s/hour meter of its own accord. `open` and
                    // `metered` also read as two adjectives where the buttons are verbs
                    // everywhere else.
                    Text("budget", color = Dim, fontSize = 11.sp)
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
            val canApply = link != null && !busy && seeded && state == MeshLinkState.Bound
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
private fun presetLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}  ${p.txPowerDbm} dBm  path ${p.pathHashSize}B"

private fun radioProblemLine(problem: RadioScanProblem?): String = when (problem) {
    RadioScanProblem.PermissionDenied -> "Bluetooth permission is off. Grant it, then scan again."
    RadioScanProblem.BluetoothOff -> "Bluetooth is off."
    RadioScanProblem.Unsupported -> "this device has no Bluetooth LE radio."
    RadioScanProblem.Failed -> "the scan failed. Try again."
    null -> ""
}

private fun radioLinkProblemLine(state: MeshLinkState): String = when (state) {
    MeshLinkState.NeedsRadio -> "choose a radio first"
    MeshLinkState.NeedsPermission -> "Bluetooth permission is off"
    MeshLinkState.RadioUnavailable -> "the radio is not answering"
    MeshLinkState.NoPath -> "the host path does not fit the terminal"
    else -> meshLinkLabel(state)
}
