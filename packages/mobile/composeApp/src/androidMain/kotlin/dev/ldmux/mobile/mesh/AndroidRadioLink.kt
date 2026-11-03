package dev.ldmux.mobile.mesh

import android.Manifest
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.SystemClock
import android.util.Log
import dev.ldmux.mobile.AndroidApp
import dev.ldmux.mobile.appLog
import io.iotone.meshcore.android.AndroidBleTransport
import io.iotone.meshcore.android.MeshcoreSession
import io.iotone.meshcore.android.RawSendCallback
import io.iotone.meshcore.android.SessionListener
import io.iotone.meshcore.android.SessionState
import io.iotone.meshcore.frames.ContactFrame
import io.iotone.meshcore.frames.DeviceInfoFrame
import io.iotone.meshcore.frames.EndOfContactsFrame
import io.iotone.meshcore.frames.ErrorFrame
import io.iotone.meshcore.frames.MeshcoreInbound
import io.iotone.meshcore.frames.RawDataFrame
import io.iotone.meshcore.model.Contact
import io.iotone.meshcore.model.DeviceInfo
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
import kotlinx.coroutines.withTimeoutOrNull
import java.io.IOException

/**
 * The Android half of [RadioLink]: one BLE connection to a MeshCore companion,
 * one companion session, and one raw-custom send in flight.
 *
 * All library callbacks arrive on the Android main thread. This object does no
 * blocking there: sends are suspended on a [CompletableDeferred] and inbound
 * records are handed to a bounded channel. The channel is bounded because a raw
 * push carries no sender identity: any nearby node can feed it before the L3
 * session gets to reject the record.
 */
