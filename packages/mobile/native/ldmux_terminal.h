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
 *   header (48 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle,
 *                      u8 defFgR, u8 defFgG, u8 defFgB,
 *                      u8 defBgR, u8 defBgG, u8 defBgB,
 *                      u8 passwordInput, u8 viewportActive, u8 mouseTracking,
 *                      u32 scrollTotal, u32 scrollOffset, u32 scrollLen,
 *                      u8 mouseSgr, u8 alternateOn,
 *                      u8 selectionOn, u16 selStartCol, u8 selStartRow,
 *                      u16 selEndCol, u8 selEndRow,
 *                      u8 cursorKnown, u8 reserved[7])
 *                      (the terminal's default colours so the client can paint
 *                      the whole surface in the same background; whether the
 *                      cursor is at a password prompt, which decides whether
 *                      input may be echoed before the server confirms it; the
 *                      scrollbar geometry so the client can show position
 *                      without a second call; and the mouse modes, so a tap can
 *                      be sent as a click the moment a program asks for the
 *                      mouse rather than when the next server snapshot happens
 *                      to arrive; and whether the cursor's coordinates are real,
 *                      which the emulator reports separately from whether the
 *                      program wants a cursor at all — `pi` hides the hardware
 *                      cursor and draws its own, so a client that conflated the
 *                      two had no cursor to show and no cell to walk to)
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
#define FRAME_HEADER 48

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

/**
 * What the remote program asked of the system clipboard, if anything.
 *
 * The clipboard is a single slot by definition — a later write replaces an
 * earlier one — so this is deliberately a slot and not a queue. If a program
 * writes twice before the embedder drains, the newer value is the correct final
 * state and the older one has nowhere legitimate to go. (The rule elsewhere is
 * the opposite: a value written more than once before it is read must be a
 * queue. This is the case where the thing being modelled really is one cell.)
 */
#define LDMUX_CLIPBOARD_NONE 0
#define LDMUX_CLIPBOARD_SET 1
#define LDMUX_CLIPBOARD_CLEAR 2

/**
 * Take the clipboard write the remote program last requested, if any.
 *
 * Returns one of the LDMUX_CLIPBOARD_* values and clears the request, so a
 * second call answers NONE until the program writes again. On LDMUX_CLIPBOARD_SET,
 * `*out` and `*len` describe the text: borrowed, and valid until the next call to
 * this function or to [ldmux_terminal_free]. The bytes are UTF-8 and are copied
 * out rather than converted here, because this file is the portable half and a
 * binding already has a correct decoder for its own string type.
 *
 * The write arrived as OSC 52, OSC 1337 or OSC 5522 — libghostty-vt normalises
 * and decodes all three, so what comes out is the value rather than whatever
 * spelling the program used. A program can also ask to *read* the clipboard;
 * that request is deliberately not answered (see ldmux_terminal.c), because
 * answering it would let anything running in the shell read whatever the user
 * has copied anywhere else.
 */
int ldmux_terminal_take_clipboard(LdmuxTerminal *t, const uint8_t **out,
                                  size_t *len);

/* ---------------------------------------------------------------------------
 * Selection
 *
 * The gesture state machine belongs to the engine, not to this file and not to
 * the client: click counting, the cell/word/line behaviours, the anchor and the
 * soft-wrap arithmetic are all terminal semantics, and the engine already
 * implements them. What the client actually has is three pointer events and a
 * cell size, so that is the whole interface below.
 *
 * Coordinates are 0-based VIEWPORT cells, which is what a tap resolves to: the
 * anchor must be a cell on the screen the user is looking at, not a row index
 * into scrollback that shifts when the pane scrolls.
 *
 * Every call that can change the selection returns 1 when it did and 0 when it
 * did not, so the caller repaints only when there is something new to draw.
 * (The geometry call below returns whether the geometry changed, which is the
 * same kind of answer for the same reason.)
 */

/**
 * Tell the core how the grid is drawn, in surface pixels.
 *
 * The engine needs this to reason about pointer distance (the slop that decides
 * whether two taps are a double-tap, and where a word boundary is under the
 * finger). It is geometry, not state: a font-size change or a rotation changes
 * it and nothing else does, so callers set it whenever those change rather than
 * per event.
 */
int ldmux_terminal_set_geometry(LdmuxTerminal *t, uint32_t columns,
                                uint32_t cell_w, uint32_t cell_h,
                                uint32_t pad_left, uint32_t screen_h);

