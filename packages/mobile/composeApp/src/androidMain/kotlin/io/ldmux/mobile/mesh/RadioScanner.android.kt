package io.ldmux.mobile.mesh

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import android.os.Build
import android.os.Handler
import android.os.Looper
import io.ldmux.mobile.AndroidApp
import io.ldmux.mobile.appLog
import io.ldmux.mobile.ble.BleScan
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

    private val serviceUuid: UUID = UUID.fromString(BleScan.SERVICE_UUID)
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
        cancelNow(report = true)
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
            // Remember that a dialog was asked for: after the answer, `neverAsked` must be false
            // so the recovery button can tell "answer the dialog again" from "open settings".
            activity.getSharedPreferences("ldmux_radio_permission", Context.MODE_PRIVATE)
                .edit().putBoolean("asked", true).apply()
            activity.requestPermissions(neededPermissions(), REQ_RADIO)
            return
        }
        startScan()
    }

    /**
     * Stop whatever is running.
     *
     * [report] is for a scan another scan replaced: that caller is still waiting for an answer and
     * would otherwise wait for ever (`scanning…` never flipping was the visible symptom), so it is
     * told this one produced no list. The public `cancel()` keeps its contract — the user closed
     * the picker, nobody is waiting — and stays silent.
     */
    private fun cancelNow(report: Boolean = false) {
        val waiting = pending
        pending = null
        if (report) waiting?.invoke(RadioScanResult(emptyList(), RadioScanProblem.Failed))
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

    // Every Bluetooth read below runs after a permission check and inside `runCatching`, because
    // a revocation between the two raises `SecurityException` rather than returning a value. Lint
    // asks for the check or the catch; it cannot see either through `runCatching`, so the reason is
    // written here instead of the whole file being silenced.
    @SuppressLint("MissingPermission")
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

    /**
     * What this phone is already bonded to, filtered to radios.
     *
     * A bonded device keeps its name, which is the only field a bonded list carries: there is no
     * scan record, so no service UUIDs and no RSSI. The name rule is the same one `record` uses for
     * a device that advertises without the service UUID, and it is deliberately the only rule —
     * offering every bonded accessory as a "radio" would be a list of wrong answers.
     */
    @SuppressLint("MissingPermission") // `hasScanPermissions` above, and each read in `runCatching`
    actual fun bonded(): List<RadioDevice> {
        val context = AndroidApp.context
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)) {
            return emptyList()
        }
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter = manager?.adapter ?: return emptyList()
        if (!adapter.isEnabled) return emptyList()
        // The bonded list is a permission-gated read on Android 12+; a denial is an empty list,
        // which the picker already draws a reason for.
        if (!hasScanPermissions(context)) return emptyList()
        val bonded = runCatching { adapter.bondedDevices }.getOrNull() ?: return emptyList()
        return bonded
            .mapNotNull { d ->
                val address = d.address ?: return@mapNotNull null
                val name = runCatching { d.name }.getOrNull()
                if (name?.startsWith("MeshCore") != true) return@mapNotNull null
                RadioDevice(address, name, null)
            }
            .sortedBy { it.name }
    }

    @SuppressLint("MissingPermission") // the scan only exists when it was permitted; the stop is guarded
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

    internal const val REQ_PERMISSION = REQ_RADIO

    internal fun permissionsGranted(context: Context): Boolean = hasScanPermissions(context)

    internal fun permissionsNeeded(): Array<String> = neededPermissions()

    internal fun hostActivity(): Activity? = host?.get()

    /**
     * Whether a *dialog* can still be drawn for a missing permission.
     *
     * `shouldShowRequestPermissionRationale` is false for a first request and false after a
     * permanent denial; only the activity can tell them apart, and only when it is alive.
     */
    internal fun rationale(): Boolean {
        val activity = host?.get() ?: return false
        return neededPermissions().any { activity.shouldShowRequestPermissionRationale(it) }
    }

    /**
     * Whether this app has asked before. Persisted, because the process can be restarted between
     * the denial and the next press, and "never asked" and "denied for good" must not be confused.
     */
    internal fun neverAsked(context: Context): Boolean =
        !context.getSharedPreferences("ldmux_radio_permission", Context.MODE_PRIVATE)
            .getBoolean("asked", false)

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

/**
 * The permission seam's Android half.
 *
 * "Asked before" is persisted: `shouldShowRequestPermissionRationale` answers false both for
 * "never asked" and for "denied for good", and the two need opposite buttons. The first press
 * must ask (a dialog appears); every press after a denial where the system no longer shows a
 * dialog must open the app's settings page, which is the only place a permanent denial can be
 * undone.
 */
actual object RadioPermission {
    private const val PREFS = "ldmux_radio_permission"
    private const val KEY_ASKED = "asked"

    actual fun granted(): Boolean = RadioScanner.permissionsGranted(AndroidApp.context)

    actual fun canAsk(): Boolean {
        val context = AndroidApp.context
        if (granted()) return true
        if (RadioScanner.neverAsked(context) || RadioScanner.rationale()) return true
        return false
    }

    actual fun resolve() {
        val context = AndroidApp.context
        val activity = RadioScanner.hostActivity()
        if (canAsk() && activity != null) {
            val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            prefs.edit().putBoolean(KEY_ASKED, true).apply()
            activity.requestPermissions(RadioScanner.permissionsNeeded(), RadioScanner.REQ_PERMISSION)
            return
        }
        // Permanent denial, or no activity to ask from: the system settings page for this app is
        // the only place the permission can be granted.
        val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
            .setData(Uri.fromParts("package", context.packageName, null))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { context.startActivity(intent) }
            .onFailure { appLog("could not open the app's settings page: $it") }
    }
}
