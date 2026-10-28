package dev.ldmux.mobile.mesh

/**
 * The MeshMore gateway wire contract (L2), Kotlin side.
 *
 * The reference implementation and the authority is the Dart package
 * `meshmore-sns/meshmore_sns_app/packages/meshcore_gateway` (`constants.dart`,
 * `frame.dart`, `messages.dart`). This file is a transcription pinned to it by
 * `MeshGatewayFrameTest`, which asserts the exact bytes the Dart codec emits. If the two
 * disagree, the Dart package is right and this file is the bug.
 *
 * Frame: `'M' 'G' | version | type | flags | length u16 LE | payload`.
 */
object MeshGateway {
    const val VERSION = 1
    const val HEADER_BYTES = 7
    const val MAX_PAYLOAD = 512
    const val MAX_FRAME = HEADER_BYTES + MAX_PAYLOAD
    const val DEFAULT_PORT = 41731
    const val PAIRING_TOKEN_BYTES = 16
    const val PEER_KEY_BYTES = 32

    /** Absolute ceiling for one record. The real limit is advertised in `BIND_OK`. */
    const val RECORD_MAX = 172

    /** `rssi` byte meaning "no reading". A signed byte. */
    const val UNKNOWN_RSSI = -128

    /** `batteryPercent` byte meaning "not reported". */
    const val UNKNOWN_BATTERY = 0xFF

    /** Capability bit 1: the gateway pushes `STATUS` when the radio state changes. */
    const val CAP_STATUS_PUSH = 1 shl 1

    val MAGIC = byteArrayOf(0x4D, 0x47)
}

/** Frame type codes. Values are frozen by the contract; an unknown type is ignored. */
object MeshFrameType {
    const val HELLO = 0x01
    const val HELLO_OK = 0x02
    const val HELLO_ERR = 0x03
    const val PAIR = 0x10
    const val PAIR_OK = 0x11
    const val PAIR_ERR = 0x12
    const val BIND = 0x20
    const val BIND_OK = 0x21
    const val BIND_ERR = 0x22
    const val RECORD = 0x30
    const val RECV = 0x31
    const val SEND_RESULT = 0x32
    const val STATUS = 0x40
    const val CLOSE = 0x60
}

/** What the radio did with a record. **Not** delivery: a blind carrier cannot know that. */
enum class MeshSendOutcome(val code: Int) {
    SentDirect(1),
    SentFlood(2),
    ErrNotFound(3),
    ErrBusy(4),
    ErrTooLong(5),
    LocalTimeout(6),
    Unknown(-1);

    companion object {
        fun of(code: Int): MeshSendOutcome = entries.firstOrNull { it.code == code } ?: Unknown
    }
}

/** Why the gateway closed the connection (`CLOSE`). */
enum class MeshCloseReason(val code: Int) {
    Protocol(1),
    Version(2),
    Busy(3),
    PreAuthTimeout(4),
    Shutdown(5),
    Unknown(-1);

    companion object {
        fun of(code: Int): MeshCloseReason = entries.firstOrNull { it.code == code } ?: Unknown
    }
}

/** A frame, or the reason there is not one. Construction refuses an oversize payload. */
class MeshFrame(val type: Int, val payload: ByteArray = ByteArray(0)) {
    init {
        require(type in 0..0xFF) { "type must fit one byte" }
        require(payload.size <= MeshGateway.MAX_PAYLOAD) {
            "payload is ${payload.size} bytes, maximum ${MeshGateway.MAX_PAYLOAD}"
        }
    }

    fun encode(): ByteArray {
        val out = ByteArray(MeshGateway.HEADER_BYTES + payload.size)
        out[0] = MeshGateway.MAGIC[0]
        out[1] = MeshGateway.MAGIC[1]
        out[2] = MeshGateway.VERSION.toByte()
        out[3] = type.toByte()
        out[4] = 0 // flags: reserved, must be 0 in v1
        out[5] = (payload.size and 0xFF).toByte()
        out[6] = ((payload.size shr 8) and 0xFF).toByte()
        payload.copyInto(out, MeshGateway.HEADER_BYTES)
        return out
    }

