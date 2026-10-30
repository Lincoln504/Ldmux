package dev.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.Modifiers
import dev.ldmux.mobile.StickyKey
import dev.ldmux.mobile.TerminalHandle
import dev.ldmux.mobile.modifiedKey
import dev.ldmux.mobile.ModifiedKey
import dev.ldmux.mobile.spendLatchOnFirst

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
        const val IDLE_MS = 700L
        /** The longest a byte waits, however steadily the user types. */
        const val MAX_HOLD_MS = 2_500L
        /** More than this unsent is refused rather than queued for an hour. */
        const val CAPACITY = 1_024
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
 * ownership) is absent on this link and is left at the interface's no-op defaults: the host's
 * `absent` list says so on screen.
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
        pendingLabel = readable(batcher.pending)
        if (ok) onQueued()
        return ok
    }

    /** The pump calls this after it sent a piece, so the line under the screen shrinks. */
    fun refreshPending() { pendingLabel = readable(batcher.pending) }

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
