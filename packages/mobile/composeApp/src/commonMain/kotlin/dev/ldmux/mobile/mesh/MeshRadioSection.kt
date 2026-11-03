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
                    Text("no MeshCore radio found. Turn it on and near the phone, then scan again.", color = Dim, fontSize = 12.sp)
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
    onBudget: (RadioBudgetMode) -> Unit,
    onDismiss: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var link by remember(address) { mutableStateOf<RadioLink?>(null) }
    var note by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var region by remember { mutableStateOf(RadioRegion.UsCanada) }
    var seed by remember { mutableStateOf(0) }
    var selected by remember { mutableStateOf(privatePreset(RadioRegion.UsCanada, Random(0))) }
    val fallbackState = remember { MutableStateFlow(MeshLinkState.NeedsRadio) }
    val fallbackInfo = remember { MutableStateFlow<RadioSelfInfo?>(null) }
    val state by (link?.state ?: fallbackState).collectAsState()
    val info by (link?.selfInfo ?: fallbackInfo).collectAsState()

    LaunchedEffect(address) {
        // A private tuple is the default and it is not a public MeshCore
        // channel, so the app-side airtime meter starts open. The radio's own
        // duty cycle still applies.
        onBudget(RadioBudgetMode.Open)
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
        l.setBudgetMode(RadioBudgetMode.Open)
        try {
            l.connect()
            l.readDeviceInfo()
        } catch (e: RadioLinkException) {
            note = radioLinkProblemLine(e.state)
        } catch (e: Exception) {
            note = e.message ?: "radio did not answer"
        }
    }
    DisposableEffect(link) { onDispose { link?.release() } }

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
                info?.let {
                    Text(
                        "${it.frequencyMhz} MHz  ${it.bandwidthKhz} kHz  SF${it.spreadingFactor}  CR${it.codingRate}  ${it.txPowerDbm} dBm",
                        color = Dim,
                        fontSize = 11.sp,
                        fontFamily = FontFamily.Monospace,
                    )
                }
                Text("region", color = Dim, fontSize = 11.sp)
                Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                    for (r in RadioRegion.entries) {
                        MiniTextButton(onClick = {
                            region = r
                            selected = privatePreset(r, Random(seed))
                        }) {
                            Text(r.label, color = if (region == r) Accent else Dim, fontSize = 10.sp, maxLines = 1)
                        }
                    }
                }
                Text("private tuple", color = Dim, fontSize = 11.sp)
                Text(
                    presetLine(selected),
                    color = Fg,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                )
                MiniTextButton(onClick = {
                    seed += 1
                    selected = privatePreset(region, Random(seed))
                }) {
                    Text("new tuple", color = Accent, fontSize = 11.sp)
                }
                note?.let { Text(it, color = Warn, fontSize = 11.sp) }
            }
        },
        confirmButton = {
            MiniTextButton(
                enabled = link != null && !busy && state == MeshLinkState.Bound,
                onClick = {
                    val l = link ?: return@MiniTextButton
                    busy = true
                    scope.launch {
                        note = try {
                            if (l.applyPreset(selected)) "applied" else "not confirmed"
                        } catch (e: RadioLinkException) {
                            radioLinkProblemLine(e.state)
                        } catch (e: Exception) {
                            e.message ?: "did not finish"
                        } finally {
                            busy = false
                        }
                    }
                },
            ) {
                Text(if (busy) "applying…" else "apply", color = Accent, fontSize = 12.sp)
            }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("close", color = Dim) } },
    )
}

private fun presetLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}  ${p.txPowerDbm} dBm"

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
