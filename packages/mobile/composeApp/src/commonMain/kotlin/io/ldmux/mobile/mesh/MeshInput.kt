package io.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.ldmux.mobile.Modifiers
import io.ldmux.mobile.StickyKey
import io.ldmux.mobile.TerminalHandle
import io.ldmux.mobile.modifiedKey
import io.ldmux.mobile.ModifiedKey
import io.ldmux.mobile.spendLatchOnFirst

/** One piece the pump hands the driver: typed text as bytes, or named keys. One record each. */
sealed interface MeshOut {
    class Text(val bytes: ByteArray) : MeshOut {
        override fun equals(other: Any?) = other is Text && bytes.contentEquals(other.bytes)
        override fun hashCode() = bytes.contentHashCode()
        override fun toString() = "Text(${bytes.decodeToString()})"
    }
    /** Key names for the host to encode for the pane's own mode (see [TermMessage.Key]). */
    data class Keys(val names: List<String>) : MeshOut
}

/**
 * Keystrokes turned into `Input` and `Key` records: **coalesced**, never one send per key.
 *
 * Typed text leaves as bytes (`Input`). A named key (`Up`, `Enter`, `C-c`) leaves as its name
 * (`Key`): the host owns the one key table and knows the pane's cursor-key and keyboard mode, which
 * this side cannot see. Text and keys stay in the order they were typed, so `ls`, `Enter`, `Up`
 * reaches the shell as that and not as whatever order two queues drain in.
 *
 * The link carries a record in about 600 ms at best and the hourly budget is a few dozen records,
 * so a record per keystroke would spend the hour on a sentence. Pieces are at most
 * [MeshTerm.MAX_INPUT] bytes (one record, ADR-0025). When a piece leaves:
 *
 *  - **at once** when anything waiting is a line end (CR, `Enter`) or an interrupt (ETX, `C-c`):
 *    the user has finished a thought, and an interrupt that waits is not one;
 *  - **at once** when a full record's worth has collected;
 *  - **after [IDLE_MS] of quiet**, so typing without pressing Enter is never silent;
 *  - **after [MAX_HOLD_MS]** from the first waiting byte, so continuous typing still goes out.
 *
 * Both windows are close together on purpose. The old pair (700 / 2500) made the link feel
 * inconsistent rather than slow: a pause of a moment sent one character in a frame of its own,
 * while fast typing accumulated for seconds. A tight ceiling bounds how stale the pane can be,
 * and a short idle joins the last few characters into the same frame. The floor is physical —
 * one frame each way is about 48 ms at 500 kHz SF5 — so the point is to be *regular*, not to
 * chase zero.
 *
 * A backspace that meets an ordinary printable character still waiting here removes it instead
 * of being sent: a typo fixed before it left costs nothing on the air.
 *
 * Pure and clock-injected: nothing here sleeps, so the rule is tested without time.
 */
