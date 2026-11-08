package io.ldmux.mobile.mesh

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
     * Version 5 adds the absolute line number to `Commit`: the rows leaving the top of the screen
     * are history the phone already holds, which is the fact a local scrollback is built on.
     * Version 6 adds a shell's working directory to each `Sessions` entry, so the picker shows the
     * folder instead of the id it would send.
     */
    const val VERSION = 10
    const val MIN_VERSION = 1
    /** The first version that has `Key` and the shell id in `Profile`. */
    const val KEY_VERSION = 2
    /** The first version that has `Resize`. */
    const val RESIZE_VERSION = 3

    /** First version whose records may carry several messages (the host packs them). */
    const val STREAM_VERSION = 4
    /** The first version whose `Commit` names the absolute line at the top of the screen. */
    const val LINE_VERSION = 5
    /** The first version whose `Sessions` entries carry the shell's working directory. */
    const val CWD_VERSION = 6

    /**
     * The first version that can start and end shells, not only view them (version 7).
     *
     * Version 6 and older gave the phone `List`, `Attach`, `Input`, `Key`, `Resize` and `Detach`, so
     * over the radio a phone could type into the shells the desk had started and could not start one
     * of its own, could not end one, and could not put one on the desk. That is the whole difference
     * between the radio route and the tailnet launcher, and it is why this list had no `+ shell`.
     */
    const val SHELL_CONTROL_VERSION = 7

    /** The first version whose `Sessions` entries carry who started the shell (version 7). */
    const val ORIGIN_VERSION = 7

    /**
     * The first version that says **who holds a shell now**, and lets a phone take one from the
     * desk (version 8).
     *
     * A phone that attached to a shell a desk window was watching took it silently: the claim
     * displaced the window and the pty was fitted to the phone. The tailnet route has always asked
     * instead — `HeldBy`, then a takeover the user chooses — and version 8 gives the radio route
     * the same words: the holder rides each `Sessions` entry, a plain `Attach` on a held shell is
     * refused with `Closed { HELD }`, and `Takeover` is the phone saying yes.
     */
    const val OWNER_VERSION = 8

    /** Holder codes on the wire: one byte, because the vocabulary is five words. */
    object Holder {
        const val NONE = 0
        const val PHONE = 1
        const val DESKTOP = 2
        const val LOCAL = 3
        const val MESH = 4
    }
    /** Most key names in one `Key` message. */
    const val MAX_KEYS = 16
    const val MAX_BODY = 300
    // 134, not 130: a version-7 entry is `2 + id 40 + label 24 + cwd 61 + origin 7`, and a page
    // must hold at least one entry. The message is 137 bytes, inside one record's 143.
    const val MAX_ROWS_BODY = 134
    const val MAX_MESSAGE = 1 + 2 + MAX_ROWS_BODY
    /** 172 - 24 (ADR-0020) - 5 (ARQ header incl. the per-record mark): one record's capacity. */
    const val RECORD_PAYLOAD = 143
    const val MAX_INPUT = 96
    const val MAX_SEGMENT = 120
    const val MAX_SELECTOR = 40
    /** Longest shell label in a `Sessions` entry. */
    const val MAX_LABEL = 24
    /** Longest shell working directory in a `Sessions` entry: worst case 3 + 40 + 24 + 60 < 130. */
    const val MAX_CWD = 60

    /** The longest origin word on the wire: `desktop` is the longest of phone/desktop/local. */
    const val MAX_ORIGIN = 7
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

        /**
         * Another device holds the shell and the host refused to displace it (version 8).
         *
         * The answer to a plain `Attach` on a shell a desk window is watching. The screen says
         * so and offers `take it back`, which is [`MeshTerm.Takeover`].
         */
        const val HELD = 6
    }

    /** `Commit.flags`. */
    object CommitFlags {
        const val ALTERNATE = 1 shl 0
        const val DEAD = 1 shl 1

        /**
         * The host says the pane has stopped changing; this commit carries no rows.
         *
         * It exists so a wait for an answer ends when the answer ends rather than [QUIET_MS]
         * later. Hosts that do not send it are covered by the quiet timer, and a decoder that
         * does not know the bit ignores it (only the bits it reads are meaningful).
         */
        const val IDLE = 1 shl 2
    }

    internal const val T_ATTACH = 0x01
    internal const val T_INPUT = 0x02
    internal const val T_RESYNC = 0x03
    internal const val T_DETACH = 0x04
    internal const val T_LIST = 0x05
    internal const val T_KEY = 0x06
    internal const val T_RESIZE = 0x07
    internal const val T_CREATE = 0x08
    internal const val T_CLOSE = 0x09
    internal const val T_MAKE_LIVE = 0x0a
    internal const val T_CREATED = 0x86
    internal const val T_TAKEOVER = 0x0b
    internal const val T_SESSIONS = 0x85

    /** The marks table (host to phone, version 9). */
    internal const val T_MARKS = 0x87

    /** The first version whose host sends [TermMessage.Marks]. */
    const val MARKS_VERSION = 9

    /**
     * Version 10: each `Sessions` entry carries the **shell** it was created with (`bash`).
     *
     * A row is named after the conversation and its second line says where it is; the shell belongs
     * on that line too, because a folder alone does not say what kind of shell is inside it.
     */
    const val SHELL_VERSION = 10
    internal const val T_PROFILE = 0x81
    internal const val T_ROWS = 0x82
    internal const val T_COMMIT = 0x83
    internal const val T_CLOSED = 0x84
}

