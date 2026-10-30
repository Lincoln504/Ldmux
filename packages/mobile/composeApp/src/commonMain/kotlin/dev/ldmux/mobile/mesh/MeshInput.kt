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

/**
 * Keystrokes turned into `Input` records: **coalesced**, never one send per key.
 *
 * The link carries a record in about 600 ms at best and the hourly budget is a few dozen records,
 * so a record per keystroke would spend the hour on a sentence. Bytes collect here and leave in
 * pieces of at most [MeshTerm.MAX_INPUT] (one record, ADR-0025). When a piece leaves:
 *
 *  - **at once** when it holds a line end (CR) or an interrupt (ETX): the user has finished a
 *    thought, and an interrupt that waits is not one;
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
    private var buf = ByteArray(0)
    private var firstAt = 0L
    private var lastAt = 0L
    private var urgent = false

    /** What is waiting, for the screen to show (typing is not silent while it waits). */
    val pending: ByteArray get() = buf.copyOf()

    val isEmpty: Boolean get() = buf.isEmpty()

    /** Add keystrokes. False, and nothing added, when it would pass [capacity]: the caller says so. */
    fun append(bytes: ByteArray, nowMs: Long): Boolean {
        if (bytes.isEmpty()) return true
        if (bytes.size == 1 && bytes[0] == DEL && buf.isNotEmpty() && isPlain(buf.last())) {
            buf = buf.copyOf(buf.size - 1)
            lastAt = nowMs
            if (buf.isEmpty()) urgent = false
            return true
        }
        if (buf.size + bytes.size > capacity) return false
        if (buf.isEmpty()) firstAt = nowMs
        buf += bytes
        lastAt = nowMs
        if (bytes.any { it == CR || it == ETX }) urgent = true
        return true
    }

    /** True when [take] would return a piece now. */
    fun due(nowMs: Long): Boolean = when {
        buf.isEmpty() -> false
        urgent || buf.size >= maxBytes -> true
        nowMs - lastAt >= IDLE_MS -> true
        nowMs - firstAt >= MAX_HOLD_MS -> true
        else -> false
    }

    /** When [due] becomes true by waiting alone, or null when nothing is waiting. */
    fun nextDueMs(): Long? = when {
        buf.isEmpty() -> null
        urgent || buf.size >= maxBytes -> 0L
        else -> minOf(lastAt + IDLE_MS, firstAt + MAX_HOLD_MS)
    }

    /**
     * The next piece, at most [maxBytes], not splitting a UTF-8 character; null when nothing is due.
     * It is **not removed**: [accepted] removes it once the driver took it, so a full queue keeps
     * the bytes for the next try.
     */
    fun peek(nowMs: Long): ByteArray? {
        if (!due(nowMs)) return null
        var n = minOf(buf.size, maxBytes)
        if (n < buf.size) {
            // Back off to the start of a character: a continuation byte is 10xxxxxx.
            while (n > 1 && (buf[n].toInt() and 0xC0) == 0x80) n--
        }
        return buf.copyOf(n)
    }

    /** The first [n] waiting bytes were taken by the driver. */
    fun accepted(n: Int) {
        buf = buf.copyOfRange(minOf(n, buf.size), buf.size)
        if (buf.isEmpty()) urgent = false
        // What is left waits its own idle window from now; a CR still waiting stays urgent.
        else urgent = buf.any { it == CR || it == ETX }
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
        private const val DEL: Byte = 0x7F
    }
}

/** What one [MeshInputPump.step] did. */
enum class PumpStep { Idle, Sent, Waiting, Blocked }

/**
 * Moves due keystrokes from the batcher to the driver, one record per step. A step never sleeps
 * and never retries by itself: [Waiting] means the session's queue was full (the bytes stay put),
 * [Blocked] means the terminal is not live (they stay put too, and show as pending).
 */
class MeshInputPump(
    private val batcher: MeshInputBatcher,
    private val send: suspend (ByteArray) -> InputOutcome,
    private val clock: () -> Long,
) {
    suspend fun step(): PumpStep {
        val piece = batcher.peek(clock()) ?: return PumpStep.Idle
        return when (send(piece)) {
            InputOutcome.Accepted -> { batcher.accepted(piece.size); PumpStep.Sent }
            InputOutcome.QueueFull -> PumpStep.Waiting
            InputOutcome.NotLive -> PumpStep.Blocked
            // Neither can happen to a non-empty piece within the cap; drop it rather than loop on it.
            InputOutcome.TooLong, InputOutcome.Empty -> { batcher.accepted(piece.size); PumpStep.Sent }
        }
    }
}

/**
 * The mesh route's [TerminalHandle]: what the shared key bar drives. A key becomes bytes
 * ([meshKeyBytes]) and joins the batcher; the sticky latch is the same one the tailnet handle
 * applies, spent by the same rule ([modifiedKey], [spendLatchOnFirst]).
 *
 * Everything the tailnet handle does for a surface (selection, scrollback, mouse, clipboard,
 * ownership) is absent on this link and is left at the interface's no-op defaults: the host's
 * `absent` list says so on screen.
 */
class MeshTerminalHandle(
    private val batcher: MeshInputBatcher,
    private val clock: () -> Long,
    /** Called after bytes were added, so the pump looks again now rather than at its next tick. */
    private val onQueued: () -> Unit = {},
) : TerminalHandle {
    override var modifiers: Modifiers by mutableStateOf(Modifiers())
        private set

    /** What is waiting to be sent, as readable text (CR as ⏎, ESC as ␛, other controls as ^X). */
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
        push(meshKeyBytes(name))
    }

    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        when (val k = modifiedKey(key, modifiers)) {
            ModifiedKey.None -> {}
            is ModifiedKey.Literal -> push(k.bytes.encodeToByteArray())
            is ModifiedKey.Named -> push(meshKeyBytes(k.name))
        }
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        val armed = modifiers
        if (!armed.armed) { push(text.replace('\n', '\r').encodeToByteArray()); return }
        val typed = spendLatchOnFirst(text, armed)
        modifiers = typed.latchAfter
        if (typed.head.isEmpty()) {
            push(typed.rest.replace('\n', '\r').encodeToByteArray())
        } else {
            sendModifiedKey(typed.head, typed.headModifiers)
            if (typed.rest.isNotEmpty()) push(typed.rest.replace('\n', '\r').encodeToByteArray())
        }
    }

    /** A paste is text that must fit what can wait; a refusal is returned, never half-sent. */
    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        return push(text.replace("\r\n", "\r").replace('\n', '\r').encodeToByteArray())
    }

    private fun push(bytes: ByteArray?): Boolean {
        if (bytes == null) return false
        val ok = batcher.append(bytes, clock())
        overflow = !ok
        pendingLabel = readable(batcher.pending)
        if (ok) onQueued()
        return ok
    }

    /** The pump calls this after it sent a piece, so the line under the screen shrinks. */
    fun refreshPending() { pendingLabel = readable(batcher.pending) }

    private fun readable(b: ByteArray): String = buildString {
        for (x in b.decodeToString()) {
            when {
                x == '\r' -> append('⏎')
                x == '\u001b' -> append('␛')
                x == '\t' -> append('⇥')
                x.code < 0x20 -> append('^').append((x.code + 0x40).toChar())
                x.code == 0x7f -> append('␡')
                else -> append(x)
            }
        }
    }
}
