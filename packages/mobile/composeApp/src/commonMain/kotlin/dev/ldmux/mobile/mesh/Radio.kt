package dev.ldmux.mobile.mesh

import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.pow
import kotlin.random.Random
import kotlin.time.Clock
import kotlin.time.TimeSource
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow

/**
 * A MeshCore radio the phone can own directly.
 *
 * The common layer knows only this seam. The Android implementation is the
 * vendored `libmeshcore-android` session; the driver and the terminal protocol
 * stay exactly where they were.
 */
interface RadioLink {
    val state: StateFlow<MeshLinkState>
    val status: StateFlow<MeshStatus?>
    val selfInfo: StateFlow<RadioSelfInfo?>
    val deviceInfo: StateFlow<RadioDeviceInfo?>
    val received: Flow<ByteArray>
    val uplinkRecordMax: Int

    /** Connect, run the companion handshake and wait until the radio is READY. */
    suspend fun connect()

    /** Send one raw-custom record. The path belongs to the link; the caller sends only the record. */
    suspend fun send(record: ByteArray): RadioSendResult

    /** Read the radio's firmware description (path-hash mode, limits). */
    suspend fun readDeviceInfo(): RadioDeviceInfo?

    /** Apply a radio preset to the attached radio. False when the write could not be verified. */
    suspend fun applyPreset(preset: RadioPreset): Boolean

    /** Meter data airtime, or leave the operator's private channel unmetered. */
    fun setBudgetMode(mode: RadioBudgetMode) {}

    fun close()

    /**
     * Drop one owner's reference. The BLE link closes when the last owner
     * releases it, so Settings can inspect the same radio the terminal is
     * already using instead of opening a second GATT client.
     */
    fun release() = close()
}

data class RadioPreset(
    val frequencyMhz: Double,
    val bandwidthKhz: Double,
    val spreadingFactor: Int,
    val codingRate: Int,
    val txPowerDbm: Int,
    /** Path-hash width in bytes: 1, 2 or 3. The firmware mode is this minus one. */
    val pathHashSize: Int = 1,
) {
    companion object {
        /** The bench preset: 910.525 MHz, 500 kHz, SF7, CR5, 22 dBm, 1-byte paths. */
        val LDMUX_PRIVATE = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 500.0,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
            pathHashSize = 1,
        )

        /**
         * The public USA preset: same frequency, 62.5 kHz, 1-byte paths.
         *
         * The path width is **the firmware's default**, not a value the feed states: the MeshCore
         * preset feed carries `path_hash_size: null` for USA (fetched 2026-10-28), which means
         * "whatever the radio was built with". It is written as 1 because that is that default,
         * and a radio whose build differs would report its own value back through `SELF_INFO`.
         */
        val USA = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 62.5,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
            pathHashSize = 1,
        )

        /** The Canadian variant defaults to 3-byte path hashes. */
        val CANADA = USA.copy(pathHashSize = 3)

        /** The older name, kept because the design doc and tests use it. */
        val USA_CANADA = USA
    }
}

/** A regional legal envelope used to choose a private tuple. */
enum class RadioRegion(val label: String) {
    UsCanada("USA / Canada"),
    EuUk("EU / UK"),
    Australia("Australia"),
    NewZealand("New Zealand"),
}

/** A named public preset, plus the region it belongs to. */
data class RadioPresetChoice(val label: String, val preset: RadioPreset)

/**
 * The public presets a region actually uses. Values mirror the MeshCore app
 * feed on 2026-11-02; the feed is a suggestion, not a legal filter.
 */
