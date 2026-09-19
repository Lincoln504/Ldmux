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

JNIEXPORT jlong JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_create(JNIEnv *env, jclass cls,
                                                 jint cols, jint rows) {
  (void)env; (void)cls;
  return (jlong)(intptr_t)ldmux_terminal_new((uint16_t)cols, (uint16_t)rows);
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
  jbyte *buf = (*env)->GetByteArrayElements(env, data, NULL);
  if (!buf) return;
  ldmux_terminal_write(t, (const uint8_t *)buf, (size_t)len);
  (*env)->ReleaseByteArrayElements(env, data, buf, JNI_ABORT);
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_resize(JNIEnv *env, jclass cls,
                                                 jlong handle, jint cols,
                                                 jint rows) {
  (void)env; (void)cls;
  ldmux_terminal_resize(handle_of(handle), (uint16_t)cols, (uint16_t)rows);
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
  jbyteArray result = (*env)->NewByteArray(env, (jsize)len);
  if (!result) return NULL;
  (*env)->SetByteArrayRegion(env, result, 0, (jsize)len, (const jbyte *)buf);
  return result;
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_clean(JNIEnv *env, jclass cls,
                                                jlong handle) {
  (void)env; (void)cls;
  ldmux_terminal_clean(handle_of(handle));
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
  ldmux_terminal_set_geometry(handle_of(handle), (uint32_t)columns, (uint32_t)cell_w,
                              (uint32_t)cell_h, (uint32_t)pad_left,
                              (uint32_t)screen_h);
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionPress(JNIEnv *env, jclass cls,
                                                         jlong handle, jint col,
                                                         jint row, jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_press(handle_of(handle), (uint32_t)col, (uint32_t)row,
                                     (uint64_t)time_ms) ? JNI_TRUE
                                                        : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionDeepPress(JNIEnv *env, jclass cls,
                                                             jlong handle,
                                                             jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_deep_press(handle_of(handle), (uint64_t)time_ms)
             ? JNI_TRUE
             : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionDrag(JNIEnv *env, jclass cls,
                                                        jlong handle, jint col,
                                                        jint row, jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_drag(handle_of(handle), (uint32_t)col, (uint32_t)row,
                                    (uint64_t)time_ms) ? JNI_TRUE
                                                       : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_selectionRelease(JNIEnv *env, jclass cls,
                                                            jlong handle, jint col,
                                                            jint row, jboolean valid,
                                                            jlong time_ms) {
  (void)env; (void)cls;
  return ldmux_terminal_select_release(handle_of(handle), (uint32_t)col, (uint32_t)row,
                                       valid == JNI_TRUE ? 1 : 0, (uint64_t)time_ms)
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