/**
 * One row of [TermMessage.Marks]: a shell and the word about it.
 *
 * [kind] is [MeshTerm.DONE] when an agent finished a turn and [MeshTerm.ATTENTION] when a shell
 * wants a look. The note a tailnet buzz may carry is not sent over the radio, where the budget is
 * records and a row label is enough.
 */
data class MarkEntry(val id: String, val kind: Int)

/** The two marks, as the app draws them. */
enum class ShellMark(val wire: Int) {
    /** An agent finished a turn. */
    Done(0),

    /** The shell wants a look: a question, an error, a prompt waiting. */
    Attention(1),
    ;

    companion object {
        fun of(wire: Int): ShellMark? = entries.firstOrNull { it.wire == wire }
    }
}

/**
 * One running shell in a `Sessions` reply. [id] is usable as an `Attach` selector, [cwd] is where
 * the shell is (version 6; empty from a version-5 host, which does not send the field at all).
 */
data class SessionEntry(
    val id: String,
    val label: String,
    val cwd: String = "",
    /**
     * Who started the shell: `phone`, `desktop` or `local` (version 7; empty from a version-6 host).
     *
     * The mesh launcher needs the same fact the tailnet one has: a shell this phone opened keeps its
     * close and open-on-desktop actions after the app restarts instead of passing for a desk shell.
     */
    val origin: String = "",
    /** The shell the session was created with, as a bare name (`bash`), version 10. */
    val shell: String = "",
    /**
     * Who holds it now: `desktop`, `phone`, `mesh`, or empty for nobody (version 8).
     *
     * A shell with a holder other than this phone is one it must ask to take. The list shows that,
     * and the attach is refused until the user chooses `take it back`.
     */
    val owner: String = "",
) {
    /** Whether this phone started the shell, so the row may offer to end it or put it on the desk. */
    val openedByPhone: Boolean get() = origin == "phone"

    /**
     * Whether another device is watching this shell right now.
     *
     * A shell held by the desk is one this phone must ask to take: the attach is refused with
     * `Closed { HELD }` and the screen offers `take it back`, which is `Takeover`.
     */
    val heldElsewhere: Boolean get() = owner.isNotEmpty() && owner != "mesh" && owner != "phone"
}

/** A holder or origin word as its one-byte wire code. */
private fun holderCode(word: String): Int = when (word) {
    "phone" -> MeshTerm.Holder.PHONE
    "desktop" -> MeshTerm.Holder.DESKTOP
    "mesh" -> MeshTerm.Holder.MESH
    "" -> MeshTerm.Holder.NONE
    else -> MeshTerm.Holder.LOCAL
}

