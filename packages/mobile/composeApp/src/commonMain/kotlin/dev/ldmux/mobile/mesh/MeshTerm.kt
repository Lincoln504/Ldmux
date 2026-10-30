package dev.ldmux.mobile.mesh

/**
 * The mesh terminal profile (ADR-0025): what goes *inside* the L3 session. A port of
 * `crates/protocol/src/mesh_term.rs`, and held to it by bytes the Rust side produced
 * (`crates/protocol/test/fixtures/mesh-term-vectors.json`, read by `MeshTermCodecTest`), not by
 * bytes this file made for itself. Change one side and the other's suite fails.
 *
 * Pure: no I/O, no clock, no platform type. Knows nothing about a radio or a gateway; it sees
 * only the ordered byte stream the session delivers (`Delivered.bytes`) and the bytes it hands
 * back to `MeshSession.send`.
 *
 * Framing: `tag u8 | len (LEB128, 1 or 2 bytes) | body`. The ARQ may split the stream anywhere,
 * so [TermDecoder] reassembles. An unknown tag is skipped; a body over [MeshTerm.MAX_BODY] ends
 * the attach.
 */
object MeshTerm {
    /**
     * Version 2 adds `Key` and names the attached shell in `Profile`. A host answers in the
     * version the phone attached with and accepts [MIN_VERSION]..[VERSION]. Version 3 adds
     * `Resize`: the phone's grid, so the host fits its pty to it as it does for a tailnet client.
     */
    const val VERSION = 3
    const val MIN_VERSION = 1
    /** The first version that has `Key` and the shell id in `Profile`. */
    const val KEY_VERSION = 2
    /** The first version that has `Resize`. */
    const val RESIZE_VERSION = 3
    /** Most key names in one `Key` message. */
    const val MAX_KEYS = 16
    const val MAX_BODY = 300
    const val MAX_ROWS_BODY = 130
    const val MAX_MESSAGE = 1 + 2 + MAX_ROWS_BODY
    /** 172 - 24 (ADR-0020) - 4 (ARQ header): what one record carries of this stream. */
    const val RECORD_PAYLOAD = 144
    const val MAX_INPUT = 96
    const val MAX_SEGMENT = 120
    const val MAX_SELECTOR = 40
    /** Longest shell label in a `Sessions` entry. */
    const val MAX_LABEL = 24
    /** Most shells a host lists. */
    const val MAX_SESSIONS = 16
    const val MAX_BUFFER = 4096

    /** `Profile.unsupported` bits: features this link does not carry. A set bit means "absent". */
    object Unsupported {
        const val SCROLLBACK = 1 shl 0
        const val STYLE = 1 shl 1
        const val HYPERLINKS = 1 shl 2
        const val MOUSE = 1 shl 3
        const val IMAGES = 1 shl 4
        const val RESIZE = 1 shl 5
        const val CLIPBOARD = 1 shl 6
    }

    /** `Closed.reason`. */
    object Closed {
        const val NOT_FOUND = 1
        const val AMBIGUOUS = 2
        const val EXITED = 3
        const val UNAVAILABLE = 4
        const val VERSION = 5
    }

    /** `Commit.flags`. */
    object CommitFlags {
        const val ALTERNATE = 1 shl 0
        const val DEAD = 1 shl 1
    }

    internal const val T_ATTACH = 0x01
    internal const val T_INPUT = 0x02
    internal const val T_RESYNC = 0x03
    internal const val T_DETACH = 0x04
    internal const val T_LIST = 0x05
    internal const val T_KEY = 0x06
    internal const val T_RESIZE = 0x07
    internal const val T_SESSIONS = 0x85
    internal const val T_PROFILE = 0x81
    internal const val T_ROWS = 0x82
    internal const val T_COMMIT = 0x83
    internal const val T_CLOSED = 0x84
}

/** One running shell in a `Sessions` reply. [id] is usable as an `Attach` selector. */
data class SessionEntry(val id: String, val label: String)

data class TermPatch(val y: Int, val append: Boolean, val text: String)

