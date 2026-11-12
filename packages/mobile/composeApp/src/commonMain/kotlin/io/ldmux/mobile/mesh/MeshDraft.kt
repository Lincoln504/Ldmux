package io.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.ldmux.mobile.ModifierLatch
import io.ldmux.mobile.Modifiers
import io.ldmux.mobile.ModifiedKey
import io.ldmux.mobile.StickyKey
import io.ldmux.mobile.TerminalHandle
import io.ldmux.mobile.modifiedKey
import io.ldmux.mobile.spendLatchOnFirst

/**
 * One item of a composed command, in the order the operator wrote it.
 *
 * **A command is a list of items, not a byte stream.** The host writes text at the prompt, wraps a
 * paste for the pane's bracketed-paste mode when it has one, and encodes a key name for the pane's
 * live cursor-key and Kitty mode — three different things that have to arrive in the order they
 * were typed. `docs/RADIO-PLAN.md` §6.1 and §7.
 */
sealed interface DraftItem {
    /** Typed text, written as it is. */
    class Text(val bytes: ByteArray) : DraftItem {
        override fun equals(other: Any?) = other is Text && bytes.contentEquals(other.bytes)
        override fun hashCode() = bytes.contentHashCode()
        override fun toString() = "Text(${bytes.decodeToString()})"
    }

    /** The same bytes, marked as a paste so the host can wrap them. */
    class Paste(val bytes: ByteArray) : DraftItem {
        override fun equals(other: Any?) = other is Paste && bytes.contentEquals(other.bytes)
        override fun hashCode() = bytes.contentHashCode()
        override fun toString() = "Paste(${bytes.decodeToString()})"
    }

    /** Key names (`Enter`, `Up`, `C-c`), encoded by the host for the pane's own modes. */
    data class Keys(val names: List<String>) : DraftItem
}

/**
 * What is typed but not sent.
 *
 * **This replaces the batch model's coalescing queue, not its purpose.** The old queue emitted a
 * piece by itself after a quiet window, because one keystroke was one record and the window was
 * what kept the hour affordable. In v15 a command is **one** message however long it is, so there
 * is nothing to coalesce and nothing to pace: the draft simply accumulates, and one press sends it.
 * `docs/RADIO-PLAN.md` §7, `AR-8`.
 *
 * The limits are the wire's, taken from `mesh_term`: at most [MAX_ITEMS] items, at most
 * [MAX_ITEM_BYTES] bytes in one of them, and a run of text longer than that is split at a character
 * boundary into consecutive `Text` items — which the host joins back into one write, so a pasted
 * paragraph is the same bytes whether it crossed in one item or three.
 */
class MeshDraft {
    private val items = mutableListOf<DraftItem>()

    /** True when something is waiting to be sent. */
    val isEmpty: Boolean get() = items.isEmpty()

    /** The items as composed, for a caller that wants to count them. */
    val size: Int get() = items.size

    /** A copy of what is waiting, in order. */
    fun peek(): List<DraftItem> = items.toList()

    /** Take everything, leaving the draft empty. */
    fun take(): List<DraftItem> {
        val taken = items.toList()
        items.clear()
        return taken
    }

    fun clear() = items.clear()

    /**
     * Add typed text, split only when one item would exceed the wire's own item limit.
     *
     * Returns false when the draft is already as large as the wire will carry; nothing is added in
     * that case, so a refused keystroke is refused whole rather than half-typed.
     */
    fun appendText(bytes: ByteArray): Boolean = appendRuns(bytes, paste = false)

    /** Add pasted bytes, marked so the host wraps them for the pane's paste mode. */
    fun appendPaste(bytes: ByteArray): Boolean = appendRuns(bytes, paste = true)

    /**
     * Add one named key.
     *
     * Consecutive keys share an item while there is room, which is what keeps `Tab`, `Enter` as one
     * `Keys` item rather than two: the host encodes them in one pass and the command stays one
     * message.
     */
    fun appendKey(name: String): Boolean {
        val last = items.lastOrNull()
        if (last is DraftItem.Keys && last.names.size < MAX_KEYS) {
            items[items.size - 1] = DraftItem.Keys(last.names + name)
            return true
        }
        if (items.size >= MAX_ITEMS) return false
        items.add(DraftItem.Keys(listOf(name)))
        return true
    }

    private fun appendRuns(bytes: ByteArray, paste: Boolean): Boolean {
        if (bytes.isEmpty()) return true
        var at = 0
        while (at < bytes.size) {
            val end = boundary(bytes, at)
            if (items.size >= MAX_ITEMS) {
                // Put back nothing: every byte added before this point is a complete item, and the
                // caller is told the key did not fit. A half-written final item is never left.
                return false
            }
            val run = bytes.copyOfRange(at, end)
            items.add(if (paste) DraftItem.Paste(run) else DraftItem.Text(run))
            at = end
        }
        return true
    }

