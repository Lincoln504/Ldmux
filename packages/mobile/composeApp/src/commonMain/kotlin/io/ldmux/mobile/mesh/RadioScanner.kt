package io.ldmux.mobile.mesh

/** Why a radio scan could not produce a list. */
enum class RadioScanProblem {
    PermissionDenied,
    BluetoothOff,
    Unsupported,
    Failed,
}

/** The result of one bounded BLE scan for MeshCore companions. */
data class RadioScanResult(
    val devices: List<RadioDevice>,
    val problem: RadioScanProblem? = null,
) {
    val ok: Boolean get() = problem == null
}

/**
 * A bounded scan for MeshCore companion radios.
 *
 * The platform owns BLE scanning and the runtime permission. Common code only
 * states what a scan is for: one list of radios, or the one reason there is no
 * list. The scan stops itself; call [cancel] when the picker closes early.
 */
expect object RadioScanner {
    /** Start one scan. [onResult] is called once, on the main thread. */
    fun scan(onResult: (RadioScanResult) -> Unit)

    /** Stop a scan that is no longer wanted. The pending callback is not called. */
    fun cancel()

    /**
     * Radios this phone has already paired with at the Bluetooth level, whether or not they are
     * advertising right now.
     *
     * **A scan is not the only way to choose a radio.** A companion stops advertising while
     * something holds its connection — the same app on another phone, or the other app on this one
     * — so the picker's empty list was a dead end for the one case the operator needed a second
     * attempt from. A bonded device can still be *tried*: the connection either works or it says
     * why, which is a better answer than "no radio".
     *
     * Only devices that look like MeshCore companions are returned, by the same rule the scan
     * uses, so a bonded headset is not offered as a radio.
     */
    fun bonded(): List<RadioDevice>
}
