package io.ldmux.mobile

/**
 * What a paste must go through before it is written to the pty.
 *
 * Two defects lived here. The daemon refuses an `input` message over 16 KiB
 * (`MAX_INPUT_BYTES`, answered with `input_too_large` and no input), so one large
 * paste was dropped whole and silently. And a bracketed paste wrapped the clipboard
 * verbatim, so a clipboard holding `ESC [ 2 0 1 ~` ended the paste early and the rest
 * ran as typed commands (the classic paste-injection).
 */

/** Characters per message. 4000 UTF-16 units are at most 12000 UTF-8 bytes, under the daemon's 16384. */
const val INPUT_CHUNK_CHARS = 4000

/**
 * Remove ESC from text that is about to be wrapped in bracketed-paste markers.
 *
 * ESC is the only byte that can begin the closing marker, and nothing legitimate
 * in pasted text needs it: xterm and VTE strip it for the same reason.
 */
fun sanitizeBracketedPaste(text: String): String = text.replace("\u001b", "")

/** Split [text] into pieces of at most [max] chars, never between the halves of a surrogate pair. */
fun chunkInput(text: String, max: Int = INPUT_CHUNK_CHARS): List<String> {
    require(max >= 2)
    if (text.length <= max) return if (text.isEmpty()) emptyList() else listOf(text)
    val out = ArrayList<String>(text.length / max + 1)
    var i = 0
    while (i < text.length) {
        var end = minOf(i + max, text.length)
        if (end < text.length && text[end - 1].isHighSurrogate()) end--
        out.add(text.substring(i, end))
        i = end
    }
    return out
}

/**
 * The key name a hardware chord should send, or null when it is not a chord.
 *
 * A physical keyboard's Ctrl+C or Alt+B is a key event with a modifier, and the
 * surface used to skip it ("a ctrl chord must not become its letter") and then map
 * nothing, so every hardware chord was dropped. It goes by name like the phone's own
 * modifier buttons, so the daemon's one table decides the bytes.
 */
fun hardwareChord(char: Char, ctrl: Boolean, alt: Boolean): String? {
    if (!ctrl && !alt) return null
    return (modifiedKey(char.toString(), Modifiers(ctrl = ctrl, alt = alt)) as? ModifiedKey.Named)?.name
}
