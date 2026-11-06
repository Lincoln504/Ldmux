package io.ldmux.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
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
    ModalCard(
        if (p.isMeshOnly) "use this computer over mesh?" else "pair with this computer?",
        onDismiss,
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (prompt.name.isNotEmpty()) PairingFact("name", prompt.name)
            PairingFact("address", prompt.address)
            PairingFact(if (p.isMeshOnly) "key" else "token", prompt.fingerprint)
            Text(
                if (p.isMeshOnly) {
                    "Everything you type will be sent over the LoRa radio link to this computer. " +
                        "Your tailnet pairing is not changed."
                } else {
                    "Everything you type will be sent to this address."
                },
                color = Muted,
                fontSize = 11.sp,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
        ModalActions {
            OutlinedBtn("cancel", Modifier.weight(1f), onClick = onDismiss)
            AccentBtn("pair", Modifier.weight(1f), onClick = onConfirm)
        }
    }
}

@Composable
private fun PairingFact(label: String, value: String) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(label, color = Dim, fontSize = 12.sp, modifier = Modifier.width(56.dp))
        Text(value, color = Fg, fontFamily = MonoFont, fontSize = 12.sp)
    }
}

/**
 * A pairing link pasted or typed, for a phone whose scanner cannot run.
 *
 * The scanner is Google's code scanner, which does not exist on a device without Play
 * services (or a build of Android without it), and until this dialog existed such a phone
 * could not pair at all: the pairing row said "scanner unavailable" and offered no other
 * verb. The link is the same string the QR code encodes, so the same parser reads it
 * ([parseScannedPairing]), and what the dialog shows before the press is the address and the
 * token fingerprint [ConfirmPairing] shows for a link that arrived from outside. The press
 * is the question, as it is for a scan.
 */
@Composable
internal fun PastePairingLink(onDismiss: () -> Unit, onPair: (Pairing) -> Unit) {
    var text by remember { mutableStateOf("") }
    // Parsed on every edit, so the one control says whether the link is usable before it is
    // pressed. Eight lines of `ldmux code` output is easy to truncate when copying.
    val pairing = remember(text) { parseScannedPairing(text) }
    val prompt = pairing?.let(::pairingPrompt)
    ModalCard("paste a pairing link", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                "on the host, run `ldmux code` and copy the whole link",
                color = Muted,
                fontSize = 11.sp,
            )
            Field(
                value = text,
                onValueChange = { text = it },
                placeholder = "ldmux://pair?v=1&…",
            )
            when {
                text.isBlank() -> Unit
                prompt == null -> Text(NOT_A_CODE, color = Warn, fontSize = 11.sp)
                else -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    if (prompt.name.isNotEmpty()) PairingFact("name", prompt.name)
                    PairingFact("address", prompt.address)
                    PairingFact(if (pairing!!.isMeshOnly) "key" else "token", prompt.fingerprint)
                }
            }
        }
        ModalActions {
            OutlinedBtn("cancel", Modifier.weight(1f), onClick = onDismiss)
            AccentBtn("pair", Modifier.weight(1f), enabled = pairing != null) {
                pairing?.let(onPair)
            }
        }
    }
}

@Composable
internal fun NotACode(onDismiss: () -> Unit) {
    ModalCard(NOT_A_CODE, onDismiss) {
        ModalActions {
            AccentBtn("ok", Modifier.weight(1f), onClick = onDismiss)
        }
    }
}

@Composable
fun ConfirmKill(target: Session, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    ModalCard("close this shell?", onDismiss) {
        Text(target.label, color = Muted, fontSize = 12.sp)
        ModalActions {
            OutlinedBtn("cancel", Modifier.weight(1f), onClick = onDismiss)
            DangerBtn("close", Modifier.weight(1f), onClick = onConfirm)
        }
    }
}