/** The word for a holder code, for the rows the app draws. */
internal fun holderWord(code: Int): String = when (code) {
    MeshTerm.Holder.PHONE -> "phone"
    MeshTerm.Holder.DESKTOP -> "desktop"
    MeshTerm.Holder.MESH -> "mesh"
    MeshTerm.Holder.LOCAL -> "local"
    else -> ""
}

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
    /**
     * The screen is settled. [firstLine] is the host's absolute line number for row 0 (version 5),
     * which is the only way a phone can tell that rows leaving the top went into history rather
     * than being rewritten: without it a local scrollback has to guess by matching content, and a
     * guess is wrong exactly when the content repeats.
     */
    data class Commit(
        val epoch: Int,
        val cursorX: Int,
        val cursorY: Int,
        val flags: Int,
        val firstLine: Int = 0,
    ) : TermMessage
    data class Closed(val reason: Int) : TermMessage
    /** Phone to host: which shells are running? */
    data object ListRequest : TermMessage
    /**
     * Host to phone: one page of the answer; [last] marks the final page (an empty list is one
     * empty last page). [cwd] says the entries carry their working directory (version 6); the host
     * sets it only for a phone that attached with [MeshTerm.CWD_VERSION] or later.
     */
    data class Sessions(
        val last: Boolean,
        val entries: List<SessionEntry>,
        val cwd: Boolean = false,
        val origin: Boolean = false,
        val owner: Boolean = false,
        /** The entries carry their shell (version 10). */
        val shell: Boolean = false,
    ) : TermMessage

    /**
     * Host to phone: the marks that are true now, one per shell (version 9).
     *
     * The radio route replays rather than pushes: the host cannot reach a phone that is not
     * connected, and keeping a session up just to be reachable would cost airtime for something the
     * list already says. Sent on attach, drawn beside the rows, forgotten when the shell is opened.
     * A host below version 9 never sends it, which reads as "no marks".
     */
    /**
     * A page of the mark table.
     *
     * [first] opens a run of pages and [last] closes it: a page that is not [last] is appended to
     * what has arrived, and the page that is [last] replaces the table. A body is capped at 134
     * bytes and an entry is its id plus two, so a dozen marked shells need more than one message —
     * and replacing the table per page would show only the last page.
     */
    data class Marks(
        val first: Boolean,
        val last: Boolean,
        val entries: List<MarkEntry>,
    ) : TermMessage

    /** Phone asks to attach to a shell somebody else holds, displacing them (version 8). */
    data class Takeover(val selector: String) : TermMessage

    /**
     * Phone asks the host to start a shell in [cwd] (empty = the host's default). Version 7 and later
     * only; a version-6 host answers `Closed { VERSION }` rather than starting a shell the phone
     * cannot then refer to.
     */
    data class Create(val version: Int, val cwd: String) : TermMessage

    /**
     * Phone asks the host to end that shell, or to put it down when a desk window holds it: the
     * host's `close`, exactly as the tailnet client's is (ADR-0010). Version 7 and later only.
     */
    data class Close(val selector: String) : TermMessage

    /** Phone asks the host to open that shell in a desk window. Version 7 and later only. */
    data class MakeLive(val selector: String) : TermMessage

    /** Host: the shell `Create` just made, so the phone attaches to that one and not to a guess. */
    data class Created(val id: String) : TermMessage
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

