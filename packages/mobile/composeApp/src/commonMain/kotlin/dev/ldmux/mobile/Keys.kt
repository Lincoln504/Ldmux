package dev.ldmux.mobile

/**
 * Keys the native sidebar can send, using tmux key names.
 *
 * Grouped by meaning, and each group is laid out as its own wrapping flow: the
 * widest group came out a few dp wider than the panel when the rows were laid out
 * by hand, so its last key was clipped off the edge. Grouping keeps the arrow
 * cluster together while still allowing a wrap when the font or width changes.
 */
val SIDEBAR_KEYS = listOf(
    listOf("Esc" to "Escape", "Tab" to "Tab", "⏎" to "Enter"),
    listOf("←" to "Left", "↑" to "Up", "↓" to "Down", "→" to "Right"),
    listOf("C-c" to "C-c", "C-d" to "C-d", "|" to "|", "-" to "-", "/" to "/"),
)