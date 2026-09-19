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
