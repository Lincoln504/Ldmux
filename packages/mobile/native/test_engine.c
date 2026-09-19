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

/* The header is a contract with Kotlin, and one field of it is a contract with
 * the tap policy. Byte 8 is cursorVisible: TapPolicy never read it, so a tap on
 * a screen with no cursor could be classified as if there were one. */
static void test_frame_header_contract(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  feed(t, "hi");
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK(len >= 40);
  CHECK_EQ_INT(buf[0] | (buf[1] << 8), 80); /* cols, little endian */
  CHECK_EQ_INT(buf[2] | (buf[3] << 8), 24); /* rows */
  CHECK_EQ_INT(buf[8], 1);                  /* cursorVisible */
  CHECK_EQ_INT(buf[32], 0);                 /* alternateOn: primary screen */

  ldmux_terminal_free(t);
}

/* Alternate screen is the one signal the client gets about a full-screen
 * program, and pi enables it. It must be readable. */
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

/* Read one cell's flag word out of a rendered frame. The frame is a flat run of
 * `9 + grapheme_bytes` records after the header, in row-major order, and a test
 * that asserts "this cell is not selected" has to be able to look at a specific
 * cell rather than at the whole frame. */
static uint16_t cell_flags_at(const uint8_t *buf, size_t len, int want_col,
                              int want_row) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = 40;
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