fun publicPresets(region: RadioRegion): List<RadioPresetChoice> = when (region) {
    RadioRegion.UsCanada -> listOf(
        RadioPresetChoice("USA", RadioPreset.USA),
        RadioPresetChoice("Canada (3-byte paths)", RadioPreset.CANADA),
    )
    RadioRegion.EuUk -> listOf(
        RadioPresetChoice("EU/UK narrow", RadioPreset(
            869.618, 62.5, 8, 8, 22, 1,
        )),
        RadioPresetChoice("EU/UK wide (deprecated)", RadioPreset(
            869.525, 250.0, 11, 5, 22, 1,
        )),
    )
    RadioRegion.Australia -> listOf(
        // CR7, not CR8: the feed's `coding_rate` for this row is "7" (fetched 2026-10-28). The
        // coding rate does not change whether two radios hear each other, but the airtime meter
        // is computed from it, and that is the number the app uses to gate a send.
        RadioPresetChoice("Australia narrow", RadioPreset(
            916.575, 62.5, 7, 7, 22, 1,
        )),
        RadioPresetChoice("Australia mid", RadioPreset(
            915.075, 125.0, 9, 5, 22, 1,
        )),
        RadioPresetChoice("Australia SA/WA", RadioPreset(
            923.125, 62.5, 8, 8, 22, 1,
        )),
    )
    RadioRegion.NewZealand -> listOf(
        // 2-byte paths, not 1: the feed says so for this row (fetched 2026-10-28), and the width
        // is what a node stamps on what it sends — a 1-byte stamp is a shorter trace than its
        // neighbours expect to read, and a bigger payload budget than the region allows for.
        RadioPresetChoice("New Zealand narrow", RadioPreset(
            917.375, 62.5, 7, 5, 22, 2,
        )),
    )
}

/**
 * Whether [preset] is a **public channel**: a tuple that other operators' radios also use.
 *
 * Compared on the four fields that make a channel — frequency, bandwidth, spreading factor and
 * coding rate — and deliberately not on transmit power or path-hash width, which do not change
 * who shares the frequency. A private tuple is anything else, including a public frequency on a
 * different bandwidth: 910.525 at 500 kHz is not the public USA channel at 62.5 kHz, and the two
 * do not hear each other.
 *
 * The path-hash width is excluded because it is a per-node stamping choice, not a channel.
 */
fun isPublicChannel(preset: RadioPreset): Boolean = RadioRegion.entries.any { r ->
    publicPresets(r).any {
        val p = it.preset
        p.frequencyMhz == preset.frequencyMhz &&
            p.bandwidthKhz == preset.bandwidthKhz &&
            p.spreadingFactor == preset.spreadingFactor &&
            p.codingRate == preset.codingRate
    }
}

/** The region a tuple most likely belongs to, used to open the preset dialog on the applied tuple. */
fun regionOfPreset(preset: RadioPreset): RadioRegion {
    for (r in RadioRegion.entries) {
        if (publicPresets(r).any { it.preset == preset }) return r
    }
    return when {
        preset.frequencyMhz < 880.0 -> RadioRegion.EuUk
        preset.frequencyMhz < 915.0 -> RadioRegion.UsCanada
        preset.bandwidthKhz >= 500.0 -> RadioRegion.UsCanada
        else -> RadioRegion.Australia
    }
}

/**
 * A private tuple inside the region's legal envelope. It is deliberately not
 * a public MeshCore preset: the point is a channel the operator's two radios
 * share without joining (or interfering with) the public mesh.
 *
 * - USA/Canada: 500 kHz DTS, centre kept 250 kHz from both band edges.
 * - EU/UK: 125 kHz inside the 869.4-869.65 MHz sub-band, away from the
 *   869.525 and 869.618 public centres.
 * - Australia/New Zealand: 125 kHz inside 915-928 MHz.
 */
