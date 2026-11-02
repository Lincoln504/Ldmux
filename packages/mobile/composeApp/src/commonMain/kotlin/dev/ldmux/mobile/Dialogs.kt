package dev.ldmux.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The thin bar that rides directly above the keyboard in portrait. Deliberately
 * sparse — the way back to landscape is the important control, and the rest is
 * only the handful of keys a soft keyboard cannot produce.
 */
/**
 * Ending a shell throws away whatever is running in it, so it is always
 * confirmed — and named, so the wrong one cannot be closed by reflex.
 */
@Composable
internal fun ConfirmPairing(p: Pairing, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    // The address is **always** shown, and the name is only a label beside it: the name
    // is whatever the code's author typed, and showing it in place of the host let a
    // code say "work laptop" and point anywhere on the tailnet. See [pairingPrompt].
    val prompt = pairingPrompt(p)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (p.isMeshOnly) "use this computer over mesh?" else "pair with this computer?", color = Fg) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                if (prompt.name.isNotEmpty()) PairingFact("name", prompt.name)
                PairingFact("address", prompt.address)
                PairingFact(if (p.isMeshOnly) "key" else "token", prompt.fingerprint)
                Text(
                    if (p.isMeshOnly) {
                        "Everything you type will be sent over the mesh radio link to this computer. " +
                            "Your tailnet pairing is not changed."
                    } else {
                        "Everything you type will be sent to this address."
                    },
                    color = Dim,
                    fontSize = 11.sp,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        },
        containerColor = Surface,
        tonalElevation = 0.dp,
        confirmButton = { MiniTextButton(onClick = onConfirm) { Text("pair", color = Accent) } },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

@Composable
private fun PairingFact(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label, color = Dim, fontSize = 12.sp, modifier = Modifier.width(56.dp))
        Text(value, color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
    }
}

@Composable
internal fun NotACode(onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(NOT_A_CODE, color = Fg) },
        containerColor = Surface,
        tonalElevation = 0.dp,
        confirmButton = { MiniTextButton(onClick = onDismiss) { Text("ok", color = Accent) } },
    )
}

@Composable
fun ConfirmKill(target: Session, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Close this shell?", color = Fg) },
        containerColor = Surface,
        text = { Text(target.label, color = Dim, fontSize = 12.sp) },
        confirmButton = {
            MiniTextButton(onClick = onConfirm) { Text("close", color = Color(0xFFE08A8A)) }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}
