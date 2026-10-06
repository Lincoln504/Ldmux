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
 * THREADING CONTRACT. An LdmuxTerminal has no locks and is NOT thread-safe: every
 * call on one handle (including ldmux_terminal_free, and including the borrowed
 * buffers that render / take_clipboard / selection_text return) must come from
 * one thread at a time, and nothing may touch the handle after free. A binding
 * that can be closed from another thread than the one using it (the Android
 * `TerminalEngine`) must serialise the calls itself; the core will not.
 *
 * Frame format returned by ldmux_terminal_render (little-endian):
 *   header (LDMUX_FRAME_HEADER = 48 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle,
 *                      u8 defFgR, u8 defFgG, u8 defFgB,
 *                      u8 defBgR, u8 defBgG, u8 defBgB,
 *                      u8 passwordInput, u8 viewportActive, u8 mouseTracking,
 *                      u32 scrollTotal, u32 scrollOffset, u32 scrollLen,
 *                      u8 mouseSgr, u8 alternateOn,
 *                      u8 selectionOn, u16 selStartCol, u8 selStartRow,
 *                      u16 selEndCol, u8 selEndRow,
 *                      u8 cursorKnown, u8 frameVersion, u32 extLen,
 *                      u8 selStartRowHi, u8 selEndRowHi)
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
 *                      u8 utf8[utf8Len]            (utf8Len <= LDMUX_CELL_MAX_UTF8)
 *   then an extension block of extLen bytes (see below), absent when extLen = 0.
 *
 *   flags (u16): bit0 bold, bit1 italic, bit2 underline (ANY style), bit3 faint,
 *          bit4 blink, bit5 inverse, bit6 invisible, bit7 strikethrough,
 *          bit8 overline, bit9 selected,
 *          bit10 wide (head of a double-width glyph; the next cell is its tail),
 *          bit11 spacerTail (the tail: zero-length text that must not be
 *                painted, and which is NOT a blank to draw a glyph into),
 *          bits12..14 underlineStyle (0 none, 1 single, 2 double, 3 curly,
 *                4 dotted, 5 dashed; 6 and 7 are reserved, read them as single),
 *          bit15 hyperlink (an OSC 8 link whose URI is in the extension block)
 *
 * VERSIONING. The format is additive. `frameVersion` (header byte 41) is 0 for
 * frames written before the extension (bytes 41..47 were reserved zeros) and
 * LDMUX_FRAME_VERSION (1) now. A reader must (a) ignore flag bits it does not
 * know, (b) ignore any extension section whose kind it does not know, (c) ignore
 * the whole extension block when frameVersion is greater than it understands (it
 * still reads the cells: their layout never changes, only grows trailing data).
 * Bit 2 keeps meaning "underlined" for every style, so a reader that predates the
 * style bits still draws a single underline.
 *
 * EXTENSION BLOCK (follows the last cell record, extLen bytes, a run of sections):
 *   section = u8 kind, u32 len, u8 payload[len]
 *   kind 1  underline colour: (u32 cellIndex, u8 r, u8 g, u8 b)*   (7 bytes each)
 *           only cells with underlineStyle != 0 and an explicit colour appear;
 *           cellIndex = row * cols + col. Absent means "use the foreground".
 *   kind 2  hyperlinks: u16 uriCount, uriCount * (u16 len, u8 uri[len]),
 *           u32 refCount, refCount * (u32 cellIndex, u16 uriIndex).
 *           URIs are raw bytes (UTF-8 in practice), at most LDMUX_URI_MAX each
 *           and LDMUX_URI_TABLE_MAX in total per frame; a link that does not fit
 *           is dropped and its cell's hyperlink flag is NOT set, so the flag
 *           always has a URI behind it.
 * Header bytes 46/47 are the high bytes of the selection start/end rows (the
 * u8 rows at 36/39 wrap at 256), so a row is `lo | hi << 8`.
 *
 * The Kotlin side pins the same numbers in `TerminalFrame.kt`; a test there
 * checks the header size, and both sides must change together.
 */
