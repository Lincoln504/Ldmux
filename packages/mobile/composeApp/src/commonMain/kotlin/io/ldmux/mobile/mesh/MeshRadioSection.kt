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

    SettingsPanel("choose LoRa radio", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (scanning) {
                Text("scanning…", color = Muted, fontSize = 12.sp)
            }
            val r = result
            if (r != null && !r.ok) {
                Text(radioProblemLine(r.problem), color = Warn, fontSize = 12.sp)
                // **A denial needs a verb.** After the second denial on Android 11+ the system
                // stops drawing the dialog, so a screen that only states the fact is a dead end:
                // the same sentence returns on every press and nothing can be granted from here.
                if (r.problem == RadioScanProblem.PermissionDenied) {
                    val ask = RadioPermission.canAsk()
                    MiniTextButton(onClick = {
                        RadioPermission.resolve()
                        token += 1
                    }) {
                        Text(
                            if (ask) "allow Bluetooth" else "open app settings",
                            color = Accent,
                            fontSize = 12.sp,
                        )
                    }
                }
            }
            // **A radio that is held advertises nothing**, which is the state a scan cannot tell
            // from "powered off". So the list is not the only way in: the radios this phone is
            // already bonded to are offered below, and trying one either works or says why.
            val saved = remember(r) { RadioScanner.bonded().filter { d -> r?.devices?.none { it.address == d.address } != false } }
            if (r != null && r.devices.isEmpty() && r.ok) {
                // The two causes a scan cannot tell apart, said instead of "no radio found":
                // a radio that is off advertises nothing, and one another app (or another phone)
                // already holds advertises but never answers the companion handshake.
                Text("no LoRa radio advertising", color = Muted, fontSize = 12.sp)
                Text(
                    if (saved.isEmpty()) "check: on, not connected elsewhere" else "saved radios below",
                    color = Muted,
                    fontSize = 11.sp,
                )
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
            if (saved.isNotEmpty()) {
                Text("saved", color = Dim, fontSize = 11.sp)
                saved.forEach { d ->
                    MiniTextButton(onClick = { onPick(d); onDismiss() }) {
                        Text(
                            "${d.name}  ${d.address}",
                            color = Dim,
                            fontSize = 12.sp,
                            fontFamily = MonoFont,
                            maxLines = 1,
                        )
                    }
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
    /**
     * The tuple the desktop's radio reported in the pairing code, when there was one.
     *
     * **Two radios have to be on the same channel, and only one of them is in this dialog.** Moving
     * this one away from the desktop's leaves the link down until the desktop is moved to match —
     * over the radio there is no way to ask it, because the request would have to travel over the
     * channel it is being moved off. So the dialog says what will break before the press, instead of
     * leaving the operator with a link that silently stopped.
     */
    hostTuple: RadioPreset? = null,
) {
    val scope = rememberCoroutineScope()
    var link by remember(address) { mutableStateOf<RadioLink?>(null) }
    var note by remember { mutableStateOf<Notice?>(null) }
    var confirmUnset by remember { mutableStateOf(false) }
    // A press that moves this radio off the desktop's channel asks once, because undoing it needs
    // the desktop.
    var confirmBreak by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var region by remember { mutableStateOf(RadioRegion.UsCanada) }
    var seed by remember { mutableStateOf(0) }
    var selected by remember { mutableStateOf(privatePreset(RadioRegion.UsCanada, Random(0))) }
    // A confirmation belongs to the tuple it was asked about: picking another one asks again.
    LaunchedEffect(selected) { confirmBreak = false }
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
    val fallbackProblem = remember { MutableStateFlow<String?>(null) }
    val state by (link?.state ?: fallbackState).collectAsState()
    val info by (link?.selfInfo ?: fallbackInfo).collectAsState()
    val device by (link?.deviceInfo ?: fallbackDevice).collectAsState()
    val status by (link?.status ?: fallbackStatus).collectAsState()
    val problem by (link?.lastProblem ?: fallbackProblem).collectAsState()
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
            note = Notice("choose LoRa radio first", ok = false)
            return@LaunchedEffect
        }
        val l = meshRadioLink(address, ByteArray(0))
        if (l == null) {
            note = Notice("no LoRa radio support in this build", ok = false)
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
            note = Notice(e.message ?: "LoRa radio did not answer", ok = false)
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

    // Applying a tuple that lands in a duty-cycle band switches the meter on. Transmitting there
    // without it is outside the band's conditions, and a setting that permits that by default is
    // a setting that ships a radio operating illegally.
    fun chooseWithBudget(p: RadioPreset) {
        choose(p)
        if (dutyCycleRequired(p) && openAirtime) onBudget(RadioBudgetMode.Metered)
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
                // The link's own sentence about the last attempt, when it has one. A block of
                // "not read" said that nothing had answered, never why: Bluetooth off, no
                // permission, no bond, another app holding the radio and the wrong firmware all
                // looked the same on this screen.
                RadioFacts(info, device, status, problem)
                // Wrapped, not one row: the region names are wider than a phone's panel, and a
                // row that overflows loses the last one — the same bug the portrait key bar was
                // rebuilt to avoid.
                SettingsChoice("private channel", summary = "random tuple in region's rules") {
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        for (r in RadioRegion.entries) {
                            OptionButton(r.label, selected = region == r) {
                                region = r
                                chooseWithBudget(privatePreset(r, Random(seed)))
                            }
                        }
                    }
                }
                SettingsRow("preset", summary = presetLine(selected))
                if (custom) {
                    // One line, four numbers, in the order the radio prints them: MHz, kHz, SF, CR.
                    // The host's own preset is what `ldmux mesh status` prints; typing it here is
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
                            "MHz kHz SF CR — `ldmux mesh status` on host prints its preset"
                        } else if (typed == null) {
                            "not a tuple · four numbers: 137-1020 MHz, 62.5-500 kHz, SF5-12, CR5-8"
                        } else {
                            "host radio's own tuple"
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
                    SettingsRow("public presets", summary = "public table · pick one")
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        for (c in PublicPresets) {
                            OptionButton(c.label, selected = c.preset == selected) {
                                chooseWithBudget(c.preset)
                                region = c.region ?: region
                            }
                        }
                    }
                }
                // The cost of the press, where the press is. It names the cure as well as the
                // break: the desktop's own command, and this radio's `match` row once the desktop
                // has moved.
                if (hostTuple != null && !sameChannel(selected, hostTuple)) {
                    Text(
                        "desktop radio on " + channelLine(hostTuple) +
                            " · apply leaves link down until desktop moves " +
                            "(`ldmux mesh preset` there) or you press match here",
                        color = Warn,
                        fontSize = 11.sp,
                        fontFamily = MonoFont,
                    )
                } else if (hostTuple != null && selected.pathHashSize != hostTuple.pathHashSize) {
                    // **Not a link warning.** The four numbers match, so the two radios hear each
                    // other; only a path through a repeater cares about the width. Said plainly, and
                    // the press stays a press rather than an "apply anyway?".
                    Text(
                        "same channel · path differs: desktop " + hostTuple.pathHashSize +
                            " B, here " + selected.pathHashSize + " B · a repeater needs the same width",
                        color = Dim,
                        fontSize = 11.sp,
                        fontFamily = MonoFont,
                    )
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
            // What this press costs, before it is made: the desktop is on another channel.
            // Only a channel change costs the link. A path-width change does not, so it does not
            // ask twice either.
            val breaksLink = hostTuple != null && !sameChannel(selected, hostTuple)
            val canApply = link != null && !busy && seeded && state == MeshLinkState.Bound && customOk
            AccentBtn(
                label = when {
                    busy -> "applying…"
                    breaksLink && !confirmBreak -> "apply anyway?"
                    else -> "apply"
                },
                modifier = Modifier.weight(1f),
                enabled = canApply,
            ) {
                if (breaksLink && !confirmBreak) {
                    confirmBreak = true
                } else {
                    applyNow(selected)
                }
            }
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
/**
 * Whether two radios are on the same **channel**.
 *
 * The four numbers the firmware validates, and the path-hash width. The width is not local: a
 * repeater's hash is one byte on MeshCore's public presets, so a phone on two bytes cannot follow a
 * route a public repeater built — and comparing only the four numbers hid exactly that, with the
 * settings row staying silent while the link could not work (measured 2026-11-07: host one byte,
 * phone two, no warning). Transmit power is still local and still not compared: a phone in Europe
 * is limited to 14 dBm and that is nobody else's business.
 */
/** Whether two radios are on the same **channel**: the four numbers that decide if they hear each
 * other at all. A difference here really does leave the link down until both ends move. */
internal fun sameChannel(a: RadioPreset, b: RadioPreset): Boolean =
    (a.frequencyMhz - b.frequencyMhz).let { kotlin.math.abs(it) < 0.01 } &&
        (a.bandwidthKhz - b.bandwidthKhz).let { kotlin.math.abs(it) < 0.1 } &&
        a.spreadingFactor == b.spreadingFactor &&
        a.codingRate == b.codingRate

/**
 * Whether two radios are on the same channel **and** the same path width.
 *
 * The row that offers `match` wants this: a width difference is worth naming, because a repeater's
 * hash is one byte on MeshCore's public presets and a two-byte reader cannot follow the route it
 * built. It is *not* a link failure, though — two radios on the same four numbers hear each other
 * whatever their widths — and the preset panel said "apply leaves link down" and offered
 * `apply anyway?` for exactly that difference (seen on the device, 2026-11-07).
 */
internal fun sameTuple(a: RadioPreset, b: RadioPreset): Boolean =
    sameChannel(a, b) && a.pathHashSize == b.pathHashSize

internal fun presetLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}  ${p.txPowerDbm} dBm  path ${p.pathHashSize}B"

/**
 * The channel [p] names, keeping **this** radio's transmit power and path-hash width.
 *
 * A tuple that came from another machine says which frequency, bandwidth, spreading factor and
 * coding rate to meet on. It says nothing about power or path width, which are local choices: the
 * host's own code carries the host's limits, and adopting them would put a phone in Europe on
 * 22 dBm, past the 14 dBm its band allows, and re-stamp its packets with a path width the operator
 * did not choose. Comparing and applying both take only the four channel fields for that reason.
 */
internal fun RadioPreset.channelOf(local: RadioPreset): RadioPreset =
    // **The width travels with the channel, the power does not.** A radio's transmit power is its
    // own business (a regional limit, a battery), but the path hash width is the network's: on
    // MeshCore's public presets a repeater's hash is one byte, so a phone that matched the
    // frequency and kept its own two-byte width would match half a channel and only notice once a
    // repeater was in the way.
    copy(txPowerDbm = local.txPowerDbm)

/** The channel as one line: the four numbers that have to match, and nothing local. */
internal fun channelLine(p: RadioPreset): String =
    "${p.frequencyMhz} MHz  ${p.bandwidthKhz} kHz  SF${p.spreadingFactor}  CR${p.codingRate}"

private fun radioProblemLine(problem: RadioScanProblem?): String = when (problem) {
    RadioScanProblem.PermissionDenied -> "Bluetooth permission off · grant, then scan"
    RadioScanProblem.BluetoothOff -> "Bluetooth off"
    RadioScanProblem.Unsupported -> "no Bluetooth LE here"
    RadioScanProblem.Failed -> "scan failed · try again"
    null -> ""
}

private fun radioLinkProblemLine(state: MeshLinkState): String = when (state) {
    MeshLinkState.NeedsRadio -> "choose LoRa radio first"
    MeshLinkState.NeedsPermission -> "Bluetooth permission off"
    MeshLinkState.RadioUnavailable -> "LoRa radio not answering"
    MeshLinkState.NoPath -> "host path does not fit"
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
private fun RadioFacts(
    info: RadioSelfInfo?,
    device: RadioDeviceInfo?,
    status: MeshStatus?,
    problem: String?,
) {
    if (info == null && device == null && status == null && problem == null) return
    problem?.let { FactLine("problem", it) }
    if (info != null) {
        FactLine("name", info.name + (device?.let { "  ·  " + it.firmwareVersion } ?: ""))
    } else if (device != null) {
        FactLine("firmware", device.firmwareVersion)
    }
    val rssi = status?.rssiDbm
    // **Voltage, not a percentage.** The companion reports millivolts, and this app has no
    // discharge curve for the cell the board happens to carry; the old `batteryPercent` field was
    // never assigned by anything, so its readout here could not fire.
    val volts = device?.batteryMillivolts?.let { "%.2f V".format(it / 1000.0) }
    if (rssi != null || volts != null) {
        FactLine(
            "signal",
            listOfNotNull(rssi?.let { "$it dBm" }, volts).joinToString("  ·  "),
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