    override fun toString(): String = "MeshFrame(0x${type.toString(16)}, ${payload.size} B)"
}

/** What stopped a [MeshFrameDecoder]. Any of these is fatal: close, do not resync. */
enum class MeshFrameError { BadMagic, BadVersion, BadFlags, LengthTooLarge }

/**
 * Incremental decoder over a TCP byte stream.
 *
 * Chunks arrive on arbitrary boundaries, so a frame may come in pieces and several may come
 * at once. The error is **sticky**: after the first fatal error [add] returns nothing and
 * [error] stays set, and the caller must close. There is deliberately no resync, because a
 * stream that has lost framing cannot be trusted to find it again. Never allocates from an
 * unvalidated length.
 *
 * Frames decoded before the fatal error are still returned by the same [add] call.
 */
class MeshFrameDecoder {
    private var buf = ByteArray(0)
    var error: MeshFrameError? = null
        private set

    fun add(chunk: ByteArray, length: Int = chunk.size): List<MeshFrame> {
        if (error != null) return emptyList()
        buf += chunk.copyOf(length)
        val out = ArrayList<MeshFrame>()
        var pos = 0
        while (buf.size - pos >= MeshGateway.HEADER_BYTES) {
            if (buf[pos] != MeshGateway.MAGIC[0] || buf[pos + 1] != MeshGateway.MAGIC[1]) {
                return fail(MeshFrameError.BadMagic, out)
            }
            if ((buf[pos + 2].toInt() and 0xFF) != MeshGateway.VERSION) {
                return fail(MeshFrameError.BadVersion, out)
            }
            if (buf[pos + 4].toInt() != 0) return fail(MeshFrameError.BadFlags, out)
            val len = (buf[pos + 5].toInt() and 0xFF) or ((buf[pos + 6].toInt() and 0xFF) shl 8)
            if (len > MeshGateway.MAX_PAYLOAD) return fail(MeshFrameError.LengthTooLarge, out)
            val total = MeshGateway.HEADER_BYTES + len
            if (buf.size - pos < total) break
            out += MeshFrame(
                buf[pos + 3].toInt() and 0xFF,
                buf.copyOfRange(pos + MeshGateway.HEADER_BYTES, pos + total),
            )
            pos += total
        }
        buf = if (pos == 0) buf else buf.copyOfRange(pos, buf.size)
        return out
    }

    private fun fail(e: MeshFrameError, out: List<MeshFrame>): List<MeshFrame> {
        error = e
        buf = ByteArray(0)
        return out
    }
}

/** A decoded `STATUS`. Sentinels are already mapped to null; never render them as numbers. */
data class MeshStatus(
    val radioState: Int,
    val rssiDbm: Int?,
    val batteryPercent: Int?,
    /** The gateway's OWN transmit airtime this hour, 0.1 s units. An estimate, display only. */
    val txAirtimeTenthsThisHour: Int,
) {
    val radioConnected: Boolean get() = radioState == RADIO_CONNECTED

    companion object {
        const val RADIO_CONNECTED = 0
    }
}

data class MeshHelloOk(val version: Int, val capabilities: Int, val radioState: Int)

data class MeshBindOk(val capabilities: Int, val uplinkRecordMax: Int)

data class MeshRecord(val seq: Int, val record: ByteArray) {
    override fun equals(other: Any?): Boolean =
        other is MeshRecord && seq == other.seq && record.contentEquals(other.record)

    override fun hashCode(): Int = 31 * seq + record.contentHashCode()
}

/**
 * Typed builders and readers. Builders refuse bad input rather than send it to be rejected;
 * readers are total and return null for a wrong type or a short payload, and the caller
 * closes with a protocol error when it gets null for a frame it expected.
 */
