/*
 * Host-runnable tests for the portable terminal shim.
 *
 * This file exists because the shim had no test at all, and the first bug it
 * would have caught was already shipped to a device: a clipboard write that the
 * engine asked us to perform and we silently denied. A device test cannot see
 * that — the phone agrees that nothing happened and says nothing — so the
 * question has to be asked somewhere the answer is visible. That is here.
 *
 * It links the *host* build of libghostty-vt, not the Android one, so it needs
 * no emulator, no phone and no daemon, and it runs in milliseconds. The engine
 * is the same engine and the shim is the same shim, compiled from the same
 * sources, so what is true here is true on the device.
 *
 *   scripts/build-libghostty-vt.sh --host   # once, or use an existing host build
 *   cc -o test_engine test_engine.c ldmux_terminal.c $(host libghostty-vt.a) -lm
 *
 * Deliberately dependency-free: no framework, no build system, no assertions
 * library. A test that needs scaffolding to be written is a test that does not
 * get written.
 */
#include "ldmux_terminal.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int checks = 0;
static int failures = 0;

#define CHECK(cond)                                                            \
  do {                                                                         \
    checks++;                                                                  \
    if (!(cond)) {                                                             \
      failures++;                                                              \
      fprintf(stderr, "  FAIL %s:%d  %s\n", __FILE__, __LINE__, #cond);        \
    }                                                                          \
  } while (0)

#define CHECK_EQ_INT(got, want)                                                \
  do {                                                                         \
    checks++;                                                                  \
    long g_ = (long)(got), w_ = (long)(want);                                  \
    if (g_ != w_) {                                                            \
      failures++;                                                              \
      fprintf(stderr, "  FAIL %s:%d  %s: got %ld, want %ld\n", __FILE__,       \
              __LINE__, #got, g_, w_);                                         \
    }                                                                          \
  } while (0)

static void feed(LdmuxTerminal *t, const char *s) {
  ldmux_terminal_write(t, (const uint8_t *)s, strlen(s));
}

/* The clipboard slot, read the one way it may be read: a single call that both
 * answers and consumes, because two reads of a consumed value is exactly the
 * class of bug this repo has already paid for once. */
typedef struct {
  int kind;
  char text[8192];
  size_t len;
} Clip;

static bool kind_is_set(int k) { return k == LDMUX_CLIPBOARD_SET; }

static Clip take(LdmuxTerminal *t) {
  Clip c;
  memset(&c, 0, sizeof(c));
  const uint8_t *out = NULL;
  size_t len = 0;
  c.kind = ldmux_terminal_take_clipboard(t, &out, &len);
  c.len = len;
  if (kind_is_set(c.kind) && out && len < sizeof(c.text)) {
    memcpy(c.text, out, len);
  }
  return c;
}

/* ---------------------------------------------------------------- clipboard */

/* The sequence a program actually emits. `c` is the CLIPBOARD selection. */
#define OSC52_CLIPBOARD "c"

static void test_write_sets_the_clipboard(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  CHECK(t != NULL);

  feed(t, "\x1b]52;" OSC52_CLIPBOARD ";aGVsbG8=\x07"); /* base64 "hello" */
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK_EQ_INT(c.len, 5);
  CHECK(c.len == 5 && memcmp(c.text, "hello", 5) == 0);

  ldmux_terminal_free(t);
}

/* ST is the other terminator the standard allows, and programs use both. */
static void test_st_terminator_too(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVsbG8=\x1b\\");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK(c.len == 5 && memcmp(c.text, "hello", 5) == 0);
  ldmux_terminal_free(t);
}

/* An empty contents list is the clear it says it is, not an empty value: the
 * user copied nothing and their clipboard should be empty, not holding "". */
static void test_empty_payload_clears(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVsbG8=\x07");
  (void)take(t); /* consume the set */
  feed(t, "\x1b]52;c;\x07");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_CLEAR);
  ldmux_terminal_free(t);
}

static void test_nothing_reads_as_none(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "no clipboard here\n");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_NONE);
  ldmux_terminal_free(t);
}

/* Reading consumes. A second call must not hand the same value out again. */
static void test_read_consumes(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVsbG8=\x07");
  Clip first = take(t);
  Clip second = take(t);
  CHECK_EQ_INT(first.kind, LDMUX_CLIPBOARD_SET);
  CHECK_EQ_INT(second.kind, LDMUX_CLIPBOARD_NONE);
  ldmux_terminal_free(t);
}

/* Last writer wins, and only the last: the clipboard is a slot, not a queue.
 * A queue here would paste stale values in order, which is the behaviour no
 * terminal has. */
static void test_last_write_wins(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;b25l\x07");    /* "one" */
  feed(t, "\x1b]52;c;dHdv\x07");    /* "two" */
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK(c.len == 3 && memcmp(c.text, "two", 3) == 0);
  CHECK_EQ_INT(take(t).kind, LDMUX_CLIPBOARD_NONE);
  ldmux_terminal_free(t);
}

/* Non-BMP text is the reason the value crosses the JNI seam as bytes and not as
 * a jstring: Java's modified UTF-8 cannot represent it, so a String round-trip
 * would mangle an emoji in a copied path. Base64 "8J+YgA==" is U+1F600. */
static void test_non_bmp_survives_exactly(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;8J+YgA==\x07");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK_EQ_INT(c.len, 4);
  CHECK(c.len == 4 && memcmp(c.text, "\xf0\x9f\x98\x80", 4) == 0);
  ldmux_terminal_free(t);
}

/* A sequence split across two writes must still be one write. Anything else
 * fails on real output, where a pty read boundary lands wherever it lands. */
static void test_split_across_writes(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVs");
  feed(t, "bG8=\x07");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK(c.len == 5 && memcmp(c.text, "hello", 5) == 0);
  ldmux_terminal_free(t);
}

/* Programs wrap base64 at 76 columns, and a terminal must tolerate the line
 * breaks the standard permits. */
static void test_wrapped_base64(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVs\nbG8=\x07");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK(c.len == 5 && memcmp(c.text, "hello", 5) == 0);
  ldmux_terminal_free(t);
}

/* The buffer grows. An initial size that only fits small copies is a size that
 * drops large ones silently, which looks identical to the application's bug. */
static void test_large_payload_grows(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  /* "aGVsbG8h" is base64 "hello!" with no padding, so it may be repeated. */
  const int reps = 700;
  char seq[16 + 700 * 8 + 2];
  size_t p = 0;
  const char *head = "\x1b]52;c;";
  memcpy(seq, head, strlen(head));
  p = strlen(head);
  for (int i = 0; i < reps; i++) {
    memcpy(seq + p, "aGVsbG8h", 8);
    p += 8;
  }
  seq[p++] = '\x07';

  ldmux_terminal_write(t, (const uint8_t *)seq, p);
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK_EQ_INT(c.len, (long)reps * 6);
  bool ok = c.len == (size_t)reps * 6;
  for (int i = 0; ok && i < reps; i++) {
    ok = memcmp(c.text + i * 6, "hello!", 6) == 0;
  }
  CHECK(ok);
  ldmux_terminal_free(t);
}