class MeshInputBatcher(
    private val maxBytes: Int = MeshTerm.MAX_INPUT,
    private val capacity: Int = CAPACITY,
) {
    private sealed interface Run {
        class Text(var bytes: ByteArray) : Run
        class Keys(val names: ArrayList<String>) : Run
    }

    private val runs = ArrayList<Run>()
    private var size = 0
    private var firstAt = 0L
    private var lastAt = 0L
    private var urgent = false

    /** What is waiting, in order, for the screen to show (typing is not silent while it waits). */
    val pending: List<MeshOut>
        get() = runs.map {
            when (it) {
                is Run.Text -> MeshOut.Text(it.bytes.copyOf())
                is Run.Keys -> MeshOut.Keys(it.names.toList())
            }
        }

    val isEmpty: Boolean get() = runs.isEmpty()

    /** Throw away everything waiting. The one caller is the public-channel mode's "discard". */
    fun clear() {
        runs.clear()
        size = 0
        urgent = false
        firstAt = 0L
        lastAt = 0L
    }

    /** Add typed text. False, and nothing added, when it would pass [capacity]: the caller says so. */
    fun appendText(bytes: ByteArray, nowMs: Long): Boolean {
        if (bytes.isEmpty()) return true
        if (size + bytes.size > capacity) return false
        touch(nowMs)
        val last = runs.lastOrNull()
        if (last is Run.Text) last.bytes += bytes else runs += Run.Text(bytes.copyOf())
        size += bytes.size
        if (bytes.any { it == CR || it == ETX }) urgent = true
        return true
    }

    /** Add a named key. False, and nothing added, when it would pass [capacity]. */
    fun appendKey(name: String, nowMs: Long): Boolean {
        if (!validKeyName(name)) return true // not a name the host could know: nothing to send
        val last = runs.lastOrNull()
        if (name == "BSpace" && last is Run.Text && isPlain(last.bytes.last())) {
            last.bytes = last.bytes.copyOf(last.bytes.size - 1)
            size -= 1
            if (last.bytes.isEmpty()) runs.removeAt(runs.lastIndex)
            lastAt = nowMs
            if (runs.isEmpty()) urgent = false
            return true
        }
        val cost = name.length + 1
        if (size + cost > capacity) return false
        touch(nowMs)
        if (last is Run.Keys && last.names.size < MeshTerm.MAX_KEYS &&
            last.names.sumOf { it.length + 1 } + name.length <= MeshTerm.MAX_INPUT
        ) {
            last.names += name
        } else {
            runs += Run.Keys(arrayListOf(name))
        }
        size += cost
        if (name == "Enter" || name == "C-c") urgent = true
        return true
    }

    private fun touch(nowMs: Long) {
        if (runs.isEmpty()) firstAt = nowMs
        lastAt = nowMs
    }

    /** True when [peek] would return a piece now. */
    fun due(nowMs: Long): Boolean = when {
        runs.isEmpty() -> false
        urgent || size >= maxBytes -> true
        nowMs - lastAt >= IDLE_MS -> true
        nowMs - firstAt >= MAX_HOLD_MS -> true
        else -> false
    }

    /** When [due] becomes true by waiting alone, or null when nothing is waiting. */
    fun nextDueMs(): Long? = when {
        runs.isEmpty() -> null
        urgent || size >= maxBytes -> 0L
        else -> minOf(lastAt + IDLE_MS, firstAt + MAX_HOLD_MS)
    }

    /**
     * The next piece, at most one record, not splitting a UTF-8 character; null when nothing is due.
     * It is **not removed**: [accepted] removes it once the driver took it, so a full queue keeps
     * it for the next try.
     */
    fun peek(nowMs: Long): MeshOut? {
        if (!due(nowMs)) return null
        return when (val r = runs.first()) {
            is Run.Keys -> MeshOut.Keys(r.names.toList())
            is Run.Text -> {
                var n = minOf(r.bytes.size, maxBytes)
                if (n < r.bytes.size) {
                    // Back off to the start of a character: a continuation byte is 10xxxxxx.
                    while (n > 1 && (r.bytes[n].toInt() and 0xC0) == 0x80) n--
                }
                MeshOut.Text(r.bytes.copyOf(n))
            }
        }
    }

    /** The piece [peek] returned was taken by the driver. */
    fun accepted(piece: MeshOut) {
        val r = runs.firstOrNull() ?: return
        when {
            piece is MeshOut.Keys && r is Run.Keys -> {
                size -= r.names.sumOf { it.length + 1 }
                runs.removeAt(0)
            }
            piece is MeshOut.Text && r is Run.Text -> {
                val n = minOf(piece.bytes.size, r.bytes.size)
                r.bytes = r.bytes.copyOfRange(n, r.bytes.size)
                size -= n
                if (r.bytes.isEmpty()) runs.removeAt(0)
            }
        }
        // What is left waits its own idle window from now; a line end still waiting stays urgent.
        urgent = runs.any {
            when (it) {
                is Run.Text -> it.bytes.any { b -> b == CR || b == ETX }
                is Run.Keys -> it.names.any { n -> n == "Enter" || n == "C-c" }
            }
        }
    }

    private fun isPlain(b: Byte): Boolean = b.toInt() in 0x20..0x7e

    companion object {
        /** Quiet that means "done typing for now": about one round trip at the default preset. */
        const val IDLE_MS = 100L
        /** The longest a byte waits, however steadily the user types. */
        const val MAX_HOLD_MS = 200L
        /**
         * The most text that may wait unsent.
         *
         * **This is a memory bound, not an airtime one.** A `Input` message carries at most 96 bytes
         * and the pump splits a longer paste across records by itself, so a long command was never
         * one packet — the old 1 KB cap refused a paste that could be sent perfectly well, and told
         * the user "input refused: the held text is at its limit" for a limit that existed only to
         * keep the queue small. The cost that matters is records, and the bar states it in records
         * before the press (`about N records`), which is the honest place to see it.
         */
        const val CAPACITY = 65_536
        private const val CR: Byte = 0x0D
        private const val ETX: Byte = 0x03
    }
}

