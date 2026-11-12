package io.ldmux.mobile.mesh

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.Context
import io.iotone.meshcore.android.AndroidBleTransport
import io.iotone.meshcore.transport.MeshcoreTransport
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * The only BLE code left on the radio route.
 *
 * It opens one GATT connection, hands every notification to the engine, and writes the frames the
 * engine produces. It owns **no protocol**: no queue, no handshake, no timing, no retry, no route.
 * All of that is `ldmux_client_*` (`docs/RADIO-PLAN.md` §8.1), which is why this file can be short.
 *
 * **Frames are the only thing that crosses, in both directions.** A MeshCore companion is a byte
 * pipe: one BLE notification is one companion frame, and the engine takes and produces exactly that.
 * Reassembling or splitting a frame here would be a second framing implementation competing with
 * the firmware's, which is precisely the class of defect this rebuild removed.
 *
 * The pipe is **pull only** like the engine: it never writes on its own. A caller with a frame calls
 * [write]; a caller with nothing to do does not, and the radio stays silent. That is also what keeps
 * one write in flight — [write] is refused while the previous write has not completed (`RC-2`).
 *
 * **The pipe holds the radio lease.** A MeshCore companion accepts exactly one central, and Android
 * will not arbitrate which app that is, so the apps agree between themselves (ADR-0036). That used to
 * live in `AndroidRadioLink`, which meant the one thing that owns the GATT link did not own the
 * permission to it. It does now: taken before the first connection, released when the link closes.
 */
class AndroidBlePipe(
    /**
     * The Android context, needed only to open a GATT connection.
     *
     * Nullable because the pipe's whole job is frames, and a test that supplies its own transport
     * has nothing to ask Android for. A null context and no transport is a build mistake, reported
     * by [connect] rather than by a crash at construction.
     */
    private val context: Context? = null,
    /** One whole companion frame arrived from the radio. Never called on the caller's thread. */
    onFrame: (ByteArray) -> Unit,
    /** The link became usable, or stopped being usable. */
    onLink: (Boolean) -> Unit = {},
    /** Why a write failed, in words, when it is not the ordinary "not connected yet". */
    onProblem: (String) -> Unit = {},
    transport: MeshcoreTransport? = null,
) {
    private val ble: MeshcoreTransport = transport ?: AndroidBleTransport(
        requireNotNull(context) { "a pipe with no transport needs a Context to build one" },
    )

    /** Where the lease is taken and given back; a pipe's own scope, cancelled with it. */
    private val leaseScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** Whether this pipe has announced that it wants the radio. */
    @Volatile
    private var leased: Boolean = false

    // Held under names of their own because the listener below declares methods called `onFrame`
    // and `onConnectionChanged`: an unqualified call inside it would recurse into the listener
    // instead of reaching the caller. (It did, and the test caught it.)
    private val frameSink = onFrame
    private val linkSink = onLink
    private val problemSink = onProblem

    @Volatile
    var connected: Boolean = false
        private set

    /**
     * True while a write is outstanding.
     *
     * A second write while this is set is refused rather than queued: the caller owns the retry, and
     * a queue here would hide the very interleaving that broke the link in the field — an advert's
     * answer arriving in front of a record's, which the old code read as a refused record.
     */
    @Volatile
    var writing: Boolean = false
        private set

    init {
        ble.setListener(object : MeshcoreTransport.Listener {
            override fun onFrame(frame: ByteArray) {
                // A push that arrives mid-write is still a push: the radio answers asynchronously,
                // and holding it back would stall the engine's own FIFO for the whole write time.
                frameSink(frame)
            }

            override fun onConnectionChanged(up: Boolean) {
                connected = up
                linkSink(up)
            }
        })
    }

    /**
     * Whether a frame may be handed to [write] right now.
     *
     * A caller polls the engine only when this is true, so a frame the pipe cannot take is never
     * taken from the engine in the first place: `ldmux_client_poll` hands a frame over exactly
     * once, and dropping one loses a companion command.
     */
    val ready: Boolean get() = connected && !writing

    /** Open the GATT connection to the radio at [address]. Returns false when the address is unknown. */
    @SuppressLint("MissingPermission")
    fun connect(address: String): Boolean {
        if (address.isBlank()) {
            problemSink("no radio is chosen")
            return false
        }
        val ctx = context ?: run {
            problemSink("this build has no Bluetooth")
            return false
        }
        val manager = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter: BluetoothAdapter = manager?.adapter ?: run {
            problemSink("this device has no Bluetooth")
            return false
        }
        if (!adapter.isEnabled) {
            problemSink("Bluetooth is off")
            return false
        }
        val device = try {
            adapter.getRemoteDevice(address)
        } catch (e: IllegalArgumentException) {
            problemSink("not a Bluetooth address")
            return false
        }
        // **Ask the other apps to let go before opening the GATT client.** A companion accepts one
        // central; a second one often connects and is dropped later, which reads as a flaky radio
        // rather than as a busy one (ADR-0036). The connect proceeds either way — a counterpart that
        // is not running answers nothing, and a link that is attempted and fails names its reason
        // where a connect that never happened explains nothing.
        if (!leased) {
            leased = true
            leaseScope.launch { runCatching { RadioLease.acquire(ctx) } }
        }
        (ble as? AndroidBleTransport)?.connectToDevice(device)
            ?: run {
                problemSink("this transport cannot be connected")
                return false
            }
        return true
    }

    /**
     * Write one frame. False when the link is not ready or a write is already outstanding, which is
     * the caller's cue to keep the frame and try again on the next turn.
     */
    fun write(frame: ByteArray): Boolean {
        if (!connected || writing) return false
        writing = true
        return try {
            ble.send(frame)
            true
        } catch (e: IllegalStateException) {
            // The link dropped between the check and the write. Not a protocol fault.
            connected = false
            false
        } catch (e: Exception) {
            problemSink("the radio refused a write: ${e.message ?: e::class.simpleName}")
            false
        } finally {
            writing = false
        }
    }

    /** Ask the radio to start advertising, so a scan can find it again. */
    fun reconnectHint() {
        problemSink("the radio went away; open the radio settings to reconnect")
    }

    fun close() {
        connected = false
        try {
            ble.close()
        } catch (e: Exception) {
            // Closing a link that is already gone is not a failure worth a sentence.
        }
        // **The lease goes back with the link.** It used to be released only from `close()` of the
        // old link object, which `closeNow()` never called, so a phone that abandoned the radio left
        // the other app believing it still held it (`RC-12`, `S17`).
        val ctx = context
        if (leased && ctx != null) {
            leased = false
            RadioLease.release(ctx)
        }
    }
}
