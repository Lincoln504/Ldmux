package dev.ldmux.mobile

/**
 * The sticky modifier keys: Ctrl and Alt, latched for exactly one following key.
 *
 * "Sticky" is the point. A phone keyboard has no way to hold a chord — you
 * cannot press Ctrl and `c` at the same instant with one finger, and the keys
 * live in a sidebar you would have to hold with a second hand while reaching for
 * a third. So the modifier is a mode: arm it, press one key, and it disarms.
 *
 * ## How a modified key is delivered: always by name
 *
 * A modified key goes to the daemon as a **key name** (`C-a`, `M-f`,
 * `M-C-Left`), never as the bytes we think it should produce, and that includes
 * plain characters.
 *
 * The client does not know what the program on the pty asked for (legacy bytes,
 * extended keys), and the daemon is the one place that owns the name-to-bytes
 * table (`key_name_to_bytes` in the daemon's `stream.rs`). A name is a request
 * the daemon can answer correctly in one place; literal bytes sent from here
 * would freeze one encoding into the client. The C/c button was already a name,
 * so every Ctrl chord now takes the same path.
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
 * - [Named] carries a protocol key name (the `key` path).
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
 * is NUL. The byte itself is the daemon's to produce (the key goes by name); this
 * decides only whether a Ctrl chord exists for the character at all.
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
 * Protocol names for the keys that are not single characters.
 *
 * Only these can carry a modifier as a *name*; everything else is a character,
 * and key names for punctuation with a modifier (`C-|`) are not valid — which
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
 * physical key would produce) or one of the protocol's key names.
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
        // Outside ASCII there are no control codes and no key names to carry a
        // modifier; the character goes out as itself.
        if (ch.code !in 0x20..0x7e) return ModifiedKey.Literal(key)
        // Ctrl only exists where there is a C0 code for it (Ctrl-| has none):
        // dropping it types a plausible character, inventing one would not.
        val ctrl = mods.ctrl && ch in CTRL_BYTES
        // A letter's Ctrl chord has no case — Ctrl-C is Ctrl-c — and `C-A` would
        // reach an extended-keys program as a different key (`CSI 65;5u`).
        val base = when {
            ch == ' ' -> "Space"
            ctrl && ch in 'A'..'Z' -> ch.lowercaseChar().toString()
            else -> key
        }
        var prefix = ""
        if (mods.alt) prefix += "M-"
        if (ctrl) prefix += "C-"
        if (prefix.isEmpty()) return ModifiedKey.Literal(key)
        return ModifiedKey.Named(prefix + base)
    }

    // A named key carries its modifier *in the name*, and the daemon knows which
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
