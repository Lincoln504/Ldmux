package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
internal fun SettingsDialog(
    current: LdmuxConfig,
    onDismiss: () -> Unit,
    onSave: (LdmuxConfig) -> Unit,
    /** A good scan: pair and close. Pressing `pair` was the question, so nothing is asked. */
    onPaired: (Pairing) -> Unit,
    /** Forget the pairing now (ADR-0017). */
    onUnpair: () -> Unit,
) {
    var side by remember { mutableStateOf(current.side) }
    var fontSp by remember { mutableStateOf(current.fontSp) }
    var compact by remember { mutableStateOf(current.compact) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Settings", color = Fg) },
        containerColor = Surface,
        // Material's dialogs mix their background from `primary` by default, which
        // is how a pure grey card picked up a violet cast on a black screen.
        tonalElevation = 0.dp,
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                PairingRow(current, onPaired, onUnpair)
                Box(Modifier.height(LocalTouch.current.rowPad * 2))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("panel", color = Dim, fontSize = 12.sp)
                    MiniTextButton(onClick = { side = "left" }) { Text("left", color = if (side == "left") Accent else Dim) }
                    MiniTextButton(onClick = { side = "right" }) { Text("right", color = if (side == "right") Accent else Dim) }
                }
                Box(Modifier.height(LocalTouch.current.rowPad * 2))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("control size", color = Dim, fontSize = 12.sp)
                    MiniTextButton(onClick = { compact = false }) {
                        Text("tall", color = if (!compact) Accent else Dim)
                    }
                    MiniTextButton(onClick = { compact = true }) {
                        Text("thin", color = if (compact) Accent else Dim)
                    }
                }
                Box(Modifier.height(LocalTouch.current.rowPad * 2))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("text size", color = Dim, fontSize = 12.sp)
                    // Disabled at the ends of the range rather than silently doing
                    // nothing. A live button that produces no visible change is the
                    // worst of both: it looks broken, and it hides the reason. The
                    // terminal arrow already knew that.
                    val smaller = fontSp > LdmuxConfig.MIN_FONT_SP
                    val larger = fontSp < LdmuxConfig.MAX_FONT_SP
                    MiniTextButton(
                        onClick = { fontSp = (fontSp - 1f).coerceAtLeast(LdmuxConfig.MIN_FONT_SP) },
                        enabled = smaller,
                    ) {
                        Text("−", color = if (smaller) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
                    }
                    Text("${fontSp.toInt()}", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
                    MiniTextButton(
                        onClick = { fontSp = (fontSp + 1f).coerceAtMost(LdmuxConfig.MAX_FONT_SP) },
                        enabled = larger,
                    ) {
                        Text("+", color = if (larger) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
                    }
                }
            }
        },
        confirmButton = {
            MiniTextButton(onClick = {
                // A copy, never a rebuild. This used to construct the config by
                // position, and a field it did not list (the device id, once) was
                // silently cleared on every save. The pairing is not this button's:
                // it changes the moment `pair` or `unpair` is pressed.
                onSave(
                    current.copy(
                        side = side,
                        fontSp = fontSp.coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP),
                        compact = compact,
                    ),
                )
            }) {
                Text("save", color = Accent)
            }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

/**
 * The pairing, as one row (ADR-0017).
 *
 * Unpaired: `unpaired` and `pair`, which opens the scanner. Paired: the machine's
 * name and `unpair`, which asks once (`unpair?`) because it ends every shell's link.
 * A scan that is not an ldmux code says so on the row, in three words.
 */
@Composable
private fun PairingRow(current: LdmuxConfig, onPaired: (Pairing) -> Unit, onUnpair: () -> Unit) {
    var problem by remember { mutableStateOf<String?>(null) }
    var confirmUnpair by remember(current.connectionKey) { mutableStateOf(false) }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (current.isPaired) {
                Text(current.pairedName, color = Fg, fontSize = 13.sp, maxLines = 1, modifier = Modifier.weight(1f, fill = false))
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
