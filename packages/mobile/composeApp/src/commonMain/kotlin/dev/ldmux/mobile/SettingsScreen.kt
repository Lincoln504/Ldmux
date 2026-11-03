package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.mesh.MeshRouteActions
import dev.ldmux.mobile.mesh.MeshRouteSection
import dev.ldmux.mobile.mesh.MeshRoute

/**
 * Settings, as a screen.
 *
 * It was an `AlertDialog`, and a dialog is the wrong shape for this: the mesh route's setup is a
 * radio picker, a preset and a set of credentials, and a modal card over a terminal is a place to
 * read one row, not to do that. It is also the only way to switch which link the app is on, and a
 * mode is a screen-sized decision.
 *
 * **Edits apply as they are made.** The dialog had a `save` button because a dialog has to be
 * able to be dismissed; a screen that returns to the launcher has no such need, and a pairing, a
 * radio pick and a font size all want to be visible immediately. Every control therefore calls
 * [onSave] with the whole config, and the caller persists it.
 *
 * There is no link switch on the launcher: this is where the link is chosen, and the launcher
 * then shows the app on that link only. The mesh section is absent where the platform has no
 * mesh route (Apple: tailnet only), which is decided by the caller and by
 * [MeshRouteSection]'s own `meshExists` check rather than guessed here.
 */
@Composable
internal fun SettingsScreen(
    current: LdmuxConfig,
    onBack: () -> Unit,
    /** Persist a change now. The screen never keeps a draft. */
    onSave: (LdmuxConfig) -> Unit,
    /** A good scan: pair and stay where the user is. Pressing `pair` was the question. */
    onPaired: (Pairing) -> Unit,
    /** Forget the pairing now (ADR-0017). */
    onUnpair: () -> Unit,
    /** The mesh route's setup, or null where this build has no mesh route. */
    mesh: MeshRouteActions? = null,
) {
    Column(
        Modifier
            .fillMaxSize()
            .background(Bg)
            .safeDrawingPadding()
            .imePadding(),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = LocalTouch.current.rowPad * 3, vertical = LocalTouch.current.rowPad * 2),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            MiniTextButton(onClick = onBack) { Text("\u2039 back", color = Accent, fontSize = 14.sp) }
            Spacer(Modifier.width(12.dp))
            Text("Settings", color = Fg, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
        }
        Column(
            Modifier
                .weight(1f)
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = LocalTouch.current.rowPad * 3),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad * 2),
        ) {
            Section("host") {
                PairingRow(current, onPaired, onUnpair)
            }
            if (mesh != null) {
                Section("link") {
                    LinkChoice(current, onSave)
                }
                // The radio, its preset and its credentials are one block, and it is the same
                // block the dialog had: it decides nothing itself.
                MeshRouteSection(
                    current = current,
                    readCredentials = mesh.credentials,
                    onMeshPairing = mesh.onPairing,
                    onConfig = mesh.onConfig,
                    onUnpairMesh = mesh.onUnpair,
                )
            }
            Section("display") {
                PanelSide(current, onSave)
                ControlSize(current, onSave)
                TextSize(current, onSave)
            }
            Spacer(Modifier.height(LocalTouch.current.rowPad * 4))
        }
    }
}

/** A titled block: the small grey label above its rows, and nothing around it. */
@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad)) {
        Text(title, color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
        content()
    }
}

/**
 * Which link the app runs on: the tailnet, or the phone's own radio.
 *
 * Two buttons rather than a switch, because there are two named things and a switch cannot say
 * either of their names. Choosing `lora` does not start the radio - the mesh screen does that
 * when it opens - and it does not touch the tailnet pairing, which stays valid in case the user
 * switches back.
 */