/* A write that never terminates must not be read out as if it had. */
static void test_unterminated_is_not_delivered(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b]52;c;aGVsbG8="); /* no BEL, no ST */
  CHECK_EQ_INT(take(t).kind, LDMUX_CLIPBOARD_NONE);
  ldmux_terminal_free(t);
}

/* ------------------------------------------------------------------- frame */

/* The header is a contract with Kotlin, and two of its fields are a contract with
 * the tap policy: byte 8 is whether the program asks for a cursor at all, and byte
 * 40 is whether the coordinates are real. A client that conflates them cannot
 * serve `pi`, which hides the hardware cursor and draws its own. */
static void test_frame_header_contract(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "hi");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(len >= 48);
  CHECK_EQ_INT(buf[0] | (buf[1] << 8), 80); /* cols, little endian */
  CHECK_EQ_INT(buf[2] | (buf[3] << 8), 24); /* rows */
  CHECK_EQ_INT(buf[8], 1);                  /* cursorVisible */
  CHECK_EQ_INT(buf[32], 0);                 /* alternateOn: primary screen */
  CHECK_EQ_INT(buf[40], 1);                 /* cursorKnown */

  ldmux_terminal_free(t);
}

/* Alternate screen is the one signal the client gets about a full-screen
 * program. What it must NOT be read as is "this is a TUI composer": measured
 * 2026-09-27, `pi` in its default `regular` mode emits no `?1049h` at all (and no
 * mouse modes) and draws its composer on the primary screen, so a client that
 * keyed tap handling off this bit alone would mis-serve it. The bit reports the
 * screen and nothing else. */
static void test_alternate_screen_bit(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\x1b[?1049h");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[32], 1);
  feed(t, "\x1b[?1049l");
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[32], 0);
  ldmux_terminal_free(t);
}

/* The cursor is where the client believes it is, or tap-to-cursor aims at the
 * wrong cell. A printable byte must advance exactly one column. */
static void test_cursor_advances_one_column(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "abc");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[4] | (buf[5] << 8), 3); /* cursorX */
  CHECK_EQ_INT(buf[6] | (buf[7] << 8), 0); /* cursorY */
  ldmux_terminal_free(t);
}

/* One cell's grapheme bytes. A wide glyph and the cell it spills into are only
 * distinguishable by looking at the bytes, so this is the only way to answer
 * "does the tail repeat the glyph?" with evidence. */
static bool cell_text_at(const uint8_t *buf, size_t len, int want_col, int want_row,
                         const uint8_t **out, size_t *out_len) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = LDMUX_FRAME_HEADER;
  for (int r = 0; r < rows; r++) {
    for (int c = 0; c < cols; c++) {
      if (p + 9 > len) return false;
      uint8_t n = buf[p];
      if (r == want_row && c == want_col) {
        if (p + 9 + n > len) return false;
        *out = buf + p + 9;
        *out_len = n;
        return true;
      }
      p += 9 + (size_t)n;
    }
  }
  return false;
}

/* Read one cell's flag word out of a rendered frame. The frame is a flat run of
 * `9 + grapheme_bytes` records after the header, in row-major order, and a test
 * that asserts "this cell is not selected" has to be able to look at a specific
 * cell rather than at the whole frame. */
static uint16_t cell_flags_at(const uint8_t *buf, size_t len, int want_col,
                              int want_row) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = LDMUX_FRAME_HEADER;
  for (int r = 0; r < rows; r++) {
    for (int c = 0; c < cols; c++) {
      if (p + 9 > len) return 0;
      uint8_t n = buf[p];
      uint16_t flags = (uint16_t)(buf[p + 1] | (buf[p + 2] << 8));
      if (r == want_row && c == want_col) return flags;
      p += 9 + (size_t)n;
    }
  }
  return 0;
}

/* Does a row of a rendered frame start with [want], one grapheme per cell?
 * A cell holds a single character, so a test that asserts on what a row *says*
 * has to read the cells one at a time — which is what a replay has to get
 * right, and what "the screen is the new one" means in cells. */
static bool row_text_starts(const uint8_t *buf, size_t len, int want_row,
                            const char *want) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = LDMUX_FRAME_HEADER;
  for (int r = 0; r < want_row && r < rows; r++) {
    for (int c = 0; c < cols; c++) {
      if (p + 9 > len) return false;
      p += 9 + (size_t)buf[p];
    }
  }
  for (int c = 0; c < cols && want[c] != 0; c++) {
    if (p + 9 > len) return false;
    uint8_t n = buf[p];
    if (n != 1 || buf[p + 9] != (uint8_t)want[c]) return false;
    p += 9 + (size_t)n;
  }
  return true;
}

/* A frame is the *whole* grid, every time.
 *
 * The render state tracks dirty rows, and its `next_dirty` iterator skips rows that
 * are clean. The shim uses the plain iterator and pads whatever it does not get
 * with blank cells, so the two only agree while every row is produced. If a partial
 * change ever made the emulator hand back fewer rows, the rows it omitted would be
 * *erased* on screen rather than left alone. Measured rather than argued from the
 * header's wording, because the header describes both iterators and only one of them
 * filters. */
static void test_every_frame_is_the_full_grid(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "one");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));

  /* Change one row. Row 0 is now the row the emulator has no reason to redraw. */
  feed(t, "\r\n\x1b[Ktwo");
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(row_text_starts(buf, len, 0, "one"));
  CHECK(row_text_starts(buf, len, 1, "two"));

  /* ...and a render with nothing new is the same frame, not a blank one. */
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(row_text_starts(buf, len, 0, "one"));
  CHECK(row_text_starts(buf, len, 1, "two"));
  ldmux_terminal_free(t);
}

/* A wide glyph occupies two cells. What the second one holds is not specified
 * anywhere a client can see, and the painter draws a cell's grapheme whenever it
 * has one — so a tail that repeated the glyph would draw it twice, and a tail the
 * engine reports as empty is the only thing that makes drawing per cell correct. */