#ifndef LDMUX_TERMINAL_H
#define LDMUX_TERMINAL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Longest cell text carried, in bytes. A cluster longer than this is cut at the
 * last whole code point that fits (never inside one). 64 is the Unicode
 * Stream-Safe limit (31 non-starters) and ~2x the longest real ZWJ sequence; the
 * u8 length on the wire allows up to 255. */
#define LDMUX_CELL_MAX_UTF8 64
/* The fixed part of a cell record, before its text. */
#define LDMUX_CELL_FIXED 9
#define LDMUX_CELL_RECORD_MAX (LDMUX_CELL_FIXED + LDMUX_CELL_MAX_UTF8)
#define LDMUX_FRAME_HEADER 48
#define LDMUX_FRAME_VERSION 1
/* Hyperlink bounds per frame (a hostile program can paint a link per cell). */
#define LDMUX_URI_MAX 2048
#define LDMUX_URI_TABLE_MAX (64 * 1024)
#define LDMUX_URI_DISTINCT_MAX 256
/* Extension section kinds. */
#define LDMUX_EXT_UNDERLINE_COLOUR 1
#define LDMUX_EXT_HYPERLINKS 2
/* The most text one OSC 52 / 1337 / 5522 clipboard write may carry before the
 * shim refuses it (the shim buffers it in memory). */
#define LDMUX_CLIPBOARD_MAX_BYTES (256u * 1024u)

/**
 * Scroll deltas that mean "the ends", not a number of rows. They are sentinel
 * values rather than a separate function so a binding has one scroll call.
 */
#define LDMUX_SCROLL_TOP INT32_MIN
#define LDMUX_SCROLL_BOTTOM INT32_MAX

/** Opaque: the fields are the core's business and no binding may depend on them. */
typedef struct LdmuxTerminal LdmuxTerminal;

/** The largest grid on a side. `cols`/`rows` outside [1, LDMUX_MAX_DIM] are clamped
 *  by [ldmux_terminal_new] and [ldmux_terminal_resize]: the engine allocates for
 *  the whole grid, so an absurd size (65535 x 65535) is a hang, not an error. */
#define LDMUX_MAX_DIM 2048

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
 * The client drives the selection through its two handles (selection_set), a
 * range of viewport cells. The engine's own pointer-gesture entry points (press,
 * deep press, drag, release) were removed: nothing called them.
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
 * This is for the *handles* the user drags, where the two ends are independent
 * and the range may be inverted by dragging one past the other. A negative column
 * or row is
 * clamped to zero; the engine is asked for the reference and a refusal is a
 * refusal, not a crash.
 */
int ldmux_terminal_selection_set(LdmuxTerminal *t, int start_col, int start_row,
                                 int end_col, int end_row);

/*
 * Where the selection currently is, in viewport cells, inclusive. Answers 0 when
 * nothing is selected, in which case the out-parameters are untouched.
 *
 * (The frame's selection rows are u8 at bytes 36/39 plus a high byte at 46/47, so
 * they are exact up to 65535; before the high bytes existed a reader saw rows
 * wrap above 255.)
 */
int ldmux_terminal_selection_bounds(LdmuxTerminal *t, int *start_col, int *start_row,
                                    int *end_col, int *end_row);

/** Set on a cell whose style flags put it inside the selection (bit 9). */
#define LDMUX_FLAG_SELECTED (1u << 9)
/** Head of a double-width glyph (bit 10). */
#define LDMUX_FLAG_WIDE (1u << 10)
/** Tail of a double-width glyph: zero-length text, never painted (bit 11). */
#define LDMUX_FLAG_SPACER_TAIL (1u << 11)
/** Underline style, 3 bits at 12..14: the GHOSTTY_SGR_UNDERLINE_* values. */
#define LDMUX_UNDERLINE_STYLE_SHIFT 12
#define LDMUX_UNDERLINE_STYLE_MASK 7u
/** The cell is inside an OSC 8 hyperlink with a URI in the extension (bit 15). */
#define LDMUX_FLAG_HYPERLINK (1u << 15)

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
