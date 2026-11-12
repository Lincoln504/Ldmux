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

data class RadioPreset(
    val frequencyMhz: Double,
    val bandwidthKhz: Double,
    val spreadingFactor: Int,
    val codingRate: Int,
    val txPowerDbm: Int,
    /**
     * Path-hash width in bytes: 1, 2 or 3. The firmware mode is this minus one.
     *
     * **Two is the default**, not the firmware's one. A one-byte hash has 254 usable values and
     * collides at ≈20 co-channel repeaters (a coin flip); two bytes have 65 536 and hold to a
     * few hundred. The cost is the hop ceiling, 32 instead of 63, which is above every regional path
     * length measured — and a collision is not cosmetic: two repeaters sharing a prefix both forward
     * a direct message meant for one of them.
     */
    val pathHashSize: Int = 2,
) {
    companion object {
        /** The bench preset: 910.525 MHz, 500 kHz, SF7, CR5, 22 dBm, two-byte paths. */
        val BENCH = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 500.0,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
            pathHashSize = 2,
        )

        /**
         * The public USA preset: same frequency, 62.5 kHz, two-byte paths.
         *
         * The path width is **not stated by the feed**: the MeshCore preset feed carries
         * `path_hash_size: null` for USA (fetched 2026-10-28), which means "whatever the radio was
         * built with" — and that is one byte, which the project now departs from deliberately. The
         * app writes the wider width at bind when the radio reports the legacy value, so this field
         * is the width ldmux uses, and a radio whose build differs still reports its own value back
         * through `SELF_INFO`.
         */
        val USA = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 62.5,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
            pathHashSize = 2,
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
    /** The row exactly as the community feed carries it, for comparison with every other client. */
    val preset: RadioPreset,
    /**
     * The path-hash width ldmux **writes** when this row is applied; the row's own unless stated.
     *
     * **The table is a port and the policy is ours; keeping them apart is what lets both be
     * checked.** The USA/Canada feed value is `null` — "whatever the radio was built with", which is
     * one byte — and one byte has 254 usable hashes and reaches a fifty-percent chance of a prefix
     * collision at about twenty co-channel repeaters, where two bytes need about three hundred
     * (a Canadian survey observed 237 colliding one-byte slices). Two bytes cost about a sixth more
     * airtime on a measured 9.25-hop network and cap the path at 32 hops instead of 63, which is
     * still above every path length measured here (20-29). Canada's row is three bytes because the
     * Canadian community asks for it. Nothing here rewrites a radio that is already running, and a
     * mesh whose repeaters are older than firmware 1.14 needs one byte, because those drop
     * multi-byte packets unheard.
     */
    val appliedPathHashSize: Int = preset.pathHashSize,
) {
    /** The row as ldmux applies it: the feed's four numbers and channels, at ldmux's width. */
    val applied: RadioPreset get() = preset.copy(pathHashSize = appliedPathHashSize)
}

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
    // **The row is the feed's; the width written is ldmux's** (see `appliedPathHashSize`). The feed
    // carries `path_hash_size: null` here, which the port fills in with the firmware default of one
    // byte, and that is what every other client applies.
    PublicPreset(
        "us_canada",
        "USA / Canada",
        RadioRegion.UsCanada,
        RadioPreset(910.525, 62.5, 7, 5, 22, 1),
        appliedPathHashSize = 2,
    ),
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
        // **Either form counts.** The radio reports the width it actually holds, which is the
        // applied one; the feed's row is what another client would hold.
        if (publicPresets(r).any { it.preset == preset || it.applied == preset }) return r
    }
    return when {
        preset.frequencyMhz < 880.0 -> RadioRegion.EuUk
        preset.frequencyMhz < 915.0 -> RadioRegion.UsCanada
        preset.bandwidthKhz >= 500.0 -> RadioRegion.UsCanada
        else -> RadioRegion.Australia
    }
}

