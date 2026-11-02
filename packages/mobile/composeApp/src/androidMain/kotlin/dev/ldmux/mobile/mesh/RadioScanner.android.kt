package dev.ldmux.mobile.mesh

import android.Manifest
import android.app.Activity
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import dev.ldmux.mobile.AndroidApp
import dev.ldmux.mobile.appLog
import io.iotone.meshcore.MeshcoreBle
import java.lang.ref.WeakReference
import java.util.UUID

/**
 * Android BLE scan for MeshCore companions.
 *
 * The scan is unfiltered and then classified locally. The companion's service
 * UUID is the strong signal; the `MeshCore-` name is accepted as a fallback
 * because some firmware puts the name in the scan response before the service
 * list is visible. An unrelated device is never offered just because it was
 * nearby.
 *
 * Every mutable field is owned by the main thread. Scan callbacks arrive on a
 * binder thread, so they are posted before touching `seen`, `pending` or the
 * generation token. `cancel()` invalidates the generation, which makes a
 * callback that was already queued harmless.
 */
actual object RadioScanner {
    private const val REQ_RADIO = 0x2a
    private const val SCAN_MS = 8_000L

    private val serviceUuid: UUID = UUID.fromString(MeshcoreBle.SERVICE_UUID)
    private val main = Handler(Looper.getMainLooper())
    private var host: WeakReference<Activity>? = null
    private var pending: ((RadioScanResult) -> Unit)? = null
    private var scanner: android.bluetooth.le.BluetoothLeScanner? = null
    private var callback: ScanCallback? = null
    private val seen = linkedMapOf<String, RadioDevice>()
    private var finished = false
    private var scanGeneration = 0

    /** The activity that can receive the permission answer. Android only. */
    fun attachHost(activity: Activity) {
        host = WeakReference(activity)
    }

    actual fun scan(onResult: (RadioScanResult) -> Unit) {
        // The dialog calls this from the main thread, but a caller from another
        // dispatcher must not race `cancel()` or a binder callback.
        main.post { beginScan(onResult) }
    }

    actual fun cancel() {
        main.post { cancelNow() }
    }

    /** Called by MainActivity after the runtime permission answer. */
    fun onRequestPermissionsResult(requestCode: Int, grantResults: IntArray) {
        if (requestCode != REQ_RADIO) return
        main.post {
            if (pending == null || finished) return@post
            val granted = grantResults.isNotEmpty() &&
                grantResults.all { it == PackageManager.PERMISSION_GRANTED }
            if (!granted) {
                finish(RadioScanResult(emptyList(), RadioScanProblem.PermissionDenied))
            } else {
                startScan()
            }
        }
    }

    /** Called by MainActivity when the activity is destroyed. */
    fun detachHost() {
        main.post {
            host = null
            cancelNow()
        }
    }

    private fun beginScan(onResult: (RadioScanResult) -> Unit) {
        cancelNow()
        pending = onResult
        seen.clear()
        finished = false
        scanGeneration++
        val context = AndroidApp.context
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)) {
            finish(RadioScanResult(emptyList(), RadioScanProblem.Unsupported))
            return
        }
        if (!hasScanPermissions(context)) {
            val activity = host?.get()
            if (activity == null) {
                finish(RadioScanResult(emptyList(), RadioScanProblem.PermissionDenied))
                return
            }
            activity.requestPermissions(neededPermissions(), REQ_RADIO)
            return
        }
        startScan()
    }

    private fun cancelNow() {
        pending = null
        stopScan()
        seen.clear()
        finished = true
        scanGeneration++
    }

    private fun startScan() {
        if (pending == null || finished) return
        val context = AndroidApp.context
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter = manager?.adapter
        if (adapter == null || !adapter.isEnabled) {
            finish(RadioScanResult(emptyList(), RadioScanProblem.BluetoothOff))
            return
        }
        val le = adapter.bluetoothLeScanner
        if (le == null) {
            finish(RadioScanResult(emptyList(), RadioScanProblem.BluetoothOff))
            return
        }
        scanner = le
        val generation = scanGeneration
        val cb = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                // Binder thread. Classify on main, where `seen` and the
                // generation token are owned.
                main.post { record(result, generation) }
            }

            override fun onScanFailed(errorCode: Int) {
                main.post {
                    if (finished || generation != scanGeneration) return@post
                    appLog("mesh radio scan failed: $errorCode")
                    finish(RadioScanResult(emptyList(), RadioScanProblem.Failed))
                }
            }
        }
        callback = cb
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .build()
        val started = runCatching { le.startScan(null, settings, cb) }.isSuccess
        if (!started) {
            appLog("mesh radio scan refused")
            finish(RadioScanResult(emptyList(), RadioScanProblem.Failed))
            return
        }
        main.postDelayed({
            if (finished || generation != scanGeneration) return@postDelayed
            finish(RadioScanResult(seen.values.sortedByDescending { it.rssi ?: Int.MIN_VALUE }))
        }, SCAN_MS)
    }

    private fun record(result: ScanResult, generation: Int) {
        if (finished || generation != scanGeneration) return
        val device = result.device ?: return
        val scanRecord = result.scanRecord
        val name = scanRecord?.deviceName ?: runCatching { device.name }.getOrNull()
        val advertisesService = scanRecord?.serviceUuids?.any { it.uuid == serviceUuid } == true
        if (!advertisesService && name?.startsWith("MeshCore") != true) return
        val address = device.address ?: return
        val shown = name?.takeIf { it.isNotBlank() } ?: address
        seen[address] = RadioDevice(address, shown, result.rssi)
    }

    private fun stopScan() {
        val le = scanner
        val cb = callback
        scanner = null
        callback = null
        if (le != null && cb != null) {
            runCatching { le.stopScan(cb) }
        }
    }

    private fun finish(result: RadioScanResult) {
        if (finished) return
        finished = true
        stopScan()
        val cb = pending
        pending = null
        cb?.invoke(result)
    }

    private fun hasScanPermissions(context: Context): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            return context.checkSelfPermission(Manifest.permission.BLUETOOTH_SCAN) == PackageManager.PERMISSION_GRANTED &&
                context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
        }
        @Suppress("DEPRECATION")
        return context.checkSelfPermission(Manifest.permission.BLUETOOTH) == PackageManager.PERMISSION_GRANTED &&
            context.checkSelfPermission(Manifest.permission.BLUETOOTH_ADMIN) == PackageManager.PERMISSION_GRANTED &&
            context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
    }

    private fun neededPermissions(): Array<String> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            arrayOf(Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT)
        } else {
            arrayOf(
                Manifest.permission.BLUETOOTH,
                Manifest.permission.BLUETOOTH_ADMIN,
                Manifest.permission.ACCESS_FINE_LOCATION,
            )
        }
}