fun privatePreset(region: RadioRegion, random: Random = Random.Default): RadioPreset = when (region) {
    RadioRegion.UsCanada -> RadioPreset(
        frequencyMhz = 902.25 + 0.5 * random.nextInt(0, 51),
        bandwidthKhz = 500.0,
        spreadingFactor = if (random.nextBoolean()) 7 else 8,
        codingRate = 5,
        txPowerDbm = 22,
        pathHashSize = 1,
    )
    RadioRegion.EuUk -> RadioPreset(
        frequencyMhz = listOf(869.475, 869.4875, 869.5625, 869.575).random(random),
        bandwidthKhz = 125.0,
        spreadingFactor = if (random.nextBoolean()) 8 else 9,
        codingRate = 5,
        txPowerDbm = 22,
        pathHashSize = 1,
    )
    RadioRegion.Australia, RadioRegion.NewZealand -> RadioPreset(
        frequencyMhz = 915.5 + 0.25 * random.nextInt(0, 49),
        bandwidthKhz = 125.0,
        spreadingFactor = if (random.nextBoolean()) 9 else 10,
        codingRate = 5,
        txPowerDbm = 22,
        pathHashSize = 1,
    )
}

/** Whether data records are metered against the 36 s/hour courtesy budget. */
enum class RadioBudgetMode { Metered, Open }

/**
 * LoRa time-on-air, used only to meter our own data records. It is an
 * estimate (preamble, explicit header, CRC on); a round trip is still never
 * inferred from it.
 */
object RadioAirtime {
    const val BUDGET_MS_PER_HOUR = 36_000.0

    fun timeOnAirMs(preset: RadioPreset, payloadBytes: Int): Double {
        val bwHz = preset.bandwidthKhz * 1000.0
        val tsymMs = (1 shl preset.spreadingFactor) / bwHz * 1000.0
        val de = if (tsymMs > 16.0) 1 else 0
        val preamble = if (preset.spreadingFactor <= 8) 32 else 16
        val cr = preset.codingRate.coerceIn(5, 8)
        val numerator = 8.0 * payloadBytes - 4.0 * preset.spreadingFactor + 28.0 + 16.0
        val denominator = 4.0 * (preset.spreadingFactor - 2 * de)
        val payloadSymbols = 8.0 + max(0.0, ceil(numerator / denominator) * cr)
        return (preamble + 4.25 + payloadSymbols) * tsymMs
    }

    fun opsPerHour(preset: RadioPreset, payloadBytes: Int = 172, budgetMs: Double = BUDGET_MS_PER_HOUR): Int =
        max(0, (budgetMs / timeOnAirMs(preset, payloadBytes)).toInt())
}

/** A scanned MeshCore companion. The address is a BLE MAC on Android. */
data class RadioDevice(val address: String, val name: String, val rssi: Int?)

/** The radio's own description, read from SELF_INFO after the handshake. */
data class RadioSelfInfo(
    val name: String,
    val frequencyMhz: Double,
    val bandwidthKhz: Double,
    val spreadingFactor: Int,
    val codingRate: Int,
    val txPowerDbm: Int,
    val maxTxPowerDbm: Int,
)

/** The radio's firmware description, read from DEVICE_INFO after the handshake. */
data class RadioDeviceInfo(
    val firmwareVersion: String,
    val firmwareBuildDate: String,
    val manufacturer: String,
    val maxContacts: Int,
    val maxGroupChannels: Int,
    /** 0 = 1-byte, 1 = 2-byte, 2 = 3-byte path hashes. */
    val pathHashMode: Int,
) {
    val pathHashSize: Int get() = (pathHashMode and 0x03) + 1
}

/** What the radio did with one raw record. Never delivery. */
enum class RadioSendResult(val outcome: MeshSendOutcome) {
    Accepted(MeshSendOutcome.SentDirect),
    Busy(MeshSendOutcome.ErrBusy),
    TooLong(MeshSendOutcome.ErrTooLong),
    NotFound(MeshSendOutcome.ErrNotFound),
    NoPath(MeshSendOutcome.ErrNotFound),
    Budget(MeshSendOutcome.ErrBudget),
    Failed(MeshSendOutcome.Unknown),
}

/** A connection failure that already has the state the screen should show. */
class RadioLinkException(val state: MeshLinkState, message: String) : Exception(message)