sealed interface TermMessage {
    data class Attach(val version: Int, val selector: String) : TermMessage
    class Input(val bytes: ByteArray) : TermMessage {
        override fun equals(other: Any?) = other is Input && bytes.contentEquals(other.bytes)
        override fun hashCode() = bytes.contentHashCode()
        override fun toString() = "Input(${bytes.size} bytes)"
    }
    data object Resync : TermMessage
    data object Detach : TermMessage
    /** [shell] is the attached shell's id (version 2; empty from a version-1 host). */
    data class Profile(val version: Int, val cols: Int, val rows: Int, val unsupported: Int, val shell: String = "") : TermMessage
    /**
     * Phone to host: named keys (`Up`, `S-Tab`, `C-c`). The host encodes them for the pane's own
     * cursor-key and keyboard mode, so the phone has no key table. Printable ASCII, no space.
     */
    data class Key(val names: List<String>) : TermMessage
    /** Phone to host (version 3): the phone's grid in cells, each 1..255. The host answers with a fresh `Profile`. */
    data class Resize(val cols: Int, val rows: Int) : TermMessage
    data class Rows(val epoch: Int, val full: Boolean, val patches: List<TermPatch>) : TermMessage
    data class Commit(val epoch: Int, val cursorX: Int, val cursorY: Int, val flags: Int) : TermMessage
    data class Closed(val reason: Int) : TermMessage
    /** Phone to host: which shells are running? */
    data object ListRequest : TermMessage
    /** Host to phone: one page of the answer; [last] marks the final page (an empty list is one empty last page). */
    data class Sessions(val last: Boolean, val entries: List<SessionEntry>) : TermMessage
}

/** A field is over its limit, or an `Input` is empty. Nothing is truncated: the caller decides. */
class TermEncodeException(message: String) : Exception(message)

private fun putLen(out: ArrayList<Byte>, n0: Int) {
    var n = n0
    while (true) {
        val b = n and 0x7f
        n = n ushr 7
        if (n == 0) {
            out.add(b.toByte())
            return
        }
        out.add((b or 0x80).toByte())
    }
}

/** Printable ASCII with no space: the space is the separator and nothing else is a key name. */
internal fun validKeyName(name: String): Boolean =
    name.isNotEmpty() && name.all { it.code in 0x21..0x7e }

private fun u8(v: Int, what: String): Byte {
    if (v !in 0..255) throw TermEncodeException("$what does not fit one byte")
    return v.toByte()
}

private fun u16(v: Int, what: String): List<Byte> {
    if (v !in 0..0xffff) throw TermEncodeException("$what does not fit two bytes")
    return listOf((v and 0xff).toByte(), ((v ushr 8) and 0xff).toByte())
}

