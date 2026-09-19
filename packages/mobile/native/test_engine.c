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
