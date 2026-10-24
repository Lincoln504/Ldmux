package dev.ldmux.mobile

/**
 * The sticky modifier keys: Ctrl, Alt and Shift, latched for exactly one
 * following key.
 *
 * "Sticky" is the point. A phone keyboard has no way to hold a chord — you
 * cannot press Ctrl and `c` at the same instant with one finger, and the keys
 * live in a sidebar you would have to hold with a second hand while reaching for
 * a third. So the modifier is a mode: arm it, press one key, and it disarms.
 *
 * Shift is latched for the same reason a soft keyboard cannot provide one at the
 * same moment as another key: **Shift+Enter is a newline** in a TUI such as
 * `pi`, and the only way to produce it from the bar is to arm Shift and press
 * the bar's Enter. It also gives Shift+Tab, and Shift on arrows and Home/End
 * for the programs that select with them.
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

/** Which modifiers are armed. Any combination of them may be armed at once. */
data class Modifiers(
    val ctrl: Boolean = false,
    val alt: Boolean = false,
    val shift: Boolean = false,
) {
    val armed: Boolean get() = ctrl || alt || shift

    /** Arm or disarm one modifier, leaving the others alone. */
    fun toggled(which: StickyKey): Modifiers = when (which) {
        StickyKey.Ctrl -> copy(ctrl = !ctrl)
        StickyKey.Alt -> copy(alt = !alt)
        StickyKey.Shift -> copy(shift = !shift)
    }

    /** Disarm everything — after a key has consumed them, or on a tap. */
    fun cleared(): Modifiers = Modifiers()
}

/**
 * Which modifier is armed. Named `Sticky` rather than `Modifier` because Compose
 * owns that name in every file that draws anything, and a shadowed enum is a
 * compile error in the exact place this is used.
 */
enum class StickyKey { Ctrl, Alt, Shift }

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
    //
    // The wire's order is `M-C-S-<base>`: Shift is the innermost prefix.
    val base = key.removePrefix("M-").removePrefix("C-").removePrefix("S-")
    if (base !in NAMED_KEYS && base.length != 1) return ModifiedKey.Named(key)
    var prefix = ""
    if (mods.alt) prefix += "M-"
    // Ctrl has no meaning on Escape or Enter (the daemon sends ESC and CR whatever
    // the modifier), so a latched Ctrl is spent by them without being named: `C-Escape`
    // and `C-Enter` are not keys any program is waiting for.
    if (mods.ctrl && base !in CTRL_INERT) prefix += "C-"
    // Shift, unlike Ctrl, *is* a key on Escape, Enter and Space: under the Kitty
    // keyboard protocol `S-Enter` is `CSI 13;2u`, which is the newline in `pi`.
    // A daemon without the protocol collapses it to the unshifted key, so the
    // name is always safe to send.
    if (mods.shift) prefix += "S-"
    return ModifiedKey.Named(if (prefix.isEmpty()) base else prefix + base)
}

/** Named keys a Ctrl modifier does not change. */
private val CTRL_INERT: Set<String> = setOf("Escape", "Enter")

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
