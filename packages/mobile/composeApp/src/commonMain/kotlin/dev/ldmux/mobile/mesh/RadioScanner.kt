package dev.ldmux.mobile.mesh

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
}
