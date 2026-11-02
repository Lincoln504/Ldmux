package dev.ldmux.mobile.mesh

import android.Manifest
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import io.iotone.meshcore.android.AndroidBleTransport
import io.iotone.meshcore.android.MeshcoreSession
import io.iotone.meshcore.android.RawSendCallback
import io.iotone.meshcore.android.SessionListener
import dev.ldmux.mobile.AndroidApp
import io.iotone.meshcore.android.SessionState
import io.iotone.meshcore.frames.ContactFrame
import io.iotone.meshcore.frames.RawDataFrame
import io.iotone.meshcore.model.RadioParams
import io.iotone.meshcore.model.SelfInfo
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import java.io.IOException

/**
 * The Android half of [RadioLink]: one BLE connection to a MeshCore companion,
 * one companion session, and one raw-custom send in flight.
 *
 * All library callbacks arrive on the Android main thread. This object does no
 * blocking there: sends are suspended on a [CompletableDeferred] and inbound
 * records are handed to an unbounded channel.
 */
class AndroidRadioLink(
    private val context: Context,
    private val address: String,
    private val hostKey: ByteArray,
) : RadioLink {

    private val _state = MutableStateFlow(if (address.isEmpty()) MeshLinkState.NeedsRadio else MeshLinkState.Idle)
    override val state: StateFlow<MeshLinkState> = _state.asStateFlow()

    private val _status = MutableStateFlow(
        MeshStatus(radioState = 0, rssiDbm = null, batteryPercent = null, txAirtimeTenthsThisHour = 0, opsLeftThisHour = null),
    )
    override val status: StateFlow<MeshStatus?> = _status.asStateFlow()

    private val _selfInfo = MutableStateFlow<RadioSelfInfo?>(null)
    override val selfInfo: StateFlow<RadioSelfInfo?> = _selfInfo.asStateFlow()

    private val inbox = Channel<ByteArray>(Channel.UNLIMITED)
    override val received: Flow<ByteArray> = inbox.receiveAsFlow()

    /** A zero-hop direct path until a contact teaches us the real one. */
    @Volatile
    private var hostPath: ByteArray = ByteArray(0)

    override val uplinkRecordMax: Int
        get() = minOf(172, 174 - hostPath.size)

    private val mutex = Mutex()
    private var transport: AndroidBleTransport? = null
    private var session: MeshcoreSession? = null
    private var ready: CompletableDeferred<Unit>? = null
    @Volatile
    private var closing = false

    private val listener = object : SessionListener {
        override fun onStateChanged(state: SessionState) {
            if (closing) return
            when (state) {
                SessionState.DISCONNECTED -> {
                    _state.value = MeshLinkState.RadioUnavailable
                    ready?.completeExceptionally(RadioLinkException(MeshLinkState.RadioUnavailable, "radio disconnected"))
                }
                SessionState.CONNECTING, SessionState.HANDSHAKING -> _state.value = MeshLinkState.Connecting
                SessionState.READY -> Unit
            }
        }

        override fun onReady(selfInfo: SelfInfo) {
            if (closing) return
            _selfInfo.value = selfInfo.toRadioSelfInfo()
            _status.value = _status.value.copy(radioState = 0)
            _state.value = MeshLinkState.Bound
            ready?.complete(Unit)
            // The host must be a contact for a multi-hop direct path. Asking is cheap
            // and the answer only improves the next send; a zero-hop send still works.
            session?.requestContacts()
        }

        override fun onRawData(frame: RawDataFrame) {
            _status.value = _status.value.copy(rssiDbm = frame.rssi())
            inbox.trySend(frame.payload())
        }

        override fun onContact(frame: ContactFrame) {
            val c = frame.contact()
            if (hostKey.isNotEmpty() && c.publicKey().contentEquals(hostKey)) {
                hostPath = c.activePath()
            }
        }
    }

    override suspend fun connect() = mutex.withLock {
        if (_state.value == MeshLinkState.Bound) return@withLock
        if (address.isEmpty()) {
            _state.value = MeshLinkState.NeedsRadio
            throw RadioLinkException(MeshLinkState.NeedsRadio, "no radio selected")
        }
        if (!hasBlePermission()) {
            _state.value = MeshLinkState.NeedsPermission
            throw RadioLinkException(MeshLinkState.NeedsPermission, "Bluetooth permission is not granted")
        }
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter = manager?.adapter
        if (adapter == null || !adapter.isEnabled) {
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, "Bluetooth is off")
        }
        val device = try {
            adapter.getRemoteDevice(address)
        } catch (e: IllegalArgumentException) {
            _state.value = MeshLinkState.NeedsRadio
            throw RadioLinkException(MeshLinkState.NeedsRadio, "not a Bluetooth address: $address")
        } catch (e: SecurityException) {
            _state.value = MeshLinkState.NeedsPermission
            throw RadioLinkException(MeshLinkState.NeedsPermission, "Bluetooth permission is not granted")
        }

        closing = false
        _state.value = MeshLinkState.Connecting
        val t = AndroidBleTransport(context)
        val s = MeshcoreSession("ldmux", t, listener)
        transport = t
        session = s
        val gate = CompletableDeferred<Unit>()
        ready = gate
        try {
            t.connectToDevice(device)
            withTimeout(CONNECT_TIMEOUT_MS) { gate.await() }
        } catch (e: TimeoutCancellationException) {
            closeTransport()
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, "the radio did not answer APP_START")
        } catch (e: RadioLinkException) {
            closeTransport()
            throw e
        } catch (e: Exception) {
            closeTransport()
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, e.message ?: "radio connect failed")
        }
    }

    override suspend fun send(record: ByteArray): RadioSendResult {
        if (record.isEmpty()) return RadioSendResult.TooLong
        if (record.size > uplinkRecordMax) return RadioSendResult.TooLong
        val s = session ?: return RadioSendResult.Failed
        if (_state.value != MeshLinkState.Bound) return RadioSendResult.Failed
        val answer = CompletableDeferred<RadioSendResult>()
        val callback = RawSendCallback { ok, errorCode ->
            answer.complete(if (ok) RadioSendResult.Accepted else errorFor(errorCode))
        }
        val queued = try {
            s.sendRawData(hostPath, record, callback)
        } catch (e: Exception) {
            false
        }
        if (!queued) return RadioSendResult.Failed
        return try {
            withTimeout(SEND_TIMEOUT_MS) { answer.await() }
        } catch (e: TimeoutCancellationException) {
            // The companion protocol has no sequence number: a late OK could resolve
            // the next command. A timeout therefore ends the transport; the caller's
            // rebuild is the only safe next step.
            _state.value = MeshLinkState.RadioUnavailable
            RadioSendResult.Failed
        }
    }

    override suspend fun applyPreset(preset: RadioPreset): Boolean {
        if (_state.value != MeshLinkState.Bound) connect()
        val s = session ?: return false
        s.setRadioParams(
            RadioParams.of(preset.frequencyMhz, preset.bandwidthKhz, preset.spreadingFactor, preset.codingRate),
        )
        s.setTxPower(preset.txPowerDbm)
        // The setters are fire-and-forget on the companion API; the read-back after a
        // reconnect is the verification. Give the radio time to persist/savePrefs.
        delay(750)
        close()
        connect()
        val got = selfInfo.value ?: return false
        return kotlin.math.abs(got.frequencyMhz - preset.frequencyMhz) < 0.01 &&
            kotlin.math.abs(got.bandwidthKhz - preset.bandwidthKhz) < 0.1 &&
            got.spreadingFactor == preset.spreadingFactor &&
            got.codingRate == preset.codingRate
    }

    override fun close() {
        closing = true
        ready?.completeExceptionally(RadioLinkException(MeshLinkState.Closed, "closed"))
        ready = null
        closeTransport()
        _state.value = MeshLinkState.Closed
    }

    private fun closeTransport() {
        val s = session
        val t = transport
        session = null
        transport = null
        try {
            s?.close()
        } catch (_: Exception) {
        }
        try {
            t?.close()
        } catch (_: IOException) {
        } catch (_: Exception) {
        }
    }

    private fun hasBlePermission(): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            return context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
        }
        @Suppress("DEPRECATION")
        return context.checkSelfPermission(Manifest.permission.BLUETOOTH) == PackageManager.PERMISSION_GRANTED
    }

    private fun errorFor(code: Int): RadioSendResult = when (code) {
        2 -> RadioSendResult.NotFound
        3, 4 -> RadioSendResult.Busy
        1, 6 -> RadioSendResult.TooLong
        else -> RadioSendResult.Failed
    }

    private fun SelfInfo.toRadioSelfInfo() = RadioSelfInfo(
        name = name(),
        frequencyMhz = frequencyMhz(),
        bandwidthKhz = bandwidthKhz(),
        spreadingFactor = spreadingFactor(),
        codingRate = codingRate(),
        txPowerDbm = txPowerDbm(),
        maxTxPowerDbm = maxTxPowerDbm(),
    )

    private companion object {
        const val CONNECT_TIMEOUT_MS = 20_000L
        const val SEND_TIMEOUT_MS = 10_000L
    }
}

actual fun meshRadioLink(address: String, hostKey: ByteArray): RadioLink? =
    AndroidRadioLink(AndroidApp.context, address, hostKey)
