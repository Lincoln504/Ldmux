package io.ldmux.mobile.mesh

import android.annotation.SuppressLint
import android.Manifest
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import io.ldmux.mobile.AndroidApp
import io.ldmux.mobile.appLog
import io.iotone.meshcore.android.AndroidBleTransport
import io.iotone.meshcore.android.MeshcoreSession
import io.iotone.meshcore.android.RawSendCallback
import io.iotone.meshcore.android.SessionListener
import io.iotone.meshcore.android.SessionState
import io.iotone.meshcore.frames.AdvertPathFrame
import io.iotone.meshcore.frames.ContactFrame
import io.iotone.meshcore.frames.BatteryStorageFrame
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
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
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
        contactPath = null
        advertPath = null
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

    // **Unknown is not connected.** `radioState` used to start at 0, which *is* RADIO_CONNECTED, so
    // a brand-new link that had never opened a GATT client claimed its radio was up.
    private val _status = MutableStateFlow(
        MeshStatus(radioState = MeshStatus.RADIO_UNKNOWN, rssiDbm = null, txAirtimeTenthsThisHour = 0, opsLeftThisHour = null),
    )
    override val status: StateFlow<MeshStatus?> = _status.asStateFlow()

    private val _lastProblem = MutableStateFlow<String?>(null)
    override val lastProblem: StateFlow<String?> = _lastProblem.asStateFlow()

    private val _selfInfo = MutableStateFlow<RadioSelfInfo?>(null)
    override val selfInfo: StateFlow<RadioSelfInfo?> = _selfInfo.asStateFlow()

    /**
     * This radio's own public key, from `SELF_INFO`.
     *
     * The host needs it to look up a return route (see `RouteHint`), and it is the radio's identity
     * rather than a secret: every advert carries it.
     */
    @Volatile
    override var radioPublicKey: ByteArray? = null
        private set

    /** When a record last arrived from the peer, and when this radio last announced itself. */
    @Volatile
    private var lastRxAt: Long = 0L

    @Volatile
    private var lastAnnounceAt: Long = 0L

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

    /**
     * The route from the radio's contact record, and the one read off the host's advert.
     *
     * **Two sources, kept apart on purpose.** The contact's out-path is authoritative when the
     * firmware has one, but it is normally empty — a companion never asks for a path return, and an
     * advert does not teach the radio a route. Sharing one variable meant a periodic contact read
     * that answered "no path" wiped a perfectly good advert route every ten minutes, dropping the
     * link to zero-hop for the retry window. `hostPath` takes whichever of the two spoke most
     * recently, so a fresh advert can displace a record that no longer describes the world.
     */
    @Volatile
    private var contactPath: RadioPath? = null

    @Volatile
    private var advertPath: RadioPath? = null

    /** Which of the two route sources the maintenance loop asks for on its next tick. */
    @Volatile
    private var advertNext = true

    private val hostPath: RadioPath?
        get() = RadioRoute.preferHeard(contactPath, advertPath)

    private val mutex = Mutex()
    private var transport: AndroidBleTransport? = null
    private var session: MeshcoreSession? = null
    private var ready: CompletableDeferred<Unit>? = null
    private var contactDone: CompletableDeferred<Unit>? = null
    private var routeDone: CompletableDeferred<Unit>? = null
    private var deviceInfoDone: CompletableDeferred<RadioDeviceInfo>? = null

    @Volatile
    private var closing = false

    /**
     * Keeps the host's path current, because a path is learned from the air and the air changes.
     *
     * The contact query used to happen once, inside `connect()`. With both radios on one desk that
     * is enough, because the zero-hop fallback works. Behind a repeater it is not: the host's
     * advert may arrive after the connect, and a path that worked from one seat stops working
     * after a walk. This asks again while the link is up — often while no path is known, rarely
     * once one is — so a repeater-reached host becomes reachable without a reconnect.
     */
    private var pathScope: CoroutineScope? = null

    @Volatile
    private var budgetMode: RadioBudgetMode = RadioBudgetMode.Metered

    /**
     * **The hour's spend survives leaving the page.**
     *
     * This meter was per-link and in memory, and `RadioLinks.release` throws the link away at the
     * last owner, so: open a shell, spend the hour's 36 s, go back to the list, reopen it, budget
     * full again. In 869.4-869.65 MHz the meter is a condition of use, not a courtesy, and nothing
     * else enforces it on a radio the phone owns, so a tap could reset a legal limit. It is now
     * keyed by the radio's address and stored with a wall clock (the monotonic one does not
     * survive a reboot, and neither should an hour's window).
     */
    private val airtimePrefs = context.getSharedPreferences("ldmux_radio_airtime", Context.MODE_PRIVATE)
    /**
     * Whether this radio's path width has ever been set on purpose — by us, or by the operator
     * matching the host's channel.
     *
     * **"One byte" means two different things**, and the firmware cannot tell them apart: a radio
     * that was never configured (the factory default, which collides at about twenty co-channel
     * repeaters) and a radio deliberately put on one byte for MeshCore's public channel. The
     * upgrade to two bytes therefore happens **once per radio** and is recorded here; after that a
     * one-byte width is somebody's decision and is left alone. Without this, the upgrade ran on
     * every connect and silently undid the very setting the operator had just applied — measured
     * 2026-11-07: `applyPreset` wrote one byte, the reconnect read it back as one byte, called it
     * "never configured", asked for two, and the panel then said the radio "keeps its own path
     * width".
     */
    private val widthPrefs = context.getSharedPreferences("ldmux_radio_width", Context.MODE_PRIVATE)
    private val airtimeKeyStart = "window_start_$address"
    private val airtimeKeyUsed = "used_ms_$address"
    private var airtimeWindowStartMs: Long = airtimePrefs.getLong(airtimeKeyStart, 0L)
    private var airtimeUsedMs: Double = airtimePrefs.getFloat(airtimeKeyUsed, 0f).toDouble()

    /**
     * **Every send uses `CMD_SEND_RAW_PACKET` (0x41), at every path width.**
     *
     * `CMD_SEND_RAW_DATA` (0x19) writes one path-length byte that the firmware reads twice with
     * two meanings: a byte count in the bounds check (`i + path_len + 4 <= len`) and an encoded
     * value in `Packet::writePath` (`hash_count = path_len & 63`, `hash_size = (path_len >> 6) + 1`).
     * The two readings agree only at a one-byte hash. Firmware 1.18.0 lists the multi-byte fix,
     * so 0x19 was broken for 2- and 3-byte hashes on every earlier build, which includes the
     * 1.17.1 the radios here run.
     *
     * 0x41 carries a complete packet, so the path encoding is ours to write correctly and no
     * firmware reading of it can disagree with us. It costs two bytes of payload, which is the
     * whole price of removing the ambiguity instead of gating on a detected version.
     */
    private val carrierPayloadMax: Int
        get() {
            val path = hostPath ?: RadioPath(0, ByteArray(0))
            // 176-byte companion frame, minus the opcode and priority byte, minus the packet
            // header and its path-length byte, minus the path itself.
            return 172 - path.bytes.size
        }

    override val uplinkRecordMax: Int
        get() = minOf(172, carrierPayloadMax)

    private val listener = object : SessionListener {
        override fun onStateChanged(state: SessionState) {
            if (closing) return
            when (state) {
                SessionState.DISCONNECTED -> {
                    // Deliberately no `lastProblem` write for a mid-session drop: the radio simply
                    // went away, and the sentence this field carries is about a failed *attempt*.
                    // The state is the fact, and the rebuild writes the next attempt's reason.
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
            radioPublicKey = selfInfo.publicKey()
            refreshStatus()
            _status.value = _status.value.copy(radioState = MeshStatus.RADIO_CONNECTED)
            _state.value = MeshLinkState.Bound
            _lastProblem.value = null
            ready?.complete(Unit)
            // **One tracked command at a time.** The battery request is deliberately *not* sent
            // here: the companion session has one uncorrelated answer window, so an untracked
            // command written into the same window as the tracked contact query lets an
            // `ErrorFrame`/`OkFrame` meant for the battery free the contact slot early — and
            // `isIdle()` then lies during the preset write, whose only proof is the read-back.
            // `connect()` asks for the battery once the contact answer has settled.
            //
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
            lastRxAt = System.currentTimeMillis()
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
                // malformed path. Neither is a route. Said, not just dropped: an empty answer is
                // also the newest thing this source has said, and it must be able to displace an
                // older advert route the same way a path does.
                contactPath = null
            } else {
                // The firmware's own out-path, already in sending order. Validated by the same rule
                // the advert path goes through, so one malformed answer cannot become a route.
                contactPath = RadioRoute.fromContact(c.outPathLen(), c.activePath())
                    ?.let { RadioPath(it.encoded, it.bytes) }
            }
            contactDone?.complete(Unit)
        }

        override fun onOtherFrame(frame: MeshcoreInbound) {
            when (frame) {
                is EndOfContactsFrame -> contactDone?.complete(Unit)
                // **A route heard on the air.** The advert-path cache is filled by every advert the
                // radio hears; this is its answer for the host's key. The hops arrive in
                // travel order, so they are reversed into sending order here (see `RadioRoute`).
                is AdvertPathFrame -> {
                    val path = RadioRoute.reversed(frame.pathLength(), frame.path())
                    if (path != null) {
                        advertPath = RadioPath(path.encoded, path.bytes)
                        appLog("mesh advert route: ${path.encoded and 0x3F} hop(s)")
                    } else {
                        appLog("mesh advert route: malformed path, ignored")
                    }
                    routeDone?.complete(Unit)
                }
                is ErrorFrame -> {
                    // One answer window, two possible questions: an error is "no" to both, and
                    // both waiters should stop waiting rather than sit out their deadlines.
                    contactDone?.complete(Unit)
                    routeDone?.complete(Unit)
                }
                is DeviceInfoFrame -> {
                    val info = frame.info().toRadioDeviceInfo().copy(batteryMillivolts = batteryMv)
                    _deviceInfo.value = info
                    deviceInfoDone?.complete(info)
                }
                is BatteryStorageFrame -> {
                    // May arrive before the device query's reply; kept so the device info carries it
                    // when it does arrive, and applied immediately when it already has.
                    batteryMv = frame.battery().batteryMillivolts()
                    _deviceInfo.value = _deviceInfo.value?.copy(batteryMillivolts = batteryMv)
                }
                else -> Unit
            }
        }
    }

    override suspend fun connect() = mutex.withLock {
        if (_state.value == MeshLinkState.Bound) return@withLock
        // A fresh attempt starts with no failure to report; whatever happens next says its own.
        _lastProblem.value = null
        if (address.isEmpty()) {
            _lastProblem.value = "choose a LoRa radio in Settings"
            _state.value = MeshLinkState.NeedsRadio
            throw RadioLinkException(MeshLinkState.NeedsRadio, "no radio selected")
        }
        if (!hasConnectPermission()) {
            _lastProblem.value = "Bluetooth permission is off for LDMUX"
            _state.value = MeshLinkState.NeedsPermission
            throw RadioLinkException(MeshLinkState.NeedsPermission, "Bluetooth permission is not granted")
        }
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter = manager?.adapter
        if (adapter == null || !adapter.isEnabled) {
            _lastProblem.value = "Bluetooth is off"
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, "Bluetooth is off")
        }
        val device = try {
            adapter.getRemoteDevice(address)
        } catch (e: IllegalArgumentException) {
            _lastProblem.value = "not a Bluetooth address: $address"
            _state.value = MeshLinkState.NeedsRadio
            throw RadioLinkException(MeshLinkState.NeedsRadio, "not a Bluetooth address: $address")
        } catch (e: SecurityException) {
            _lastProblem.value = "Bluetooth permission is off for LDMUX"
            _state.value = MeshLinkState.NeedsPermission
            throw RadioLinkException(MeshLinkState.NeedsPermission, "Bluetooth permission is not granted")
        }

        // **A first bond takes longer than a connect.** The transport bonds inside `initialize()`
        // and can wait up to 30 s for the system pairing prompt, so a 30 s deadline measured from
        // before `connectToDevice` expires during the one moment a brand-new radio is being set
        // up - and then reports "the radio did not answer APP_START", which names the wrong thing.
        // Unbonded devices get a deadline that covers the prompt; the sentence says what to do.
        // `bondState` needs BLUETOOTH_CONNECT, which `hasConnectPermission()` above has already
        // checked (a denial threw before this line), and the read is also in `runCatching` for
        // the revocation that lands between the check and the call.
        @SuppressLint("MissingPermission")
        val bonded = runCatching { device.bondState == BluetoothDevice.BOND_BONDED }.getOrDefault(false)
        val deadline = if (bonded) CONNECT_TIMEOUT_MS else BOND_CONNECT_TIMEOUT_MS
        if (!bonded) _lastProblem.value = "accept the Bluetooth pairing request"

        closing = false
        contactPath = null
        advertPath = null
        radioPublicKey = null
        lastRxAt = 0L
        // A rebuilt link is a link the peer may have forgotten: its advert-path entry expires, and
        // the reconnect exists because something went wrong. Without this a rebuild within
        // ANNOUNCE_EVERY_MS of the last advert stayed silent for the rest of that window.
        lastAnnounceAt = 0L
        _selfInfo.value = null
        _deviceInfo.value = null
        // A voltage from the previous session must not be attached to this one's DEVICE_INFO.
        batteryMv = null
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
            withTimeout(deadline) { gate.await() }
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
                _lastProblem.value = "the radio did not finish its contact query"
                _state.value = MeshLinkState.RadioUnavailable
                throw RadioLinkException(MeshLinkState.RadioUnavailable, "the radio did not finish its contact query")
            }
            // One command, one reply, and the only way the phone learns how much charge its own
            // radio has - asked here, after the tracked contact query has settled, so it cannot
            // race the one answer window the companion has.
            if (s.isIdle()) s.requestBatteryStorage()
            // **Two bytes per path hop, unless this radio was deliberately set otherwise.** The
            // firmware ships at mode 0 (one byte), indistinguishable from "never configured": 254
            // usable values collide at ≈20 co-channel repeaters, and a collision makes two
            // repeaters forward a direct message meant for one of them. Two bytes still allows 32
            // hops, above every observed regional path length. Only mode 0 is upgraded, once per
            // bind; a radio an operator set to three bytes keeps it.
            //
            // This is the radio's *own* advert width — it forwards whatever width arrives either
            // way — and it is the same rule the daemon applies to the host's radio.
            // **Asked here, not only from the settings panel.** `deviceInfo` used to be filled by
            // the radio page, so on a fresh start this check saw nothing and the upgrade never ran.
            // One `DEVICE_QUERY` per connect is the price of knowing a fact about the radio that
            // decides how many neighbours it can name.
            val info = deviceInfo.value ?: readDeviceInfo()
            // Once per radio, and never over a width somebody chose: the flag is set by this
            // upgrade and by every deliberate write in `applyPreset`.
            val widthConfigured = widthPrefs.getBoolean(address, false)
            if (info != null && info.pathHashMode == 0 && !widthConfigured && s.isIdle()) {
                s.setPathHashMode(DEFAULT_PATH_HASH_MODE)
                withTimeoutOrNull(PATH_MODE_SETTLE_MS) { while (!s.isIdle()) delay(10) }
                widthPrefs.edit().putBoolean(address, true).apply()
                // A refusal is not a failure: `currentPreset` still reports two bytes for ldmux's
                // own sends, and the radio's advert keeps its old width.
                appLog("mesh radio was on one-byte paths; asked for two")
            }
            // **Announce now, because the host needs this before it can answer anything.** Its route
            // back to us exists only in its own radio's advert-path cache, and that cache is filled
            // by our advert arriving through the repeaters. Waiting for the maintenance loop's first
            // tick was a 45-second delay on the one connection that needs it most — the first one
            // after the phone moved.
            announceIfQuiet(s)
            withTimeoutOrNull(ANNOUNCE_SETTLE_MS) { while (!s.isIdle()) delay(10) }
            // **A route before the first record, when the radio can give one.** The contact answer
            // above may have carried an out-path; when it did not, the advert-path cache is the only
            // place a route can come from, and asking before the first send means one record goes
            // out once instead of a zero-hop attempt that only reaches earshot.
            //
            // Asked on **every** connect attempt, not only when there is no route: a route learned
            // in the last place the phone was is still a well-formed route, so nothing refuses it
            // and nothing notices it is wrong. Re-reading it per attempt is what turns "walked
            // outside, the link died" into "walked outside, the next attempt used the new path".
            val key = hostKey.takeIf { it.size == 32 }
            if (key != null && s.isIdle()) {
                val done = CompletableDeferred<Unit>()
                routeDone = done
                s.getAdvertPath(key)
                withTimeoutOrNull(ROUTE_WAIT_MS) { done.await() }
                routeDone = null
            }
            // **Leave with the answer window free.** A tracked command lives ten seconds, and a
            // connect that returned with one outstanding made the caller's route hint and its
            // handshake message both refused by the `isIdle` gate — a whole handshake rung spent on
            // a link that was ready. The wait is bounded: a radio that never answers gets the
            // transport torn down by the caller's own timeout.
            withTimeoutOrNull(ROUTE_WAIT_MS) { while (!s.isIdle()) delay(10) }
            startPathRefresh(s)
            if (closing) {
                closeTransport()
                throw RadioLinkException(MeshLinkState.Closed, "closed while connecting")
            }
        } catch (e: TimeoutCancellationException) {
            Log.d("ldmux", "radio connect timeout: " + e.message)
            closeTransport()
            _lastProblem.value = if (bonded) {
                "the radio did not answer APP_START"
            } else {
                "no answer while pairing. Accept the Bluetooth pairing request, then try again"
            }
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, _lastProblem.value!!)
        } catch (e: RadioLinkException) {
            closeTransport()
            _lastProblem.value = e.message
            throw e
        } catch (e: Exception) {
            Log.d("ldmux", "radio connect failed: " + e)
            closeTransport()
            _lastProblem.value = e.message ?: "radio connect failed"
            _state.value = MeshLinkState.RadioUnavailable
            throw RadioLinkException(MeshLinkState.RadioUnavailable, _lastProblem.value!!)
        }
    }

    /**
     * Announce this radio when the peer has gone quiet.
     *
     * **A flooded advert is the only thing that teaches the peer a route back.** Direct packets lose
     * a hash per repeater, so nothing this phone sends carries a return route; the peer reads the
     * route out of its own advert-path cache, and that cache is filled by our advert arriving through
     * the repeaters. An advert is a broadcast — a few hundred milliseconds of air — so it is spent
     * only while the peer is silent, and at most every [ANNOUNCE_EVERY_MS].
     *
     * **Called from the maintenance loop, never from `send`.** The advert is a *tracked* command
     * (the firmware answers it with an `OK`), so issuing it while a record is being written would
     * occupy the one answer window and the record's own answer would be taken for the advert's.
     * The maintenance loop runs only when the session is idle, which is the ordering this needs.
     */
    private fun announceIfQuiet(s: MeshcoreSession) {
        val now = System.currentTimeMillis()
        val quiet = lastRxAt == 0L || now - lastRxAt > ANNOUNCE_QUIET_MS
        if (!quiet || now - lastAnnounceAt < ANNOUNCE_EVERY_MS) return
        lastAnnounceAt = now
        runCatching { s.sendSelfAdvert(true) }
            // **An advert is airtime, and this meter is the one that gates sends.** The firmware's
            // own figure (what the settings page draws) already includes it; this counter is the
            // phone's own allowance, and leaving the broadcasts out of it spent the budget on
            // announces the gate never saw. Charged at the full record size: an advert is smaller
            // than that at every tuple, so this errs towards spending less, never more.
            .onSuccess { recordAirtime(MeshRecordLimits.MAX) }
            .onFailure { appLog("mesh advert refused: ${it.message}") }
    }

    override suspend fun send(record: ByteArray): RadioSendResult {
        if (record.isEmpty()) return RadioSendResult.TooLong
        if (record.size > uplinkRecordMax) return RadioSendResult.TooLong
        if (uplinkRecordMax < MeshRecordLimits.FLOOR) {
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
        val answer = CompletableDeferred<RadioSendResult>()
        val callback = RawSendCallback { ok, errorCode ->
            answer.complete(if (ok) RadioSendResult.Accepted else errorFor(errorCode))
        }
        val queued = try {
            s.sendRawPacket(0, rawPacketFor(path, record), callback)
        } catch (e: Exception) {
            false
        }
        if (!queued) return RadioSendResult.Failed
        return try {
            val result = withTimeout(SEND_TIMEOUT_MS) { answer.await() }
            if (result == RadioSendResult.NotFound && path.encoded > 0) {
                // A stored direct path can outlive the peer. The radio has just told us it cannot
                // route there; drop it and try the neighbour once, rather than blackholing the
                // session until a reboot.
                //
                // **The retry comes before the fresh lookup.** Asking for the route first is a
                // tracked command, the tracked command holds the one answer window, and the retry's
                // own `isIdle` gate then refuses the record — so the "retry zero-hop" this block
                // promises was never sent, and the ARQ spent an attempt on a record that never
                // reached the air. `startPathRefresh` asks for the route on its own cadence, and the
                // zero-hop retry reaches the peer whenever it is in earshot.
                contactPath = null
                advertPath = null
                appLog("mesh path not found; retrying zero-hop")
                val retried = send(record)
                // **Tell the radio too, not just this process — and only once the retry has left.**
                // Its contact record still holds the path that just failed, so the next contact read
                // would hand the same dead route straight back. `CMD_RESET_PATH` is the firmware's
                // own verb for this (`MyMesh.cpp:1277`): it sets `out_path_len` to unknown, so the
                // next route comes from an advert instead of from a record that is only getting
                // older. Sent after the retry because it is a *tracked* command: issued first it
                // would hold the one answer window, and the retry's own `isIdle` gate would refuse
                // the record — the "retrying zero-hop" line above would then be a lie.
                //
                // It only fires if the radio ever answers a raw send with `NOT_FOUND`, which
                // firmware 1.17 does not (`MyMesh.cpp:2003-2016` answers OK, ILLEGAL_ARG or
                // TABLE_FULL), so this is the defensive half: the send above has already done the
                // work, and the reset keeps the radio's own cache from disagreeing with it.
                if (retried != RadioSendResult.Failed) runCatching { s.resetPath(hostKey) }
                return retried
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
        // **The width is asked for first, and refused on its own.** A radio can take a channel and
        // refuse its hash width: measured 2026-11-07 on a MeshCore-WIOld, which takes
        // 910.525/62.5/SF7/CR5 and keeps two-byte paths when one is written. Judged together, that
        // refusal failed the whole apply and rolled the *channel* back too, so joining the host's
        // channel looked impossible. Asked separately, a refusal costs the width and nothing else,
        // and the app says which half it kept.
        val widthWanted = oldHashMode != null && oldHashMode != hashMode
        // **A setter that cannot queue says so.** The client drops a frame when the session is not
        // READY (and when its tracked queue is full), and it used to drop it silently: the read-back
        // then looked like a radio refusing a value it was never sent. Each call reports whether it
        // was queued, and the first refusal is named.
        var refused: String? = null
        // **One line per apply, so a refused field is readable from the phone.** Three separate
        // reasons can leave a channel unchanged — the radio refused, our frame was never queued, or
        // we did not ask for the field at all — and from a read-back they look identical.
        appLog(
            "preset: want ${preset.frequencyMhz}/${preset.bandwidthKhz}/SF${preset.spreadingFactor}" +
                "/CR${preset.codingRate} path ${preset.pathHashSize}B · radio " +
                "${old?.frequencyMhz}/${old?.bandwidthKhz}/SF${old?.spreadingFactor}/CR${old?.codingRate}" +
                " mode ${oldHashMode ?: -1} · want mode $hashMode · width write ${widthWanted}",
        )
        if (widthWanted) {
            if (!s.setPathHashMode(hashMode)) {
                refused = "path width"
            } else {
                // **Recorded before the reconnect**, which re-reads the radio: a one-byte width
                // written here must not be mistaken for the factory default and upgraded back.
                widthPrefs.edit().putBoolean(address, true).apply()
            }
            withTimeoutOrNull(PRESET_COMMAND_WAIT_MS) { while (!s.isIdle()) delay(25) }
        }
        val channel = RadioParams.of(
            preset.frequencyMhz,
            preset.bandwidthKhz,
            preset.spreadingFactor,
            preset.codingRate,
        )
        if (!s.setRadioParams(channel) && refused == null) refused = "channel"
        if (!s.setTxPower(tx) && refused == null) refused = "power"
        if (refused != null) {
            _lastProblem.value = "radio did not accept the change ($refused) · try again"
        }
        // Setters are tracked commands, not fire-and-forget proof. Wait for the
        // answer window to clear before the reconnect; the read-back after it is
        // the only proof that the radio took the operating point.
        withTimeoutOrNull(PRESET_COMMAND_WAIT_MS) { while (!s.isIdle()) delay(25) }
        delay(PRESET_SETTLE_MS)
        resetLink()
        connect()
        val got = selfInfo.value
        if (got == null) {
            // **The setters were queued and the radio never answered afterwards.** That is an
            // unknown, not a refusal: the change may well have landed. Say which, because a bare
            // "not confirmed" left the operator with no way to tell a lost read-back from a radio
            // that ignored the write (the `apply` press reads in this order: write, reset, read).
            _lastProblem.value = "no answer after the change · reopen this panel to read the radio"
            return false
        }
        // Null means the radio never answered the read-back: an unknown, not a disagreement.
        val info = readDeviceInfo()
        if (info == null) {
            _lastProblem.value = "radio did not answer the read-back · applied anyway"
        } else {
            _lastProblem.value = null
        }
        val widthTaken = !widthWanted || info?.pathHashMode == hashMode
        if (widthWanted && !widthTaken) {
            // Said once, in the row that keeps it: the channel is the host's, the width is this
            // radio's own, and a repeater in the path is what makes the difference matter.
            _lastProblem.value = "channel taken · this radio keeps its own path width"
        }
        val sameRadio = kotlin.math.abs(got.frequencyMhz - preset.frequencyMhz) < 0.01 &&
            kotlin.math.abs(got.bandwidthKhz - preset.bandwidthKhz) < 0.1 &&
            got.spreadingFactor == preset.spreadingFactor &&
            got.codingRate == preset.codingRate &&
            got.txPowerDbm == tx
        if (!sameRadio && info != null && old != null) {
            // Put the radio back on the tuple it had before the failed write.
            // Best effort, then reconnect again so the link is usable even when
            // the radio ignored one field.
            val restore = session
            restore?.setRadioParams(
                RadioParams.of(old.frequencyMhz, old.bandwidthKhz, old.spreadingFactor, old.codingRate),
            )
            restore?.setTxPower(old.txPowerDbm)
            // The width is not part of the rollback: a refusal of it is not a failure of the
            // channel, and re-writing it here would only refuse again.
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

    /**
     * Read the route to the host now, because a shell is being opened.
     *
     * **Both sources, once each, and never while the answer window is busy.** The advert cache is
     * what a companion can actually read (a contact's out-path is usually empty because a companion
     * never asks for a path return), so the advert is asked first; the contact read follows because
     * on firmwares that do store an out-path it is the authoritative one. `hostPath` prefers
     * whichever arrives freshest, so one failing source cannot erase the other.
     *
     * Silence on failure, by design: opening a shell must not be refused because the radio was
     * briefly busy, and a route that cannot be read now is still the one in hand.
     */
    override suspend fun refreshRoute() {
        val s = session ?: return
        val key = hostKey.takeIf { it.size == 32 } ?: return
        if (closing || !s.isIdle()) return
        val done = CompletableDeferred<Unit>()
        routeDone = done
        runCatching { s.getAdvertPath(key) }
        withTimeoutOrNull(ROUTE_WAIT_MS) { done.await() }
        routeDone = null
        withTimeoutOrNull(ROUTE_WAIT_MS) { while (!s.isIdle()) delay(10) }
        if (closing || !s.isIdle()) return
        runCatching { s.getContact(hostKey) }
    }

    override suspend fun readDeviceInfo(): RadioDeviceInfo? {
        val s = session ?: return _deviceInfo.value
        val gate = CompletableDeferred<RadioDeviceInfo>()
        deviceInfoDone = gate
        // The argument IS the protocol version this client claims to understand, and the
        // firmware latches it: `app_target_ver = cmd_frame[1]` (MyMesh.cpp:1028-1031). Both
        // message queues then choose their frame by it, emitting the V3 forms only at
        // `>= 3` (`MyMesh.cpp:435` for contacts, `:548` for channels), and the V3 forms are
        // the only ones carrying the SNR. Sending 1 asked for the legacy frames the vendored
        // decoder still supports, so nothing broke: the SNR simply never arrived, and the V3
        // branch was unreachable. 3 is what the rest of this client already assumes.
        s.deviceQuery(3)
        // **A read-back that timed out is not an answer.** It used to fall back to the cached
        // value, so a slow firmware looked like a refusal: `applyPreset` compared the new width
        // against a stale one, called it "the radio did not take the tuple", and rolled back a
        // channel the radio had in fact accepted (measured 2026-11-07). One retry, then null, and
        // the caller says the radio did not answer instead of pretending to know.
        withTimeoutOrNull(DEVICE_INFO_WAIT_MS) { gate.await() }?.let { return it }
        if (session == null) return _deviceInfo.value
        val again = CompletableDeferred<RadioDeviceInfo>()
        deviceInfoDone = again
        s.deviceQuery(3)
        return withTimeoutOrNull(DEVICE_INFO_WAIT_MS) { again.await() }
    }

    /**
     * The path-hash width ldmux puts a radio on when it has never been configured: two bytes.
     *
     * One byte is the firmware's factory default and the legacy width, and the 2026 MeshCore
     * guidance is explicit that new deployments should not use it. Two bytes is still 32 hops.
     */
    private val DEFAULT_PATH_HASH_MODE = 1

    private fun currentPreset(): RadioPreset {
        val s = selfInfo.value ?: return RadioPreset.BENCH
        return RadioPreset(
            frequencyMhz = s.frequencyMhz,
            bandwidthKhz = s.bandwidthKhz,
            spreadingFactor = s.spreadingFactor,
            codingRate = s.codingRate,
            txPowerDbm = s.txPowerDbm,
            // **Clamped on the way in, not only on the way out.** The firmware stores
            // `path_hash_mode` as an unvalidated byte, and mode 3 decodes to a four-byte hash,
            // which `isValidPathLen` rejects outright. A radio left in that state would make every
            // send fail with "bad hash size" — including zero-hop ones, which need no width at all —
            // so a misconfigured radio would read as a dead link instead of a wide one.
            pathHashSize = ((deviceInfo.value?.pathHashMode ?: 0) + 1).coerceIn(1, 3),
        )
    }

    private fun remainingAirtimeMs(): Double {
        // Wall clock, not `elapsedRealtime`: the spend is persisted per radio address, and a
        // monotonic origin restarts at boot, which would make a stored window look ancient (or
        // future) for reasons that have nothing to do with the hour it measures.
        val now = System.currentTimeMillis()
        if (airtimeWindowStartMs <= 0L || now - airtimeWindowStartMs >= 3_600_000L || now < airtimeWindowStartMs) {
            airtimeWindowStartMs = now
            airtimeUsedMs = 0.0
            saveAirtime()
        }
        return (RadioAirtime.BUDGET_MS_PER_HOUR - airtimeUsedMs).coerceAtLeast(0.0)
    }

    private fun recordAirtime(otaBytes: Int) {
        if (budgetMode != RadioBudgetMode.Metered) return
        remainingAirtimeMs()
        airtimeUsedMs += RadioAirtime.timeOnAirMs(currentPreset(), otaBytes)
        saveAirtime()
        refreshStatus()
    }

    /**
     * The hour's spend, written where the next link for this radio finds it.
     *
     * `apply()`, because `recordAirtime` runs on the caller's dispatcher and the main thread must
     * not wait on disk for a metering write. Losing the last second to a process kill would cost a
     * little budget, never the limit: the window start is written too, so a reopened page cannot
     * read a fresh hour out of an old spend.
     */
    private fun saveAirtime() {
        airtimePrefs.edit()
            .putLong(airtimeKeyStart, airtimeWindowStartMs)
            .putFloat(airtimeKeyUsed, airtimeUsedMs.toFloat())
            .apply()
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

    /**
     * Keep the route to the host current, from the two sources the radio offers.
     *
     * **The contact when it has one, the advert path when it does not.** A contact's out-path is
     * authoritative — it comes from a path return and the firmware has already reversed it — and it
     * is normally empty, because a companion never asks for a path return and an advert does not
     * teach the radio a route (`BaseChatMesh::onAdvertRecv` never touches `out_path`). The advert
     * path is the route the host's last advert took to arrive here, reversed into sending order; it
     * needs no contact on either radio and no decrypted exchange, which is what makes it work on
     * firmware nobody reconfigured.
     *
     * **Both queries are local commands**, so a refresh costs no airtime at all. What it does cost
     * is the one answer window the companion has, so it asks one question per tick and never while
     * another tracked command is outstanding. A refresh is not wasted work even with a route in
     * hand: the phone moves, and a route is only true while the repeaters that carried it are still
     * there.
     */
    private fun startPathRefresh(s: MeshcoreSession) {
        pathScope?.cancel()
        if (hostKey.isEmpty()) return
        val key = hostKey.takeIf { it.size == 32 }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
        pathScope = scope
        scope.launch {
            while (isActive && !closing) {
                // **Half the documented interval, because the two sources alternate.** Each tick
                // reads one of them, so a full interval per tick would read each source every *two*
                // intervals — 90 s and 20 minutes where the docs say 45 s and 10. A tick is a timer
                // wakeup and a serial command: no airtime at all.
                val wait = if (advertPath == null) PATH_RETRY_MS / 2 else PATH_REFRESH_MS / 2
                delay(wait)
                if (closing || !s.isIdle()) continue
                // **Announce from here, not from a send.** A flooded advert is what teaches the
                // peer its route back to us; it is a tracked command, so it may only be issued when
                // the answer window is free. This loop is the one place that is always true.
                announceIfQuiet(s)
                // The advert is a tracked command, so it holds the answer window for a moment. Wait
                // it out rather than skipping the tick: skipping meant an announce cost a whole
                // interval and the route read was pushed past the case it exists for.
                withTimeoutOrNull(ANNOUNCE_SETTLE_MS) { while (!s.isIdle()) delay(10) }
                if (closing || !s.isIdle()) continue
                // One command per tick: the companion has one answer window, and a second request
                // inside it makes both answers ambiguous.
                //
                // **Both sources are read, alternately.** The contact's out-path is preferred when
                // the firmware has one, but a contact learned where the phone used to be does not
                // expire, so refreshing only the contact froze the advert route at whatever it was
                // when the contact first appeared. Alternating means neither source can starve the
                // other, and one tick is still one command.
                if (key != null) {
                    if (advertNext) runCatching { s.getAdvertPath(key) }
                    else runCatching { s.getContact(hostKey) }
                    advertNext = !advertNext
                }
            }
        }
    }

    private fun closeTransport() {
        pathScope?.cancel()
        pathScope = null
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

    /**
     * The firmware's own error codes, each named for what it means.
     *
     * 1 unsupported, 2 not found, 3 table full, 4 bad state, 5 file I/O, 6 illegal argument.
     * The old mapping folded 4 into "busy", so a radio in the wrong state reported backpressure,
     * and 6 into "too long", so a refused argument reported a size. The code is logged either
     * way, because the outcome is a category and the number is the fact.
     */
    private fun errorFor(code: Int): RadioSendResult {
        appLog("radio refused a raw send: err $code")
        return when (code) {
            2 -> RadioSendResult.NotFound
            3 -> RadioSendResult.Busy
            4 -> RadioSendResult.Failed
            5 -> RadioSendResult.Failed
            // 6 is ERR_CODE_ILLEGAL_ARG: the firmware could not parse the packet we built. On 0x41
            // that is the only non-table-full failure, so it means a bad packet, never "too long" —
            // a record over the limit is caught before it is handed to the radio. Reporting it as
            // TooLong hid the one failure a developer needs to see behind a "no path" message.
            6 -> RadioSendResult.Failed
            1 -> {
                _state.value = MeshLinkState.SessionFailed
                RadioSendResult.Failed
            }
            else -> RadioSendResult.Failed
        }
    }

    /** The last battery voltage the radio reported, carried into [RadioDeviceInfo] as it arrives. */
    private var batteryMv: Int? = null

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
        /**
         * A connect that includes a first bond.
         *
         * The transport's own bond wait is 300 polls of 100 ms - 30 s - and that is before
         * service discovery and the companion handshake. A deadline that expires during the
         * prompt is the worst possible first-run message: the user did what was asked and the
         * app said the radio was silent.
         */
        const val BOND_CONNECT_TIMEOUT_MS = 75_000L
        const val CONTACT_WAIT_MS = 3_000L

        /// How long the one-off path-mode write waits for its `OK` before carrying on.
        ///
        /// It is a preferences write on the radio, so it answers at once; waiting longer only
        /// delays the first record of a fresh link.
        const val PATH_MODE_SETTLE_MS = 1_500L
        /** Nothing heard from the peer for this long means the route back may be stale or unknown. */
        const val ANNOUNCE_QUIET_MS = 120_000L
        /** At most one advert per this interval, because each one is a broadcast on a shared band. */
        const val ANNOUNCE_EVERY_MS = 180_000L
        /** How long connect() waits for the advert-path answer, which is a local cache read. */
        const val ROUTE_WAIT_MS = 2_000L
        /** How long an advert's own `OK` is waited for, so the next command is not refused. */
        const val ANNOUNCE_SETTLE_MS = 300L
        /** Re-ask for the host's path while none is known: often enough to catch a late advert. */
        const val PATH_RETRY_MS = 45_000L
        /** Re-ask once a path is known: paths move with the radios, but slowly. */
        const val PATH_REFRESH_MS = 600_000L
        const val COMMAND_SETTLE_MS = 2_500L
        const val SEND_TIMEOUT_MS = 10_000L
        const val PRESET_COMMAND_WAIT_MS = 3_000L
        const val PRESET_SETTLE_MS = 750L
        const val DEVICE_INFO_WAIT_MS = 2_000L
        const val INBOX_CAPACITY = 128
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
        val address = entries.entries.firstOrNull { it.value.link === link }?.key
        if (address == null) {
            // **A link outside the registry still has to close.** An empty radio address means
            // "no radio yet", and that link is constructed directly rather than acquired, so
            // returning here left its transport open for the life of the process.
            link.closeNow()
            return
        }
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