/** The wire bytes of this message. */
fun TermMessage.encode(): ByteArray {
    val body = ArrayList<Byte>()
    val tag: Int = when (this) {
        is TermMessage.Attach -> {
            val sel = selector.encodeToByteArray()
            if (sel.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("selector too long")
            body.add(u8(version, "version")); sel.forEach { body.add(it) }
            MeshTerm.T_ATTACH
        }
        is TermMessage.Input -> {
            if (bytes.isEmpty()) throw TermEncodeException("empty input")
            if (bytes.size > MeshTerm.MAX_INPUT) throw TermEncodeException("input too long")
            bytes.forEach { body.add(it) }
            MeshTerm.T_INPUT
        }
        TermMessage.Resync -> MeshTerm.T_RESYNC
        TermMessage.Detach -> MeshTerm.T_DETACH
        is TermMessage.Profile -> {
            body.add(u8(version, "version")); body.add(u8(cols, "cols")); body.add(u8(rows, "rows"))
            body.addAll(u16(unsupported, "unsupported"))
            if (version >= MeshTerm.KEY_VERSION) {
                val sh = shell.encodeToByteArray()
                if (sh.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("shell id too long")
                body.add(sh.size.toByte()); sh.forEach { body.add(it) }
            }
            MeshTerm.T_PROFILE
        }
        is TermMessage.Rows -> {
            body.addAll(u16(epoch, "epoch")); body.add(if (full) 1 else 0)
            for (p in patches) {
                val t = p.text.encodeToByteArray()
                if (t.size > MeshTerm.MAX_SEGMENT) throw TermEncodeException("row segment too long")
                body.add(u8(p.y, "y")); body.add(if (p.append) 1 else 0); body.add(t.size.toByte())
                t.forEach { body.add(it) }
            }
            if (body.size > MeshTerm.MAX_ROWS_BODY) throw TermEncodeException("rows body too long")
            MeshTerm.T_ROWS
        }
        is TermMessage.Commit -> {
            body.addAll(u16(epoch, "epoch"))
            body.add(u8(cursorX, "cursorX")); body.add(u8(cursorY, "cursorY")); body.add(u8(flags, "flags"))
            MeshTerm.T_COMMIT
        }
        is TermMessage.Closed -> {
            body.add(u8(reason, "reason"))
            MeshTerm.T_CLOSED
        }
        TermMessage.ListRequest -> MeshTerm.T_LIST
        is TermMessage.Key -> {
            if (names.isEmpty()) throw TermEncodeException("empty key")
            if (names.size > MeshTerm.MAX_KEYS || names.any { !validKeyName(it) }) throw TermEncodeException("bad key names")
            names.joinToString(" ").encodeToByteArray().forEach { body.add(it) }
            if (body.size > MeshTerm.MAX_INPUT) throw TermEncodeException("keys too long")
            MeshTerm.T_KEY
        }
        is TermMessage.Resize -> {
            if (cols !in 1..255 || rows !in 1..255) throw TermEncodeException("resize out of range")
            body.add(cols.toByte()); body.add(rows.toByte())
            MeshTerm.T_RESIZE
        }
        is TermMessage.Sessions -> {
            body.add(if (last) 1 else 0)
            for (e in entries) {
                val id = e.id.encodeToByteArray()
                val label = e.label.encodeToByteArray()
                if (id.size > MeshTerm.MAX_SELECTOR || label.size > MeshTerm.MAX_LABEL) {
                    throw TermEncodeException("session entry too long")
                }
                body.add(id.size.toByte()); id.forEach { body.add(it) }
                body.add(label.size.toByte()); label.forEach { body.add(it) }
            }
            if (body.size > MeshTerm.MAX_ROWS_BODY) throw TermEncodeException("sessions body too long")
            MeshTerm.T_SESSIONS
        }
    }
    val out = ArrayList<Byte>(body.size + 3)
    out.add(tag.toByte())
    putLen(out, body.size)
    out.addAll(body)
    return out.toByteArray()
}

/** One decoded item: a message, or a known tag whose body did not parse (that message only is lost). */
sealed interface TermItem {
    data class Message(val message: TermMessage) : TermItem
    data class Malformed(val tag: Int) : TermItem
}

/** The stream is unusable; the caller drops the attach. */
class TermStreamException(val kind: Kind) : Exception("mesh terminal stream: $kind") {
    enum class Kind { BodyTooLong, Overflow }
}

/** Reassembles messages from an arbitrarily chunked byte stream. */
class TermDecoder {
    private var buf = ByteArray(0)

    /** Bytes held back waiting for the rest of a message. */
    val buffered: Int get() = buf.size

    fun push(bytes: ByteArray): List<TermItem> {
        if (buf.size + bytes.size > MeshTerm.MAX_BUFFER) {
            buf = ByteArray(0)
            throw TermStreamException(TermStreamException.Kind.Overflow)
        }
        buf += bytes
        val out = ArrayList<TermItem>()
        var pos = 0
        while (pos < buf.size) {
            val tag = buf[pos].toInt() and 0xff
            if (pos + 1 >= buf.size) break
            val b1 = buf[pos + 1].toInt() and 0xff
            val len: Int
            val head: Int
            if (b1 and 0x80 == 0) {
                len = b1; head = 2
            } else {
                if (pos + 2 >= buf.size) break
                val b2 = buf[pos + 2].toInt() and 0xff
                if (b2 and 0x80 != 0) {
                    buf = ByteArray(0)
                    throw TermStreamException(TermStreamException.Kind.BodyTooLong)
                }
                len = (b1 and 0x7f) or (b2 shl 7); head = 3
            }
            if (len > MeshTerm.MAX_BODY) {
                buf = ByteArray(0)
                throw TermStreamException(TermStreamException.Kind.BodyTooLong)
            }
            if (buf.size - pos < head + len) break
            val body = buf.copyOfRange(pos + head, pos + head + len)
            pos += head + len
            when (val r = parse(tag, body)) {
                Parsed.Unknown -> {}
                Parsed.Bad -> out += TermItem.Malformed(tag)
                is Parsed.Ok -> out += TermItem.Message(r.message)
            }
        }
        buf = if (pos == 0) buf else buf.copyOfRange(pos, buf.size)
        return out
    }

    private sealed interface Parsed {
        data object Unknown : Parsed
        data object Bad : Parsed
        data class Ok(val message: TermMessage) : Parsed
    }

    private fun utf8(b: ByteArray): String? =
        try { b.decodeToString(throwOnInvalidSequence = true) } catch (_: Exception) { null }

    private fun le16(b: ByteArray, i: Int) = (b[i].toInt() and 0xff) or ((b[i + 1].toInt() and 0xff) shl 8)
    private fun b(b: ByteArray, i: Int) = b[i].toInt() and 0xff

    private fun parse(tag: Int, body: ByteArray): Parsed {
        fun ok(m: TermMessage) = Parsed.Ok(m)
        return when (tag) {
            MeshTerm.T_ATTACH -> {
                if (body.isEmpty() || body.size - 1 > MeshTerm.MAX_SELECTOR) return Parsed.Bad
                val sel = utf8(body.copyOfRange(1, body.size)) ?: return Parsed.Bad
                ok(TermMessage.Attach(b(body, 0), sel))
            }
            MeshTerm.T_INPUT ->
                if (body.isEmpty() || body.size > MeshTerm.MAX_INPUT) Parsed.Bad else ok(TermMessage.Input(body))
            MeshTerm.T_RESYNC -> if (body.isEmpty()) ok(TermMessage.Resync) else Parsed.Bad
            MeshTerm.T_DETACH -> if (body.isEmpty()) ok(TermMessage.Detach) else Parsed.Bad
            // Version 1 is exactly five bytes and is never an error; a longer body is version 2 and
            // must carry its shell id.
            MeshTerm.T_PROFILE -> when {
                body.size == 5 -> ok(TermMessage.Profile(b(body, 0), b(body, 1), b(body, 2), le16(body, 3)))
                body.size > 5 && b(body, 0) >= MeshTerm.KEY_VERSION -> {
                    val n = b(body, 5)
                    if (n > MeshTerm.MAX_SELECTOR || body.size != 6 + n) return Parsed.Bad
                    val shell = utf8(body.copyOfRange(6, body.size)) ?: return Parsed.Bad
                    ok(TermMessage.Profile(b(body, 0), b(body, 1), b(body, 2), le16(body, 3), shell))
                }
                else -> Parsed.Bad
            }
            MeshTerm.T_ROWS -> {
                if (body.size < 3) return Parsed.Bad
                val epoch = le16(body, 0)
                val full = body[2].toInt() and 1 == 1
                val patches = ArrayList<TermPatch>()
                var p = 3
                while (p < body.size) {
                    if (body.size - p < 3) return Parsed.Bad
                    val n = b(body, p + 2)
                    if (body.size - p < 3 + n || n > MeshTerm.MAX_SEGMENT) return Parsed.Bad
                    val text = utf8(body.copyOfRange(p + 3, p + 3 + n)) ?: return Parsed.Bad
                    patches += TermPatch(b(body, p), body[p + 1].toInt() and 1 == 1, text)
                    p += 3 + n
                }
                ok(TermMessage.Rows(epoch, full, patches))
            }
            MeshTerm.T_COMMIT ->
                if (body.size == 5) ok(TermMessage.Commit(le16(body, 0), b(body, 2), b(body, 3), b(body, 4))) else Parsed.Bad
            MeshTerm.T_CLOSED -> if (body.size == 1) ok(TermMessage.Closed(b(body, 0))) else Parsed.Bad
            MeshTerm.T_LIST -> if (body.isEmpty()) ok(TermMessage.ListRequest) else Parsed.Bad
            MeshTerm.T_KEY -> {
                if (body.isEmpty() || body.size > MeshTerm.MAX_INPUT) return Parsed.Bad
                val text = utf8(body) ?: return Parsed.Bad
                val names = text.split(' ')
                if (names.size > MeshTerm.MAX_KEYS || names.any { !validKeyName(it) }) Parsed.Bad
                else ok(TermMessage.Key(names))
            }
            MeshTerm.T_RESIZE ->
                if (body.size == 2 && b(body, 0) != 0 && b(body, 1) != 0) ok(TermMessage.Resize(b(body, 0), b(body, 1))) else Parsed.Bad
            MeshTerm.T_SESSIONS -> {
                if (body.isEmpty()) return Parsed.Bad
                val entries = ArrayList<SessionEntry>()
                var p = 1
                fun field(max: Int): String? {
                    if (p >= body.size) return null
                    val n = b(body, p)
                    if (n > max || body.size - (p + 1) < n) return null
                    val t = utf8(body.copyOfRange(p + 1, p + 1 + n)) ?: return null
                    p += 1 + n
                    return t
                }
                while (p < body.size) {
                    val id = field(MeshTerm.MAX_SELECTOR) ?: return Parsed.Bad
                    val label = field(MeshTerm.MAX_LABEL) ?: return Parsed.Bad
                    entries += SessionEntry(id, label)
                }
                ok(TermMessage.Sessions(body[0].toInt() and 1 == 1, entries))
            }
            else -> Parsed.Unknown
        }
    }
}

/**
 * The phone's screen, as the host described it: a reference-equal port of Rust's `Mirror`.
 * Rows are held until their `Commit` and painted only then, so a screen is never half-drawn.
 */
class RowMirror {
    data class ProfileInfo(val cols: Int, val rows: Int, val unsupported: Int)

    var profile: ProfileInfo? = null
        private set
    /** The shell the host named in its `Profile` (empty from a version-1 host). */
    var shell: String = ""
        private set
    var lines: List<String> = emptyList()
        private set
    var cursorX = 0
        private set
    var cursorY = 0
        private set
    var flags = 0
        private set
    var epoch = 0
        private set
    /** The `Closed` reason, once the host ended the attach. */
    var closed: Int? = null
        private set

    private var pendingEpoch = -1
    private var pendingFull = false
    private var pending = ArrayList<TermPatch>()
    private var hasPending = false

    /** Apply a host→phone message. True when the visible screen changed (a `Commit` was applied). */
    fun apply(m: TermMessage): Boolean {
        when (m) {
            is TermMessage.Profile -> {
                profile = ProfileInfo(m.cols, m.rows, m.unsupported)
                shell = m.shell
                lines = List(m.rows) { "" }
                return false
            }
            is TermMessage.Rows -> {
                if (hasPending && pendingEpoch == m.epoch) {
                    pendingFull = pendingFull || m.full
                    pending.addAll(m.patches)
                } else {
                    hasPending = true; pendingEpoch = m.epoch; pendingFull = m.full
                    pending = ArrayList(m.patches)
                }
                return false
            }
            is TermMessage.Commit -> {
                var full = false
                var patches: List<TermPatch> = emptyList()
                if (hasPending && pendingEpoch == m.epoch) { full = pendingFull; patches = pending }
                hasPending = false; pending = ArrayList()
                val next = lines.toMutableList()
                if (full) for (i in next.indices) next[i] = ""
                for (p in patches) {
                    if (p.y >= next.size) continue
                    next[p.y] = if (p.append) next[p.y] + p.text else p.text
                }
                lines = next
                cursorX = m.cursorX; cursorY = m.cursorY; flags = m.flags; epoch = m.epoch
                return true
            }
            is TermMessage.Closed -> { closed = m.reason; return false }
            else -> return false
        }
    }
}