object MeshMessages {
    fun hello(capabilities: Int = 0, version: Int = MeshGateway.VERSION): MeshFrame =
        MeshFrame(MeshFrameType.HELLO, byteArrayOf(version.toByte(), capabilities.toByte()))

    fun pair(token: ByteArray): MeshFrame {
        require(token.size == MeshGateway.PAIRING_TOKEN_BYTES) {
            "pairing token is ${token.size} bytes, must be ${MeshGateway.PAIRING_TOKEN_BYTES}"
        }
        return MeshFrame(MeshFrameType.PAIR, token)
    }

    fun bind(peerPubKey: ByteArray): MeshFrame {
        require(peerPubKey.size == MeshGateway.PEER_KEY_BYTES) {
            "peer public key is ${peerPubKey.size} bytes, must be ${MeshGateway.PEER_KEY_BYTES}"
        }
        return MeshFrame(MeshFrameType.BIND, peerPubKey)
    }

    /** `RECORD`: `seq(1) + record`. The record is opaque and never parsed. */
    fun record(seq: Int, record: ByteArray): MeshFrame {
        require(seq in 0..0xFF) { "seq must fit one byte" }
        require(record.isNotEmpty()) { "a record must not be empty" }
        require(record.size <= MeshGateway.RECORD_MAX) {
            "record is ${record.size} bytes, ceiling ${MeshGateway.RECORD_MAX}"
        }
        return MeshFrame(MeshFrameType.RECORD, byteArrayOf(seq.toByte()) + record)
    }

    fun readHelloOk(f: MeshFrame): MeshHelloOk? {
        if (f.type != MeshFrameType.HELLO_OK || f.payload.size < 3) return null
        return MeshHelloOk(u8(f, 0), u8(f, 1), u8(f, 2))
    }

    fun readHelloErr(f: MeshFrame): Int? = reason(f, MeshFrameType.HELLO_ERR)
    fun readPairErr(f: MeshFrame): Int? = reason(f, MeshFrameType.PAIR_ERR)
    fun readBindErr(f: MeshFrame): Int? = reason(f, MeshFrameType.BIND_ERR)

    fun readBindOk(f: MeshFrame): MeshBindOk? {
        if (f.type != MeshFrameType.BIND_OK || f.payload.size < 2) return null
        return MeshBindOk(u8(f, 0), u8(f, 1))
    }

    fun readRecv(f: MeshFrame): ByteArray? =
        if (f.type == MeshFrameType.RECV) f.payload.copyOf() else null

    fun readSendResult(f: MeshFrame): Pair<Int, MeshSendOutcome>? {
        if (f.type != MeshFrameType.SEND_RESULT || f.payload.size < 2) return null
        return u8(f, 0) to MeshSendOutcome.of(u8(f, 1))
    }

    fun readStatus(f: MeshFrame): MeshStatus? {
        if (f.type != MeshFrameType.STATUS || f.payload.size < 5) return null
        val rssi = f.payload[1].toInt() // signed byte
        val battery = u8(f, 2)
        return MeshStatus(
            radioState = u8(f, 0),
            rssiDbm = if (rssi == MeshGateway.UNKNOWN_RSSI) null else rssi,
            batteryPercent = if (battery == MeshGateway.UNKNOWN_BATTERY) null else battery,
            txAirtimeTenthsThisHour = u8(f, 3) or (u8(f, 4) shl 8),
        )
    }

    fun readClose(f: MeshFrame): MeshCloseReason? {
        val code = reason(f, MeshFrameType.CLOSE) ?: return null
        return MeshCloseReason.of(code)
    }

    private fun reason(f: MeshFrame, type: Int): Int? =
        if (f.type == type && f.payload.isNotEmpty()) u8(f, 0) else null

    private fun u8(f: MeshFrame, i: Int): Int = f.payload[i].toInt() and 0xFF
}