class AndroidRadioLink(
    private val context: Context,
    private val address: String,
    hostKey: ByteArray,
) : RadioLink {

    @Volatile
    private var hostKey: ByteArray = hostKey

    /** The terminal may learn the host key after Settings opened the link first. */
    fun updateHostKey(key: ByteArray) {
        if (key.isEmpty() || key.contentEquals(hostKey)) return
        hostKey = key.copyOf()
        hostPath = null
        if (_state.value == MeshLinkState.Bound) {
            // The path belongs to the previous host key; ask for this host's
            // contact instead of keeping a route that now points at nobody.
            session?.getContact(hostKey)
        }
    }

    override fun release() {
        RadioLinks.release(this)
    }

    private val _state = MutableStateFlow(if (address.isEmpty()) MeshLinkState.NeedsRadio else MeshLinkState.Idle)
    override val state: StateFlow<MeshLinkState> = _state.asStateFlow()

    private val _status = MutableStateFlow(
        MeshStatus(radioState = 0, rssiDbm = null, batteryPercent = null, txAirtimeTenthsThisHour = 0, opsLeftThisHour = null),
    )
    override val status: StateFlow<MeshStatus?> = _status.asStateFlow()

    private val _selfInfo = MutableStateFlow<RadioSelfInfo?>(null)
    override val selfInfo: StateFlow<RadioSelfInfo?> = _selfInfo.asStateFlow()

    private val _deviceInfo = MutableStateFlow<RadioDeviceInfo?>(null)
    override val deviceInfo: StateFlow<RadioDeviceInfo?> = _deviceInfo.asStateFlow()

    private val inbox = Channel<ByteArray>(INBOX_CAPACITY)
    override val received: Flow<ByteArray> = inbox.receiveAsFlow()

    /**
     * The host's direct path as one immutable value.
     *
     * [encoded] is the firmware's encoded path byte (low six bits hops, high
     * two bits hash size minus one); [bytes] is the actual path. Keeping them
     * together means a send cannot read a new encoded value with an old byte
     * array. Null means no contact path is known; the adapter can still use a
     * zero-hop direct send to a neighbour, but it never invents a multi-hop
     * path.
     */
    private data class RadioPath(val encoded: Int, val bytes: ByteArray)

    @Volatile
    private var hostPath: RadioPath? = null

    private val mutex = Mutex()
    private var transport: AndroidBleTransport? = null
    private var session: MeshcoreSession? = null
    private var ready: CompletableDeferred<Unit>? = null
    private var contactDone: CompletableDeferred<Unit>? = null
    private var deviceInfoDone: CompletableDeferred<RadioDeviceInfo>? = null

    @Volatile
    private var closing = false

    @Volatile
    private var budgetMode: RadioBudgetMode = RadioBudgetMode.Metered
    private var airtimeWindowStartMs: Long = SystemClock.elapsedRealtime()
    private var airtimeUsedMs: Double = 0.0

    /**
     * A two- or three-byte path hash cannot use `CMD_SEND_RAW_DATA`:
     * firmware 1.17.1/1.18 treats the encoded path byte as a byte count in
     * its bounds check, so a one-hop two-byte path is rejected as too short
     * for its own value. `CMD_SEND_RAW_PACKET` carries a complete packet and
     * has no such ambiguity.
     */
    private val useRawPacket: Boolean
        get() = (hostPath?.encoded ?: 0) > 0x3F

    private val carrierPayloadMax: Int
        get() {
            val path = hostPath ?: RadioPath(0, ByteArray(0))
            return if (path.encoded > 0x3F) 172 - path.bytes.size else 174 - path.bytes.size
        }

    override val uplinkRecordMax: Int
        get() = minOf(172, carrierPayloadMax)

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
            refreshStatus()
            _status.value = _status.value.copy(radioState = 0)
            _state.value = MeshLinkState.Bound
            ready?.complete(Unit)
            // Learning the host's path is one command by full key, not a full
            // contact dump. The wait is bounded in connect(); if the radio has
            // no contact yet the zero-hop route is still a valid direct send to
            // the host on the same desk.
            if (hostKey.isEmpty()) {
                contactDone?.complete(Unit)
            } else {
                session?.getContact(hostKey)
            }
        }

        override fun onRawData(frame: RawDataFrame) {
            _status.value = _status.value.copy(rssiDbm = frame.rssi())
            if (!inbox.trySend(frame.payload()).isSuccess) {
                appLog("mesh radio inbox full, dropping one raw record")
            }
        }

        override fun onContact(frame: ContactFrame) {
            val c = frame.contact()
            if (hostKey.isEmpty() || !c.publicKey().contentEquals(hostKey)) return
            if (!c.hasValidPathEncoding()) {
                // 0xFF is "no known path"; the reserved hash-size code is a
                // malformed path. Neither is a route.
                hostPath = null
            } else {
                val bytes = c.activePath()
                val expected = c.pathHopCount() * c.pathHashSize()
                hostPath = if (bytes.size == expected && bytes.size <= 64) {
                    RadioPath(c.outPathLen(), bytes)
                } else {
                    null
                }
            }
            contactDone?.complete(Unit)
        }

        override fun onOtherFrame(frame: MeshcoreInbound) {
            when (frame) {
                is EndOfContactsFrame -> contactDone?.complete(Unit)
                is ErrorFrame -> contactDone?.complete(Unit)
                is DeviceInfoFrame -> {
                    val info = frame.info().toRadioDeviceInfo()
                    _deviceInfo.value = info
                    deviceInfoDone?.complete(info)
                }
                else -> Unit
            }
        }
    }

    override suspend fun connect() = mutex.withLock {
        if (_state.value == MeshLinkState.Bound) return@withLock
        if (address.isEmpty()) {
            _state.value = MeshLinkState.NeedsRadio
            throw RadioLinkException(MeshLinkState.NeedsRadio, "no radio selected")
        }
        if (!hasConnectPermission()) {
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
        hostPath = null
        _selfInfo.value = null
        _deviceInfo.value = null
        _state.value = MeshLinkState.Connecting
        val t = AndroidBleTransport(context)
        val s = MeshcoreSession("ldmux", t, listener)
        // LDMUX has no MeshCore message consumer. Draining the firmware queue
        // would spend an uncorrelated command for every queued chat message
        // while the terminal is trying to use the same answer window.
        s.setAutoDrain(false)
        transport = t
        session = s
        val gate = CompletableDeferred<Unit>()
        ready = gate
        val contact = CompletableDeferred<Unit>()
        contactDone = contact
        try {
            t.connectToDevice(device)
            withTimeout(CONNECT_TIMEOUT_MS) { gate.await() }
            // Wait for the contact dump so the first raw send does not race
            // the only other command the link sends. A radio with no contacts
            // still answers END_OF_CONTACTS; a dead command times out and the
            // zero-hop route remains usable.
            withTimeoutOrNull(CONTACT_WAIT_MS) { contact.await() }
            // A contacts answer that never arrives leaves one tracked command
            // outstanding; raw sends are refused until it clears. Wait out the
            // firmware's answer window, then treat a still-busy session as dead.
            withTimeoutOrNull(COMMAND_SETTLE_MS) { while (!s.isIdle()) delay(20) }
            if (!s.isIdle()) {
                closeTransport()
                _state.value = MeshLinkState.RadioUnavailable
                throw RadioLinkException(MeshLinkState.RadioUnavailable, "the radio did not finish its contact query")
            }
            if (closing) {
                closeTransport()
                throw RadioLinkException(MeshLinkState.Closed, "closed while connecting")
            }
        } catch (e: TimeoutCancellationException) {
            Log.d("ldmux", "radio connect timeout: " + e.message)
            closeTransport()
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, "the radio did not answer APP_START")
        } catch (e: RadioLinkException) {
            closeTransport()
            throw e
        } catch (e: Exception) {
            Log.d("ldmux", "radio connect failed: " + e)
            closeTransport()
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, e.message ?: "radio connect failed")
        }
    }

    override suspend fun send(record: ByteArray): RadioSendResult {
        if (record.isEmpty()) return RadioSendResult.TooLong
        if (record.size > uplinkRecordMax) return RadioSendResult.TooLong
        if (uplinkRecordMax < RECORD_FLOOR) {
            _state.value = MeshLinkState.NoPath
            return RadioSendResult.NoPath
        }
        val s = session ?: return RadioSendResult.Failed
        if (_state.value != MeshLinkState.Bound) return RadioSendResult.Failed
        if (budgetMode == RadioBudgetMode.Metered && remainingAirtimeMs() <= 0.0) {
            refreshStatus()
            return RadioSendResult.Budget
        }
        // The companion has one uncorrelated answer window. A tracked command
        // still outstanding means a raw send would race it.
        if (!s.isIdle()) return RadioSendResult.Failed
        // One immutable path read for this send, including the packet build.
        val path = hostPath ?: RadioPath(0, ByteArray(0))
        val rawPacket = path.encoded > 0x3F
        val answer = CompletableDeferred<RadioSendResult>()
        val callback = RawSendCallback { ok, errorCode ->
            answer.complete(if (ok) RadioSendResult.Accepted else errorFor(errorCode))
        }
        val queued = try {
            if (rawPacket) {
                s.sendRawPacket(0, rawPacketFor(path, record), callback)
            } else {
                s.sendRawData(path.encoded, path.bytes, record, callback)
            }
        } catch (e: Exception) {
            false
        }
        if (!queued) return RadioSendResult.Failed
        return try {
            val result = withTimeout(SEND_TIMEOUT_MS) { answer.await() }
            if (result == RadioSendResult.NotFound && path.encoded > 0) {
                // A stored direct path can outlive the peer. The radio has just
                // told us it cannot route there; drop it and try the neighbour
                // once, rather than blackholing the session until a reboot.
                hostPath = null
                appLog("mesh path not found; retrying zero-hop")
                return send(record)
            }
            if (result == RadioSendResult.Accepted) {
                recordAirtime(record.size + 2 + path.bytes.size + 1)
            }
            result
        } catch (e: TimeoutCancellationException) {
            // The companion protocol has no sequence number: a late OK could
            // resolve the next command. A timeout therefore ends the transport;
            // the caller's rebuild is the only safe next step.
            _state.value = MeshLinkState.RadioUnavailable
            closeTransport()
            RadioSendResult.Failed
        }
    }

    override fun setBudgetMode(mode: RadioBudgetMode) {
        budgetMode = mode
        refreshStatus()
    }

    override suspend fun applyPreset(preset: RadioPreset): Boolean {
        if (_state.value != MeshLinkState.Bound) connect()
        val s = session ?: return false
        val old = selfInfo.value
        val oldInfo = deviceInfo.value ?: readDeviceInfo()
        val oldHashMode = oldInfo?.pathHashMode
        val hashMode = (preset.pathHashSize - 1).coerceIn(0, 2)
        val tx = minOf(preset.txPowerDbm, old?.maxTxPowerDbm ?: preset.txPowerDbm)
        s.setRadioParams(
            RadioParams.of(preset.frequencyMhz, preset.bandwidthKhz, preset.spreadingFactor, preset.codingRate),
        )
        s.setTxPower(tx)
        s.setPathHashMode(hashMode)
        // Setters are tracked commands, not fire-and-forget proof. Wait for the
        // answer window to clear before the reconnect; the read-back after it is
        // the only proof that the radio took the operating point.
        withTimeoutOrNull(PRESET_COMMAND_WAIT_MS) { while (!s.isIdle()) delay(25) }
        delay(PRESET_SETTLE_MS)
        resetLink()
        connect()
        val got = selfInfo.value ?: return false
        val info = readDeviceInfo()
        val sameRadio = kotlin.math.abs(got.frequencyMhz - preset.frequencyMhz) < 0.01 &&
            kotlin.math.abs(got.bandwidthKhz - preset.bandwidthKhz) < 0.1 &&
            got.spreadingFactor == preset.spreadingFactor &&
            got.codingRate == preset.codingRate &&
            got.txPowerDbm == tx &&
            info?.pathHashMode == hashMode
        if (!sameRadio && old != null) {
            // Put the radio back on the tuple it had before the failed write.
            // Best effort, then reconnect again so the link is usable even when
            // the radio ignored one field.
            val restore = session
            restore?.setRadioParams(
                RadioParams.of(old.frequencyMhz, old.bandwidthKhz, old.spreadingFactor, old.codingRate),
            )
            restore?.setTxPower(old.txPowerDbm)
            if (oldHashMode != null) restore?.setPathHashMode(oldHashMode)
            withTimeoutOrNull(PRESET_COMMAND_WAIT_MS) { while (restore?.isIdle() == false) delay(25) }
            delay(PRESET_SETTLE_MS)
            resetLink()
            runCatching { connect() }
            appLog("mesh preset rollback was best effort; reconnect ${if (_state.value == MeshLinkState.Bound) "ok" else "failed"}")
        }
        return sameRadio
    }

    override fun close() {
        RadioLinks.close(this)
    }

    /** Tear down this transport without leaving the shared-link registry. */
    private fun resetLink() {
        closing = true
        ready?.completeExceptionally(RadioLinkException(MeshLinkState.Closed, "closed"))
        ready = null
        contactDone = null
        deviceInfoDone = null
        closeTransport()
        _state.value = MeshLinkState.Closed
    }

    /** The registry's final close. */
    internal fun closeNow() {
        resetLink()
    }

    override suspend fun readDeviceInfo(): RadioDeviceInfo? {
        val s = session ?: return _deviceInfo.value
        val gate = CompletableDeferred<RadioDeviceInfo>()
        deviceInfoDone = gate
        s.deviceQuery(1)
        return withTimeoutOrNull(DEVICE_INFO_WAIT_MS) { gate.await() } ?: _deviceInfo.value
    }

    private fun currentPreset(): RadioPreset {
        val s = selfInfo.value ?: return RadioPreset.LDMUX_PRIVATE
        return RadioPreset(
            frequencyMhz = s.frequencyMhz,
            bandwidthKhz = s.bandwidthKhz,
            spreadingFactor = s.spreadingFactor,
            codingRate = s.codingRate,
            txPowerDbm = s.txPowerDbm,
            pathHashSize = (deviceInfo.value?.pathHashMode ?: 0) + 1,
        )
    }

    private fun remainingAirtimeMs(): Double {
        val now = SystemClock.elapsedRealtime()
        if (now - airtimeWindowStartMs >= 3_600_000L) {
            airtimeWindowStartMs = now
            airtimeUsedMs = 0.0
        }
        return (RadioAirtime.BUDGET_MS_PER_HOUR - airtimeUsedMs).coerceAtLeast(0.0)
    }

    private fun recordAirtime(otaBytes: Int) {
        if (budgetMode != RadioBudgetMode.Metered) return
        remainingAirtimeMs()
        airtimeUsedMs += RadioAirtime.timeOnAirMs(currentPreset(), otaBytes)
        refreshStatus()
    }

    private fun refreshStatus() {
        val used = (airtimeUsedMs / 100.0).toInt()
        val ops = if (budgetMode == RadioBudgetMode.Open) {
            null
        } else {
            val perRecord = RadioAirtime.timeOnAirMs(currentPreset(), 172)
            (remainingAirtimeMs() / perRecord).toInt().coerceIn(0, 255)
        }
        _status.value = _status.value.copy(
            txAirtimeTenthsThisHour = used,
            opsLeftThisHour = ops,
        )
    }

    private fun rawPacketFor(path: RadioPath, record: ByteArray): ByteArray {
        val out = ByteArray(2 + path.bytes.size + record.size)
        out[0] = RAW_CUSTOM_DIRECT_HEADER
        out[1] = path.encoded.toByte()
        path.bytes.copyInto(out, 2)
        record.copyInto(out, 2 + path.bytes.size)
        return out
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

    private fun hasConnectPermission(): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            return context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED
        }
        @Suppress("DEPRECATION")
        return context.checkSelfPermission(Manifest.permission.BLUETOOTH) == PackageManager.PERMISSION_GRANTED
    }

    private fun errorFor(code: Int): RadioSendResult = when (code) {
        2 -> RadioSendResult.NotFound
        3, 4 -> RadioSendResult.Busy
        6 -> RadioSendResult.TooLong
        1 -> {
            _state.value = MeshLinkState.SessionFailed
            RadioSendResult.Failed
        }
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

    private fun DeviceInfo.toRadioDeviceInfo() = RadioDeviceInfo(
        firmwareVersion = firmwareVersion(),
        firmwareBuildDate = firmwareBuildDate(),
        manufacturer = manufacturer(),
        maxContacts = maxContacts(),
        maxGroupChannels = maxGroupChannels(),
        pathHashMode = pathHashMode(),
    )

    private companion object {
        const val CONNECT_TIMEOUT_MS = 30_000L
        const val CONTACT_WAIT_MS = 3_000L
        const val COMMAND_SETTLE_MS = 2_500L
        const val SEND_TIMEOUT_MS = 10_000L
        const val PRESET_COMMAND_WAIT_MS = 3_000L
        const val PRESET_SETTLE_MS = 750L
        const val DEVICE_INFO_WAIT_MS = 2_000L
        const val INBOX_CAPACITY = 128
        const val RECORD_FLOOR = 160
        const val RAW_CUSTOM_DIRECT_HEADER: Byte = 0x3E
    }
}

