/*
 * Host tests for the JNI binding's argument handling.
 *
 * `ldmux_jni.c` is the only file that trusts values from Java: a length that may
 * exceed the array it describes, and signed coordinates that the core takes as
 * unsigned. There is no JVM here, so this supplies a fake JNIEnv with exactly the
 * functions the binding calls. The fake array BOUNDS-CHECKS the way a real VM does
 * (a region outside the array is an ArrayIndexOutOfBoundsException, recorded here as
 * `oob`), and its storage is an exact-size heap block so AddressSanitizer catches
 * any read that gets past it.
 *
 * Build: see scripts/test-native.sh (needs a jni.h: the JDK's or the NDK's).
 */
#include <jni.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ldmux_terminal.h"

#define JNI(name) Java_dev_ldmux_mobile_term_NativeTerminal_##name

JNIEXPORT jlong JNICALL JNI(create)(JNIEnv *, jclass, jint, jint);
JNIEXPORT void JNICALL JNI(destroy)(JNIEnv *, jclass, jlong);
JNIEXPORT void JNICALL JNI(write)(JNIEnv *, jclass, jlong, jbyteArray, jint);
JNIEXPORT void JNICALL JNI(resize)(JNIEnv *, jclass, jlong, jint, jint);
JNIEXPORT jboolean JNICALL JNI(selectionPress)(JNIEnv *, jclass, jlong, jint, jint, jlong);
JNIEXPORT jboolean JNICALL JNI(selectionDrag)(JNIEnv *, jclass, jlong, jint, jint, jlong);
JNIEXPORT jboolean JNICALL JNI(selectionRelease)(JNIEnv *, jclass, jlong, jint, jint,
                                                 jboolean, jlong);
JNIEXPORT jbyteArray JNICALL JNI(selectionText)(JNIEnv *, jclass, jlong);
JNIEXPORT jboolean JNICALL JNI(selectionSet)(JNIEnv *, jclass, jlong, jint, jint, jint, jint);
JNIEXPORT void JNICALL JNI(setGeometry)(JNIEnv *, jclass, jlong, jint, jint, jint, jint, jint);
JNIEXPORT jbyteArray JNICALL JNI(render)(JNIEnv *, jclass, jlong);