/**
 * The driver's `GatewayPort` over a [RadioLink].
 *
 * The gateway protocol's sequence number and `SEND_RESULT` were a loopback
 * detail. A radio answers OK/ERR for the command, so there is no meaningful
 * sequence; the ARQ above this sees one result per send and owns delivery.
 */
class MeshRadioGateway(private val link: RadioLink) : GatewayPort {
    override val state: StateFlow<MeshLinkState> = link.state
    override val status: StateFlow<MeshStatus?> = link.status
    override val received: Flow<ByteArray> = link.received
    override val uplinkRecordMax: Int get() = link.uplinkRecordMax

    override suspend fun send(record: ByteArray): MeshSendResult =
        MeshSendResult(0, link.send(record).outcome)

}

/**
 * One connection from the mesh screen: open the BLE radio, then drive the L3
 * session. A failed connect carries its own state into the driver, so the
 * screen shows "radio unavailable" rather than a generic handshake timeout.
 */
class MeshRadioConnection(
    private val link: RadioLink,
    private val creds: MeshCredentials,
    private val lib: SessionLib,
    private val selector: String,
) {
    private val origin = TimeSource.Monotonic.markNow()
    private val clock: () -> Long = { origin.elapsedNow().inWholeMilliseconds }

    /**
     * The radio this connection owns, for the two facts the screen needs about it and nothing
     * else: its own `SELF_INFO` (which channel it is really on) and its device info. The driver
     * sends through the same object; this is a read-only window onto it, not a second owner.
     */
    val radio: RadioLink get() = link

    val driver = MeshTermDriver(
        gateway = MeshRadioGateway(link),
        begin = {
            MeshSession.begin(
                lib,
                creds.keys.localPrivate,
                creds.keys.localPublic,
                creds.keys.hostPublic,
                creds.keys.psk,
                Clock.System.now().toEpochMilliseconds(),
            )
        },
        selector = selector,
        clock = clock,
    )

    private var released = false

    suspend fun run() {
        try {
            link.connect()
            driver.run()
        } catch (e: RadioLinkException) {
            driver.onGatewayState(e.state)
        }
    }

    /** Ownership release only. The link stays up across a driver rebuild. */
    fun close() {
        if (released) return
        released = true
        link.release()
    }
}

/**
 * The platform's radio link, or null when this build has no BLE support.
 *
 * Android returns [RadioLink]; a future iOS build returns null until its
 * MeshCore transport exists.
 */
expect fun meshRadioLink(address: String, hostKey: ByteArray): RadioLink?

/** A link that exists only so the common screen can compile where no radio is built. */
internal class UnsupportedRadioLink : RadioLink {
    private val _state = MutableStateFlow(MeshLinkState.NeedsRadio)
    override val state: StateFlow<MeshLinkState> = _state.asStateFlow()
    private val _status = MutableStateFlow<MeshStatus?>(null)
    override val status: StateFlow<MeshStatus?> = _status.asStateFlow()
    private val _selfInfo = MutableStateFlow<RadioSelfInfo?>(null)
    override val selfInfo: StateFlow<RadioSelfInfo?> = _selfInfo.asStateFlow()
    private val _deviceInfo = MutableStateFlow<RadioDeviceInfo?>(null)
    override val deviceInfo: StateFlow<RadioDeviceInfo?> = _deviceInfo.asStateFlow()
    override val received: Flow<ByteArray> = emptyFlow()
    // A number this link never uses: it throws on every call. Mirroring the shared constant
    // keeps it from reading like a measurement of a link that has one.
    override val uplinkRecordMax: Int = MeshRecordLimits.MAX
    override suspend fun connect() = throw RadioLinkException(MeshLinkState.NeedsRadio, "no radio platform")
    override suspend fun send(record: ByteArray): RadioSendResult = RadioSendResult.Failed
    override suspend fun readDeviceInfo(): RadioDeviceInfo? = null
    override suspend fun applyPreset(preset: RadioPreset): Boolean = false
    override fun close() {}
}