static void test_a_wide_glyph_does_not_paint_twice(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "\xe4\xbd\xa0" "a"); /* U+4F60 is wide, then a narrow character */
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));

  const uint8_t *cell = NULL;
  size_t n = 0;
  CHECK(cell_text_at(buf, len, 0, 0, &cell, &n));
  CHECK(n == 3 && memcmp(cell, "\xe4\xbd\xa0", 3) == 0); /* the glyph, once */
  CHECK(cell_text_at(buf, len, 1, 0, &cell, &n));
  CHECK(n == 0); /* the tail carries no grapheme, so nothing draws twice */
  CHECK(cell_text_at(buf, len, 2, 0, &cell, &n));
  CHECK(n == 1 && cell[0] == 'a'); /* ...and the glyph advanced two columns */
  CHECK_EQ_INT(buf[4] | (buf[5] << 8), 3); /* cursorX */
  ldmux_terminal_free(t);
}

static int selected_cell_count(const uint8_t *buf, size_t len) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = LDMUX_FRAME_HEADER;
  int count = 0;
  for (int r = 0; r < rows; r++) {
    for (int c = 0; c < cols; c++) {
      if (p + 9 > len) return count;
      uint8_t n = buf[p];
      if ((buf[p + 1] | (buf[p + 2] << 8)) & LDMUX_FLAG_SELECTED) count++;
      p += 9 + (size_t)n;
    }
  }
  return count;
}

/* The geometry the client would report: 80 columns, 10x20 pixel cells. */
static void use_geometry(LdmuxTerminal *t) {
  ldmux_terminal_set_geometry(t, 80, 10, 20, 0, 480);
}

static const char *selection_cstr(LdmuxTerminal *t) {
  static char out[8192];
  size_t len = 0;
  const uint8_t *p = ldmux_terminal_selection_text(t, &len);
  if (!p) return NULL;
  if (len >= sizeof(out)) len = sizeof(out) - 1;
  memcpy(out, p, len);
  out[len] = 0;
  return out;
}

/* A press, a drag and a release is the whole gesture, and the text that comes
 * back is the thing the user will see on the clipboard. */
/* A press, a drag and a release is the whole gesture, and the text that comes
 * back is the thing the user will see on the clipboard. */
/* **`pi`'s cursor, exactly — the case that made it invisible.**
 *
 * pi hides the hardware cursor (`ESC[?25l`) and draws its own as an inverse-video
 * cell. The client read `cursorVisible = 0` and drew no hardware cursor (right),
 * but it also never rendered the inverse attribute, so the cell pi drew for its
 * own cursor came out as ordinary text — and on a blank cell, where the painter
 * skips cells with no text, as nothing at all. Two facts have to survive the wire
 * for that to work: the cursor is hidden, and its coordinates are still real. */
static void test_pi_style_cursor_is_hidden_and_inverted(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "\x1b[?25l");        /* pi hides the hardware cursor... */
  feed(t, "\x1b[7m \x1b[27m"); /* ...and draws its own, inverted */
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[8], 0);  /* nothing for the client to draw itself */
  CHECK_EQ_INT(buf[40], 1); /* but its position is real, so a tap can walk to it */
  CHECK((cell_flags_at(buf, len, 0, 0) & LDMUX_FLAG_INVERSE) != 0);
  CHECK((cell_flags_at(buf, len, 1, 0) & LDMUX_FLAG_INVERSE) == 0);
  /* The inverse bit is the style block's, and setting it must not disturb the
   * flags around it: bit 9 is selection and this cell is not selected. */
  CHECK((cell_flags_at(buf, len, 0, 0) & LDMUX_FLAG_SELECTED) == 0);
  ldmux_terminal_free(t);
}

static void test_set_selects_text(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 10, 0));
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello world"), 0);
  ldmux_terminal_free(t);
}

/* The painter draws what the frame says, so the frame has to mark exactly the
 * cells inside the selection and no others. */
static void test_frame_marks_selected_cells(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 4, 0));
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK((cell_flags_at(buf, len, 0, 0) & LDMUX_FLAG_SELECTED) != 0);
  CHECK((cell_flags_at(buf, len, 4, 0) & LDMUX_FLAG_SELECTED) != 0);
  CHECK((cell_flags_at(buf, len, 5, 0) & LDMUX_FLAG_SELECTED) == 0);
  CHECK((cell_flags_at(buf, len, 0, 1) & LDMUX_FLAG_SELECTED) == 0);
  /* And a cell outside the selection keeps every flag it had: this is one more
   * bit, not a different style. */
  CHECK((cell_flags_at(buf, len, 5, 0) & 0x3ffu) == 0);
  ldmux_terminal_free(t);
}

static void test_clear_removes_the_flag_and_the_text(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 10, 0));
  CHECK(selection_cstr(t) != NULL);
  CHECK(ldmux_terminal_select_clear(t));
  CHECK(selection_cstr(t) == NULL);
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(selected_cell_count(buf, len), 0);
  ldmux_terminal_free(t);
}

static void test_selection_spans_lines(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "one\r\ntwo");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 2, 1));
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "one\ntwo"), 0);
  ldmux_terminal_free(t);
}

static void test_select_all_covers_the_screen(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "abc");
  CHECK(ldmux_terminal_select_all(t));
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  /* Selecting all means the screen's content, not its dimensions: trailing
   * blanks are trimmed. Measured as three cells, not 80x24. */
  CHECK_EQ_INT(selected_cell_count(buf, len), 3);
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "abc"), 0);
  ldmux_terminal_free(t);
}

/* An empty screen has nothing to select, and asking must answer "nothing"
 * rather than an empty string the caller would put on the clipboard. */
/* ---------------------------------------------------------------------------
 * The handle API: the two handles the user drags, where the ends are independent.
 * ------------------------------------------------------------------------- */

static void test_set_round_trips_through_bounds(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 4, 0));
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sc, 0);
  CHECK_EQ_INT(sr, 0);
  CHECK_EQ_INT(ec, 4);
  CHECK_EQ_INT(er, 0);
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello"), 0);
  ldmux_terminal_free(t);
}

static void test_set_orders_an_inverted_range(void) {
  /* Dragging the end handle back above the start is ordinary, and an inverted
   * range formats to nothing — a copy that silently produces an empty string. */
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK(ldmux_terminal_selection_set(t, 6, 0, 0, 0));
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sc, 0);
  CHECK_EQ_INT(ec, 6);
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello w"), 0);
  ldmux_terminal_free(t);
}

static void test_set_replaces_an_earlier_selection(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  /* A word, then the handles widen it. */
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 4, 0));
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sc, 0);
  CHECK_EQ_INT(ec, 4);
  CHECK(ldmux_terminal_selection_set(t, sc, sr, 10, 0));
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello world"), 0);
  ldmux_terminal_free(t);
}