/** What one [MeshInputPump.step] did. */
enum class PumpStep { Idle, Sent, Waiting, Blocked }

/**
 * Moves due keystrokes from the batcher to the driver, one record per step. A step never sleeps
 * and never retries by itself: [Waiting] means the session's queue was full (the piece stays put),
 * [Blocked] means the terminal is not live (it stays put too, and shows as pending).
 */
class MeshInputPump(
    private val batcher: MeshInputBatcher,
    private val send: suspend (MeshOut) -> InputOutcome,
    private val clock: () -> Long,
) {
    suspend fun step(): PumpStep {
        val piece = batcher.peek(clock()) ?: return PumpStep.Idle
        return when (send(piece)) {
            InputOutcome.Accepted -> { batcher.accepted(piece); PumpStep.Sent }
            InputOutcome.QueueFull -> PumpStep.Waiting
            InputOutcome.NotLive -> PumpStep.Blocked
            // Neither can happen to a non-empty piece within the cap; drop it rather than loop on it.
            InputOutcome.TooLong, InputOutcome.Empty -> { batcher.accepted(piece); PumpStep.Sent }
        }
    }
}

/**
 * The mesh route's [TerminalHandle]: what the shared key bar drives. A key joins the batcher **by
 * name**, as it does on the tailnet, and the host turns it into bytes for the pane's own mode; the
 * sticky latch is the same one the tailnet handle applies, spent by the same rule ([modifiedKey],
 * [spendLatchOnFirst]).
 *
 * Everything the tailnet handle does for a surface (selection, scrollback, mouse, clipboard,
 * ownership) is absent on this link and is left at the interface's no-op defaults. The host still
 * says which of them it lacks - [absentFeatures] decodes it and the driver keeps it on the view -
 * but the launcher no longer prints the list above the shells, because a limit that applies to
 * every shell is not news about the one being opened.
 */
