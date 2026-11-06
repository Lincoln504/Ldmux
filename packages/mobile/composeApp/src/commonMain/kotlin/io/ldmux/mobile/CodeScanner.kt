package io.ldmux.mobile

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * Scan one QR code (ADR-0017).
 *
 * A seam because reading a camera is the platform's: Google's code scanner on
 * Android, VisionKit's `DataScannerViewController` on iOS. The declaration is the
 * shared concept — "a code's text, or none" — and takes no Activity, the same shape
 * as [io.ldmux.mobile.voice.Dictation]: the Android half attaches its own host.
 * What the text *means* is decided here, in [scanOutcome], never on the platform.
 */
expect object CodeScanner {
    /** Open the scanner; [onResult] is called once, on the main thread. */
    fun scan(onResult: (ScanResult) -> Unit)
}

sealed interface ScanResult {
    data class Code(val text: String) : ScanResult
    /** The user backed out. Not an error, so nothing is said. */
    data object Cancelled : ScanResult
    /** No scanner on this device (Android: no Google Play services). */
    data object Unavailable : ScanResult
}

/** What a scan comes to, for the pairing row. */
sealed interface PairOutcome {
    data class Paired(val pairing: Pairing) : PairOutcome
    /** Nothing to say. */
    data object Nothing : PairOutcome
    /** A short line for the row: what went wrong, not what to do about it. */
    data class Problem(val text: String) : PairOutcome
}

fun scanOutcome(result: ScanResult): PairOutcome = when (result) {
    // **Any ldmux code.** There is one pairing store and one code: a code that carries only the
    // radio keys is still a pairing, and `ConfigStore.pair` applies whichever halves it holds. The
    // old split made the user choose a row before scanning, and refused a code that was perfectly
    // good under the other row.
    is ScanResult.Code -> parseScannedPairing(result.text)?.let { PairOutcome.Paired(it) }
        ?: PairOutcome.Problem(NOT_A_CODE)
    ScanResult.Cancelled -> PairOutcome.Nothing
    ScanResult.Unavailable -> PairOutcome.Problem("scanner unavailable")
}

internal const val NOT_A_CODE = "not an ldmux code"

/**
 * Pairing links opened from outside the app: the system camera, a message, a
 * browser (ADR-0017).
 *
 * Delivery is the platform's (Android's VIEW intent, iOS's `onOpenURL`), and it can
 * happen before any UI exists — a cold start from the camera — so this holds the
 * link until the app reads it rather than firing an event nobody hears. It is not a
 * seam: only the delivery differs, and the delivery is one call to [offer].
 *
 * A link from outside is untrusted and is **asked about** before it replaces
 * anything; the scan from Settings is not, because pressing `pair` was the question.
 */
object PairingInbox {
    private val _link = MutableStateFlow<String?>(null)
    val link: StateFlow<String?> = _link

    fun offer(link: String) {
        _link.value = link
    }

    fun clear() {
        _link.value = null
    }
}

/**
 * The shell a mark's notification was about, handed from the activity to the UI.
 *
 * The same shape as [PairingInbox] and for the same reason: the tap arrives outside the
 * composition, and the screen that can act on it (open a shell as a page, or switch to it) is the
 * one that reads this.
 */
object ShellInbox {
    private val _shell = MutableStateFlow<String?>(null)
    val shell: StateFlow<String?> = _shell

    fun offer(shellId: String) {
        if (shellId.isNotEmpty()) _shell.value = shellId
    }

    /** Taken, so the next tap is a new event. */
    fun clear() {
        _shell.value = null
    }
}

/** What to do with a link from [PairingInbox]. Pure, so the rule is testable. */
sealed interface InboundPairing {
    data object Invalid : InboundPairing
    /** The same machine and token this app is already paired with. */
    data class Same(val pairing: Pairing) : InboundPairing
    /** A different pairing (or the first one): ask before replacing anything. */
    data class Ask(val pairing: Pairing) : InboundPairing
}

fun inboundPairing(link: String, current: LdmuxConfig): InboundPairing {
    // A mesh-only link is a pairing too, and is always asked about: it is never "the same" as a
    // tailnet pairing, and it must not silently replace the mesh keys either.
    val p = parseScannedPairing(link) ?: return InboundPairing.Invalid
    return if (!p.isMeshOnly && current.isPaired && current.host == p.host && current.token == p.token) {
        InboundPairing.Same(p)
    } else {
        InboundPairing.Ask(p)
    }
}
