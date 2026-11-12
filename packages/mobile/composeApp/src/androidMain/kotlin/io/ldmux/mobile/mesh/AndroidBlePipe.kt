package io.ldmux.mobile.mesh

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.Context
import io.iotone.meshcore.android.AndroidBleTransport
import io.iotone.meshcore.transport.MeshcoreTransport

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
 * one command in flight — [write] is refused while the previous write has not completed, so a caller
 * that also issues radio-management commands can serialise them here rather than racing the engine
 * (`RC-2`).
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
    }
}
