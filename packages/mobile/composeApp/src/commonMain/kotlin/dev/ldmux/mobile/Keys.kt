package dev.ldmux.mobile

/**
 * Keys the native sidebar can send, using tmux key names.
 *
 * Grouped by meaning, and each group is laid out as its own wrapping flow: the
 * widest group came out a few dp wider than the panel when the rows were laid out
 * by hand, so its last key was clipped off the edge. Grouping keeps the arrow
 * cluster together while still allowing a wrap when the font or width changes.
 */
/**
 * A control key as a person writes it: `C/c`, never `C-c`.
 *
 * `C-c` is tmux's name for the key and is what has to go on the wire, so the
 * label is derived from the name rather than typed out next to it. Two bars show
 * these keys and only one of them used to be right, which is what typing a label
 * beside a key gets you.
 */
fun controlLabel(key: String): String =
    if (key.startsWith("C-")) "C/" + key.removePrefix("C-") else key

val SIDEBAR_KEYS = listOf(
    listOf("Esc" to "Escape", "Tab" to "Tab", "⏎" to "Enter"),
    listOf("←" to "Left", "↑" to "Up", "↓" to "Down", "→" to "Right"),
    listOf(
        controlLabel("C-c") to "C-c",
        "|" to "|",
        "-" to "-",
        "/" to "/",
    ),
)