    /**
     * Where one item ends, at or before `at + MAX_ITEM_BYTES`.
     *
     * A UTF-8 sequence is never split: a cut between a lead byte and its continuation would make
     * the item invalid, and the host decodes each item as text.
     */
    private fun boundary(bytes: ByteArray, at: Int): Int {
        var end = minOf(at + MAX_ITEM_BYTES, bytes.size)
        if (end == bytes.size) return end
        while (end > at + 1 && (bytes[end].toInt() and 0xC0) == 0x80) end--
        return end
    }

    companion object {
        /** `mesh_term::MAX_SEND_ITEMS`. */
        const val MAX_ITEMS = 16

        /** `mesh_term::MAX_SEND_ITEM`, in bytes. */
        const val MAX_ITEM_BYTES = 400

        /** `mesh_term::MAX_KEYS`, the names one `Keys` item may carry. */
        const val MAX_KEYS = 16
    }
}

/**
 * The mesh route's [TerminalHandle]: what the shared key bar drives.
 *
 * A key joins the draft **by name**, as it does on the tailnet, and the host turns it into bytes for
 * the pane's own mode; the sticky latch is the same one the tailnet handle applies, spent by the
 * same rule ([modifiedKey], [spendLatchOnFirst]). What changed with the engine is only where the
 * keys go: an ordered draft that one press sends, instead of a queue that sent itself.
 *
 * Everything the tailnet handle does for a surface (selection, scrollback, mouse, clipboard,
 * ownership) is absent on this link and is left at the interface's no-op defaults.
 */
