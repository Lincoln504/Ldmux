/*
 * ldmux_jni.c — the Android binding over the terminal core.
 *
 * Every function here does three things and nothing else: turn the handle into
 * a pointer, call the core, turn the answer back into a JNI type. No terminal
 * logic lives in this file, so the core is the same code on any platform and
 * this is the only translation unit that mentions JNI.
 *
 * The Kotlin declarations these implement are in
 * `androidMain/.../term/NativeTerminal.kt`; the names must match exactly.
 */
#include <jni.h>
#include <stdint.h>

#include "ldmux_terminal.h"

static LdmuxTerminal *handle_of(jlong handle) {
  return (LdmuxTerminal *)(intptr_t)handle;
}

/*
 * Everything that crosses from Java is untrusted for range. A jint is signed and
 * the core takes unsigned or 16-bit values, so a negative coordinate used to become
 * a huge uint32 and then truncate to some arbitrary u16 column. These clamp to the
 * range the core can represent instead.
 */
static uint32_t clamp_u16(jint v) {
  if (v < 0) return 0;
  if (v > 0xFFFF) return 0xFFFF;
  return (uint32_t)v;
}

static uint32_t clamp_px(jint v) { return v < 0 ? 0u : (uint32_t)v; }

static uint64_t clock_ms(jlong v) { return v < 0 ? 0u : (uint64_t)v; }

/* A grid is at least 1x1 and at most LDMUX_MAX_DIM on a side. The core clamps as
 * well; this keeps the jint -> u16 narrowing from wrapping first. */
static uint16_t clamp_dim(jint v) {
  if (v < 1) return 1;
  if (v > LDMUX_MAX_DIM) return LDMUX_MAX_DIM;
  return (uint16_t)v;
}

/* Bytes copied per JNI call in `write`. The array is never pinned whole: that
 * either copied it entire per call or held the GC off it for the parse. */
#define LDMUX_JNI_CHUNK 16384

