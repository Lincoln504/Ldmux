package io.ldmux.mobile

import androidx.compose.ui.text.font.FontFamily

/**
 * The app's monospace face: Fira Mono, bundled rather than asked for.
 *
 * **Why a bundled font.** `FontFamily.Monospace` is whatever the device calls monospace — Roboto
 * Mono on most Android builds, but a different face with different advances on an OEM build, and a
 * terminal whose cell width changes with the phone is a terminal whose grid changes with the phone.
 * Every measured layout in this app (the key rows, the header, the mesh rows, the terminal's cell
 * grid) assumes one advance per character, so the face is the app's, not the platform's.
 *
 * Fira Mono is SIL OFL 1.1; the licence and the notice are in `THIRD-PARTY-NOTICES.md`. It has no italic cut, so Compose synthesises a slant for the terminal's
 * italic cells — a synthesis that is a slant, never a different advance, which is what the grid
 * needs.
 */
expect val MonoFont: FontFamily
