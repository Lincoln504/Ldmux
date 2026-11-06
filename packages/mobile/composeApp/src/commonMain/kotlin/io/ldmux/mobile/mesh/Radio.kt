package io.ldmux.mobile.mesh

import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.pow
import kotlin.math.roundToLong
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

    /**
     * Why the last attempt failed, in the words the user should read, or null while it is fine.
     *
     * The link already built these sentences and threw them away: `run` carried only the state on,
     * so "Bluetooth is off", "not a Bluetooth address", "accept the Bluetooth pairing request"
     * and "the radio did not finish its contact query" all reached the screen as one of four
     * labels. A state says what kind of thing is wrong; this says which thing. It is cleared when
     * the link becomes [MeshLinkState.Bound] and at the start of every attempt.
     */
    val lastProblem: StateFlow<String?>

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
        val BENCH = RadioPreset(
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

/** One public preset, as the community table defines it. */
data class PublicPreset(
    val id: String,
    val label: String,
    /** The region whose private band it sits in, or null where ldmux offers no private one. */
    val region: RadioRegion?,
    val preset: RadioPreset,
)

/**
 * **The whole public table, ported value-for-value.**
 *
 * Source: MeshMore's `Regions.kt` (itself a port of `lib/meshcore/region_presets.dart`), the
 * community-curated set mirrored from the canonical MeshCore companion preset table. Read
 * 2026-11-05.
 *
 * ldmux used to carry its own four-region list, and it had **drifted**: Australia was
 * 915.075/125/SF9 here against 915.8/250/SF10 there, New Zealand SF7 against SF11, the EU narrow
 * row CR8 against CR5, and every row carried 22 dBm regardless of what the region allows. Two
 * radios in the same country on the two different tables do not hear each other, and nothing in
 * either app says so. A second copy of a table of legal operating points is a second copy that
 * transmits somewhere it should not, so this is a port, not a reinterpretation.
 *
 * Transmit power is part of each row rather than a constant: the EU rows are 14 dBm and the
 * Japanese 13, and a radio left at 22 in either place is operating outside its limit.
 */
val PublicPresets: List<PublicPreset> = listOf(
    PublicPreset("us_canada", "USA / Canada", RadioRegion.UsCanada, RadioPreset(910.525, 62.5, 7, 5, 22, 1)),
    PublicPreset("us_arizona", "USA \u2014 Arizona", RadioRegion.UsCanada, RadioPreset(908.205, 62.5, 10, 5, 20, 1)),
    PublicPreset("jp_arib_t108", "Japan (ARIB T108)", null, RadioPreset(923.200, 125.0, 10, 5, 13, 1)),
    PublicPreset("jp_meshcore", "Japan (MeshCore)", null, RadioPreset(920.800, 125.0, 12, 8, 13, 1)),
    PublicPreset("eu_uk_long", "EU / UK long", RadioRegion.EuUk, RadioPreset(869.525, 250.0, 11, 5, 14, 1)),
    PublicPreset("eu_uk_medium", "EU / UK medium", RadioRegion.EuUk, RadioPreset(869.525, 250.0, 10, 5, 14, 1)),
    PublicPreset("eu_uk_narrow", "EU / UK narrow", RadioRegion.EuUk, RadioPreset(869.618, 62.5, 8, 5, 14, 1)),
    PublicPreset("ch", "Switzerland", RadioRegion.EuUk, RadioPreset(869.618, 62.5, 8, 5, 14, 1)),
    PublicPreset("cz", "Czech Republic", RadioRegion.EuUk, RadioPreset(869.432, 62.5, 7, 5, 14, 1)),
    PublicPreset("pt_869", "Portugal 869", RadioRegion.EuUk, RadioPreset(869.618, 62.5, 7, 5, 14, 1)),
    PublicPreset("au_default", "Australia", RadioRegion.Australia, RadioPreset(915.800, 250.0, 10, 5, 20, 1)),
    PublicPreset("au_narrow", "Australia narrow", RadioRegion.Australia, RadioPreset(916.575, 62.5, 7, 5, 20, 1)),
    PublicPreset("au_sa_wa_qld", "Australia SA/WA/QLD", RadioRegion.Australia, RadioPreset(923.125, 62.5, 8, 5, 20, 1)),
    PublicPreset("nz_default", "New Zealand", RadioRegion.NewZealand, RadioPreset(917.375, 250.0, 11, 5, 20, 1)),
    PublicPreset("vn", "Vietnam", null, RadioPreset(920.250, 250.0, 11, 5, 20, 1)),
)

/** The public presets inside one region's private band, for the region row's own list. */
fun publicPresets(region: RadioRegion): List<PublicPreset> =
    PublicPresets.filter { it.region == region }

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
/**
 * A tuple as the radio prints it: `MHz kHz SF CR`, in that order, separated by spaces or commas.
 *
 * The order is the firmware's own (`preset  910.525 MHz / 500.0 kHz / SF7 / CR5`), because the one
 * place a user reads a tuple is a radio's own output — the host's gateway start-up line, a MeshCore
 * serial console, `info` in another app. Reordering it for the app's convenience would mean copying
 * four numbers in a different order than the screen they came from, which is how a frequency ends
 * up in the bandwidth field.
 *
 * [base] supplies the two fields that are not part of "which channel": the transmit power and the
 * path-hash width. A tuple that parses changes the channel and leaves those alone.
 *
 * Returns null for anything that is not four numbers in range. Deliberately strict: the caller is
 * about to change a radio's frequency, and "close enough" is a radio that hears nobody.
 */
fun parseRadioTuple(text: String, base: RadioPreset): RadioPreset? {
    val parts = text.split(' ', ',', '/', '\t')
        .map { it.trim() }
        // The firmware's line carries units; "910.525" and "910.525MHz" are the same number to a
        // person reading it, and only the number matters. A token that is *only* a unit ("MHz" after
        // a comma) is dropped rather than read as a number.
        .map { it.removeSuffix("MHz").removeSuffix("kHz") }
        .filter { it.isNotEmpty() }
    if (parts.size != 4) return null
    val mhz = parts[0].toDoubleOrNull() ?: return null
    val khz = parts[1].toDoubleOrNull() ?: return null
    val sf = parts[2].removePrefix("SF").removePrefix("sf").toIntOrNull() ?: return null
    val cr = parts[3].removePrefix("CR").removePrefix("cr").toIntOrNull() ?: return null
    if (mhz < 137.0 || mhz > 1020.0) return null
    if (khz < 62.5 || khz > 500.0) return null
    if (sf < 5 || sf > 12) return null
    if (cr < 5 || cr > 8) return null
    return base.copy(
        frequencyMhz = mhz,
        bandwidthKhz = khz,
        spreadingFactor = sf,
        codingRate = cr,
    )
}

/** [preset] as the radio prints it, so the custom field opens on what it would change. */
fun radioTupleText(preset: RadioPreset): String =
    "${preset.frequencyMhz} ${preset.bandwidthKhz} ${preset.spreadingFactor} ${preset.codingRate}"

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
fun privatePreset(region: RadioRegion, random: Random = Random.Default): RadioPreset {
    val env = envelopeOf(region)
    // The band edges are the *occupied* edges: a 500 kHz signal centred on 902.25 occupies
    // 902.0-902.5, so the centre never comes closer than half a bandwidth to either limit. That
    // arithmetic lives here rather than in the numbers, because it is the whole reason the
    // generated tuples are legal.
    val half = env.bandwidthKhz / 2000.0
    val steps = ((env.freqMaxMhz - half) - (env.freqMinMhz + half)) / env.freqStepMhz
    val n = steps.toInt().coerceAtLeast(0)
    val freq = if (n <= 0) {
        env.freqMinMhz + half
    } else {
        (env.freqMinMhz + half) + env.freqStepMhz * random.nextInt(0, n + 1)
    }
    return RadioPreset(
        // Rounded, not formatted. `"%.4f".format` uses the default locale, so a German, French,
        // Spanish or Portuguese phone produced "902,2500" and `toDouble()` threw — a crash in the
        // preset randomiser, on exactly the phones the EU rows exist for.
        frequencyMhz = (freq * 10_000.0).roundToLong() / 10_000.0,
        bandwidthKhz = env.bandwidthKhz,
        spreadingFactor = env.spreadingFactors.random(random),
        codingRate = env.codingRate,
        txPowerDbm = env.txPowerDbm,
        pathHashSize = 1,
    )
}

/**
 * The legal envelope of a region, as the one source the randomiser draws from.
 *
 * Every field is a limit rather than a preference, and each has a reason:
 *
 * - **USA / Canada** — 902-928 MHz is the FCC's 15.247 band; a 500 kHz channel meets the
 *   bandwidth floor the power argument depends on, and 22 dBm conducted is far under the 30 dBm
 *   the band allows. The centre stays 250 kHz from each edge because the *occupied* width is
 *   what has to fit.
 * - **EU / UK** — the non-specific SRD band is 869.4-869.65 MHz at 500 mW ERP with a **10 %
 *   duty cycle**, which is a legal requirement rather than a courtesy. Hence 14 dBm in the
 *   public rows and [requiresDutyCycle] true: the meter must not default to off there. The
 *   private centre is 125 kHz wide inside that 250 kHz window, away from the public 869.525 and
 *   869.618 centres.
 * - **Australia / New Zealand** — 915-928 MHz, 1 W EIRP, so 22 dBm is legal; 125 kHz keeps a
 *   private channel out of the public AU rows' 250 kHz.
 *
 * Power is part of the envelope, not a constant beside it: a radio left at 22 dBm in Japan or at
 * 27 in the EU is transmitting outside its limit whatever the frequency says.
 */
internal data class RadioEnvelope(
    val freqMinMhz: Double,
    val freqMaxMhz: Double,
    val freqStepMhz: Double,
    val bandwidthKhz: Double,
    val spreadingFactors: List<Int>,
    val codingRate: Int,
    val txPowerDbm: Int,
    /** The band's regulator requires a duty cycle, so the meter should not default to off. */
    val requiresDutyCycle: Boolean,
)

internal fun envelopeOf(region: RadioRegion): RadioEnvelope = when (region) {
    // 902.0-928.0 at 500 kHz, centres 902.25..927.25 in half-megahertz steps.
    RadioRegion.UsCanada -> RadioEnvelope(902.0, 928.0, 0.5, 500.0, listOf(7, 8), 5, 22, false)
    // 869.4-869.65 at 125 kHz: centres 869.4625..869.5875.
    RadioRegion.EuUk -> RadioEnvelope(869.4, 869.65, 0.0125, 125.0, listOf(8, 9), 5, 14, true)
    // 915.0-928.0 at 125 kHz.
    RadioRegion.Australia -> RadioEnvelope(915.0, 928.0, 0.25, 125.0, listOf(9, 10), 5, 22, false)
    RadioRegion.NewZealand -> RadioEnvelope(915.0, 928.0, 0.25, 125.0, listOf(9, 10), 5, 22, false)
}

/** Does the band [preset] sits in require a duty cycle by law? */
fun dutyCycleRequired(preset: RadioPreset): Boolean =
    preset.frequencyMhz in 869.4..869.65

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

/**
 * The Bluetooth permission, as the two screens that can hit it need it.
 *
 * A denial is not one state on Android 11+: the first one is a dialog the user can answer again,
 * and the second one is permanent, after which `requestPermissions` returns without drawing
 * anything. The screens used to print "permission is off, grant it, then scan again" in both
 * cases, with no control — so after a permanent denial the app kept saying that forever and there
 * was no way to grant anything from inside it. One seam, because the picker and the terminal are
 * looking at the same permission and must not disagree about what pressing the button will do.
 */
expect object RadioPermission {
    /** Whether both the scan and the connect permission are held. */
    fun granted(): Boolean

    /**
     * Whether asking again would draw a dialog. False means the system will refuse silently and
     * the only cure is the app's system settings page.
     */
    fun canAsk(): Boolean

    /** Ask for the permission, or open this app's settings page when asking cannot work. */
    fun resolve()
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
    /**
     * The radio's own battery voltage in millivolts, or null when it has not reported one.
     *
     * **Voltage, not percent.** The companion reports millivolts (`RESP_CODE_BATT_AND_STORAGE`),
     * and turning volts into a percentage needs the cell chemistry and a discharge curve this app
     * does not know — a made-up percentage is worse than a measured voltage. The host's gateway
     * does report a percentage, from the same firmware, and the app does not second-guess it.
     */
    val batteryMillivolts: Int? = null,
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
    override val problem: String? get() = link.lastProblem.value
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

    /**
     * The host said the pane has stopped changing, forwarded from the driver.
     *
     * The screen wires this to its hold, so a sync's wait ends when the answer ends instead of on
     * a local quiet timer.
     */
    var onIdle: (() -> Unit)?
        get() = driver.onIdle
        set(value) { driver.onIdle = value }

    private var released = false

    suspend fun run() {
        try {
            link.connect()
            driver.run()
        } catch (e: RadioLinkException) {
            // The link's sentence, not just its category: "Bluetooth is off" and "the radio did
            // not finish its contact query" are both `RadioUnavailable`, and they are cured by
            // two different actions.
            driver.onGatewayState(e.state, e.message)
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
    private val _lastProblem = MutableStateFlow<String?>("the mesh route is not available on this device")
    override val lastProblem: StateFlow<String?> = _lastProblem.asStateFlow()
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