JNIEXPORT jlong JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_create(JNIEnv *env, jclass cls,
                                                 jint cols, jint rows) {
  (void)env; (void)cls;
  return (jlong)(intptr_t)ldmux_terminal_new(clamp_dim(cols), clamp_dim(rows));
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_destroy(JNIEnv *env, jclass cls,
                                                  jlong handle) {
  (void)env; (void)cls;
  ldmux_terminal_free(handle_of(handle));
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_write(JNIEnv *env, jclass cls,
                                                jlong handle, jbyteArray data,
                                                jint len) {
  (void)cls;
  LdmuxTerminal *t = handle_of(handle);
  if (!t || !data || len <= 0) return;
  /* `len` is the caller's claim; the array is the truth. Reading past the array's
   * end through a pinned pointer would be an out-of-bounds read of the VM's heap. */
  jsize have = (*env)->GetArrayLength(env, data);
  if (have <= 0) return;
  jsize total = len < have ? len : have;
  jbyte chunk[LDMUX_JNI_CHUNK];
  for (jsize off = 0; off < total;) {
    jsize n = total - off;
    if (n > LDMUX_JNI_CHUNK) n = LDMUX_JNI_CHUNK;
    (*env)->GetByteArrayRegion(env, data, off, n, chunk);
    if ((*env)->ExceptionCheck(env)) return; /* leave it pending for the caller */
    ldmux_terminal_write(t, (const uint8_t *)chunk, (size_t)n);
    off += n;
  }
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_resize(JNIEnv *env, jclass cls,
                                                 jlong handle, jint cols,
                                                 jint rows) {
  (void)env; (void)cls;
  ldmux_terminal_resize(handle_of(handle), clamp_dim(cols), clamp_dim(rows));
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_scroll(JNIEnv *env, jclass cls,
                                                 jlong handle, jint delta) {
  (void)env; (void)cls;
  ldmux_terminal_scroll(handle_of(handle), (int32_t)delta);
}

JNIEXPORT jbyteArray JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_render(JNIEnv *env, jclass cls,
                                                 jlong handle) {
  (void)cls;
  const uint8_t *buf = NULL;
  size_t len = 0;
  if (!ldmux_terminal_render(handle_of(handle), &buf, &len)) return NULL;
  if (len > (size_t)INT32_MAX) return NULL;
  jbyteArray result = (*env)->NewByteArray(env, (jsize)len);
  if (!result) return NULL;
  (*env)->SetByteArrayRegion(env, result, 0, (jsize)len, (const jbyte *)buf);
  return result;
}

/*
 * One call, not two. A `kind` call followed by a `bytes` call would be two reads
 * of a value that is consumed by reading it, which is the shape of bug this
 * project has already paid for once (a `created` slot that was written more than
 * once before anyone read it). So the answer is one array: the kind in byte 0,
 * then the payload. An empty payload is legal and distinct from no answer at all,
 * which is what NULL means.
 */
JNIEXPORT jbyteArray JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_takeClipboard(JNIEnv *env, jclass cls,
                                                        jlong handle) {
  (void)cls;
  const uint8_t *buf = NULL;
  size_t len = 0;
  int kind = ldmux_terminal_take_clipboard(handle_of(handle), &buf, &len);
  if (kind == LDMUX_CLIPBOARD_NONE) return NULL;
  if (len > (size_t)INT32_MAX - 1) len = 0; /* unreachable under the shim's cap */
  jbyte kind_byte = (jbyte)kind;
  jbyteArray result = (*env)->NewByteArray(env, (jsize)(len + 1));
  if (!result) return NULL;
  (*env)->SetByteArrayRegion(env, result, 0, 1, &kind_byte);
  if (len > 0) {
    (*env)->SetByteArrayRegion(env, result, 1, (jsize)len, (const jbyte *)buf);
  }
  return result;
}

/*
 * Selection. The core owns the gesture semantics, so these are thin: they hand a
 * cell and a clock down and answer whether anything changed, which is all the
 * client needs to decide to redraw.
 *
 * The text comes back as bytes rather than as a JNI string for the same reason
 * the clipboard does: a JNI string built from UTF-8 has to be *modified* UTF-8,
 * and anything outside the BMP would come back mangled.
 */
JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_setGeometry(JNIEnv *env, jclass cls,
                                                      jlong handle, jint columns,
                                                      jint cell_w, jint cell_h,
                                                      jint pad_left,
                                                      jint screen_h) {
  (void)env; (void)cls;
  ldmux_terminal_set_geometry(handle_of(handle), clamp_px(columns), clamp_px(cell_w),
                              clamp_px(cell_h), clamp_px(pad_left),
                              clamp_px(screen_h));
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionPress(JNIEnv *env, jclass cls,
                                                         jlong handle, jint col,
                                                         jint row, jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_press(handle_of(handle), clamp_u16(col), clamp_u16(row),
                                     clock_ms(time_ms)) ? JNI_TRUE
                                                        : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionDeepPress(JNIEnv *env, jclass cls,
                                                             jlong handle,
                                                             jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_deep_press(handle_of(handle), clock_ms(time_ms))
             ? JNI_TRUE
             : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionDrag(JNIEnv *env, jclass cls,
                                                        jlong handle, jint col,
                                                        jint row, jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_drag(handle_of(handle), clamp_u16(col), clamp_u16(row),
                                    clock_ms(time_ms)) ? JNI_TRUE
                                                       : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionRelease(JNIEnv *env, jclass cls,
                                                            jlong handle, jint col,
                                                            jint row, jboolean valid,
                                                            jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_release(handle_of(handle), clamp_u16(col), clamp_u16(row),
                                       valid == JNI_TRUE ? 1 : 0, clock_ms(time_ms))
             ? JNI_TRUE
             : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionClear(JNIEnv *env, jclass cls,
                                                          jlong handle) {
  (void)env; (void)cls;
  return ldmux_terminal_select_clear(handle_of(handle)) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionSelectAll(JNIEnv *env, jclass cls,
                                                              jlong handle) {
  (void)env; (void)cls;
  return ldmux_terminal_select_all(handle_of(handle)) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jbyteArray JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionText(JNIEnv *env, jclass cls,
                                                        jlong handle) {
  (void)cls;
  size_t len = 0;
  const uint8_t *buf = ldmux_terminal_selection_text(handle_of(handle), &len);
  if (!buf || len == 0) return NULL;
  jbyteArray result = (*env)->NewByteArray(env, (jsize)len);
  if (!result) return NULL;
  (*env)->SetByteArrayRegion(env, result, 0, (jsize)len, (const jbyte*)buf);
  return result;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionSet(JNIEnv *env, jclass cls,
                                                       jlong handle, jint startCol,
                                                       jint startRow, jint endCol,
                                                       jint endRow) {
  (void)env; (void)cls;
  return ldmux_terminal_selection_set(handle_of(handle), (int)clamp_u16(startCol),
                                      (int)clamp_u16(startRow), (int)clamp_u16(endCol),
                                      (int)clamp_u16(endRow))
             ? JNI_TRUE
             : JNI_FALSE;
}

/*
 * The selection's two ends as {startCol, startRow, endCol, endRow}, or null when
 * there is nothing selected.
 *
 * Four ints in one array rather than four calls: the endpoints are read together by
 * definition — half a selection is not a thing — and a caller that had to make four
 * calls could interleave them with a frame and draw a handle in the wrong place.
 */
JNIEXPORT jintArray JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionBounds(JNIEnv *env, jclass cls,
                                                          jlong handle) {
  (void)cls;
  int sc = 0, sr = 0, ec = 0, er = 0;
  if (!ldmux_terminal_selection_bounds(handle_of(handle), &sc, &sr, &ec, &er))
    return NULL;
  jintArray out = (*env)->NewIntArray(env, 4);
  if (!out) return NULL;
  jint values[4] = {sc, sr, ec, er};
  (*env)->SetIntArrayRegion(env, out, 0, 4, values);
  return out;
}