private fun u32(v: Int, what: String): List<Byte> =
    listOf(
        (v and 0xff).toByte(),
        ((v ushr 8) and 0xff).toByte(),
        ((v ushr 16) and 0xff).toByte(),
        ((v ushr 24) and 0xff).toByte(),
    )

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
            // Only when the host has line identity: an older peer reads five bytes and would
            // mistake four more for the start of the next record.
            if (firstLine > 0) body.addAll(u32(firstLine, "firstLine"))
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
        is TermMessage.Create -> {
            val cwdBytes = cwd.encodeToByteArray()
            if (cwdBytes.size > MeshTerm.MAX_CWD) throw TermEncodeException("cwd too long")
            body.add(u8(version, "version"))
            cwdBytes.forEach { body.add(it) }
            MeshTerm.T_CREATE
        }
        is TermMessage.Close -> {
            val sel = selector.encodeToByteArray()
            if (sel.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("selector too long")
            sel.forEach { body.add(it) }
            MeshTerm.T_CLOSE
        }
        is TermMessage.Takeover -> {
            val sel = selector.encodeToByteArray()
            if (sel.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("selector too long")
            sel.forEach { body.add(it) }
            MeshTerm.T_TAKEOVER
        }
        is TermMessage.MakeLive -> {
            val sel = selector.encodeToByteArray()
            if (sel.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("selector too long")
            sel.forEach { body.add(it) }
            MeshTerm.T_MAKE_LIVE
        }
        is TermMessage.Created -> {
            val i = id.encodeToByteArray()
            if (i.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("id too long")
            body.add(i.size.toByte()); i.forEach { body.add(it) }
            MeshTerm.T_CREATED
        }
        is TermMessage.Marks -> {
            body.add(((if (first) 1 else 0) or (if (last) 2 else 0)).toByte())
            for (e in entries) {
                val i = e.id.encodeToByteArray()
                if (i.size > MeshTerm.MAX_SELECTOR) throw TermEncodeException("mark id too long")
                body.add(u8(e.kind, "kind"))
                body.add(i.size.toByte()); i.forEach { body.add(it) }
            }
            MeshTerm.T_MARKS
        }
        is TermMessage.Sessions -> {
            body.add(
                ((if (last) 1 else 0) or (if (cwd) 2 else 0) or (if (origin) 4 else 0) or
                    (if (owner) 8 else 0) or (if (shell) 16 else 0)).toByte(),
            )
            for (e in entries) {
                val id = e.id.encodeToByteArray()
                val label = e.label.encodeToByteArray()
                val path = e.cwd.encodeToByteArray()
                val by = holderCode(e.origin)
                if (id.size > MeshTerm.MAX_SELECTOR || label.size > MeshTerm.MAX_LABEL) {
                    throw TermEncodeException("session entry too long")
                }
                if (cwd && path.size > MeshTerm.MAX_CWD) throw TermEncodeException("session path too long")

                body.add(id.size.toByte()); id.forEach { body.add(it) }
                body.add(label.size.toByte()); label.forEach { body.add(it) }
                if (cwd) { body.add(path.size.toByte()); path.forEach { body.add(it) } }
                if (origin) body.add(by.toByte())
                if (owner) body.add(holderCode(e.owner).toByte())
                if (shell) {
                    val shellName = e.shell.encodeToByteArray()
                    if (shellName.size > MeshTerm.MAX_LABEL) throw TermEncodeException("shell name too long")
                    body.add(shellName.size.toByte()); shellName.forEach { body.add(it) }
                }
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

    /** A line number. Four bytes, little endian, and the phone keeps it as an `Int`. */
    private fun le32(b: ByteArray, i: Int): Int =
        (b[i].toInt() and 0xff) or
            ((b[i + 1].toInt() and 0xff) shl 8) or
            ((b[i + 2].toInt() and 0xff) shl 16) or
            ((b[i + 3].toInt() and 0xff) shl 24)
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
            MeshTerm.T_COMMIT -> when (body.size) {
                // Version 4 and older: no line number, and a phone on it is told `Scrollback` is
                // absent, so nothing is lost.
                5 -> ok(TermMessage.Commit(le16(body, 0), b(body, 2), b(body, 3), b(body, 4)))
                9 -> ok(
                    TermMessage.Commit(
                        le16(body, 0), b(body, 2), b(body, 3), b(body, 4),
                        le32(body, 5),
                    ),
                )
                else -> Parsed.Bad
            }
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
            MeshTerm.T_CREATE -> {
                if (body.isEmpty()) return Parsed.Bad
                val version = b(body, 0)
                if (body.size - 1 > MeshTerm.MAX_CWD) return Parsed.Bad
                val cwd = utf8(body.copyOfRange(1, body.size)) ?: return Parsed.Bad
                ok(TermMessage.Create(version, cwd))
            }
            MeshTerm.T_TAKEOVER -> {
                if (body.size > MeshTerm.MAX_SELECTOR) return Parsed.Bad
                val selector = utf8(body) ?: return Parsed.Bad
                ok(TermMessage.Takeover(selector))
            }
            MeshTerm.T_CLOSE, MeshTerm.T_MAKE_LIVE -> {
                if (body.size > MeshTerm.MAX_SELECTOR) return Parsed.Bad
                val selector = utf8(body) ?: return Parsed.Bad
                if (tag == MeshTerm.T_CLOSE) ok(TermMessage.Close(selector)) else ok(TermMessage.MakeLive(selector))
            }
            MeshTerm.T_CREATED -> {
                if (body.isEmpty()) return Parsed.Bad
                val n = b(body, 0)
                if (n > MeshTerm.MAX_SELECTOR || body.size - 1 != n) return Parsed.Bad
                val id = utf8(body.copyOfRange(1, body.size)) ?: return Parsed.Bad
                ok(TermMessage.Created(id))
            }
            MeshTerm.T_MARKS -> {
                if (body.isEmpty()) return Parsed.Bad
                val flags = body[0].toInt()
                val entries = ArrayList<MarkEntry>()
                var p = 1
                while (p < body.size) {
                    if (body.size - p < 2) return Parsed.Bad
                    val kind = b(body, p)
                    val n = b(body, p + 1)
                    if (n > MeshTerm.MAX_SELECTOR || body.size - (p + 2) < n) return Parsed.Bad
                    val id = utf8(body.copyOfRange(p + 2, p + 2 + n)) ?: return Parsed.Bad
                    entries += MarkEntry(id, kind)
                    p += 2 + n
                }
                ok(TermMessage.Marks(first = flags and 1 != 0, last = flags and 2 != 0, entries = entries))
            }
            MeshTerm.T_SESSIONS -> {
                if (body.isEmpty()) return Parsed.Bad
                val withCwd = body[0].toInt() and 2 != 0
                val withOrigin = body[0].toInt() and 4 != 0
                val withOwner = body[0].toInt() and 8 != 0
                val withShell = body[0].toInt() and 16 != 0
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
                    val cwd = if (withCwd) field(MeshTerm.MAX_CWD) ?: return Parsed.Bad else ""
                    val origin = if (withOrigin) {
                        if (p >= body.size) return Parsed.Bad
                        holderWord(b(body, p)).also { p += 1 }
                    } else ""
                    val owner = if (withOwner) {
                        if (p >= body.size) return Parsed.Bad
                        holderWord(b(body, p)).also { p += 1 }
                    } else ""
                    val shell = if (withShell) field(MeshTerm.MAX_LABEL) ?: return Parsed.Bad else ""
                    entries += SessionEntry(id, label, cwd, origin, shell, owner)
                }
                ok(
                    TermMessage.Sessions(
                        last = body[0].toInt() and 1 == 1,
                        entries = entries,
                        cwd = withCwd,
                        origin = withOrigin,
                        owner = withOwner,
                        shell = withShell,
                    ),
                )
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
    /**
     * The host's absolute line number for row 0, from the last `Commit` (0 when the host has no
     * line identity). A screen that scrolls by N advances this by N: rows that leave the top are
     * history the phone already holds, and rows it never received are a gap it can name.
     */
    var firstLine = 0
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
                // A profile ends whatever frame was open: a `Rows` that arrived before it belongs
                // to the geometry being replaced, and painting it over the new pane put stale text
                // on a row the host says is blank (and nothing would ever correct it).
                hasPending = false; pending = ArrayList()
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
                firstLine = m.firstLine
                return true
            }
            is TermMessage.Closed -> { closed = m.reason; return false }
            else -> return false
        }
    }
}