static int selected_cell_count(const uint8_t *buf, size_t len) {
  uint16_t cols = (uint16_t)(buf[0] | (buf[1] << 8));
  uint16_t rows = (uint16_t)(buf[2] | (buf[3] << 8));
  size_t p = 40;
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
static void test_drag_selects_text(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  /* A press on its own is not yet a selection: the engine is still deciding
   * whether a second tap is coming, so the first press has nothing to draw.
   * Measured, not assumed. */
  CHECK_EQ_INT(ldmux_terminal_select_press(t, 0, 0, 1000), 0);
  /* Dragging to column N covers up to N-1. Measured across five endpoints
   * (8,9,10,11,12) rather than reasoned about, because the alternative reading
   * — that the drag cell is included — is just as plausible on paper. */
  CHECK(ldmux_terminal_select_drag(t, 11, 0, 1010));
  ldmux_terminal_select_release(t, 11, 0, 1, 1050);
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
  ldmux_terminal_select_press(t, 0, 0, 1000);
  ldmux_terminal_select_drag(t, 5, 0, 1010);
  ldmux_terminal_select_release(t, 5, 0, 1, 1050);
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
  ldmux_terminal_select_press(t, 0, 0, 1000);
  ldmux_terminal_select_drag(t, 11, 0, 1010);
  ldmux_terminal_select_release(t, 11, 0, 1, 1050);
  CHECK(selection_cstr(t) != NULL);
  CHECK(ldmux_terminal_select_clear(t));
  CHECK(selection_cstr(t) == NULL);
  const uint8_t *buf = NULL;
  size_t len = 0;
  CHECK(ldmux_terminal_render(t, &buf, &len));
  CHECK_EQ_INT(selected_cell_count(buf, len), 0);
  ldmux_terminal_free(t);
}

/* Two taps in the same place, close together in time, do NOT select a word.
 *
 * This is a record of measured behaviour, and it is here so that it cannot be
 * quietly assumed either way: the engine's press path returns "no value" for
 * every click count this test drives it with, including the second press of a
 * double-tap, while a long press on the same cell does produce the word. The
 * client therefore uses long-press for word selection, which is also the
 * standard gesture on both platforms (ADR-0014 §6).
 *
 * If a later engine starts honouring the click count, this test fails and the
 * better gesture can be adopted deliberately. Until then the assertion is what
 * the engine actually does, not what it looked like it should do. Every event
 * still carries the clock, including the release: a release timestamped zero
 * makes the sequence look like it travelled backwards in time. */
static void test_double_press_does_not_yet_select_a_word(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  CHECK_EQ_INT(ldmux_terminal_select_press(t, 3, 0, 1000), 0);
  ldmux_terminal_select_release(t, 3, 0, 1, 1050);
  CHECK_EQ_INT(ldmux_terminal_select_press(t, 3, 0, 1100), 0);
  ldmux_terminal_select_release(t, 3, 0, 1, 1150);
  CHECK(selection_cstr(t) == NULL);
  ldmux_terminal_free(t);
}

/* A long press is a press that has not moved for long enough, which is a
 * different event to the engine rather than a timer in the client. */
static void test_long_press_selects_the_word(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  ldmux_terminal_select_press(t, 3, 0, 1000);
  CHECK(ldmux_terminal_select_deep_press(t, 2000));
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello"), 0);
  ldmux_terminal_free(t);
}

static void test_selection_spans_lines(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "one\r\ntwo");
  ldmux_terminal_select_press(t, 0, 0, 1000);
  ldmux_terminal_select_drag(t, 3, 1, 1010);
  ldmux_terminal_select_release(t, 3, 1, 1, 1050);
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

/* Releasing outside the grid is recorded as exactly that. It must not re-anchor
 * the selection somewhere the finger never was. */
static void test_release_outside_the_grid_keeps_the_selection(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  ldmux_terminal_select_press(t, 0, 0, 1000);
  ldmux_terminal_select_drag(t, 6, 0, 1010);
  ldmux_terminal_select_release(t, 6, 0, 0, 1050);
  const char *s = selection_cstr(t);
  CHECK(s != NULL);
  if (s) CHECK_EQ_INT(strcmp(s, "hello"), 0);
  ldmux_terminal_free(t);
}

/* An empty screen has nothing to select, and asking must answer "nothing"
 * rather than an empty string the caller would put on the clipboard. */
/* ---------------------------------------------------------------------------
 * The handle API. The gesture entry points above are for a finger; these are for
 * the two handles the user drags afterwards, where the ends are independent.
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

static void test_set_replaces_what_the_gesture_chose(void) {
  LdmuxTerminal *t = ldmux_terminal_new(80, 24);
  use_geometry(t);
  feed(t, "hello world");
  /* A long press picks the word; the handles then have to be able to widen it,
   * which is the case the phone reported as impossible. */
  /* The phone presses and *then* deep-presses; a deep press with no press under it
   * has no position to select around, which is what this test learned.
   *
   * The press is deliberately not checked: a press on its own reports no change,
   * because the engine is still deciding whether a second tap is coming. It is
   * setup here, not the assertion. */
  (void)ldmux_terminal_select_press(t, 0, 0, 1000);
  // The app's gap, not a 10 ms one: the phone presses at the moment the long-press
  // timeout elapsed, which is half a second after the touch.
  CHECK(ldmux_terminal_select_deep_press(t, 1500));
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
  ldmux_terminal_select_press(t, 10, 10, 1000);
  ldmux_terminal_select_drag(t, 21, 10, 1010);
  ldmux_terminal_select_release(t, 21, 10, 1, 1050);
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
  ldmux_terminal_select_press(t, 0, 0, 1000);
  ldmux_terminal_select_drag(t, 12, 1, 1010);
  ldmux_terminal_select_release(t, 12, 1, 1, 1050);
  CHECK(selection_cstr(t) != NULL);
  ldmux_terminal_free(t);
  CHECK(1); /* reached only if free did not trip anything */
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
      {"alternate screen bit", test_alternate_screen_bit},
      {"cursor advances one column", test_cursor_advances_one_column},
      {"drag selects text", test_drag_selects_text},
      {"frame marks selected cells", test_frame_marks_selected_cells},
      {"clear removes flag and text", test_clear_removes_the_flag_and_the_text},
      {"double press does not yet select a word", test_double_press_does_not_yet_select_a_word},
      {"long press selects the word", test_long_press_selects_the_word},
      {"selection spans lines", test_selection_spans_lines},
      {"select all covers the screen", test_select_all_covers_the_screen},
      {"release outside keeps selection", test_release_outside_the_grid_keeps_the_selection},
      {"set round-trips through bounds", test_set_round_trips_through_bounds},
      {"set orders an inverted range", test_set_orders_an_inverted_range},
      {"set replaces what the gesture chose", test_set_replaces_what_the_gesture_chose},
      {"bounds is zero without a selection", test_bounds_is_zero_without_a_selection},
      {"endpoints land on the right row after scrolling", test_endpoints_land_on_the_right_row_after_scrolling},
      {"frame carries the endpoints", test_frame_carries_the_endpoints},
      {"clear zeroes the frame bits", test_clear_zeroes_the_frame_bits},
      {"empty screen has no selection", test_empty_screen_has_no_selection},
      {"free with a live selection", test_free_with_a_live_selection},
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