class MeshTerminalHandle(
    /** What is typed but not sent. The screen owns it so the bar and the echo can read it. */
    val draft: MeshDraft,
    private val clock: () -> Long,
    /** Called after something was added, so a caller can redraw the bar now rather than at a tick. */
    private val onQueued: () -> Unit = {},
) : TerminalHandle {
    /**
     * The armed modifier, with the tailnet's own expiry.
     *
     * An armed Ctrl that was only toggled stayed armed, so an Esc a minute later went out as C-Esc —
     * a chord nobody meant. Same [ModifierLatch], same six seconds, and [expireModifiers] is the
     * screen's tick that redraws the key once it has lapsed (`modifiers` is read by the bar).
     */
    private val latch = ModifierLatch(now = clock)
    override var modifiers: Modifiers by mutableStateOf(Modifiers())
        private set

    /** Let go of an armed modifier whose six seconds have passed, and redraw it as unarmed. */
    fun expireModifiers() {
        val live = latch.current()
        if (live != modifiers) modifiers = live
    }

    /** What is waiting to be sent, as readable text (CR as ⏎, ESC as ␛, other controls as ^X, keys by name). */
    var pendingLabel: String by mutableStateOf("")
        private set

    /** Set when the last thing typed did not fit; cleared by the next accepted key. */
    var overflow: Boolean by mutableStateOf(false)
        private set

    override fun toggleModifier(which: StickyKey) { modifiers = latch.toggle(which) }

    override fun clearModifiers() {
        latch.clear()
        modifiers = Modifiers()
    }

    override fun sendKey(name: String) {
        val armed = armedModifiers()
        if (armed.armed) {
            clearModifiers()
            sendModifiedKey(name, armed)
            return
        }
        pushKey(name)
    }

    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        when (val k = modifiedKey(key, modifiers)) {
            ModifiedKey.None -> {}
            is ModifiedKey.Literal -> pushText(k.bytes.encodeToByteArray())
            is ModifiedKey.Named -> pushKey(k.name)
        }
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        val armed = armedModifiers()
        if (!armed.armed) { pushText(text.replace('\n', '\r').encodeToByteArray()); return }
        val typed = spendLatchOnFirst(text, armed)
        // The keystroke spent it: `latchAfter` is empty for every spend, so the latch must let go
        // too — otherwise the next read of [armedModifiers] would put it straight back.
        latch.clear()
        modifiers = typed.latchAfter
        if (typed.head.isEmpty()) {
            pushText(typed.rest.replace('\n', '\r').encodeToByteArray())
        } else {
            sendModifiedKey(typed.head, typed.headModifiers)
            if (typed.rest.isNotEmpty()) pushText(typed.rest.replace('\n', '\r').encodeToByteArray())
        }
    }

    /**
     * A paste is text that must fit what can wait; a refusal is returned, never half-sent.
     *
     * **Newlines are kept.** They used to be turned into carriage returns here, which made every
     * pasted line a command: the first ran, and the rest went to whatever that command started.
     * Whether a pane may be given a newline without running the line is the pane's own answer
     * (bracketed paste), which only the host can read.
     *
     * **A multi-line paste is one `Paste` item even when the soft keyboard inserted it**, which is
     * what makes a Gboard paste behave like a clipboard paste instead of a run of `Enter` keys
     * (`docs/RADIO-AUDIT-REVIEW.md` `RC-9`).
     */
    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        return pushPaste(text.replace("\r\n", "\n").encodeToByteArray())
    }

    /** What is armed *now*: [ModifierLatch] has already let go of expired ones. */
    private fun armedModifiers(): Modifiers {
        val live = latch.current()
        if (live != modifiers) modifiers = live
        return live
    }

    private fun pushText(bytes: ByteArray): Boolean = settle(draft.appendText(bytes))

    private fun pushPaste(bytes: ByteArray): Boolean = settle(draft.appendPaste(bytes))

    /** One named key with no sticky modifier applied or spent (a tap's cursor walk is not a keystroke). */
    fun sendPlainKey(name: String): Boolean = pushKey(name)

    private fun pushKey(name: String): Boolean = settle(draft.appendKey(name))

    private fun settle(ok: Boolean): Boolean {
        overflow = !ok
        // **Everything the bar shows, at the moment the key is typed.** The old queue refreshed on
        // its own poll, so a fast typist saw the echo stutter behind their fingers; the draft is
        // updated here, so the bar is redrawn here.
        refreshPending()
        if (ok) onQueued()
        return ok
    }

    /** Whether anything is waiting: what `Send` acts on, and what the bar counts. */
    val hasPending: Boolean get() = !draft.isEmpty

    /**
     * Throw away everything waiting and every rendering of it.
     *
     * **What is typed is the one plaintext this app keeps in memory**, and it must not outlive the
     * moment the user is looking at it. Nothing is persisted, and this is what stops it surviving
     * the screen going away: an Android recents thumbnail or a heap dump must not show a password
     * that was never sent.
     */
    fun scrub() {
        draft.clear()
        overflow = false
        pendingLabel = ""
        heldSummary = ""
        echoText = ""
    }

    /** Recompute the strings the bar and the echo draw from the draft. */
    fun refreshPending() {
        val items = draft.peek()
        pendingLabel = readable(items)
        heldSummary = summarize(items)
        echoText = echoable(items)
    }

    /**
     * What is waiting, counted: typed characters and named keys.
     *
     * **Not records.** The old bar appended `≈N records`, an estimate nobody can act on: in v15 a
     * command is one message whatever it holds.
     */
    var heldSummary: String by mutableStateOf("")
        private set

    /**
     * What the draft should look like **as characters**, for the local echo.
     *
     * Empty while nothing is held. Typed and pasted text are themselves; `Enter` is a line end;
     * `Tab` and `BSpace` are their control characters. Everything else — arrows, `Escape`, `C-c` —
     * renders as **nothing**, because an arrow key sent to a shell is a request the shell
     * interprets, and painting an effect that has not happened would be an invention. They are
     * counted in [heldSummary] instead.
     *
     * **These are the characters, not the picture.** How they are drawn is `draftEcho`'s job, and
     * that is what turns a line end into `⏎`: written raw into a live engine, a `\n` on the last row
     * scrolls the screen out from under the operator's typing.
     */
    var echoText: String by mutableStateOf("")
        private set

    private fun echoable(items: List<DraftItem>): String = buildString {
        for (item in items) when (item) {
            is DraftItem.Text -> append(item.bytes.decodeToString())
            is DraftItem.Paste -> append(item.bytes.decodeToString())
            is DraftItem.Keys -> for (n in item.names) {
                when (n) {
                    "Enter" -> append("\r\n")
                    "Tab" -> append('\t')
                    "BSpace" -> append("\b \b")
                    else -> {}
                }
            }
        }
    }

    private fun summarize(items: List<DraftItem>): String {
        if (items.isEmpty()) return ""
        var chars = 0
        var keys = 0
        for (item in items) when (item) {
            is DraftItem.Text -> chars += item.bytes.decodeToString().length
            is DraftItem.Paste -> chars += item.bytes.decodeToString().length
            is DraftItem.Keys -> keys += item.names.size
        }
        val parts = buildList {
            if (chars > 0) add(if (chars == 1) "1 character" else "$chars characters")
            if (keys > 0) add(if (keys == 1) "1 key" else "$keys keys")
        }
        return parts.joinToString(" and ")
    }

    private fun readable(items: List<DraftItem>): String = buildString {
        for (item in items) when (item) {
            is DraftItem.Text -> appendControls(item.bytes.decodeToString())
            is DraftItem.Paste -> appendControls(item.bytes.decodeToString())
            is DraftItem.Keys -> for (n in item.names) append(keyGlyph(n))
        }
    }

    private fun StringBuilder.appendControls(text: String) {
        for (x in text) {
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

    private fun keyGlyph(name: String): String = when {
        name == "Enter" -> "⏎"
        name == "Escape" -> "␛"
        name == "Tab" -> "⇥"
        name == "BSpace" -> "␡"
        name.length == 3 && name.startsWith("C-") -> "^" + name[2].uppercaseChar()
        else -> "‹$name›"
    }
}
