/*
 * ldmux_terminal.h — the terminal core's API, with no platform in it.
 *
 * This is the whole seam between ldmux and the terminal emulator. It names
 * nothing from JNI, nothing from Android and nothing from Kotlin, so the same
 * core serves any binding: `ldmux_jni.c` is one of them, and a future Swift or
 * Objective-C binding is another. The C API it wraps (libghostty-vt) is marked
 * unstable (0.1.0-dev), and every symbol of it is confined to `ldmux_terminal.c`
 * so an upstream change breaks one translation unit rather than the app.
 *
 * Frame format returned by ldmux_terminal_render (little-endian):
 *   header (40 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle,
 *                      u8 defFgR, u8 defFgG, u8 defFgB,
 *                      u8 defBgR, u8 defBgG, u8 defBgB,
 *                      u8 passwordInput, u8 viewportActive, u8 mouseTracking,
 *                      u32 scrollTotal, u32 scrollOffset, u32 scrollLen,
 *                      u8 mouseSgr, u8 alternateOn, u8 reserved[7]
 *                      (the terminal's default colours so the client can paint
 *                      the whole surface in the same background; whether the
 *                      cursor is at a password prompt, which decides whether
 *                      input may be echoed before the server confirms it; the
 *                      scrollbar geometry so the client can show position
 *                      without a second call; and the mouse modes, so a tap can
 *                      be sent as a click the moment a program asks for the
 *                      mouse rather than when the next server snapshot happens
 *                      to arrive)
 *   then cols*rows cell records, row-major:
 *                      u8 utf8Len, u8 flagsLo, u8 flagsHi,
 *                      u8 fgR, u8 fgG, u8 fgB, u8 bgR, u8 bgG, u8 bgB,
 *                      u8 utf8[utf8Len]
 *   flags: bit0 bold, bit1 italic, bit2 underline, bit3 faint, bit4 blink,
 *          bit5 inverse, bit6 invisible, bit7 strikethrough, bit8 overline
 *
 * The Kotlin side pins the same numbers in `TerminalFrame.kt`; a test there
 * checks the header size, and both sides must change together.
 */
#ifndef LDMUX_TERMINAL_H
#define LDMUX_TERMINAL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define CELL_MAX_UTF8 16
#define CELL_RECORD_MAX (9 + CELL_MAX_UTF8)
#define FRAME_HEADER 40

/**
 * Scroll deltas that mean "the ends", not a number of rows. They are sentinel
 * values rather than a separate function so a binding has one scroll call.
 */
#define LDMUX_SCROLL_TOP INT32_MIN
#define LDMUX_SCROLL_BOTTOM INT32_MAX

/** Opaque: the fields are the core's business and no binding may depend on them. */
typedef struct LdmuxTerminal LdmuxTerminal;

/** Allocate a terminal of `cols` x `rows`. NULL if the emulator refused. */
LdmuxTerminal *ldmux_terminal_new(uint16_t cols, uint16_t rows);

/** Free a terminal and everything it owns. NULL is allowed and does nothing. */
void ldmux_terminal_free(LdmuxTerminal *t);

/** Feed bytes from the remote PTY. */
void ldmux_terminal_write(LdmuxTerminal *t, const uint8_t *bytes, size_t len);

/** Resize the emulator's grid. */
void ldmux_terminal_resize(LdmuxTerminal *t, uint16_t cols, uint16_t rows);

/**
 * Scroll the viewport. `delta` is in rows and follows the emulator's
 * convention (negative is up); [LDMUX_SCROLL_TOP] and [LDMUX_SCROLL_BOTTOM]
 * mean the ends.
 *
 * This is the emulator's own scrollback, not a client-side one, so the render
 * state simply reflects the new position on the next frame — there is no
 * separate history buffer to keep in sync.
 */
void ldmux_terminal_scroll(LdmuxTerminal *t, int32_t delta);

/**
 * Encode the current screen into the frame format above.
 *
 * The buffer belongs to the terminal and stays valid until the next render, so
 * the caller copies it out and does not free it. Returns false if the emulator
 * could not produce a frame, in which case `*out` and `*len` are untouched.
 */
bool ldmux_terminal_render(LdmuxTerminal *t, const uint8_t **out, size_t *len);

/** Clear the render state's dirty tracking. */
void ldmux_terminal_clean(LdmuxTerminal *t);

#endif /* LDMUX_TERMINAL_H */
