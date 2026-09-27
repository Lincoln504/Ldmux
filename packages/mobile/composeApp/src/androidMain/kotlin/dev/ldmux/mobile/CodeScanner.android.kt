package dev.ldmux.mobile

import android.app.Activity
import com.google.mlkit.common.MlKitException
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import java.lang.ref.WeakReference

/**
 * Google's code scanner: a full-screen scanner owned by Play services.
 *
 * Chosen over CameraX + ML Kit because it needs no camera permission (Play services
 * holds the camera, the app only receives the text), no preview UI of our own, and
 * closes itself on the first read — one tap from `pair` to paired. The cost is Play
 * services; without it [scan] answers [ScanResult.Unavailable], and a phone can still
 * pair by opening the link from its own camera app (see [PairingInbox]).
 */
actual object CodeScanner {
    private var host: WeakReference<Activity>? = null

    /** The activity the scanner is shown over; set by MainActivity, like Dictation's. */
    fun attachHost(activity: Activity) {
        host = WeakReference(activity)
    }

    actual fun scan(onResult: (ScanResult) -> Unit) {
        val activity = host?.get()
        if (activity == null) {
            onResult(ScanResult.Unavailable)
            return
        }
        val options = GmsBarcodeScannerOptions.Builder()
            .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
            .build()
        // Exactly one answer per scan: the Task reports success, cancellation or
        // failure, and never more than one of them.
        runCatching {
            GmsBarcodeScanning.getClient(activity, options).startScan()
                .addOnSuccessListener { code ->
                    val text = code.rawValue
                    onResult(if (text.isNullOrEmpty()) ScanResult.Cancelled else ScanResult.Code(text))
                }
                .addOnCanceledListener { onResult(ScanResult.Cancelled) }
                .addOnFailureListener { e ->
                    // Only "there is no scanner" is worth a sentence. Backing out is
                    // not reliably a cancel: on the device the back gesture arrives
                    // here as MlKitException INTERNAL (13), not through the cancel
                    // listener, and saying "scanner unavailable" to a user who just
                    // closed a working scanner was wrong. So an ML Kit failure is a
                    // cancel unless its code says the scanner is not there, and
                    // anything else (no Play services) is unavailable. Logged either
                    // way, with the code.
                    appLog("code scanner ended: ${e::class.simpleName} ${(e as? MlKitException)?.errorCode}")
                    val missing = e !is MlKitException ||
                        e.errorCode == MlKitException.UNAVAILABLE ||
                        e.errorCode == MlKitException.CODE_SCANNER_UNAVAILABLE
                    onResult(if (missing) ScanResult.Unavailable else ScanResult.Cancelled)
                }
        }.onFailure {
            appLog("code scanner unavailable: ${it::class.simpleName}")
            onResult(ScanResult.Unavailable)
        }
    }
}