actual fun meshRadioLink(address: String, hostKey: ByteArray): RadioLink? {
    if (address.isEmpty()) return AndroidRadioLink(AndroidApp.context, address, hostKey)
    return RadioLinks.acquire(address, hostKey)
}

/**
 * One BLE link per radio address, shared by every screen that needs it.
 *
 * The companion radio accepts one GATT client. Opening Settings while the mesh
 * terminal is live used to start a second client on the same address, which
 * left the first link half-dead and the dialog stuck at "closed".
 */
private object RadioLinks {
    private class Entry(val link: AndroidRadioLink, var refs: Int)
    private val entries = mutableMapOf<String, Entry>()

    @Synchronized
    fun acquire(address: String, hostKey: ByteArray): AndroidRadioLink {
        val entry = entries[address]
        if (entry != null) {
            entry.refs++
            entry.link.updateHostKey(hostKey)
            return entry.link
        }
        val link = AndroidRadioLink(AndroidApp.context, address, hostKey)
        entries[address] = Entry(link, 1)
        return link
    }

    @Synchronized
    fun release(link: AndroidRadioLink) {
        val address = entries.entries.firstOrNull { it.value.link === link }?.key ?: return
        val entry = entries[address] ?: return
        entry.refs--
        if (entry.refs <= 0) {
            entries.remove(address)
            link.closeNow()
        }
    }

    /** A direct close also ends the shared entry, so a later acquire starts a fresh link. */
    @Synchronized
    fun close(link: AndroidRadioLink) {
        val address = entries.entries.firstOrNull { it.value.link === link }?.key
        if (address != null) entries.remove(address)
        link.closeNow()
    }
}