static void test_bounds_is_zero_without_a_selection(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  /* The render is what caches the grid size, so this also pins that a render
   * before any selection is not a special case. */
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(!ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sc, -1);
  CHECK_EQ_INT(buf[33], 0);
  ldmux_terminal_free(t);
}

static void test_endpoints_land_on_the_right_row_after_scrolling(void) {
  /* The decisive one. A grid reference carries a row node and an x, and the header
   * does not say whether its y is a viewport row or an absolute row in the
   * scrollback — so this fills the screen past the top, which makes the two
   * conventions disagree, and asserts the viewport reading. A selection made at
   * viewport row 0 must come back as row 0 and must contain the row that is
   * actually on screen. */
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  for (int i = 0; i < 30; i++) {
    char line[32];
    snprintf(line, sizeof(line), "line-%02d\r\n", i);
    feed(t, line);
  }
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 6, 0));
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sr, 0);
  CHECK_EQ_INT(er, 0);
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  /* Which line that is depends on the scrollback, so the assertion is on shape:
   * seven characters, no newline, and the same text the frame shows on row 0. */
  if (s) {
    CHECK_EQ_INT(strlen(s), 7);
    CHECK(strchr(s, '\n') == NULL);
    CHECK(strncmp(s, "line-", 5) == 0);
  }
  ldmux_terminal_free(t);
}

static void test_a_selection_follows_its_text_when_the_viewport_moves(void) {
  /* The two halves of the auto-scroll a drag-past-the-edge depends on, asserted
   * positively rather than assumed.
   *
   * The gesture carries the end that is not being dragged by adding the scroll
   * delta to its row. For that to be right, two things must hold: a selection has
   * to be anchored to TEXT and not to the viewport, and the sign of ghostty's
   * delta — its header says "up is negative" — has to move a text-anchored
   * endpoint's viewport row the way the arithmetic assumes.
   *
   * Scrolling the viewport up shows older lines, so content moves DOWN the
   * screen: the characters at row 0 are at row 1 afterwards. The same seven
   * characters must come back, one row lower. That is the fact the gesture
   * relies on, and it is the one that was inverted. */
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  for (int i = 0; i < 30; i++) {
    char line[32];
    snprintf(line, sizeof(line), "line-%02d\r\n", i);
    feed(t, line);
  }
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));

  CHECK(ldmux_terminal_selection_set(t, 0, 0, 6, 0));
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sr, 0);
  char first[64];
  const char *s0 = selection_cstr(t);
  CHECK(s0 != NULL);
  snprintf(first, sizeof(first), "%s", s0 ? s0 : "");

  /* Up one row. */
  ldmux_terminal_scroll(t, -1);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sr, 1);
  CHECK_EQ_INT(er, 1);
  const char *s1 = selection_cstr(t);
  CHECK(s1 != NULL);
  if (s1) CHECK(strcmp(first, s1) == 0);

  /* And back down, which must return it exactly. */
  ldmux_terminal_scroll(t, 1);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  CHECK_EQ_INT(sr, 0);
  const char *s2 = selection_cstr(t);
  if (s2) CHECK(strcmp(first, s2) == 0);

  ldmux_terminal_free(t);
}

/* A u32 from the frame header at [off], little-endian, exactly as the client reads it. */
static uint32_t frame_u32(const uint8_t *buf, size_t off) {
  return (uint32_t)buf[off] | ((uint32_t)buf[off + 1] << 8) |
         ((uint32_t)buf[off + 2] << 16) | ((uint32_t)buf[off + 3] << 24);
}

static void test_the_scroll_offset_is_what_actually_moved(void) {
  /* The premise the drag's auto-scroll now rests on, asserted rather than assumed.
   *
   * `scrollBy` reports the rows the viewport moved instead of assuming it moved by
   * what was asked, and it reads that from the header's `scrollOffset`. For the
   * report to be sound two things have to hold: a scroll that happens changes the
   * offset by exactly the amount requested, and a scroll the engine refuses changes
   * it by nothing. The second is what makes "one row per move, skipped on the
   * alternate screen" true by construction, with no flag that can disagree with the
   * viewport it describes. On the alternate screen a drag used to walk the anchored
   * end away from its text while the content stood still — in vim, in htop, in less.
   */
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  for (int i = 0; i < 30; i++) {
    char line[32];
    snprintf(line, sizeof(line), "line-%02d\r\n", i);
    feed(t, line);
  }
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  const int base = (int)frame_u32(buf, 23);

  /* The offset counts into the scrollback from the top, so going back in history
   * moves it *down* the number line. */
  ldmux_terminal_scroll(t, -1);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23) - base, -1);

  ldmux_terminal_scroll(t, -3);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23) - base, -4);

  /* An end of the history is a refusal in the same sense: the request is larger
   * than what is there, the viewport pins, and the report must be a small number
   * rather than the one asked for. That is why the caller cannot use its own step. */
  const int pinned = (int)frame_u32(buf, 23);
  ldmux_terminal_scroll(t, -9999);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  const int at_top = (int)frame_u32(buf, 23);
  CHECK(at_top <= 0);
  CHECK(at_top < pinned);
  ldmux_terminal_scroll(t, -5);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23), at_top); /* already at the top: no move */

  /* And the alternate screen, which keeps no scrollback at all. */
  feed(t, "\x1b[?1049h");
  CHECK(ldmux_terminal_render(t, &buf, &len));
  const int alt = (int)frame_u32(buf, 23);
  ldmux_terminal_scroll(t, -1);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23), alt);
  ldmux_terminal_scroll(t, -10);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23), alt);

  ldmux_terminal_free(t);
}

/* The client's resize path replays a captured screen into a live engine with
 * `ESC[r ?6l 4l H 2J` + the capture, instead of rebuilding the engine — the
 * rebuild threw away the scrollback the client had accumulated, which is how a
 * rotation used to empty it. Two things have to hold for that path to be safe,
 * and both are engine behaviour, so both are asserted here against the engine:
 * the replay replaces the visible screen and nothing else, and the scrollback
 * above it survives. */
/* Output does not move the viewport.
 *
 * SPEC §7.4 calls this the property the scroll model exists for, and nothing tested
 * it — not here and not in Kotlin. The client's anchor is the emulator's own
 * viewport (whatever ADR-0004 describes as a client-owned line identity does not
 * exist in the code), so this assertion *is* the guarantee. If bytes arriving from
 * the pane dragged the view back to the bottom, reading history would be impossible
 * on any shell that is still printing — which is every shell worth reading. */
