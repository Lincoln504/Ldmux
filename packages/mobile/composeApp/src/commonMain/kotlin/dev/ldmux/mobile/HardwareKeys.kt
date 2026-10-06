package dev.ldmux.mobile

/**
 * What a hardware key event means for the shell, as pure logic.
 *
 * The platform half only *describes* the event ([HardwareKey]); every decision lives
 * here, so it is tested on the JVM and an iOS keyboard gets the same behaviour.
 *
 * Three defects were in the Android-only version:
 *
 * - **AltGr was read as Alt.** European layouts type `@ { [ ] \ | ~ €` with AltGr
 *   (Android reports it as the right Alt, and Windows-style as Ctrl+Alt). The event
 *   already carries the character that combination produces; the old code saw "Alt
 *   pressed", skipped the character and sent `M-q` for the key that types `@`.
 * - **Modifiers on named keys were dropped.** Ctrl+Left sent a bare Left although
 *   the daemon encodes `C-Left` (and `M-C-Left`).
 * - Shift+Tab is back-tab, and that stays.
 *
 * Shift on other named keys goes out as the `S-` prefix (`S-Left`, `M-C-S-End`) when
 * the caller says the daemon knows it (`shiftKeys`); otherwise it is dropped.
 */
data class HardwareKey(
    /** The protocol key name for a non-character key (`Left`, `F5`, `Enter`...), else null. */
    val named: String?,
    val shift: Boolean,
    val ctrl: Boolean,
    val alt: Boolean,
    /**
     * The *right* Alt is down. On layouts that have an AltGr it is AltGr, and Android
     * reports it as Alt; only this one composes characters. A left Alt stays a chord
     * even when a generic key map happens to give the combination a symbol.
     */
    val rightAlt: Boolean,
    val meta: Boolean,
    /**
     * The character this key produces with the pressed modifiers *except Ctrl*
     * (Ctrl turns a letter into a control code and says nothing about the layout), or
     * 0 when it produces none. Includes Alt: this is where AltGr's `@` lives.
     */
    val charWithAlt: Int,
    /** The same, with Alt also taken away: what the key types on its own (Shift kept). */
    val charWithoutAlt: Int,
    /**
     * The soft keyboard is up. Its text field then owns *text*, and handling a
     * character here as well would send it twice. Chords and named keys are still ours.
     */
    val softKeyboardUp: Boolean,
)

sealed interface HardwareAction {
    /** Not ours: let the event continue (the text field, or nobody). */
    data object Pass : HardwareAction

    /** Characters to type. */
    data class Type(val text: String) : HardwareAction

    /** A protocol key name, modifiers included, e.g. `C-Left`, `M-f`. */
    data class Press(val name: String) : HardwareAction
}

/**
 * Keys whose shifted form is a different sequence (`CSI 1;2 A`...). Shift on
 * Enter, Escape, Backspace or Space sends the same byte, so those never carry `S-`.
 */
private val SHIFTABLE_NAMED: Set<String> = setOf(
    "Tab", "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert",
    "Delete", "DC", "IC", "PPage", "NPage",
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
)

/**
 * [shiftKeys]: the daemon understands the `S-` key-name prefix. Off by default: a
 * daemon without it answers an unknown name with a `bad_key` error frame and types
 * nothing (the whole `key` message is refused), so Shift+Left would stop working
 * instead of degrading to Left. With it off, Shift is dropped on named keys, as
 * before, except Shift+Tab which is `BTab` (a name every daemon knows).
 */
fun mapHardwareKey(k: HardwareKey, shiftKeys: Boolean = false): HardwareAction {
    if (k.named != null) {
        val shifted = k.shift && shiftKeys && k.named in SHIFTABLE_NAMED
        // Back-tab is its own key and takes no further modifier on a daemon without `S-`.
        if (k.named == "Tab" && k.shift && !(shifted && (k.ctrl || k.alt))) return HardwareAction.Press("BTab")
        if (!k.ctrl && !k.alt && !shifted) return HardwareAction.Press(k.named)
        val name = when (val m = modifiedKey(k.named, Modifiers(ctrl = k.ctrl, alt = k.alt))) {
            is ModifiedKey.Named -> m.name
            else -> k.named
        }
        // `S-` is the innermost prefix: `M-C-S-Left`.
        return HardwareAction.Press(if (shifted) withShift(name) else name)
    }

    // The right Alt producing a character that differs from the plain one is a
    // layout's own composition (AltGr), not an Alt chord. Compared against the key
    // with Alt removed, so an Alt+f whose character is the same either way stays a
    // chord.
    val composed = k.rightAlt && printable(k.charWithAlt) && k.charWithAlt != k.charWithoutAlt
    if (composed && !k.meta) {
        return if (k.softKeyboardUp) HardwareAction.Pass else HardwareAction.Type(k.charWithAlt.toChar().toString())
    }

    if (!k.ctrl && !k.alt) {
        if (k.meta || k.softKeyboardUp) return HardwareAction.Pass
        return if (printable(k.charWithoutAlt)) HardwareAction.Type(k.charWithoutAlt.toChar().toString()) else HardwareAction.Pass
    }

    // A Ctrl and/or Alt chord on a character key.
    val ch = k.charWithoutAlt
    if (ch !in 0x20..0x7e) return HardwareAction.Pass
    val name = hardwareChord(ch.toChar(), k.ctrl, k.alt)
    return if (name != null) HardwareAction.Press(name) else HardwareAction.Pass
}

/** Insert `S-` after any `M-`/`C-` prefix: `M-C-Left` -> `M-C-S-Left`. */
private fun withShift(name: String): String {
    val base = name.removePrefix("M-").removePrefix("C-")
    return name.substring(0, name.length - base.length) + "S-" + base
}

/** A character a user typed, not a control code, a surrogate half or a dead-key flag. */
private fun printable(cp: Int): Boolean =
    cp in 0x20..0xFFFF && cp != 0x7F && cp !in 0x80..0x9F && cp !in 0xD800..0xDFFF