static int checks = 0, failures = 0;
#define CHECK(c)                                                              \
  do {                                                                        \
    checks++;                                                                 \
    if (!(c)) {                                                               \
      failures++;                                                             \
      fprintf(stderr, "  FAIL %s:%d  %s\n", __FILE__, __LINE__, #c);          \
    }                                                                         \
  } while (0)

typedef struct {
  jsize len;
  jbyte *data;
} FakeBytes;

static int oob = 0;          /* regions requested outside an array */
static int region_calls = 0; /* GetByteArrayRegion calls */
static size_t largest_region = 0;

static jsize JNICALL fake_len(JNIEnv *env, jarray a) {
  (void)env;
  return ((FakeBytes *)a)->len;
}
static void JNICALL fake_get_region(JNIEnv *env, jbyteArray a, jsize start, jsize n,
                                    jbyte *buf) {
  (void)env;
  FakeBytes *f = (FakeBytes *)a;
  region_calls++;
  if (start < 0 || n < 0 || (int64_t)start + n > f->len) {
    oob++;
    return;
  }
  if ((size_t)n > largest_region) largest_region = (size_t)n;
  memcpy(buf, f->data + start, (size_t)n);
}
static jbyteArray JNICALL fake_new_bytes(JNIEnv *env, jsize n) {
  (void)env;
  FakeBytes *f = (FakeBytes *)calloc(1, sizeof *f);
  f->len = n;
  f->data = (jbyte *)calloc((size_t)(n ? n : 1), 1);
  return (jbyteArray)f;
}
static void JNICALL fake_set_region(JNIEnv *env, jbyteArray a, jsize start, jsize n,
                                    const jbyte *buf) {
  (void)env;
  FakeBytes *f = (FakeBytes *)a;
  if (start < 0 || n < 0 || (int64_t)start + n > f->len) {
    oob++;
    return;
  }
  memcpy(f->data + start, buf, (size_t)n);
}
static jboolean JNICALL fake_exc(JNIEnv *env) {
  (void)env;
  return JNI_FALSE;
}

static FakeBytes *mk(const void *src, size_t n) {
  FakeBytes *f = (FakeBytes *)calloc(1, sizeof *f);
  f->len = (jsize)n;
  f->data = (jbyte *)malloc(n ? n : 1); /* exact size: ASan sees any overrun */
  if (n) memcpy(f->data, src, n);
  return f;
}

static struct JNINativeInterface_ table;
static JNIEnv env_storage = &table;
#define ENV (&env_storage)

static void drop(FakeBytes *f) {
  free(f->data);
  free(f);
}

static bool cell0_is(const uint8_t *frame, size_t len, int col, int row, char want) {
  uint16_t cols = (uint16_t)(frame[0] | (frame[1] << 8));
  uint16_t rows = (uint16_t)(frame[2] | (frame[3] << 8));
  size_t p = LDMUX_FRAME_HEADER;
  for (int r = 0; r < rows; r++)
    for (int c = 0; c < cols; c++) {
      if (p + LDMUX_CELL_FIXED > len) return false;
      uint8_t n = frame[p];
      if (r == row && c == col) return n == 1 && frame[p + LDMUX_CELL_FIXED] == (uint8_t)want;
      p += LDMUX_CELL_FIXED + n;
    }
  return false;
}

static void test_length_beyond_the_array_is_clamped(void) {
  jlong h = JNI(create)(ENV, NULL, 80, 24);
  CHECK(h != 0);
  FakeBytes *a = mk("hi", 2);
  oob = 0;
  JNI(write)(ENV, NULL, h, (jbyteArray)a, 1000000); /* the caller lies */
  CHECK(oob == 0);
  const uint8_t *frame; size_t len;
  CHECK(ldmux_terminal_render((LdmuxTerminal *)(intptr_t)h, &frame, &len));
  CHECK(cell0_is(frame, len, 0, 0, 'h') && cell0_is(frame, len, 1, 0, 'i'));
  /* negative and zero lengths and a NULL array do nothing and do not crash */
  JNI(write)(ENV, NULL, h, (jbyteArray)a, -5);
  JNI(write)(ENV, NULL, h, (jbyteArray)a, 0);
  JNI(write)(ENV, NULL, h, NULL, 10);
  CHECK(oob == 0);
  drop(a);
  JNI(destroy)(ENV, NULL, h);
}

static void test_a_big_write_is_chunked_not_pinned_whole(void) {
  jlong h = JNI(create)(ENV, NULL, 80, 24);
  size_t n = 100000;
  uint8_t *big = (uint8_t *)malloc(n);
  memset(big, '\n', n);
  /* An escape sequence that straddles the 16 KiB chunk boundary: the parser's
   * state must carry across chunks. */
  const char *tail = "\x1b[1;1HQ";
  size_t at = 16384 - 1;
  memcpy(big + at, tail, strlen(tail));
  /* ...and keep the rest as newlines after it, except that would scroll Q away, so
   * stop the payload right after the sequence. */
  FakeBytes *a = mk(big, at + strlen(tail));
  free(big);
  region_calls = 0; largest_region = 0; oob = 0;
  JNI(write)(ENV, NULL, h, (jbyteArray)a, a->len);
  CHECK(oob == 0);
  CHECK(region_calls >= 2);
  CHECK(largest_region <= 16384);
  const uint8_t *frame; size_t len;
  CHECK(ldmux_terminal_render((LdmuxTerminal *)(intptr_t)h, &frame, &len));
  CHECK(cell0_is(frame, len, 0, 0, 'Q'));
  drop(a);
  JNI(destroy)(ENV, NULL, h);
}

static void test_negative_coordinates_clamp_to_the_first_cell(void) {
  jlong h = JNI(create)(ENV, NULL, 80, 24);
  FakeBytes *a = mk("hello world", 11);
  JNI(write)(ENV, NULL, h, (jbyteArray)a, a->len);
  drop(a);
  JNI(setGeometry)(ENV, NULL, h, 80, 10, 20, 0, 480);
  /* A finger dragged off the top-left: negative cell coordinates. They used to
   * become huge uint32 values and truncate to an arbitrary u16 column. */
  JNI(selectionPress)(ENV, NULL, h, -5, -3, 1000);
  JNI(selectionDrag)(ENV, NULL, h, 3, 0, 1010); /* a drag to column N covers up to N-1 */
  JNI(selectionRelease)(ENV, NULL, h, 3, 0, JNI_TRUE, 1020);
  FakeBytes *t = (FakeBytes *)JNI(selectionText)(ENV, NULL, h);
  CHECK(t != NULL);
  if (t) {
    CHECK(t->len == 3 && memcmp(t->data, "hel", 3) == 0);
    drop(t);
  }
  /* selectionSet clamps the same way and does not wrap a huge value to column 0. */
  CHECK(JNI(selectionSet)(ENV, NULL, h, -9, -9, 4, 0) == JNI_TRUE);
  t = (FakeBytes *)JNI(selectionText)(ENV, NULL, h);
  CHECK(t != NULL && t->len == 5 && memcmp(t->data, "hello", 5) == 0);
  if (t) drop(t);
  JNI(selectionSet)(ENV, NULL, h, 70000, 0, 70001, 0); /* off the grid: refused, no crash */
  JNI(selectionPress)(ENV, NULL, h, 2147483647, 2147483647, -1);
  JNI(destroy)(ENV, NULL, h);
}

static void test_degenerate_sizes_are_clamped(void) {
  jlong h = JNI(create)(ENV, NULL, 0, -4);
  CHECK(h != 0); /* clamped to 1x1, not a failed or wrapped allocation */
  JNI(resize)(ENV, NULL, h, -1, 0);
  JNI(resize)(ENV, NULL, h, 100000, 100000); /* saturates at LDMUX_MAX_DIM, not 65535 */
  JNI(destroy)(ENV, NULL, h);
  CHECK(1);
}

int main(void) {
  setvbuf(stdout, NULL, _IONBF, 0);
  table.GetArrayLength = fake_len;
  table.GetByteArrayRegion = fake_get_region;
  table.NewByteArray = fake_new_bytes;
  table.SetByteArrayRegion = fake_set_region;
  table.ExceptionCheck = fake_exc;
  struct {
    const char *name;
    void (*fn)(void);
  } tests[] = {
      {"a length beyond the array is clamped", test_length_beyond_the_array_is_clamped},
      {"a big write is chunked, not pinned whole", test_a_big_write_is_chunked_not_pinned_whole},
      {"negative coordinates clamp to the first cell", test_negative_coordinates_clamp_to_the_first_cell},
      {"degenerate sizes are clamped", test_degenerate_sizes_are_clamped},
  };
  for (size_t i = 0; i < sizeof tests / sizeof tests[0]; i++) {
    int before = failures;
    tests[i].fn();
    printf("%s %s\n", failures == before ? "ok  " : "FAIL", tests[i].name);
  }
  printf("\n%d checks, %d failures\n", checks, failures);
  return failures == 0 ? 0 : 1;
}