/**
 * A private tuple inside the region's legal envelope. It is deliberately not a public MeshCore
 * preset: the point is a channel the operator's two radios share without joining (or interfering
 * with) the public mesh.
 *
 * - USA/Canada: 500 kHz DTS, centre kept 250 kHz from both band edges.
 * - EU/UK: 125 kHz inside the 869.4-869.65 MHz sub-band, away from the
 *   869.525 and 869.618 public centres.
 * - Australia/New Zealand: 125 kHz inside 915-928 MHz.
 *
 * **Only the frequency is drawn.** The width, coding rate and power come from the region, and the
 * spreading factor is the envelope's fastest, because those three are not "which channel" decisions:
 *
 *  - **Frequency is the only truly orthogonal dimension.** A different centre frequency sits in a
 *    different slice of the band; a different spreading factor does not separate two transmitters.
 *    Spreading factors are only *quasi*-orthogonal — measured rejection between different SFs is
 *    about −8 to −25 dB depending on the pair and is worst for a low-SF victim, so changing SF does
 *    not remove the other network, it changes which of you loses.
 *  - **SF is the range and airtime lever.** Each step buys ≈2.5 dB of sensitivity for *twice*
 *    the airtime and twice the energy, and its symbol time decides whether low-data-rate
 *    optimisation is mandatory. Drawing it at random makes a link whose range and speed change on
 *    every press, for no gain the operator can see.
 *  - **Every drawn value has to be matched by hand on the other radio.** Frequency, bandwidth and
 *    spreading factor must be identical at both ends (coding rate and power may differ), so each
 *    one the button randomises is one more number to get right, with a silent failure when it is
 *    wrong: a receiver on another SF simply does not hear the packet.
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
        // The envelope's fastest SF, not a drawn one: see the note above. The preset editor lets
        // the operator raise it when the link is longer than the arithmetic expects.
        spreadingFactor = env.spreadingFactors.first(),
        codingRate = env.codingRate,
        txPowerDbm = env.txPowerDbm,
        // The same default as every other preset; a randomiser should not put a node on the
        // legacy width either.
        pathHashSize = 2,
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

/** Whether data records are metered against the band's hourly allowance. */
enum class RadioBudgetMode { Metered, Open }

/**
 * LoRa time-on-air, used only to meter our own data records. It is an
 * estimate (preamble, explicit header, CRC on); a round trip is still never
 * inferred from it.
 */
object RadioAirtime {
    /**
     * The allowance inside `869.4-869.65 MHz`: one percent of an hour.
     *
     * The band's own rule is 10 %, and ldmux holds itself to a tenth of it because that window is
     * shared with alarms.
     */
    const val BUDGET_MS_PER_HOUR = 36_000.0

    /**
     * **The allowance is the law, or nothing at all.**
     *
     * The European sub-bands carry duty cycles; everything else — including 902-928 MHz, where
     * MeshCore's US/Canada preset sits at 910.525 MHz — has none for this kind of transmitter, so
     * there is **no limit to enforce**. A "ten percent courtesy cap" used to exist here, and it was
     * wrong twice over: it stopped a US session at a fifth of what the firmware permits
     * (`getAirtimeBudgetFactor()` returns 1.0) while reading as a legal rule about a band that has
     * no such rule.
     *
     * `null` is therefore not "no data" — it is "this band has no duty cycle", and the meter is a
     * measurement rather than a gate.
     */
    fun budgetMsPerHour(preset: RadioPreset): Double? = when {
        preset.frequencyMhz in 868.0..868.6 -> BUDGET_MS_PER_HOUR
        preset.frequencyMhz in 869.4..869.65 -> BUDGET_MS_PER_HOUR
        // A tenth of one percent: the alarm band, where one 62.5 kHz record is most of the hour.
        preset.frequencyMhz in 868.7..869.2 -> 3_600.0
        else -> null
    }

    fun timeOnAirMs(preset: RadioPreset, payloadBytes: Int): Double {
        val bwHz = preset.bandwidthKhz * 1000.0
        val tsymMs = (1 shl preset.spreadingFactor) / bwHz * 1000.0
        // RadioLib turns low-data-rate optimisation on at 16 ms per symbol (`>=`), not above it.
        val de = if (tsymMs >= 16.0) 1 else 0
        val preamble = if (preset.spreadingFactor <= 8) 32 else 16
        val cr = preset.codingRate.coerceIn(5, 8)
        val numerator = 8.0 * payloadBytes - 4.0 * preset.spreadingFactor + 28.0 + 16.0
        val denominator = 4.0 * (preset.spreadingFactor - 2 * de)
        val payloadSymbols = 8.0 + max(0.0, ceil(numerator / denominator) * cr)
        return (preamble + 4.25 + payloadSymbols) * tsymMs
    }

    fun opsPerHour(
        preset: RadioPreset,
        payloadBytes: Int = 172,
        budgetMs: Double? = budgetMsPerHour(preset),
    ): Int? = budgetMs?.let { max(0, (it / timeOnAirMs(preset, payloadBytes)).toInt()) }
}

/**
 * The hourly allowance, spent and left, as the engine wants it.
 *
 * **The allowance is the caller's number and the engine never derives it** (`crates/session/src/
 * radio.rs`): it is a band's duty-cycle rule, which is a fact about the law and the frequency rather
 * than about the radio. What the engine does with it is real — `TransactionLayer::poll` refuses to
 * start a new part when `records_left == 0` — so a caller that never pushes it leaves the radio
 * transmitting unmetered, which in 869.4-869.65 MHz is outside the band's condition of use.
 *
 * **The preset that decides is the radio's own**, not the one a screen is proposing: the allowance
 * belongs to where the radio *is* tuned. A dialog that has picked a new tuple but not applied it
 * must not meter against it.
 */
