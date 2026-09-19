package dev.ldmux.mobile

/**
 * The sticky modifier keys: Ctrl and Alt, latched for exactly one following key.
 *
 * "Sticky" is the point. A phone keyboard has no way to hold a chord — you
 * cannot press Ctrl and `c` at the same instant with one finger, and the keys
 * live in a sidebar you would have to hold with a second hand while reaching for
 * a third. So the modifier is a mode: arm it, press one key, and it disarms.
 *
 * ## How a modified key is delivered, and why there are two ways
 *
 * A real terminal sends **bytes**, and the two kinds of key need different bytes:
 *
 * - Ctrl and Alt on a *character* are genuine control bytes. Ctrl-C is 0x03, Alt-f
 *   is ESC then `f`. Those are sent as literal input, which is exactly what a
 *   keyboard would have produced; tmux passes literal input to the pty untouched.
 * - Ctrl and Alt on a *named* key (the arrows, Enter, Tab) are not control bytes
 *   at all — they are escape sequences with a modifier parameter, and the terminal
 *   decides which one from its own key table. There is nothing to compute, so
 *   those go as tmux key names, which is where that table lives.
 *
 * Both end on the wire as `send-keys`, but only the first is a byte we can work
 * out ourselves; inventing escape sequences for the second is how a client ends
 * up sending a sequence the terminal would never have produced.
 */

/** Which modifiers are armed. Both may be armed at once. */
data class Modifiers(val ctrl: Boolean = false, val alt: Boolean = false) {
    val armed: Boolean get() = ctrl || alt

    /** Arm or disarm one modifier, leaving the other alone. */
    fun toggled(which: StickyKey): Modifiers = when (which) {
        StickyKey.Ctrl -> copy(ctrl = !ctrl)
        StickyKey.Alt -> copy(alt = !alt)
    }

    /** Disarm everything — after a key has consumed them, or on a tap. */
    fun cleared(): Modifiers = Modifiers()
}

/**
 * Which modifier is armed. Named `Sticky` rather than `Modifier` because Compose
 * owns that name in every file that draws anything, and a shadowed enum is a
 * compile error in the exact place this is used.
 */
enum class StickyKey { Ctrl, Alt }

/**
 * What to do with a key press once the modifiers are known.
 *
 * - [Literal] carries bytes to write to the pty as-is (the `input` path).
 * - [Named] carries a tmux key name (the `key` path).
 * - [None] means nothing should be sent.
 */
sealed interface ModifiedKey {
    /** Nothing to send — an empty key. */
    data object None : ModifiedKey
    data class Literal(val bytes: String) : ModifiedKey
    data class Named(val name: String) : ModifiedKey
}

/**
 * Keys whose Ctrl form is a control byte, and which byte it is.
 *
 * These are the classic C0 assignments, and they are not guessable from the
 * letter's position: Ctrl-`[` is ESC (0x1b), Ctrl-`?` is DEL (0x7f), and Ctrl-`@`
 * is NUL. A lookup is the honest way to write this down.
 */
private val CTRL_BYTES: Map<Char, Char> = buildMap {
    for (c in 'a'..'z') put(c, (c - 'a' + 1).toChar())
    for (c in 'A'..'Z') put(c, (c - 'A' + 1).toChar())
    put('@', '\u0000')
    put(' ', '\u0000')
    put('[', '\u001b')
    put('\\', '\u001c')
    put(']', '\u001d')
    put('^', '\u001e')
    put('_', '\u001f')
    put('?', '\u007f')
}

/**
 * tmux names for the keys that are not single characters.
 *
 * Only these can carry a modifier as a *name*; everything else is a character,
 * and tmux key names for punctuation with a modifier (`C-|`) are not valid — which
 * is why the character path exists rather than this handling everything.
 */
private val NAMED_KEYS: Set<String> = setOf(
    "Escape", "Enter", "Tab", "Space", "BSpace",
    "Up", "Down", "Left", "Right",
    "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
)

/**
 * Turn a key press into something the daemon can deliver.
 *
 * [key] is either a single character (what the on-screen keys send, and what a
 * physical key would produce) or one of tmux's key names.
 *
 * A modified *character* always resolves, even when the exact byte is unknown
 * (Ctrl-`|` has no C0 code): in that case the Ctrl is dropped rather than the
 * press, because sending the character unmodified is a plausible intent, while
 * sending an invented control byte is not.
 */
fun modifiedKey(key: String, mods: Modifiers): ModifiedKey {
    if (key.isEmpty()) return ModifiedKey.None
    val single = key.length == 1

    if (!mods.armed) {
        return if (single) ModifiedKey.Literal(key) else ModifiedKey.Named(key)
    }

    if (single) {
        val ch = key[0]
        var bytes = ""
        // Alt first: on a real terminal Alt is ESC *prepended*, so it goes in
        // front of whatever the character is.
        if (mods.alt) bytes += '\u001b'
        bytes += if (mods.ctrl) CTRL_BYTES[ch] ?: ch else ch
        return ModifiedKey.Literal(bytes)
    }

    // A named key carries its modifier *in the name*, and tmux knows which
    // sequence that is. Some of the sidebar's own keys are already named that
    // way (`C-c`), so any existing prefix is stripped and rebuilt rather than
    // stacked into `C-C-c`, which is not a key.
    val base = key.removePrefix("M-").removePrefix("C-")
    if (base !in NAMED_KEYS && base.length != 1) return ModifiedKey.Named(key)
    var prefix = ""
    if (mods.alt) prefix += "M-"
    if (mods.ctrl) prefix += "C-"
    return ModifiedKey.Named(if (prefix.isEmpty()) base else prefix + base)
}

/**
 * What one typed keystroke does to a latched modifier, as data.
 *
 * The rule is that the **first** character carries the modifier and is what spends
 * it, and that the rule applies to whatever that character turns out to be: a latch
 * that waited for a keystroke it approved of would surprise you on the one after it.
 *
 * This is a pure function, in the file that owns modifier semantics, because the bug
 * it fixes was a rule that lived in the chrome and was therefore unreachable from the
 * one path that types. A rule nobody can call is a rule that only holds where
 * somebody remembered it, and the soft keyboard did not.
 */
data class TypedKey(
    /** The first character, to be sent with [headModifiers] applied. Empty when there is none to chord. */
    val head: String,
    /** The rest of the text. Always sent plainly. */
    val rest: String,
    /** The modifiers [head] carries, or none when [head] carries none. */
    val headModifiers: Modifiers,
    /**
     * What remains latched after this keystroke. Empty in every case except two:
     * no text at all, and nothing armed to begin with — because in both of those
     * no keystroke happened to spend it.
     */
    val latchAfter: Modifiers,
)

fun spendLatchOnFirst(text: String, armed: Modifiers): TypedKey {
    if (text.isEmpty()) return TypedKey("", "", Modifiers(), armed)
    // Nothing is latched: the whole thing is ordinary text.
    if (!armed.armed) return TypedKey("", text, Modifiers(), armed)
    // A modified *character* is a control code, and control codes only exist for
    // ASCII. Anything else goes out plainly — and the latch is still spent, because
    // the keystroke happened and a modifier that outlives a keystroke is the bug.
    if (text[0].code > 127) return TypedKey("", text, Modifiers(), Modifiers())
    return TypedKey(text.substring(0, 1), text.substring(1), armed, Modifiers())
}