/** Pointer down on a cell. `time_ms` is a monotonic clock: the engine compares
 *  it with the previous press to decide whether this is a second click of the
 *  same word or the start of a new selection. */
int ldmux_terminal_select_press(LdmuxTerminal *t, uint32_t col, uint32_t row,
                                uint64_t time_ms);

/** Pointer held still on the same cell long enough to mean "this word".
 *  Without it a long press is just a press that has not moved yet. */
int ldmux_terminal_select_deep_press(LdmuxTerminal *t, uint64_t time_ms);

/** Pointer moved while down. `col`/`row` are clamped to the grid by the core. */
int ldmux_terminal_select_drag(LdmuxTerminal *t, uint32_t col, uint32_t row,
                               uint64_t time_ms);

/** Pointer up. `valid_cell` is 0 when the pointer left the grid entirely, which
 *  the engine records as "released off the terminal" rather than guessing.
 *
 *  Every one of these takes the time, not just the press: the engine measures the
 *  gap between events, and a sequence whose clock goes backwards at release is a
 *  sequence it is right to distrust. (Learned by getting it wrong: a release
 *  timestamped 0 silently discarded the click count, so a double-tap selected
 *  nothing at all.) */
int ldmux_terminal_select_release(LdmuxTerminal *t, uint32_t col, uint32_t row,
                                  int valid_cell, uint64_t time_ms);

/** Drop the selection and any in-flight gesture. */
int ldmux_terminal_select_clear(LdmuxTerminal *t);

/** Select the whole screen. */
int ldmux_terminal_select_all(LdmuxTerminal *t);

/**
 * The selection as text, ready for the clipboard.
 *
 * Soft-wrapped lines are unwrapped and trailing whitespace is trimmed, which is
 * what copying a paragraph out of a terminal is expected to do: the wrap is a
 * property of the window, not of the text. Returns NULL when nothing is
 * selected. The bytes are borrowed and valid until the next call or
 * [ldmux_terminal_free].
 */
const uint8_t *ldmux_terminal_selection_text(LdmuxTerminal *t, size_t *len);

/*
 * Set the selection to an explicit range of viewport cells, inclusive on both
 * ends, and answer whether it changed.
 *
 * The gesture entry points above are for a *finger*: they carry a clock, they
 * decide for themselves whether two taps are a word, and their drag always moves
 * the end endpoint. This is for the *handles* the user drags afterwards, where the
 * two ends are independent, the range may be inverted by dragging one past the
 * other, and there is no gesture left to interpret. A negative column or row is
 * clamped to zero; the engine is asked for the reference and a refusal is a
 * refusal, not a crash.
 */
int ldmux_terminal_selection_set(LdmuxTerminal *t, int start_col, int start_row,
                                 int end_col, int end_row);

/*
 * Where the selection currently is, in viewport cells, inclusive. Answers 0 when
 * nothing is selected, in which case the out-parameters are untouched.
 */
int ldmux_terminal_selection_bounds(LdmuxTerminal *t, int *start_col, int *start_row,
                                    int *end_col, int *end_row);

/** Set on a cell whose style flags put it inside the selection (bit 9). */
#define LDMUX_FLAG_SELECTED (1u << 9)

/*
 * The style block, bits 0..8. Named here as well as in the cell encoder because
 * these are a wire contract with a reader in another language: Kotlin decodes the
 * same word in `TerminalFrame.kt`, cannot see these constants, and a bit that means
 * "inverse" on one side and something else on the other is a silent corruption.
 * `packages/mobile/test/frame-flags.test.ts` compares the two lists, which only
 * works while both are declarations rather than shifts inline in the encoder.
 *
 * Icons: `pi` draws its own cursor as reverse video, so bit 5 is what makes the
 * cursor visible at all on this side of the wire.
 */
#define LDMUX_FLAG_BOLD (1u << 0)
#define LDMUX_FLAG_ITALIC (1u << 1)
#define LDMUX_FLAG_UNDERLINE (1u << 2)
#define LDMUX_FLAG_FAINT (1u << 3)
#define LDMUX_FLAG_BLINK (1u << 4)
#define LDMUX_FLAG_INVERSE (1u << 5)
#define LDMUX_FLAG_INVISIBLE (1u << 6)
#define LDMUX_FLAG_STRIKETHROUGH (1u << 7)
#define LDMUX_FLAG_OVERLINE (1u << 8)

#endif /* LDMUX_TERMINAL_H */