static void test_output_does_not_move_a_scrolled_viewport(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  for (int i = 0; i < 60; i++) {
    char line[32];
    snprintf(line, sizeof(line), "line-%02d\r\n", i);
    feed(t, line);
  }
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));

  ldmux_terminal_scroll(t, -10);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[17], 0); /* the viewport is back in history, not at the bottom */
  const uint32_t anchored = frame_u32(buf, 23);
  CHECK(anchored > 0);

  /* ...and the pane keeps printing, which is the case that matters. */
  for (int i = 60; i < 90; i++) {
    char line[32];
    snprintf(line, sizeof(line), "line-%02d\r\n", i);
    feed(t, line);
  }
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT((int)frame_u32(buf, 23), (int)anchored);
  CHECK_EQ_INT(buf[17], 0);
  ldmux_terminal_free(t);
}

static void test_a_resize_replay_replaces_the_screen_and_keeps_the_scrollback(void) {
  LdmuxTerminal *t = ldmux_terminal_new(20, 6);
  char line[32];
  for (int i = 0; i < 30; i++) {
    snprintf(line, sizeof(line), "L%02d\r\n", i);
    feed(t, line);
  }
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));

  /* Park at the top, then resize: the scrollback must survive the resize
   * itself, or there would be nothing for the replay to preserve. */
  ldmux_terminal_scroll(t, LDMUX_SCROLL_TOP);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(row_text_starts(buf, len, 0, "L00"));

  ldmux_terminal_resize(t, 20, 10);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  ldmux_terminal_scroll(t, LDMUX_SCROLL_TOP);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(row_text_starts(buf, len, 0, "L00"));

  /* The replay: reset state, clear the visible screen only, write the new
   * capture, place the cursor. What the Kotlin side builds, byte for byte. */
  feed(t, "\x1b[r\x1b[?6l\x1b[4l\x1b[H\x1b[2J");
  for (int i = 0; i < 10; i++) {
    snprintf(line, sizeof(line), i == 9 ? "N%d" : "N%d\r\n", i);
    feed(t, line);
  }
  feed(t, "\x1b[1;1H");
  CHECK(ldmux_terminal_render(t, &buf, &len));
  /* The replay wrote the live screen; the viewport is still parked where it
   * was — output never moves it — so the new screen is read from the bottom.
   * `LDMUX_SCROLL_BOTTOM` is the exact value the client sends for its "back to
   * now" pill (`NativeTerminal.SCROLL_BOTTOM`), and it takes its own branch in
   * the shim, so it is the value worth exercising rather than a large number
   * that only reaches the clamp. */
  ldmux_terminal_scroll(t, LDMUX_SCROLL_BOTTOM);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  for (int i = 0; i < 10; i++) {
    char want[16];
    snprintf(want, sizeof(want), "N%d", i);
    CHECK(row_text_starts(buf, len, i, want));
  }
  CHECK_EQ_INT(buf[17], 1); /* viewportActive: at the live bottom */
  CHECK_EQ_INT(buf[6] | (buf[7] << 8), 0); /* cursorY */
  /* And the oldest scrollback row is still the oldest row there was. */
  ldmux_terminal_scroll(t, LDMUX_SCROLL_TOP);
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(row_text_starts(buf, len, 0, "L00"));

  ldmux_terminal_free(t);
}

static void test_frame_carries_the_endpoints(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "one\r\ntwo\r\nthree");
  CHECK(ldmux_terminal_selection_set(t, 2, 1, 7, 1));
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[33], 1);
  CHECK_EQ_INT(buf[34] | (buf[35] << 8), 2);
  CHECK_EQ_INT(buf[36], 1);
  CHECK_EQ_INT(buf[37] | (buf[38] << 8), 7);
  CHECK_EQ_INT(buf[39], 1);
  ldmux_terminal_free(t);
}

static void test_clear_zeroes_the_frame_bits(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello");
  CHECK(ldmux_terminal_selection_set(t, 1, 0, 3, 0));
  ldmux_terminal_select_clear(t);
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[33], 0);
  int sc = -1, sr = -1, ec = -1, er = -1;
  CHECK(!ldmux_terminal_selection_bounds(t, &sc, &sr, &ec, &er));
  ldmux_terminal_free(t);
}

static void test_empty_screen_has_no_selection(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  CHECK(ldmux_terminal_selection_set(t, 10, 10, 20, 10));
  /* No text: an empty range is not a clipboard value, so the caller cannot put
   * "" on the clipboard and call it a copy. */
  CHECK(selection_cstr(t) == NULL);
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  /* But the range itself exists and the painter will show it, which is what a
   * terminal does when you drag across blank cells. Eleven cells because the
   * drag covers columns 10..20: the anchor cell is included and the cell the
   * drag ended on is not, the same convention as above, now confirmed on a
   * screen with no text in it at all. */
  CHECK_EQ_INT(selected_cell_count(buf, len), 11);
  ldmux_terminal_free(t);
}

/* Nothing here may touch memory the terminal has freed: the gesture holds
 * tracked references, so freeing it in the wrong order is a use-after-free
 * rather than a leak. */
static void test_free_with_a_live_selection(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world\r\nsecond line");
  CHECK(ldmux_terminal_selection_set(t, 0, 0, 11, 1));
  CHECK(selection_cstr(t) != NULL);
  ldmux_terminal_free(t);
  CHECK(1); /* reached only if free did not trip anything */
}


/* ------------------------------------------- frame v1: cells, clusters, links */

/* A parsed view of one frame: where every cell record starts, and where the
 * extension block begins. The old helpers walk the same records; this one keeps the
 * offsets so a test can ask for any cell and then read the extension. */
typedef struct {
  const uint8_t *buf;
  size_t len, ext_at, ext_len;
  int cols, rows, version;
  size_t off[8192];
} Fr;

static bool parse_frame(Fr *f, const uint8_t *buf, size_t len) {
  f->buf = buf;
  f->len = len;
  f->cols = buf[0] | (buf[1] << 8);
  f->rows = buf[2] | (buf[3] << 8);
  f->version = buf[41];
  f->ext_len = (size_t)buf[42] | ((size_t)buf[43] << 8) | ((size_t)buf[44] << 16) |
               ((size_t)buf[45] << 24);
  size_t p = LDMUX_FRAME_HEADER;
  if ((size_t)f->cols * f->rows > 8192) return false;
  for (int i = 0; i < f->cols * f->rows; i++) {
    if (p + LDMUX_CELL_FIXED > len) return false;
    f->off[i] = p;
    p += LDMUX_CELL_FIXED + buf[p];
  }
  f->ext_at = p;
  return p + f->ext_len == len; /* the frame is exactly cells + extension */
}

static size_t fr_n(const Fr *f, int col, int row) { return f->buf[f->off[row * f->cols + col]]; }
static const uint8_t *fr_text(const Fr *f, int col, int row) {
  return f->buf + f->off[row * f->cols + col] + LDMUX_CELL_FIXED;
}
static unsigned fr_flags(const Fr *f, int col, int row) {
  size_t o = f->off[row * f->cols + col];
  return (unsigned)(f->buf[o + 1] | (f->buf[o + 2] << 8));
}