object RadioMeter {
    /**
     * Records of one uplink that still fit this hour, or null when nothing is metered.
     *
     * Null means "no allowance in this band" or "the radio has not said what it spent", and both are
     * the same instruction to the engine: do not gate. A **zero** is different and is passed
     * through, because it is the answer "nothing more fits this hour" and the engine must stop.
     */
    fun recordsLeft(status: RadioStatus, metered: Boolean): Int? {
        if (!metered) return null
        val preset = status.preset ?: return null
        val allowanceMs = RadioAirtime.budgetMsPerHour(preset) ?: return null
        // The airtime is the radio's own since boot, which is what the firmware counts and what a
        // band's rule is measured against over an hour. A boot in the middle of the hour makes this
        // generous by at most the hour's start; the engine's own counter has the same horizon.
        val spentMs = status.txAirSecs * 1000.0
        val leftMs = allowanceMs - spentMs
        if (leftMs <= 0.0) return 0
        val perRecord = RadioAirtime.timeOnAirMs(preset, RECORD_BYTES)
        if (perRecord <= 0.0) return null
        return max(0, (leftMs / perRecord).toInt()).coerceAtMost(255)
    }

    /**
     * One full session record on the air: the carrier's ceiling.
     *
     * The record, its session wrapper and the flood wrapper's own bytes are what the radio spends
     * the channel on, so metering against the application payload alone would undercount by about a
     * fifth and let a European radio past its allowance.
     */
    const val RECORD_BYTES = 172
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
     * The radio's own client-repeat setting, as the firmware reported it.
     *
     * **Carried into every preset write.** `CMD_SET_RADIO_PARAMS`'s sixth byte is persisted as
     * `setRepeatEn(repeat != 0)`, and a frame that stops at the coding rate silently clears an
     * operator's repeat setting — nothing on the radio says so afterwards.
     */
    val repeatEnabled: Boolean = false,
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

/**
 * One connection from the mesh screen: the engine, and the driver that translates it.
 *
 * **The radio's whole protocol is behind `RadioEngine`** — the handshake, the carrier choice, the
 * transactions, the frames — so this class owns no protocol either. It builds the engine for this
 * plan, hands it to `MeshEngineDriver`, and closes it. `docs/RADIO-PLAN.md` §7, `AR-13`.
 *
 * One connection per plan: the draft and the picture are the screen's, and the engine's identity
 * (the session keys, the radio's address) is the plan's, so a rebuilt plan is a rebuilt link.
 */
class MeshRadioConnection(
    private val creds: MeshCredentials,
    private val address: String,
    private val selector: String,
    /** The grid to state on the first page; see `MeshEngineDriver`. */
    grid: Pair<Int, Int>? = null,
    /** Whether data records are metered at the band's allowance; see `MeshEngineDriver`. */
    metered: Boolean = true,
) {
    private val origin = TimeSource.Monotonic.markNow()
    private val clock: () -> Long = { origin.elapsedNow().inWholeMilliseconds }

    /**
     * The engine this connection drives.
     *
     * A build with no native library gets [UnavailableRadioEngine], which reports the absence as
     * `RadioMissing` — the same `LinkDown(NeedsRadio)` state a phone with no radio chosen has, because
     * it is the same cure: there is no radio to talk to.
     */
    private val engine: RadioEngine = radioEngine(creds, address) ?: UnavailableRadioEngine

    /**
     * The radio's own readings, for the two facts the screen needs and nothing else: which channel
     * it is really on (to classify the tuple) and what one exchange costs on this route.
     */
    val radio: StateFlow<RadioStatus> get() = engine.radio

    /** The route to the host, for the history deadline and the bar's evidence line. */
    val route: StateFlow<Route> get() = engine.route

    val driver = MeshEngineDriver(
        engine = engine,
        clock = clock,
        // The handshake's timestamp only has to agree with the host's within its window, so the
        // wall clock is read here and nowhere else.
        wall = { Clock.System.now().toEpochMilliseconds() },
        selector = selector,
        initialGrid = grid,
        metered = metered,
    )

    /** The host said the pane stopped changing, forwarded from the driver. */
    var onIdle: (() -> Unit)?
        get() = driver.onIdle
        set(value) { driver.onIdle = value }

    private var released = false

    /** Run the driver's loop until the caller cancels it. */
    suspend fun run() = driver.run()

    /** Close the engine, the lease and the loop. Idempotent. */
    fun close() {
        if (released) return
        released = true
        driver.close()
    }
}
