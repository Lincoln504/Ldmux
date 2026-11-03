package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
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
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.Surface
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

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("mesh radio", color = Fg) },
        containerColor = Surface,
        tonalElevation = 0.dp,
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                if (scanning) {
                    Text("scanning for MeshCore radios…", color = Dim, fontSize = 12.sp)
                }
                val r = result
                if (r != null && !r.ok) {
                    Text(radioProblemLine(r.problem), color = Warn, fontSize = 12.sp)
                }
                if (r != null && r.devices.isEmpty() && r.ok) {
                    Text("no radio found", color = Dim, fontSize = 12.sp)
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
        },
        confirmButton = {
            MiniTextButton(onClick = { token += 1 }) {
                Text(if (scanning) "scanning…" else "scan again", color = Accent, fontSize = 12.sp)
            }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("close", color = Dim) } },
    )
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
    var note by remember { mutableStateOf<String?>(null) }
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
            note = "choose a radio first"
            return@LaunchedEffect
        }
        val l = meshRadioLink(address, ByteArray(0))
        if (l == null) {
            note = "this build has no radio support"
            return@LaunchedEffect
        }
        link = l
        l.setBudgetMode(if (openAirtime) RadioBudgetMode.Open else RadioBudgetMode.Metered)
        try {
            l.connect()
            l.readDeviceInfo()
        } catch (e: RadioLinkException) {
            note = radioLinkProblemLine(e.state)
        } catch (e: Exception) {
            note = e.message ?: "radio did not answer"
        }
    }
    // Open on the tuple the radio is actually using, not on a fresh proposal.
    LaunchedEffect(current, device) {
        val c = current ?: return@LaunchedEffect
        if (seeded) return@LaunchedEffect
        original = c
        region = regionOfPreset(c)
        selected = c
        seeded = true
    }
    DisposableEffect(link) { onDispose { link?.release() } }

    fun choose(p: RadioPreset) {
        seeded = true
        selected = p
    }

    fun applyNow(p: RadioPreset) {
        val l = link ?: return
        busy = true
        scope.launch {
            note = try {
                if (l.applyPreset(p)) "applied" else "not confirmed"
            } catch (e: RadioLinkException) {
                radioLinkProblemLine(e.state)
            } catch (e: Exception) {
                e.message ?: "did not finish"
            } finally {
                busy = false
            }
        }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("radio", color = Fg) },
        containerColor = Surface,
        tonalElevation = 0.dp,
        text = {
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
                Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                    for (r in RadioRegion.entries) {
                        MiniTextButton(onClick = {
                            region = r
                            choose(privatePreset(r, Random(seed)))
                        }) {
                            Text(r.label, color = if (region == r) Accent else Dim, fontSize = 10.sp, maxLines = 1)
                        }
                    }
                }
                Text("tuple", color = Dim, fontSize = 11.sp)
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
                    }) { Text("new private", color = Accent, fontSize = 11.sp, maxLines = 1) }
                    MiniTextButton(
                        enabled = original != null && !busy && state == MeshLinkState.Bound,
                        onClick = {
                            val o = original ?: return@MiniTextButton
                            region = regionOfPreset(o)
                            choose(o)
                            applyNow(o)
                        },
                    ) { Text("restore", color = Accent, fontSize = 11.sp, maxLines = 1) }
                    MiniTextButton(onClick = { showPublic = !showPublic }) {
                        Text(if (showPublic) "hide public" else "public…", color = Accent, fontSize = 11.sp, maxLines = 1)
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
                    Text("airtime", color = Dim, fontSize = 11.sp)
                    MiniTextButton(onClick = {
                        val next = if (openAirtime) RadioBudgetMode.Metered else RadioBudgetMode.Open
                        onBudget(next)
                        link?.setBudgetMode(next)
                    }) {
                        Text(if (openAirtime) "open" else "metered", color = Accent, fontSize = 11.sp, maxLines = 1)
                    }
                }
                note?.let { Text(it, color = Warn, fontSize = 11.sp) }
            }
        },
        confirmButton = {
            MiniTextButton(
                enabled = link != null && !busy && seeded && state == MeshLinkState.Bound,
                onClick = { applyNow(selected) },
            ) {
                Text(if (busy) "applying…" else "apply", color = Accent, fontSize = 12.sp)
            }
        },
        dismissButton = {
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                MiniTextButton(enabled = !busy, onClick = onUnset) { Text("unset", color = Warn, fontSize = 12.sp) }
                MiniTextButton(onClick = onDismiss) { Text("close", color = Dim, fontSize = 12.sp) }
            }
        },
    )
}

private fun presetLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}  ${p.txPowerDbm} dBm  ${p.pathHashSize}B"

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
