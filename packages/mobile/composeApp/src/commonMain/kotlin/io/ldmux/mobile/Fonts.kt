package io.ldmux.mobile

import androidx.compose.ui.text.font.FontFamily

/**
 * The app's monospace face: **one per theme, bundled rather than asked for.**
 *
 * **Why a bundled font.** `FontFamily.Monospace` is whatever the device calls monospace — Roboto
 * Mono on most Android builds, but a different face with different advances on an OEM build, and a
 * terminal whose cell width changes with the phone is a terminal whose grid changes with the phone.
 * Every measured layout in this app (the key rows, the header, the mesh rows, the terminal's cell
 * grid) assumes one advance per character, so the face is the app's, not the platform's. Five
 * themes means five faces, and none of them may be the platform's: a theme that changes the mono
 * face and not the advance would move the grid.
 *
 * Fira Mono, IBM Plex Mono, Space Mono, Cousine and Share Tech Mono are SIL OFL 1.1 (Cousine:
 * Apache 2.0); the licences and notices are in `THIRD-PARTY-NOTICES.md`. Only Fira Mono, IBM Plex,
 * Space Mono and Cousine have a bold cut upstream, so the other two synthesise one — a synthesised
 * bold is a weight, never a different advance, which is what the grid needs.
 */
expect val MonoFont: FontFamily

/**
 * The app's proportional face: **one per theme**, and the counterpart of [MonoFont].
 *
 * The split between the two is the app's own and does not change with the theme: monospace is for
 * things that are *measured* — keys, ids, addresses, the header, the terminal — and the sans face
 * is for prose. It arrives through the root's `LocalTextStyle`, so every `Text` that does not name
 * a family gets the theme's sans for free and no call site had to be told which face it is.
 */
expect val SansFont: FontFamily

/**
 * A *named* theme's monospace face, whether or not it is the active one.
 *
 * Settings draws the five themes as five little portraits, and a portrait set in the active theme's
 * face would be five rows of the same type — a list of names, not a preview. This is the one reader
 * that needs a face it is not wearing.
 */
expect fun monoFontFor(theme: LdmuxTheme): FontFamily

/** A named theme's proportional face, the counterpart of [monoFontFor]. */
expect fun sansFontFor(theme: LdmuxTheme): FontFamily