@Composable
private fun LinkChoice(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        MiniTextButton(onClick = { if (current.route != MeshRoute.Tailnet) onSave(current.copy(route = MeshRoute.Tailnet)) }) {
            Text("tailnet", color = if (current.route != MeshRoute.Mesh) Accent else Dim, fontSize = 12.sp)
        }
        MiniTextButton(onClick = { if (current.route != MeshRoute.Mesh) onSave(current.copy(route = MeshRoute.Mesh)) }) {
            Text("lora", color = if (current.route == MeshRoute.Mesh) Accent else Dim, fontSize = 12.sp)
        }
        Text(
            if (current.route == MeshRoute.Mesh) "the phone's radio" else "over Tailscale",
            color = Dim, fontSize = 11.sp,
        )
    }
}

@Composable
private fun PanelSide(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("panel", color = Dim, fontSize = 12.sp)
        MiniTextButton(onClick = { if (current.panelLeft) onSave(current.copy(side = "right")) }) {
            Text("right", color = if (!current.panelLeft) Accent else Dim, fontSize = 12.sp)
        }
        MiniTextButton(onClick = { if (!current.panelLeft) onSave(current.copy(side = "left")) }) {
            Text("left", color = if (current.panelLeft) Accent else Dim, fontSize = 12.sp)
        }
    }
}

@Composable
private fun ControlSize(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("control size", color = Dim, fontSize = 12.sp)
        MiniTextButton(onClick = { if (current.compact) onSave(current.copy(compact = false)) }) {
            Text("tall", color = if (!current.compact) Accent else Dim, fontSize = 12.sp)
        }
        MiniTextButton(onClick = { if (!current.compact) onSave(current.copy(compact = true)) }) {
            Text("thin", color = if (current.compact) Accent else Dim, fontSize = 12.sp)
        }
    }
}

@Composable
private fun TextSize(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("text size", color = Dim, fontSize = 12.sp)
        // Disabled at the ends of the range rather than silently doing nothing: a live button
        // that produces no visible change looks broken and hides the reason.
        val smaller = current.fontSp > LdmuxConfig.MIN_FONT_SP
        val larger = current.fontSp < LdmuxConfig.MAX_FONT_SP
        MiniTextButton(
            onClick = { onSave(current.copy(fontSp = (current.fontSp - 1f).coerceAtLeast(LdmuxConfig.MIN_FONT_SP))) },
            enabled = smaller,
        ) {
            Text("\u2212", color = if (smaller) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
        }
        Text("${current.fontSp.toInt()}", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
        MiniTextButton(
            onClick = { onSave(current.copy(fontSp = (current.fontSp + 1f).coerceAtMost(LdmuxConfig.MAX_FONT_SP))) },
            enabled = larger,
        ) {
            Text("+", color = if (larger) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
        }
    }
}

/**
 * The pairing, as one row (ADR-0017).
 *
 * Unpaired: `unpaired` and `pair`, which opens the scanner. Paired: the machine's name and
 * `unpair`, which asks once (`unpair?`) because it ends every shell's link. A scan that is not an
 * ldmux code says so on the row, in three words.
 */
@Composable
private fun PairingRow(current: LdmuxConfig, onPaired: (Pairing) -> Unit, onUnpair: () -> Unit) {
    var problem by remember { mutableStateOf<String?>(null) }
    var confirmUnpair by remember(current.connectionKey) { mutableStateOf(false) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (current.isPaired) {
                Text(current.pairedName, color = Fg, fontSize = 14.sp, maxLines = 1, modifier = Modifier.weight(1f, fill = false))
                MiniTextButton(onClick = {
                    if (confirmUnpair) onUnpair() else confirmUnpair = true
                }) {
                    Text(if (confirmUnpair) "unpair?" else "unpair", color = if (confirmUnpair) Accent else Dim)
                }
            } else {
                Text("unpaired", color = Dim, fontSize = 12.sp)
                MiniTextButton(onClick = {
                    problem = null
                    CodeScanner.scan { result ->
                        when (val o = scanOutcome(result)) {
                            is PairOutcome.Paired -> onPaired(o.pairing)
                            is PairOutcome.Problem -> problem = o.text
                            PairOutcome.Nothing -> Unit
                        }
                    }
                }) {
                    Text("pair", color = Accent)
                }
            }
        }
        problem?.let { Text(it, color = Dim, fontSize = 11.sp) }
    }
}