class MeshTerminalHandle(
    private val batcher: MeshInputBatcher,
    private val clock: () -> Long,
    /** Called after something was added, so the pump looks again now rather than at its next tick. */
    private val onQueued: () -> Unit = {},
) : TerminalHandle {
    override var modifiers: Modifiers by mutableStateOf(Modifiers())
        private set

    /** What is waiting to be sent, as readable text (CR as ⏎, ESC as ␛, other controls as ^X, keys by name). */
    var pendingLabel: String by mutableStateOf("")
        private set

    /** Set when the last thing typed was refused for being too much; cleared by the next accepted key. */
    var overflow: Boolean by mutableStateOf(false)
        private set

    override fun toggleModifier(which: StickyKey) { modifiers = modifiers.toggled(which) }
    override fun clearModifiers() { modifiers = Modifiers() }

    override fun sendKey(name: String) {
        val armed = modifiers
        if (armed.armed) {
            modifiers = Modifiers()
            sendModifiedKey(name, armed)
            return
        }
        pushKey(name)
    }

    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        when (val k = modifiedKey(key, modifiers)) {
            ModifiedKey.None -> {}
            is ModifiedKey.Literal -> pushText(k.bytes)
            is ModifiedKey.Named -> pushKey(k.name)
        }
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        val armed = modifiers
        if (!armed.armed) { pushText(text.replace('\n', '\r')); return }
        val typed = spendLatchOnFirst(text, armed)
        modifiers = typed.latchAfter
        if (typed.head.isEmpty()) {
            pushText(typed.rest.replace('\n', '\r'))
        } else {
            sendModifiedKey(typed.head, typed.headModifiers)
            if (typed.rest.isNotEmpty()) pushText(typed.rest.replace('\n', '\r'))
        }
    }

    /** A paste is text that must fit what can wait; a refusal is returned, never half-sent. */
    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        return pushText(text.replace("\r\n", "\r").replace('\n', '\r'))
    }

    private fun pushText(text: String): Boolean = settle(batcher.appendText(text.encodeToByteArray(), clock()))

    /** One named key with no sticky modifier applied or spent (a tap's cursor walk is not a keystroke). */
    fun sendPlainKey(name: String): Boolean = pushKey(name)

    private fun pushKey(name: String): Boolean = settle(batcher.appendKey(name, clock()))

    private fun settle(ok: Boolean): Boolean {
        overflow = !ok
        // **Everything the bar shows, at the moment the key is typed.** This used to refresh only
        // the literal line and leave the summary and the echo to the pump's 120 ms poll, so a fast
        // typist saw the echo stutter a sixth of a second behind their fingers and the count go
        // stale. The pump still refreshes after it sends; this is the typing half.
        refreshPending()
        if (ok) onQueued()
        return ok
    }

    /**
     * Whether anything is waiting. The public-channel mode's "sync" asks this, and the answer
     * decides whether there is anything to send at all: an empty sync only shows the view.
     */
    val hasPending: Boolean get() = !batcher.isEmpty

    /**
     * What is waiting, counted: typed characters, named keys, and the records it will cost.
     *
     * The record count is the number the user cannot otherwise see and the one that matters on
     * this link: an `Input` record carries [MeshTerm.MAX_INPUT] bytes and the hour has about 58
     * records, so "12 characters" and "12 characters, 1 key, about 2 records" are different
     * promises. Counted as the driver will send it, not as bytes divided by a hopeful number.
     */
    var heldSummary: String by mutableStateOf("")
        private set

    /** Throw away everything waiting. The public-channel mode's "discard", and nothing else. */
    fun discardHeld() {
        batcher.clear()
        overflow = false
        pendingLabel = ""
        heldSummary = ""
        echoText = ""
    }

    /**
     * What the held input should look like **as characters**, for the local echo.
     *
     * Empty while nothing is held. This is the only place the phone renders a key name as
     * something a terminal can show, and it is deliberately small:
     *
     *  - typed text and pasted text are themselves;
     *  - `Enter` is a line end (`\r\n`), because that is what the user will see when it runs;
     *  - `Tab` and `BSpace` are their control characters, which the emulator already draws
     *    correctly (a carriage move and an erase);
     *  - everything else — arrows, `Escape`, `C-c`, `Home`, `End` — renders as **nothing**.
     *
     * That last line is the honest one. An arrow key sent to a shell is a request the shell
     * interprets: `Up` may recall history, `Left` may move the prompt's cursor, and `C-c` may
     * cancel the line. The emulator has no shell in it, so painting any of those locally would
     * *invent* an effect that has not happened. They are counted in [heldSummary] instead, which
     * says "1 key" without pretending to know what it will do.
     */
    var echoText: String by mutableStateOf("")
        private set

    /**
     * Drop everything held, and every rendering of it.
     *
     * **Held text is the one plaintext this app keeps in memory**, and it must not outlive the
     * moment the user is looking at it. It was never persisted (no `rememberSaveable` anywhere),
     * but nothing scrubbed it when the app went to the background either, so a password typed in
     * manual-sync mode and not yet sent stayed in the heap while the app sat in the recents
     * thumbnail. The pairing keys are encrypted at rest; the text waiting to be typed into a
     * shell should at least not survive the screen going away.
     */
    fun scrub() {
        batcher.clear()
        pendingLabel = ""
        heldSummary = ""
        echoText = ""
    }

    /** The pump calls this after it sent a piece, so the line under the screen shrinks. */
    fun refreshPending() {
        pendingLabel = readable(batcher.pending)
        heldSummary = summarize(batcher.pending)
        echoText = echoable(batcher.pending)
    }

    private fun echoable(items: List<MeshOut>): String = buildString {
        for (item in items) when (item) {
            is MeshOut.Text -> append(item.bytes.decodeToString())
            is MeshOut.Keys -> for (n in item.names) {
                when (n) {
                    "Enter" -> append("\r\n")
                    "Tab" -> append('\t')
                    "BSpace" -> append("\b \b")
                    else -> {}
                }
            }
        }
    }

    private fun summarize(items: List<MeshOut>): String {
        if (items.isEmpty()) return ""
        var chars = 0
        var keys = 0
        var records = 0
        for (item in items) when (item) {
            is MeshOut.Text -> {
                chars += item.bytes.decodeToString().length
                records += recordsFor(item.bytes)
            }
            is MeshOut.Keys -> {
                keys += item.names.size
                records += 1
            }
        }
        val parts = buildList {
            if (chars > 0) add(if (chars == 1) "1 character" else "$chars characters")
            if (keys > 0) add(if (keys == 1) "1 key" else "$keys keys")
        }
        return parts.joinToString(" and ") + ", " + if (records == 1) "1 record" else "about $records records"
    }

    /**
     * The records [bytes] will cost, walking the same slice rule [peek] uses.
     *
     * Not `(size + 95) / 96`: [peek] backs off to a character boundary, so a 96-byte run ending in
     * the middle of a character becomes a 95-byte piece and a second piece — two records where the
     * division promises one. "About 2 records" being 3 is the kind of promise this app would rather
     * keep, and the count is the one number the user cannot otherwise see.
     */
    private fun recordsFor(bytes: ByteArray): Int {
        var left = bytes.size
        var records = 0
        while (left > 0) {
            var take = minOf(left, MeshTerm.MAX_INPUT)
            if (take < left) {
                // A continuation byte (10xxxxxx) cannot start a piece: back off, as `peek` does.
                while (take > 1 && (bytes[take].toInt() and 0xC0) == 0x80) take--
            }
            left -= take
            records++
        }
        return records
    }

    private fun readable(items: List<MeshOut>): String = buildString {
        for (item in items) when (item) {
            is MeshOut.Text -> for (x in item.bytes.decodeToString()) {
                when {
                    x == '\r' -> append('⏎')
                    x == '\u001b' -> append('␛')
                    x == '\t' -> append('⇥')
                    x.code < 0x20 -> append('^').append((x.code + 0x40).toChar())
                    x.code == 0x7f -> append('␡')
                    else -> append(x)
                }
            }
            is MeshOut.Keys -> for (n in item.names) append(keyGlyph(n))
        }
    }

    private fun keyGlyph(name: String): String = when {
        name == "Enter" -> "⏎"
        name == "Escape" -> "␛"
        name == "Tab" -> "⇥"
        name == "BSpace" -> "␡"
        name.length == 3 && name.startsWith("C-") -> "^" + name[2].uppercaseChar()
        else -> "‹$name›"
    }
}