/* Valid UTF-8, strictly enough to catch a cut inside a code point. */
static bool utf8_ok(const uint8_t *s, size_t n) {
  size_t i = 0;
  while (i < n) {
    uint8_t c = s[i];
    size_t need = c < 0x80 ? 0 : (c >> 5) == 6 ? 1 : (c >> 4) == 14 ? 2 : (c >> 3) == 30 ? 3 : 99;
    if (need == 99 || i + need >= n + (need ? 0 : 1)) {
      if (need == 99) return false;
      if (i + need > n - 1 + (need == 0)) return false;
    }
    for (size_t k = 1; k <= need; k++)
      if (i + k >= n || (s[i + k] & 0xC0) != 0x80) return false;
    i += need + 1;
  }
  return true;
}

/* Find an extension section by kind; returns its payload and length. */
static const uint8_t *ext_section(const Fr *f, int kind, size_t *plen) {
  size_t p = f->ext_at, end = f->ext_at + f->ext_len;
  while (p + 5 <= end) {
    uint32_t l = (uint32_t)f->buf[p + 1] | ((uint32_t)f->buf[p + 2] << 8) |
                 ((uint32_t)f->buf[p + 3] << 16) | ((uint32_t)f->buf[p + 4] << 24);
    if (p + 5 + l > end) return NULL;
    if (f->buf[p] == kind) {
      *plen = l;
      return f->buf + p + 5;
    }
    p += 5 + l;
  }
  return NULL;
}

static void test_frame_version_and_extension_length_are_consistent(void) {
  LdmuxTerminal *t = ldmux_terminal_new(20, 4);
  feed(t, "plain text");
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  CHECK_EQ_INT(f.version, LDMUX_FRAME_VERSION);
  CHECK_EQ_INT(f.ext_len, 0); /* nothing sparse on a plain screen */
  ldmux_terminal_free(t);
}

static void test_underline_style_is_in_the_flags_and_bit2_still_says_underlined(void) {
  LdmuxTerminal *t = ldmux_terminal_new(20, 4);
  feed(t, "\x1b[4mS\x1b[0m\x1b[4:2mD\x1b[0m\x1b[4:3mC\x1b[0m\x1b[4:4mO\x1b[0m\x1b[4:5mH\x1b[0mN");
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  for (int i = 0; i < 5; i++) {
    unsigned fl = fr_flags(&f, i, 0);
    CHECK(fl & LDMUX_FLAG_UNDERLINE); /* an old reader still draws a line */
    CHECK_EQ_INT((fl >> LDMUX_UNDERLINE_STYLE_SHIFT) & LDMUX_UNDERLINE_STYLE_MASK, i + 1);
  }
  CHECK(!(fr_flags(&f, 5, 0) & LDMUX_FLAG_UNDERLINE));
  CHECK_EQ_INT((fr_flags(&f, 5, 0) >> LDMUX_UNDERLINE_STYLE_SHIFT) & LDMUX_UNDERLINE_STYLE_MASK, 0);
  ldmux_terminal_free(t);
}

static void test_underline_colour_goes_in_the_extension(void) {
  LdmuxTerminal *t = ldmux_terminal_new(20, 4);
  /* cell 0: curly + RGB colour; cell 1: single + palette 9; cell 2: underlined, no colour */
  feed(t, "\x1b[4:3;58;2;255;64;0mA\x1b[0m\x1b[4;58;5;9mB\x1b[0m\x1b[4mC\x1b[0m");
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  size_t n = 0;
  const uint8_t *e = ext_section(&f, LDMUX_EXT_UNDERLINE_COLOUR, &n);
  CHECK(e != NULL);
  CHECK_EQ_INT(n, 14); /* two coloured cells, 7 bytes each; the third has none */
  if (e && n == 14) {
    CHECK_EQ_INT(e[0], 0); /* cell index 0 */
    CHECK(e[4] == 255 && e[5] == 64 && e[6] == 0);
    CHECK_EQ_INT(e[7], 1); /* cell index 1: palette 9 resolved to RGB, not an index */
    CHECK(!(e[11] == 0 && e[12] == 0 && e[13] == 0));
  }
  ldmux_terminal_free(t);
}

static void test_wide_head_and_spacer_tail_are_flagged(void) {
  LdmuxTerminal *t = ldmux_terminal_new(20, 4);
  feed(t, "\xe4\xbd\xa0" "a"); /* wide glyph, then a narrow one */
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  CHECK(fr_flags(&f, 0, 0) & LDMUX_FLAG_WIDE);
  CHECK(!(fr_flags(&f, 0, 0) & LDMUX_FLAG_SPACER_TAIL));
  CHECK(fr_flags(&f, 1, 0) & LDMUX_FLAG_SPACER_TAIL);
  CHECK_EQ_INT(fr_n(&f, 1, 0), 0);
  CHECK(!(fr_flags(&f, 2, 0) & (LDMUX_FLAG_WIDE | LDMUX_FLAG_SPACER_TAIL)));
  /* a blank the program never wrote is NOT a tail */
  CHECK(!(fr_flags(&f, 10, 0) & LDMUX_FLAG_SPACER_TAIL));
  ldmux_terminal_free(t);
}

static void test_a_zwj_family_is_carried_whole(void) {
  /* man ZWJ woman ZWJ girl ZWJ boy: 25 bytes, which the old 16-byte cap cut in the
   * middle of the third emoji. */
  static const char fam[] = "\xF0\x9F\x91\xA8\xE2\x80\x8D\xF0\x9F\x91\xA9\xE2\x80\x8D"
                            "\xF0\x9F\x91\xA7\xE2\x80\x8D\xF0\x9F\x91\xA6";
  LdmuxTerminal *t = ldmux_terminal_new(20, 4);
  /* The engine only clusters ZWJ sequences into one cell under DEC mode 2027
   * (grapheme clustering); without it the family spreads over several cells. */
  feed(t, "\x1b[?2027h");
  feed(t, fam);
  feed(t, "z");
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  CHECK_EQ_INT(fr_n(&f, 0, 0), sizeof(fam) - 1);
  CHECK(memcmp(fr_text(&f, 0, 0), fam, sizeof(fam) - 1) == 0);
  CHECK(utf8_ok(fr_text(&f, 0, 0), fr_n(&f, 0, 0)));
  CHECK(fr_flags(&f, 0, 0) & LDMUX_FLAG_WIDE);
  /* and the next character is where it should be: the cluster is one wide cell */
  CHECK(fr_n(&f, 2, 0) == 1 && fr_text(&f, 2, 0)[0] == 'z');
  ldmux_terminal_free(t);
}

