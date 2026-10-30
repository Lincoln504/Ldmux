package dev.ldmux.mobile.mesh

/**
 * A protocol key name as the bytes the shell receives, for the mesh route.
 *
 * On the tailnet a key goes to the daemon **by name** and the daemon owns the table
 * (`key_name_to_bytes`, `crates/daemon/src/stream.rs`). The mesh profile has no such message:
 * `Input` is raw bytes (ADR-0025), so the phone has to produce them. This is the same table's
 * legacy xterm path, ported, and it is deliberately the *same* names the shared key bars already
 * send (`Escape`, `BSpace`, `PageUp`, `C-c`, `M-f`, `C-S-Left` ...), so the bars are unchanged.
 *
 * What it cannot know, and does not guess: the host's cursor-key mode (DECCKM) and the Kitty
 * keyboard flags are state of the program on the host, and the profile does not carry them. An
 * arrow is therefore always `ESC [ A`, and `Shift+Enter` is a plain CR. Both are named gaps, not
 * silent differences: a program that needs `ESC O A` or `CSI 13;2u` will not get it over the mesh.
 *
 * A name this table does not know is refused (null), never typed: the old daemon fallback sent
 * unknown names as words into the user's command line.
 */
fun meshKeyBytes(name: String): ByteArray? {
    var rest = name
    var alt = false
    var ctrl = false
    var shift = false
    if (rest.startsWith("M-") && rest.length > 2) { alt = true; rest = rest.substring(2) }
    if (rest.startsWith("C-") && rest.length > 2) { ctrl = true; rest = rest.substring(2) }
    if (rest.startsWith("S-") && rest.length > 2) { shift = true; rest = rest.substring(2) }
    if (rest.isEmpty()) return null

    // xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    val m = 1 + (if (shift) 1 else 0) + (if (alt) 2 else 0) + (if (ctrl) 4 else 0)
    val modified = alt || ctrl || shift

    fun simple(b: Int): ByteArray = if (alt) byteArrayOf(0x1B, b.toByte()) else byteArrayOf(b.toByte())
    fun csiFinal(fin: Char): ByteArray =
        if (modified) "\u001b[1;$m$fin".encodeToByteArray() else "\u001b[$fin".encodeToByteArray()
    fun ss3(fin: Char): ByteArray =
        if (modified) "\u001b[1;$m$fin".encodeToByteArray() else "\u001bO$fin".encodeToByteArray()
    fun tilde(n: Int): ByteArray =
        if (modified) "\u001b[$n;$m~".encodeToByteArray() else "\u001b[$n~".encodeToByteArray()

    return when (rest) {
        "Enter" -> simple(0x0D)
        "Tab" -> when {
            shift && !alt && !ctrl -> "\u001b[Z".encodeToByteArray()
            shift -> "\u001b[1;${m}Z".encodeToByteArray()
            else -> simple(0x09)
        }
        "BTab" -> "\u001b[Z".encodeToByteArray()
        "Escape" -> simple(0x1B)
        "BSpace", "Backspace" -> simple(if (ctrl) 0x08 else 0x7F)
        "Space" -> simple(if (ctrl) 0x00 else 0x20)
        "Up" -> csiFinal('A')
        "Down" -> csiFinal('B')
        "Right" -> csiFinal('C')
        "Left" -> csiFinal('D')
        "Home" -> csiFinal('H')
        "End" -> csiFinal('F')
        "PageUp", "PPage" -> tilde(5)
        "PageDown", "NPage" -> tilde(6)
        "Delete", "DC" -> tilde(3)
        "Insert", "IC" -> tilde(2)
        "F1" -> ss3('P')
        "F2" -> ss3('Q')
        "F3" -> ss3('R')
        "F4" -> ss3('S')
        "F5" -> tilde(15)
        "F6" -> tilde(17)
        "F7" -> tilde(18)
        "F8" -> tilde(19)
        "F9" -> tilde(20)
        "F10" -> tilde(21)
        "F11" -> tilde(23)
        "F12" -> tilde(24)
        else -> single(rest, ctrl, shift, ::simple)
    }
}

private fun single(base: String, ctrl: Boolean, shift: Boolean, simple: (Int) -> ByteArray): ByteArray? {
    // A character is one code point. Text outside ASCII is text: it carries no modifier.
    if (base.codePointCountCompat() != 1) return null
    val c = base[0]
    if (c.code > 0x7e) return if (!ctrl && !shift) base.encodeToByteArray() else null
    if (c.code < 0x20) return null
    // Shift exists on named keys only; a shifted character is the typed shifted character.
    if (shift) return null
    if (ctrl) {
        val byte = when (c) {
            in 'a'..'z' -> c - 'a' + 1
            '@', ' ' -> 0x00
            '[' -> 0x1B
            '\\' -> 0x1C
            ']' -> 0x1D
            '^' -> 0x1E
            '_' -> 0x1F
            '?' -> 0x7F
            // No C0 code (`C-A`, `C-|`): refuse, never invent one.
            else -> return null
        }
        return simple(byte)
    }
    return simple(c.code)
}

/** Code points without `java.lang.String` (this is common code). */
private fun String.codePointCountCompat(): Int {
    var n = 0
    var i = 0
    while (i < length) {
        i += if (this[i].isHighSurrogate() && i + 1 < length && this[i + 1].isLowSurrogate()) 2 else 1
        n++
    }
    return n
}
