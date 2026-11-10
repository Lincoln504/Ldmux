package io.ldmux.mobile.mesh

import java.util.OptionalInt

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
import io.iotone.meshcore.frames.DefaultFloodScopeFrame
import io.iotone.meshcore.frames.RfLogFrame
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
    private data class RadioPath(val encoded: Int, val bytes: ByteArray) {
        /**
         * How many repeaters this path passes through.
         *
         * **The encoded byte is `((width - 1) << 6) | hops`, not the other way round.** This read
         * the hops out of the *width* bits and divided by the hop count, which returned 0 for every
         * path of one to three repeaters, 2 for five, 8 for seventeen and 28 only for a 28-hop
         * chain: every route-derived figure — the session's first retransmission, the repair
         * deadline, the silence bound, the handshake ladder, every answer deadline — has been sized
         * from that wrong number since 2026-11-08 (`41d6bbc`). At one repeater it said "adjacent",
         * so the session retried at 3.6 s on a link whose round trip is seven, doubling the air on a
         * half-duplex channel and walking a working session apart: "works at home, fails at range".
         * The decoder proper lives in [RadioRoute.reversed]; this is the same arithmetic.
         */
        fun hops(): Int = RadioRoute.pathHops(encoded, bytes.size)
    }

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

    /**
     * How many repeaters the route to the host uses, or null when there is no route yet.
     *
     * The driver scales its silence bound by this: twenty repeaters is twenty transmissions each
     * way, and a bound tuned for a direct link would declare a working long-range session dead.
     */
    override val routeHops: Int?
        get() = (advertPath ?: contactPath)?.hops()

    /** Which of the two route sources the maintenance loop asks for on its next tick. */
    @Volatile
    private var advertNext = true

    /**
     * When a route to the host was last **heard on the air**, in wall-clock milliseconds; 0 means
     * never.
     *
     * **A route in hand is not a route that works.** The phone leaves home holding a zero-hop path
     * learned in the same room, carries it across the city, and sends every record down it: the
     * radio transmits, nothing answers, and no error comes back because the firmware accepts a
     * direct send it cannot deliver. The timestamp is what lets a connect say "this is old, ask
     * first" instead of trusting it.
     */
    @Volatile
    private var routeHeardAt: Long = 0L

    /** When this link last put a record on the air, so an idle path is not refreshed. */
    private var lastSendAt: Long = 0L

    /** When the last route request went out, so a failing link cannot turn into a broadcaster. */
    @Volatile
    private var lastRequestAt: Long = 0L
    /** Unanswered route requests since the last route learned, for the doubling interval. */
    private var routeAsks: Int = 0
    /** Whether a record has arrived down the route in hand, which is the only proof it works. */
    private var routeProven: Boolean = false
    /** The paths that have lost a session recently, newest last: encoded length and bytes. */
    private val failedRoutes = ArrayDeque<Pair<Int, Long>>()
    /** When this radio last answered a request from the host, so one ask cannot become a storm. */
    private var lastAnsweredAt: Long = 0L

    /**
     * The **flood scope** this radio broadcasts under, as the radio itself reports it.
     *
     * **The radio is the source of truth**, because the operator's own MeshCore app sets it there
     * (`region default`, the app's Experimental Settings from v1.43.0) and a regional firmware build
     * can ship one. A client that invented a scope would be worse than one with none: a repeater
     * drops a flood whose region it does not carry, silently, while an unscoped flood is forwarded
     * by any repeater whose `*` entry allows floods — which is the default.
     *
     * `adopted` is the fallback: a region ldmux recognised from a flood it overheard on the mesh.
     */
    @Volatile
    private var radioScope: ByteArray? = null

    @Volatile
    private var adoptedScope: ByteArray? = null
    /** The name behind [adoptedScope], so a flood can be answered under the scope it arrived on. */
    private var adoptedScopeName: String? = null
    /** The radio's own region name, from `CMD_GET_DEFAULT_FLOOD_SCOPE`; null when it has none. */
    private var radioScopeName: String? = null
    /** The width last written to the log, so one line per change rather than one per query. */
    private var lastLoggedWidth: Int = 0

    /** When an overheard scoped flood was last tested against the known region names. */
    @Volatile
    private var scopeCheckAt: Long = 0L

    /** The scope key this phone's requests are scoped with, or null for an unscoped flood. */
    private val scopeKey: ByteArray?
        get() = radioScope ?: adoptedScope

    private var scopeDone: CompletableDeferred<Unit>? = null

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
     * last owner, so: open a shell, spend the hour's allowance, go back to the list, reopen it,
     * budget full again. In 869.4-869.65 MHz the meter is a condition of use, not a courtesy, and
     * nothing else enforces it on a radio the phone owns, so a tap could reset a legal limit. It is now
     * keyed by the radio's address and stored with a wall clock (the monotonic one does not
     * survive a reboot, and neither should an hour's window).
     */
    private val airtimePrefs = context.getSharedPreferences("ldmux_radio_airtime", Context.MODE_PRIVATE)
    /**
     * Whether this radio's path width has ever been set on purpose — by us, or by the operator
     * matching the host's channel.
     *
     * **"One byte" means two different things**, and the firmware cannot tell them apart: a radio
     * that was never configured (the factory default, which collides at ≈20 co-channel
     * repeaters) and a radio deliberately put on one byte for MeshCore's public channel. The
     * upgrade to two bytes therefore happens **once per radio** and is recorded here; after that a
     * one-byte width is somebody's decision and is left alone. Without this, the upgrade ran on
     * every connect and silently undid the very setting the operator had just applied — measured
     * 2026-11-07: `applyPreset` wrote one byte, the reconnect read it back as one byte, called it
     * "never configured", asked for two, and the panel then said the radio "keeps its own path
     * width".
     */
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
            return MeshRecordLimits.MAX - path.bytes.size
        }

    /**
     * The largest record this link can carry now: the budget the path leaves, capped by what a
     * radio **hands up**.
     *
     * The second cap was missing, and it is silent: a radio does not push a frame it cannot fit in
     * its 176-byte buffer, so a 172-byte record is transmitted, received and never reported to
     * either app — on every full record, with no error anywhere.
     */
    override val uplinkRecordMax: Int
        get() = minOf(MeshRecordLimits.RECEIVED_MAX, carrierPayloadMax)

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
            _status.value = _status.value.copy(rssiDbm = frame.rssi())
            val payload = frame.payload()
            // **A route reply is not a session record, but this end no longer sends one directly.**
            // The host answers a request with a *flood* (`build_route_reply_flood`, ADR-0035) whose
            // inner record is read off the receive log below; a direct `LDRR` reply was the older
            // shape and its branch here adopted the host's belief about the direction towards us,
            // which is precisely the guess that made a range handshake fail. Removed so the only
            // route this end learns from a reply is the one the reply itself travelled.
            // **A record that arrived on the path is proof the path works.** Nothing stamped this
            // before, so `routeIsFresh` aged out on a link that was carrying traffic in both
            // directions, and the path-refresh loop then re-flooded a route request every ten
            // seconds on a working link. Arrival is stronger evidence than any timer: the host's own
            // record came down this path.
            // **Whichever source is in hand, this is proof.** Measuring freshness against the
            // advert slot alone left a route that lives in the contact fallback permanently
            // "unproven", so the maintenance loop re-flooded a request every few seconds on a link
            // that was carrying traffic in both directions.
            if (hostPath != null) {
                routeHeardAt = System.currentTimeMillis()
                // Something the host sent arrived down this path: it works in the direction that
                // matters, and no shorter candidate may displace it on hops alone.
                routeProven = true
            }
            if (!inbox.trySend(payload).isSuccess) {
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
                // **And checked against the same grave.** A session that dies on the contact path
                // used to be handed the identical path back by the next read, because only
                // `adoptRoute` consulted the failed-route memory — the loop it exists to break.
                val candidate = RadioRoute.fromContact(c.outPathLen(), c.activePath())
                    ?.let { RadioPath(it.encoded, it.bytes) }
                contactPath = candidate?.takeUnless { recentlyFailed(it) }
                if (candidate != null && contactPath == null) {
                    appLog("mesh contact route ignored: it lost a session")
                }
            }
            contactDone?.complete(Unit)
        }

        override fun onOtherFrame(frame: MeshcoreInbound) {
            when (frame) {
                is EndOfContactsFrame -> contactDone?.complete(Unit)
                // **The advert itself, as it arrived.** The radio logs every packet it hears, so
                // this is a route learned on the event rather than on the next poll: with a route
                // already in hand the poll was five minutes apart, which is why a phone that moved
                // kept sending down the old one.
                is RfLogFrame -> {
                    val raw = frame.log().raw()
                    val host = hostKey
                    if (host.size == 32) {
                        // **The flooded answer to our own request.** This is the one that arrives at
                        // distance: the host answers a request that crossed repeaters with a flood,
                        // because a direct reply is one packet along one chain and one lost hop is a
                        // phone that never learns a route.
                        RadioRoute.heardRouteReply(raw, host)?.let { adoptRoute(it, "flooded reply") }
                            ?: RadioRoute.heardAdvert(raw, host)?.let { adoptRoute(it, "advert") }
                        // **A request from the host is answered, because the host cannot learn where
                        // we are any other way.** A direct packet's path is consumed in transit, so
                        // the session records we send teach it nothing: after a daemon restart it
                        // holds no route to us, and a phone that holds a fresh one never asks. This
                        // is the one round trip that ends that standoff.
                        // Off the listener's thread: the answer is a send, which suspends.
                        pathScope?.launch { answerRequest(raw) }
                    }
                    // **Learn the mesh's scope from a packet it scoped.** A scoped flood proves the
                    // region is carried by the repeaters that forwarded it, and the region's name is
                    // never on the air — only a 16-bit code over the payload — so recognising one
                    // means recomputing what each known name would have produced. Only while this
                    // radio has no scope of its own, because that is a decision someone made.
                    val now = System.currentTimeMillis()
                    if (radioScope == null && adoptedScope == null &&
                        now - scopeCheckAt >= SCOPE_CHECK_FLOOR_MS
                    ) {
                        scopeCheckAt = now
                        RadioRoute.heardScope(raw, RadioRoute.KNOWN_SCOPE_TAGS)?.let { name ->
                            adoptedScope = RadioRoute.scopeKey(name)
                            adoptedScopeName = name
                            appLog("mesh scope $name learned from the mesh")
                        }
                    }
                }
                // **The advert-path cache's answer for the host's key**, when the cache is what
                // knows: the hops arrive in travel order, so they are reversed here (see
                // `RadioRoute`). The event above usually gets there first.
                is AdvertPathFrame -> {
                    val path = RadioRoute.reversed(frame.pathLength(), frame.path())
                    if (path != null) {
                        // The firmware stamps the entry when the advert was **heard**, in seconds.
                        // A radio whose clock was never set answers in boot-seconds, so anything
                        // that does not look like a wall clock is treated as old rather than fresh.
                        val heardAt = frame.receivedAtSeconds().toLong() * 1000L
                        val now = System.currentTimeMillis()
                        val plausible = heardAt in (now - 86_400_000L)..(now + 60_000L)
                        adoptRoute(path, "cached", if (plausible) heardAt else 0L)
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
                is DefaultFloodScopeFrame -> {
                    radioScope = frame.scopeKey()
                    radioScopeName = frame.scopeName()
                    appLog("mesh radio scope: ${frame.scopeName() ?: "none (unscoped floods)"}")
                    scopeDone?.complete(Unit)
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
        radioScope = null
        // An adopted scope belonged to the air at the last place; a new attempt starts from what
        // this radio says, and learns again if it says nothing.
        adoptedScope = null
        // **Every attempt asks.** A reconnect is the moment the route matters most, and the floor
        // that keeps a failing link from broadcasting applies between attempts of *one* connect, not
        // across a rebuild: without this a reconnect within five seconds of a request skipped the
        // ping and then waited out a reply that could not come.
        lastRequestAt = 0L
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
            // **The radio's path width is the radio's, and this end does not write it.**
            //
            // It used to raise a radio reporting mode 0 to two bytes, on the argument that 254
            // one-byte hashes collide at about twenty co-channel repeaters. That argument is real
            // and it is not ours to act on: ldmux is optimising for the **public MeshCore mesh**,
            // where one byte is the convention every client and repeater shares, and a node that
            // quietly widens itself pays a byte per hop in every record it sends for a property only
            // its neighbours see. The daemon follows the radio too, so the two ends of one channel
            // agree about the width by construction rather than by both writing it.
            //
            // An operator who wants two or three bytes sets it in their own MeshCore app, or
            // explicitly through `applyPreset`, which is now the only writer.
            //
            // **The query is still asked.** `deviceInfo` used to be filled only by the radio page,
            // so on a fresh start nothing knew the radio's width and no preset readout could say it;
            // one `DEVICE_QUERY` per connect is the price of knowing a fact about the radio.
            if (deviceInfo.value == null) readDeviceInfo()
            // **Set the radio's clock**, because that clock is what stamps every advert it hears and
            // this end reads those stamps as route ages. A companion boots at zero, so without this
            // every route would look as old as the epoch and a cache answer could never be judged.
            s.setDeviceTime(System.currentTimeMillis() / 1000)
            // Wait for its `OK`: the next command must not be written into the same answer window.
            withTimeoutOrNull(DEVICE_INFO_WAIT_MS) { while (!s.isIdle()) delay(10) }
            // **Ask the radio which region it scopes its floods to.** One tracked read, answered
            // with its own typed frame. The operator's MeshCore app is where a scope is set, so
            // this follows a decision rather than making one; a radio with none stays unscoped,
            // which is what most meshes forward.
            val scopeRead = CompletableDeferred<Unit>()
            scopeDone = scopeRead
            s.getDefaultFloodScope()
            withTimeoutOrNull(DEVICE_INFO_WAIT_MS) { scopeRead.await() }
            scopeDone = null
            // **A route before the first record, and a fresh one.** A route in hand may be an hour
            // old: the phone learned it where it used to be, nothing refuses it, and the radio
            // accepts the send it cannot deliver. So when it is stale — or absent — ask the host
            // over a flood and wait, bounded, because the first handshake record needs it. The
            // radio's advert-path cache below is the fallback, and an advert heard on the air has
            // usually answered this already.
            if (!routeIsFresh(System.currentTimeMillis())) {
                ensureRoute(s)
                withTimeoutOrNull(replyWaitMs()) {
                    while (!routeIsFresh(System.currentTimeMillis()) && !closing) delay(20)
                }
                appLog("mesh connect: route ${if (routeIsFresh(System.currentTimeMillis())) "fresh" else "unknown"}")
            }
            val key = hostKey.takeIf { it.size == 32 }
            if (key != null && s.canSendRaw()) {
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

    override suspend fun send(record: ByteArray): RadioSendResult {
        if (record.isEmpty()) return RadioSendResult.TooLong
        if (record.size > uplinkRecordMax) return RadioSendResult.TooLong
        if (uplinkRecordMax < MeshRecordLimits.FLOOR) {
            _state.value = MeshLinkState.NoPath
            return RadioSendResult.NoPath
        }
        val s = session ?: return RadioSendResult.Failed
        if (_state.value != MeshLinkState.Bound) return RadioSendResult.Failed
        // The path-refresh loop reads this to leave an idle link alone: a route is a means to send.
        lastSendAt = System.currentTimeMillis()
        if (budgetMode == RadioBudgetMode.Metered && remainingAirtimeMs() <= 0.0) {
            refreshStatus()
            return RadioSendResult.Budget
        }
        // **Only an `OK`/`ERR`-answered command can race this send.** A typed read (a contact dump,
        // an advert path, a device query) answers with its own frame, so a raw send beside it is
        // unambiguous; refusing those turned every connect into the caller's eight-second retry,
        // because `connect()` reads three of them before it sends the handshake.
        if (!s.canSendRaw()) {
            // The remaining case is a config write whose `OK` is milliseconds away. Waiting is what
            // keeps the record: the caller's own retry is a whole ladder rung later.
            withTimeoutOrNull(RAW_WINDOW_WAIT_MS) { while (!s.canSendRaw()) delay(10) }
            if (!s.canSendRaw()) return RadioSendResult.Failed
        }
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
            }
            withTimeoutOrNull(PRESET_COMMAND_WAIT_MS) { while (!s.isIdle()) delay(25) }
        }
        // **The sixth byte is the radio's own repeat flag, not a zero.** Omitting it made the
        // firmware persist `repeat = 0`, so an operator whose companion was repeating lost that on
        // the next preset write with no message. What the radio reported is what goes back.
        val channel = RadioParams(
            preset.frequencyMhz,
            preset.bandwidthKhz,
            preset.spreadingFactor,
            preset.codingRate,
            OptionalInt.of(if (oldInfo?.repeatEnabled == true) 1 else 0),
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
        if (closing) return
        // A shell is being opened and the route is old: ask over the air first, because the cache
        // reads below can only answer with an advert that already crossed the mesh.
        if (!routeIsFresh(System.currentTimeMillis())) {
            ensureRoute(s)
            withTimeoutOrNull(replyWaitMs()) {
                while (!routeIsFresh(System.currentTimeMillis()) && !closing) delay(20)
            }
        }
        if (!s.canSendRaw()) return
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

    /**
     * Keep a route, and remember **when the peer was heard** — not when the radio was asked.
     *
     * The difference is the whole stale-route rule: a cache read answers with an entry that may be
     * hours old, so stamping "now" on it makes [routeIsFresh] true for ever and the phone keeps
     * sending down a path it learned in a different city. An advert heard on the air or a reply to
     * our own request is stamped now; a cache answer carries the time the radio heard it; a stored
     * contact path carries none, because on this transport it is never fresh information.
     */
    private fun adoptRoute(path: RadioRoute.Path, source: String, heardAt: Long = System.currentTimeMillis()) {
        // **An older source must not age a newer route.** The advert-path cache answers on a timer
        // with whatever advert it holds, and a reply to our own request can be a second old: letting
        // the cache overwrite it made `routeIsFresh` false immediately, so every retry asked again
        // and spent airtime re-proving a route it already had.
        if (heardAt < routeHeardAt && advertPath != null) {
            appLog("mesh $source route ignored: older than the one in hand")
            return
        }
        // **A flood arrives by every chain that carried it, and the shortest one is the route.**
        // The same reply heard through one repeater and through four is four copies of one answer;
        // taking the last would send every record the long way round, and the long way round is
        // exactly what a link at range cannot afford. This is the same rule the host applies to its
        // own candidates (`prefer_route`), and here it stops a late copy from widening a route that
        // is already working.
        val inHand = advertPath
        // **The same answer arrives several times, and the second is not news.** One flooded reply
        // was logged four times on the device in a single reception (2026-11-09), which is the
        // radio's log reporting one packet more than once; adopting it again is harmless but it
        // logged four lines and rewrote the same value. A path already in hand is not adopted again.
        if (inHand != null && inHand.encoded == path.encoded && inHand.bytes.contentEquals(path.bytes)) {
            return
        }
        val hops = path.encoded and 0x3F
        val held = inHand?.encoded?.and(0x3F) ?: Int.MAX_VALUE
        // An unproven route is a candidate, not a route: the shorter of two candidates wins.
        // **A route is trusted for a minute, and a fresh candidate may then displace it.**
        // Sixty seconds is the window the community's own path-gating proposal settled on (fewer
        // hops wins, or the incumbent is older than a minute); it is short enough that a phone that
        // moved does not sit on a dead chain, and long enough that a reply heard twice by two chains
        // does not thrash the route.
        // **A route is proven by traffic, not by how short it is.** Shortest-wins is only safe
        // between two candidates that both work, and the ones learned from a flood have been proven
        // in *one direction only*: a 0-hop path means this radio's transmission reached the host
        // directly, which says nothing about the host reaching us. Measured 2026-11-09 at six
        // repeaters, one hop count of zero and five hops in the same second, from the same answer.
        // Keeping the shortest kept the ghost: the session then carried nothing until the silence
        // bound ended it. A candidate now displaces a route only when that route is unproven (no
        // record has arrived on it) or stale, and among unproven ones the shortest still wins.
        val stale = System.currentTimeMillis() - routeHeardAt > ROUTE_TRUST_MS
        // **A route that just lost a session is not re-adopted on the next answer.** A path heard in
        // one direction only — our transmission reaching the host while the host's does not reach us,
        // which is what a 0-hop copy next to a 5-hop copy means — is otherwise adopted again straight
        // after the failure that proved it dead, and the link loops. Remembered for ten minutes so a
        // route that has genuinely recovered is usable again.
        if (recentlyFailed(path)) {
            appLog("mesh $source route ignored: it lost a session ${hops} hop(s)")
            return
        }
        // **A proven, fresh route is not displaced by anything a flood can say.** The guard used
        // to fire only for a *longer* candidate (`hops > held`), so a shorter one — including the
        // one-hop ghost that a single answer produced beside a five-hop truth — still displaced a
        // route that was carrying traffic, and the session then went silent until the silence bound
        // ended it. Being shorter is not evidence of working; only traffic is. A route that has
        // genuinely died is cleared by `forgetRoute`, which is the one thing allowed to unseat it.
        if (routeProven && !stale) {
            appLog("mesh $source route ignored: proven, ${held} hop(s) in hand")
            return
        }
        advertPath = RadioPath(path.encoded, path.bytes)
        routeHeardAt = heardAt
        // A newly adopted route has carried nothing yet, whichever source named it.
        routeProven = false
        // A route is an answer: the next time one is needed, ask at once rather than after backoff.
        routeAsks = 0
        appLog(
            "mesh $source route: ${path.encoded and 0x3F} hop(s)" +
                if (heardAt > 0L) " · ${(System.currentTimeMillis() - heardAt) / 1000}s old" else " · age unknown",
        )
    }

    /**
     * **A route that failed the session is not a route.** The driver rebuilds the connection after a
     * dead session, and this link is reused across that rebuild, so a path kept here would be handed
     * to the next attempt as if nothing had happened: the handshake would go out along the chain that
     * just lost it, fail the same way, and rebuild again — a loop that only a refresh breaks.
     * Measured 2026-11-09 at range: two handshakes accepted and every session dying for ten minutes.
     */
    override fun forgetRoute(blame: Boolean) {
        // **Only a path that lost records is blamed for it.** A session also ends for reasons that
        // say nothing about the route — a host restart, a rekey, a record the radio refused — and
        // banning the direct path for those forced a six-hop detour in the same room: measured
        // 2026-11-10 as `flooded reply route ignored: it lost a session 0 hop(s)` three times while
        // the radios were a metre apart.
        hostPath?.takeIf { blame }?.let { path ->
            // Remember what failed, so the next answer cannot hand back the same ghost.
            failedRoutes.addLast(
                ((path.encoded and 0xFF) or (path.bytes.contentHashCode() shl 8)) to
                    System.currentTimeMillis(),
            )
            while (failedRoutes.size > FAILED_ROUTE_MEMORY) failedRoutes.removeFirst()
        }
        advertPath = null
        contactPath = null
        routeHeardAt = 0L
        routeProven = false
        // The next request goes out at once: the floor exists to stop a *failing* link broadcasting,
        // and this is the moment the broadcast is worth the most.
        lastRequestAt = 0L
        appLog("mesh route dropped after a failed session")
    }

    /** The wait for a route answer, sized by the route already in hand. */
    private fun replyWaitMs(): Long =
        ROUTE_REPLY_WAIT_MS + ROUTE_REPLY_PER_HOP_MS * ((advertPath ?: contactPath)?.hops() ?: 0)

    /** A route heard recently enough to try before asking again. */
    /**
     * Answer a route request from the host with our own path, if we have one.
     *
     * Rate-limited like the host's own answers, and silent when this radio has no route to give: a
     * reply that carries nothing is a record of air that teaches the host the same nothing.
     */
    private suspend fun answerRequest(raw: ByteArray) {
        val own = radioPublicKey ?: return
        val now = System.currentTimeMillis()
        if (now - lastAnsweredAt < REQUEST_ANSWER_FLOOR_MS) return
        val scope = radioScopeName ?: adoptedScopeName
        val request = RadioRoute.heardRouteRequest(raw, own, scope) ?: return
        val path = advertPath ?: contactPath ?: return
        val reply = RadioRoute.replyFlood(own, request, RadioRoute.Path(path.encoded, path.bytes))
            ?: return
        lastAnsweredAt = now
        val result = runCatching { send(reply) }.getOrNull()
        appLog("mesh route answered: ${result?.outcome ?: "failed"}")
    }

    /** Whether this path lost a session within the memory window. */
    private fun recentlyFailed(path: RadioRoute.Path): Boolean =
        recentlyFailed(path.encoded, path.bytes)

    /** The same question about a path already unwrapped into this link's own type. */
    private fun recentlyFailed(path: RadioPath): Boolean =
        recentlyFailed(path.encoded, path.bytes)

    private fun recentlyFailed(encoded: Int, bytes: ByteArray): Boolean {
        val now = System.currentTimeMillis()
        while (failedRoutes.isNotEmpty() && now - failedRoutes.first().second > FAILED_ROUTE_HOLD_MS) {
            failedRoutes.removeFirst()
        }
        // The bytes, not their length: two five-hop paths with different repeaters share a length
        // and must not share a grave.
        val key = (encoded and 0xFF) or (bytes.contentHashCode() shl 8)
        return failedRoutes.any { it.first == key }
    }

    /** How long to wait before asking for a route again: doubling, jittered, capped. */
    private fun requestIntervalMs(): Long {
        val step = 1L shl minOf(routeAsks, 3)
        val base = ROUTE_REQUEST_FLOOR_MS * step
        // ±25 %, from the clock rather than from `Random` so the byte stream of a test is stable.
        val spread = (base / 4).coerceAtLeast(1L)
        val jitter = (System.currentTimeMillis() % (2 * spread + 1)) - spread
        return (base + jitter).coerceAtLeast(ROUTE_REQUEST_FLOOR_MS / 2)
    }

    private fun routeIsFresh(now: Long): Boolean =
        hostPath != null && routeHeardAt > 0L && now - routeHeardAt <= ROUTE_FRESH_MS

    /**
     * Ask the host for a route, right now, over a flood.
     *
     * **This is the on-demand half of discovery, and it is what makes the link work at distance.**
     * An advert is the only other flood this radio can send, and a repeater drops an advert after
     * `flood_max_advert` hops (eight by default), so a host further away than that could never be
     * found by one. A route request is a `REQ` flood — the same payload type MeshCore's own
     * flooded path request uses — addressed to the host's key hash, and the host answers it with a
     * direct reply carrying the path it uses towards us.
     *
     * Bounded twice: by [ROUTE_REQUEST_FLOOR_MS] between requests, and by the radio's own answer
     * window. A request that cannot go out is not retried here; the caller's own loop comes back.
     */
    private suspend fun ensureRoute(s: MeshcoreSession): Boolean {
        val key = hostKey.takeIf { it.size == 32 } ?: return false
        val own = radioPublicKey ?: return false
        val now = System.currentTimeMillis()
        // **A phone that cannot hear an answer must not keep asking at a fixed rate.** Every request
        // is a flood, so a fixed ten seconds spends air on every repeater in range for as long as
        // the phone is out of contact — and several phones doing it at once walk over each other's
        // answers. Each unanswered request doubles the wait, up to forty seconds, and the jitter
        // keeps two phones that started together from asking together. A route learned anywhere
        // (`adoptRoute`) puts the first interval back.
        if (now - lastRequestAt < requestIntervalMs()) return false
        if (!s.canSendRaw()) return false
        val packet = RadioRoute.routeRequest(currentPreset().pathHashSize, key, own, scopeKey) ?: return false
        lastRequestAt = now
        routeAsks = (routeAsks + 1).coerceAtMost(8)
        val answer = CompletableDeferred<RadioSendResult>()
        val queued = runCatching {
            s.sendRawPacket(0, packet, RawSendCallback { ok, error ->
                answer.complete(if (ok) RadioSendResult.Accepted else errorFor(error))
            })
        }.getOrDefault(false)
        if (!queued) return false
        val result = withTimeoutOrNull(REQUEST_ANSWER_MS) { answer.await() }
        appLog(
            "mesh route request (${scopeKey?.let { "scoped" } ?: "unscoped"}): " +
                "${result ?: RadioSendResult.Failed}",
        )
        // A request is a broadcast like an advert, so it is charged like one.
        if (result == RadioSendResult.Accepted) recordAirtime(packet.size + 3)
        return result == RadioSendResult.Accepted
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
        // **No allowance is not zero allowance.** A band with no duty cycle has no limit to run out
        // of, so the remaining figure is infinite rather than empty — the meter's only job there is
        // the measurement it publishes.
        val allowance = RadioAirtime.budgetMsPerHour(currentPreset()) ?: return Double.POSITIVE_INFINITY
        return (allowance - airtimeUsedMs).coerceAtLeast(0.0)
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
                // **A stale route is as good as none, so it is asked for at the same pace.** This
                // used to key on "is a path present", which a phone that had connected once at home
                // always answered yes to, and then refreshed only every five minutes — so a phone
                // that moved sat on a dead path for the rest of the interval.
                // **A link with no route is never idle, and the wait is the backoff's.** The tick
                // used to be ten seconds whenever the route was stale, which overrode the doubling
                // interval inside `ensureRoute` — the backoff could not be slower than the loop. A
                // pathless link is also the one case the idle rule below must not silence: nothing
                // is being sent *because* there is nowhere to send it, and going quiet there is a
                // link that never recovers without a person pressing refresh.
                val fresh = routeIsFresh(System.currentTimeMillis())
                delay(if (fresh) PATH_REFRESH_MS else minOf(requestIntervalMs(), PATH_RETRY_MS))
                if (closing) continue
                // **A route is only worth asking for when something is about to use it.** The loop
                // used to re-flood a request every ten seconds on a link whose route had merely
                // aged past `ROUTE_FRESH_MS` while the two ends were idle — measured 2026-11-09, one
                // request plus its reply every 130 s on a link carrying nothing, which is a flood
                // and two records for a route no record needed. A send that finds no route asks for
                // one itself (`refreshRoute`), so the only case this loop covers is an idle link,
                // and on an idle link the answer is nothing.
                // **A proven, fresh route is asked about for nothing.** This used to ask whenever
                // the link was busy, so a working link flooded a request every minute — one record
                // a minute, sixty an hour, on a route that had already carried traffic in both
                // directions. Evidence beats a timer: a route that has carried a record and is not
                // stale needs no question, only the free serial reads below, which notice a change
                // without spending air.
                val now = System.currentTimeMillis()
                val needsAsking = !routeIsFresh(now) || !routeProven
                if (needsAsking && now - lastSendAt <= ROUTE_IDLE_MS) ensureRoute(s)
                if (!s.canSendRaw()) continue
                // **A radio fact that never arrived is asked for again here.** `readDeviceInfo` runs
                // once at bind, racing the route request for the one answer window the companion
                // has: when it loses, the path width, the radio's limits and the panel's readout all
                // stay unknown for the life of the connection, and nothing ever asked again. The
                // maintenance tick is idle by definition, so this is where a second attempt belongs.
                if (deviceInfo.value == null) {
                    readDeviceInfo()
                    continue
                }
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
        repeatEnabled = clientRepeat() != 0,
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
        /** How long connect() waits for the advert-path answer, which is a local cache read. */
        const val ROUTE_WAIT_MS = 2_000L
        /** Re-ask for the host's path while none is known: often enough to catch a late advert. */
        const val PATH_RETRY_MS = 10_000L
        /** Re-ask once a path is known: paths move with the radios, but slowly. */
        const val PATH_REFRESH_MS = 60_000L

        /**
         * How long a link may be quiet before its route stops being refreshed.
         *
         * A route is a means to send, so refreshing one on a link that is sending nothing is a
         * flood and a record for nobody. Longer than any burst gap, shorter than the freshness
         * window the next send checks.
         */
        const val ROUTE_IDLE_MS = 30_000L
        /** A route older than this is not trusted for a new connection. */
        const val ROUTE_FRESH_MS = 120_000L

        /**
         * How long a route is trusted over a fresher candidate with more hops.
         *
         * Not the same number as [ROUTE_FRESH_MS]: freshness decides whether to *ask*, and asking
         * costs a flood, so it is generous. Trust decides whether a candidate may displace the
         * route in hand, and sixty seconds is the window the community's path-gating proposal
         * settled on.
         */
        const val ROUTE_TRUST_MS = 60_000L

        /** The least time between two answers to the host's requests. */
        const val REQUEST_ANSWER_FLOOR_MS = 5_000L

        /** How many failed paths are remembered, and for how long one stays unusable. */
        const val FAILED_ROUTE_MEMORY = 4
        const val FAILED_ROUTE_HOLD_MS = 120_000L
        /** The least gap between two route requests: a flood every repeater in range repeats. */
        const val ROUTE_REQUEST_FLOOR_MS = 5_000L
        /** How long a route request's own `OK` may take. */
        const val REQUEST_ANSWER_MS = 2_000L
        /** How long to wait for the host's reply before connecting without it. */
        /**
         * How long to wait for an answer to our own route request, before the free cache reads.
         *
         * **A reply is one record each way, so the route decides.** Four seconds covers a neighbour;
         * on a route the request itself already needs `2 × hops × airtime` to arrive and come back,
         * so a flat four would move on while the answer was still travelling and the handshake's
         * first attempt would go out with no path at all.
         */
        const val ROUTE_REPLY_WAIT_MS = 4_000L
        const val ROUTE_REPLY_PER_HOP_MS = 2_000L
        /** How long a send waits for a config write's answer window to clear. */
        const val RAW_WINDOW_WAIT_MS = 1_500L
        /**
         * The least gap between two attempts to name the region an overheard flood was scoped to.
         *
         * Naming one means recomputing an HMAC for each known region — a few dozen — on the phone's
         * own CPU. A quiet mesh gives one candidate every few minutes, so this only bounds the cost
         * on a busy one, where nothing else would.
         */
        const val SCOPE_CHECK_FLOOR_MS = 5_000L
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