/* Under ASan: a cluster longer than the cap, and one longer than the scratch. Neither
 * may overrun anything, and neither may come out cut inside a code point. */
static void test_a_huge_cluster_is_clipped_on_a_code_point_boundary(void) {
  for (int marks = 20; marks <= 600; marks += (marks < 220 ? 200 : 380)) {
    LdmuxTerminal *t = ldmux_terminal_new(20, 4);
    size_t n = 1 + (size_t)marks * 2;
    uint8_t *s = (uint8_t *)malloc(n + 1);
    s[0] = 'a';
    for (int i = 0; i < marks; i++) { s[1 + 2 * i] = 0xCC; s[2 + 2 * i] = 0x81; } /* U+0301 */
    ldmux_terminal_write(t, s, n);
    feed(t, "z");
    free(s);
    const uint8_t *buf; size_t len;
    CHECK(ldmux_terminal_render(t, &buf, &len));
    static Fr f;
    CHECK(parse_frame(&f, buf, len));
    size_t cn = fr_n(&f, 0, 0);
    CHECK(cn >= 1 && cn <= LDMUX_CELL_MAX_UTF8);
    CHECK(utf8_ok(fr_text(&f, 0, 0), cn));
    CHECK(fr_text(&f, 0, 0)[0] == 'a');
    if (marks == 20) CHECK_EQ_INT(cn, 41); /* fits: carried whole */
    /* whatever the engine did with the cluster, the grid after it is intact */
    bool z = false;
    for (int c = 0; c < 4; c++) z = z || (fr_n(&f, c, 0) == 1 && fr_text(&f, c, 0)[0] == 'z');
    CHECK(z);
    ldmux_terminal_free(t);
  }
}

static void test_hyperlinks_get_a_flag_and_a_shared_uri(void) {
  LdmuxTerminal *t = ldmux_terminal_new(40, 4);
  feed(t, "\x1b]8;;https://example.com/a\x07" "ab\x1b]8;;\x07 "
          "\x1b]8;;https://example.com/b\x07" "c\x1b]8;;\x07"
          "\x1b]8;;https://example.com/a\x07" "d\x1b]8;;\x07" "e");
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  /* a b _ c d e */
  CHECK(fr_flags(&f, 0, 0) & LDMUX_FLAG_HYPERLINK);
  CHECK(fr_flags(&f, 1, 0) & LDMUX_FLAG_HYPERLINK);
  CHECK(!(fr_flags(&f, 2, 0) & LDMUX_FLAG_HYPERLINK));
  CHECK(fr_flags(&f, 3, 0) & LDMUX_FLAG_HYPERLINK);
  CHECK(fr_flags(&f, 4, 0) & LDMUX_FLAG_HYPERLINK);
  CHECK(!(fr_flags(&f, 5, 0) & LDMUX_FLAG_HYPERLINK));
  size_t n = 0;
  const uint8_t *e = ext_section(&f, LDMUX_EXT_HYPERLINKS, &n);
  CHECK(e != NULL);
  if (e) {
    CHECK_EQ_INT(e[0] | (e[1] << 8), 2); /* two distinct URIs for four linked cells */
    size_t p = 2;
    const char *want[2] = {"https://example.com/a", "https://example.com/b"};
    for (int i = 0; i < 2; i++) {
      size_t l = (size_t)(e[p] | (e[p + 1] << 8));
      p += 2;
      CHECK(l == strlen(want[i]) && memcmp(e + p, want[i], l) == 0);
      p += l;
    }
    uint32_t refs = (uint32_t)e[p] | ((uint32_t)e[p + 1] << 8);
    CHECK_EQ_INT(refs, 4);
    CHECK_EQ_INT(n, p + 4 + refs * 6);
  }
  ldmux_terminal_free(t);
}

static void test_an_oversized_uri_is_dropped_and_its_flag_with_it(void) {
  LdmuxTerminal *t = ldmux_terminal_new(40, 4);
  size_t n = LDMUX_URI_MAX + 10;
  char *seq = (char *)malloc(n + 64);
  size_t p = 0;
  p += (size_t)sprintf(seq, "\x1b]8;;http://x/");
  memset(seq + p, 'q', n);
  p += n;
  p += (size_t)sprintf(seq + p, "\x07" "L\x1b]8;;\x07");
  ldmux_terminal_write(t, (const uint8_t *)seq, p);
  free(seq);
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  CHECK(fr_n(&f, 0, 0) == 1 && fr_text(&f, 0, 0)[0] == 'L');
  /* a flag must always have a URI behind it, so an unservable link is no link */
  CHECK(!(fr_flags(&f, 0, 0) & LDMUX_FLAG_HYPERLINK));
  size_t sn = 0;
  CHECK(ext_section(&f, LDMUX_EXT_HYPERLINKS, &sn) == NULL);
  ldmux_terminal_free(t);
}

static void test_a_link_on_every_cell_stays_bounded(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  /* a distinct URI on every cell: far past the table limits */
  for (int i = 0; i < 80 * 24; i++) {
    char b[96];
    int k = sprintf(b, "\x1b]8;;http://h/%d\x07x\x1b]8;;\x07", i);
    ldmux_terminal_write(t, (const uint8_t *)b, (size_t)k);
  }
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  static Fr f;
  CHECK(parse_frame(&f, buf, len));
  size_t n = 0;
  const uint8_t *e = ext_section(&f, LDMUX_EXT_HYPERLINKS, &n);
  CHECK(e != NULL);
  if (e) CHECK((e[0] | (e[1] << 8)) <= LDMUX_URI_DISTINCT_MAX);
  CHECK(f.ext_len <= LDMUX_URI_TABLE_MAX + 80 * 24 * 6 + 64);
  /* every flagged cell has a ref (a flag with no URI is the thing to never emit) */
  int flagged = 0;
  for (int r = 0; r < f.rows; r++)
    for (int c = 0; c < f.cols; c++) flagged += (fr_flags(&f, c, r) & LDMUX_FLAG_HYPERLINK) ? 1 : 0;
  if (e) {
    size_t p = 2;
    for (int i = 0; i < (e[0] | (e[1] << 8)); i++) p += 2 + (size_t)(e[p] | (e[p + 1] << 8));
    CHECK_EQ_INT((uint32_t)e[p] | ((uint32_t)e[p + 1] << 8), flagged);
  }
  ldmux_terminal_free(t);
}

