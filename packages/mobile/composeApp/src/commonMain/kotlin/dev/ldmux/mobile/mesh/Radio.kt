package dev.ldmux.mobile.mesh

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
    val received: Flow<ByteArray>
    val uplinkRecordMax: Int

    /** Connect, run the companion handshake and wait until the radio is READY. */
    suspend fun connect()

    /** Send one raw-custom record. The path belongs to the link; the caller sends only the record. */
    suspend fun send(record: ByteArray): RadioSendResult

    /** Apply the LDMUX preset to the attached radio. False when the write could not be verified. */
    suspend fun applyPreset(preset: RadioPreset): Boolean

    fun close()
}

data class RadioPreset(
    val frequencyMhz: Double,
    val bandwidthKhz: Double,
    val spreadingFactor: Int,
    val codingRate: Int,
    val txPowerDbm: Int,
) {
    companion object {
        /** The bench preset: 910.525 MHz, 500 kHz, SF7, CR5, 22 dBm. */
        val LDMUX_PRIVATE = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 500.0,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
        )

        /** The public USA/Canada preset: same frequency, 62.5 kHz. */
        val USA_CANADA = RadioPreset(
            frequencyMhz = 910.525,
            bandwidthKhz = 62.5,
            spreadingFactor = 7,
            codingRate = 5,
            txPowerDbm = 22,
        )
    }
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

/** What the radio did with one raw record. Never delivery. */
enum class RadioSendResult(val outcome: MeshSendOutcome) {
    Accepted(MeshSendOutcome.SentDirect),
    Busy(MeshSendOutcome.ErrBusy),
    TooLong(MeshSendOutcome.ErrTooLong),
    NotFound(MeshSendOutcome.ErrNotFound),
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

    override fun close() = link.close()
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

    suspend fun run() {
        try {
            link.connect()
        } catch (e: RadioLinkException) {
            driver.onGatewayState(e.state)
            return
        }
        try {
            driver.run()
        } finally {
            link.close()
        }
    }

    fun close() = link.close()
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
    override val received: Flow<ByteArray> = emptyFlow()
    override val uplinkRecordMax: Int = 172
    override suspend fun connect() = throw RadioLinkException(MeshLinkState.NeedsRadio, "no radio platform")
    override suspend fun send(record: ByteArray): RadioSendResult = RadioSendResult.Failed
    override suspend fun applyPreset(preset: RadioPreset): Boolean = false
    override fun close() {}
}
