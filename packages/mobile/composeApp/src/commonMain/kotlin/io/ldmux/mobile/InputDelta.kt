package io.ldmux.mobile

/**
 * What a change to the hidden input field means for the terminal.
 *
 * The field is an IME's buffer, and the difference between two of its values is the
 * only honest account of what the user did. The IME promises neither a change per
 * keystroke nor a particular shape to a change: a swipe, a paste and a backspace are
 * all just "the value is now different".
 *
 * This used to be read by resetting the field to a sentinel after every change, so
 * that a deletion would arrive as a recognisable change. The reset is what lost
 * characters, and it was measured rather than guessed. One injected burst of
 * `abcdefghij` — a single `adb shell input text` call — reached the callback as
 * `a`, ``, `c`, ``, `f`, ``, `h` … with the pane holding `acfhj`: five of the ten
 * characters were never delivered, because the write replaced the buffer the IME was
 * midway through producing into. Two complaints followed from that and both were
 * blamed on the IME — half the events, since each character cost one change to
 * deliver it and a second to undo it, and a delete that "randomly stops working"
 * whenever the IME's idea of the text had drifted from ours.
 *
 * Reading a delta instead of resetting removes the cause, and puts the rule
 * somewhere a test can reach it without a device.
 */
internal sealed interface InputOp {
    /** Named terminal keys, in order. Never text: these must not be predicted. */
    data class Keys(val names: List<String>) : InputOp

    /** A run of printable characters, sent in one piece. */
    data class Text(val value: String) : InputOp

    /**
     * A paste: text that arrived whole, line ends and all.
     *
     * Distinct from [Text] because a program that turned on bracketed paste has been promised the
     * markers, and the promise is the whole point of the mode: a shell without them runs the first
     * line of a paragraph before the user has seen the second. The clipboard button already went
     * through the paste path; a paste from the keyboard did not, and turned every line break into a
     * `Enter` key instead — five lines became five commands, each sent as its own record over a
     * radio link.
     */
    data class Paste(val value: String) : InputOp
}

/**
 * Whether [text] has a line end with at least one character after it, either ending spelled.
 *
 * That, and not the mere presence of a line end, is what makes a change a paste rather than a line
 * being typed and submitted: see [inputOpsFor].
 */
internal fun hasTextAfterALineEnd(text: String): Boolean {
    for (i in 0 until text.length - 1) {
        if ((text[i] == '\n' || text[i] == '\r') && text[i + 1] != '\n' && text[i + 1] != '\r') {
            return true
        }
    }
    return false
}

/**
 * The line endings a paste carries, as the pty will receive them.
 *
 * Folded to `\n` because the wrapping is the receiver's business: a program with bracketed paste
 * takes the text as text, and one without it is given carriage returns by the host (`paste_bytes`),
 * which is the only place that knows which it is. Leaving `\r\n` in would arrive as two line ends
 * there and paste a blank line between every line.
 */
internal fun foldPasteLineEnds(text: String): String = text.replace("\r\n", "\n").replace('\r', '\n')

/**
 * The operations that turn [previous] into [next].
 *
 * Deletions come first, because the shared prefix is where the two values stop
 * agreeing: everything after it in the old value is gone, and everything after it in
 * the new value arrived. The order is not stylistic. A backspace emitted after the
 * text it was meant to precede would delete the wrong character.
 */
internal fun inputOpsFor(previous: String, next: String): List<InputOp> {
    val shared = commonPrefixOf(previous, next)
    val ops = mutableListOf<InputOp>()
    val deleted = previous.length - shared
    if (deleted > 0) ops += InputOp.Keys(List(deleted) { "BSpace" })

    val inserted = next.substring(shared)
    // **A block of text is a paste, not a run of Enter keys.** The IME reports a paste as one change
    // to the field, so this is the only place that can tell the two apart, and the distinguishing
    // fact is not "there is a line end" but **"there is text after a line end"**:
    //
    //  - `"echo one\necho two"` carries a line end with something after it, so it is text that
    //    happens to contain lines. Running each line as a command was the defect.
    //  - `"ls\n"` is one line and its terminator, whether it was typed or pasted, and the pane
    //    wants it as `ls` and a Return: the interactive case, which a paste path would leave sitting
    //    in the prompt unexecuted for a program with bracketed paste on.
    //  - `"\n"` is the IME's own Enter key.
    if (hasTextAfterALineEnd(inserted)) {
        val paste = foldPasteLineEnds(inserted)
        if (paste.isNotEmpty()) ops += InputOp.Paste(paste)
        return ops
    }

    val pending = StringBuilder()
    fun flush() {
        if (pending.isEmpty()) return
        ops += InputOp.Text(pending.toString())
        pending.clear()
    }
    for (ch in inserted) {
        when (ch) {
            '\n', '\r' -> {
                flush()
                ops += InputOp.Keys(listOf("Enter"))
            }
            // Tab is a named key, not text: it must reach the shell as Tab so completion
            // and the program's own bindings see a real TAB, and so it can never be
            // predicted.
            '\t' -> {
                flush()
                ops += InputOp.Keys(listOf("Tab"))
            }
            else -> pending.append(ch)
        }
    }
    flush()
    return ops
}

/**
 * How many leading characters two strings share.
 *
 * An IME reports whole values, and this is what turns one into an edit. Sharing a
 * prefix means the change is at the end, which is where a text field's cursor is.
 */
internal fun commonPrefixOf(a: String, b: String): Int {
    val n = minOf(a.length, b.length)
    var i = 0
    while (i < n && a[i] == b[i]) i++
    return i
}