static void test_selection_rows_above_255_use_the_high_bytes(void) {
  LdmuxTerminal *t = ldmux_terminal_new(10, 300);
  ldmux_terminal_set_geometry(t, 10, 10, 20, 0, 6000);
  for (int i = 0; i < 300; i++) feed(t, "row\r\n");
  ldmux_terminal_selection_set(t, 1, 270, 4, 290);
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[33], 1);
  CHECK_EQ_INT(buf[36] | (buf[46] << 8), 270);
  CHECK_EQ_INT(buf[39] | (buf[47] << 8), 290);
  ldmux_terminal_free(t);
}

static void test_the_grid_is_clamped_not_trusted(void) {
  LdmuxTerminal *t = ldmux_terminal_new(0, 0);
  CHECK(t != NULL); /* 1x1, not a failed or wild allocation */
  ldmux_terminal_resize(t, 65535, 65535); /* saturates at LDMUX_MAX_DIM */
  const uint8_t *buf; size_t len;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(buf[0] | (buf[1] << 8), LDMUX_MAX_DIM);
  ldmux_terminal_free(t);
}

/* A hostile program offers a clipboard write far bigger than anyone would copy. It
 * must be refused (or at least never buffered past the cap), not realloc'd to
 * whatever it asked for. */
static void test_an_oversized_clipboard_write_is_refused(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  size_t groups = (LDMUX_CLIPBOARD_MAX_BYTES / 3) + 5000; /* base64 of ~cap+15000 bytes */
  size_t n = groups * 4 + 16;
  char *seq = (char *)malloc(n);
  size_t p = (size_t)sprintf(seq, "\x1b]52;c;");
  for (size_t i = 0; i < groups; i++) { memcpy(seq + p, "QUJD", 4); p += 4; }
  seq[p++] = '\x07';
  ldmux_terminal_write(t, (const uint8_t *)seq, p);
  free(seq);
  const uint8_t *out = NULL; size_t clen = 0;
  int kind = ldmux_terminal_take_clipboard(t, &out, &clen);
  CHECK(kind != LDMUX_CLIPBOARD_SET || clen <= LDMUX_CLIPBOARD_MAX_BYTES);
  CHECK(kind != LDMUX_CLIPBOARD_SET); /* over the cap: refused whole, never truncated */
  /* and a normal write after it still works */
  feed(t, "\x1b]52;c;aGVsbG8=\x07");
  Clip c = take(t);
  CHECK_EQ_INT(c.kind, LDMUX_CLIPBOARD_SET);
  CHECK_EQ_INT(c.len, 5);
  ldmux_terminal_free(t);
}

int main(void) {
  struct {
    const char *name;
    void (*fn)(void);
  } tests[] = {
      {"write sets the clipboard", test_write_sets_the_clipboard},
      {"ST terminator too", test_st_terminator_too},
      {"empty payload clears", test_empty_payload_clears},
      {"nothing reads as none", test_nothing_reads_as_none},
      {"read consumes", test_read_consumes},
      {"last write wins", test_last_write_wins},
      {"non-BMP survives exactly", test_non_bmp_survives_exactly},
      {"split across writes", test_split_across_writes},
      {"wrapped base64", test_wrapped_base64},
      {"large payload grows", test_large_payload_grows},
      {"unterminated is not delivered", test_unterminated_is_not_delivered},
      {"frame header contract", test_frame_header_contract},
      {"every frame is the full grid", test_every_frame_is_the_full_grid},
      {"a wide glyph does not paint twice", test_a_wide_glyph_does_not_paint_twice},
      {"pi-style cursor: hardware hidden, own cursor inverted",
       test_pi_style_cursor_is_hidden_and_inverted},
      {"alternate screen bit", test_alternate_screen_bit},
      {"cursor advances one column", test_cursor_advances_one_column},
      {"set selects text", test_set_selects_text},
      {"frame marks selected cells", test_frame_marks_selected_cells},
      {"clear removes flag and text", test_clear_removes_the_flag_and_the_text},
      {"selection spans lines", test_selection_spans_lines},
      {"select all covers the screen", test_select_all_covers_the_screen},
      {"set round-trips through bounds", test_set_round_trips_through_bounds},
      {"set orders an inverted range", test_set_orders_an_inverted_range},
      {"set replaces an earlier selection", test_set_replaces_an_earlier_selection},
      {"bounds is zero without a selection", test_bounds_is_zero_without_a_selection},
      {"endpoints land on the right row after scrolling", test_endpoints_land_on_the_right_row_after_scrolling},
    {"the scroll offset is what actually moved", test_the_scroll_offset_is_what_actually_moved},
    {"output does not move a scrolled viewport", test_output_does_not_move_a_scrolled_viewport},
    {"a resize replay replaces the screen and keeps the scrollback",
     test_a_resize_replay_replaces_the_screen_and_keeps_the_scrollback},
      {"a selection follows its text when the viewport moves",
       test_a_selection_follows_its_text_when_the_viewport_moves},
      {"frame carries the endpoints", test_frame_carries_the_endpoints},
      {"clear zeroes the frame bits", test_clear_zeroes_the_frame_bits},
      {"empty screen has no selection", test_empty_screen_has_no_selection},
      {"free with a live selection", test_free_with_a_live_selection},
      {"frame version and extension length are consistent",
       test_frame_version_and_extension_length_are_consistent},
      {"underline style is in the flags, bit 2 still says underlined",
       test_underline_style_is_in_the_flags_and_bit2_still_says_underlined},
      {"underline colour goes in the extension", test_underline_colour_goes_in_the_extension},
      {"wide head and spacer tail are flagged", test_wide_head_and_spacer_tail_are_flagged},
      {"a ZWJ family is carried whole", test_a_zwj_family_is_carried_whole},
      {"a huge cluster is clipped on a code point boundary",
       test_a_huge_cluster_is_clipped_on_a_code_point_boundary},
      {"hyperlinks get a flag and a shared URI", test_hyperlinks_get_a_flag_and_a_shared_uri},
      {"an oversized URI is dropped with its flag", test_an_oversized_uri_is_dropped_and_its_flag_with_it},
      {"a link on every cell stays bounded", test_a_link_on_every_cell_stays_bounded},
      {"selection rows above 255 use the high bytes", test_selection_rows_above_255_use_the_high_bytes},
      {"the grid is clamped, not trusted", test_the_grid_is_clamped_not_trusted},
      {"an oversized clipboard write is refused", test_an_oversized_clipboard_write_is_refused},
  };
  const int n = (int)(sizeof(tests) / sizeof(tests[0]));

  for (int i = 0; i < n; i++) {
    int before = failures;
    tests[i].fn();
    printf("%s %s\n", failures == before ? "ok  " : "FAIL", tests[i].name);
  }
  printf("\n%d checks, %d failures\n", checks, failures);
  return failures == 0 ? 0 : 1;
}
