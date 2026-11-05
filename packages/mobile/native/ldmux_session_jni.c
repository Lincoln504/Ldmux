/*
 * ldmux_session_jni.c — the Android/JVM binding over the L3 session ABI (ldmux_session.h).
 *
 * Same rule as ldmux_jni.c: every function turns a handle into a pointer, calls the core and
 * turns the answer back into a JNI type. No session logic lives here. Handles cross as jlong;
 * every output is written into an array the Kotlin side allocated, so no native pointer is
 * ever interpreted over there. Everything from Java is untrusted for size: a length is clamped
 * to the array, and arrays too large for the fixed buffer are refused with LDMUX_ERR_ARG.
 *
 * The Kotlin declarations these implement are in
 * `androidMain/.../mesh/NativeMeshSession.kt`; the names must match exactly.
 */
#include <jni.h>
#include <stdint.h>
#include <string.h>

#include "ldmux_session.h"

#define JNI(name) Java_io_ldmux_mobile_mesh_NativeMeshSession_##name

/* Largest array accepted from Java in one call. A delivered chunk and a record are both
 * under 172 bytes; send() is the only large one. */
#define JNI_BUF 16384

static LdmuxSession *session_of(jlong h) { return (LdmuxSession *)(intptr_t)h; }

/* Copy exactly `want` bytes out of `arr`, or fail. Fixed-size inputs (keys, messages). */
static int get_exact(JNIEnv *env, jbyteArray arr, uint8_t *dst, jsize want) {
  if (!arr) return 0;
  if ((*env)->GetArrayLength(env, arr) != want) return 0;
  (*env)->GetByteArrayRegion(env, arr, 0, want, (jbyte *)dst);
  return !(*env)->ExceptionCheck(env);
}

static int put_exact(JNIEnv *env, jbyteArray arr, const uint8_t *src, jsize n) {
  if (!arr || (*env)->GetArrayLength(env, arr) < n) return 0;
  (*env)->SetByteArrayRegion(env, arr, 0, n, (const jbyte *)src);
  return !(*env)->ExceptionCheck(env);
}

static int put_long(JNIEnv *env, jlongArray arr, jsize idx, jlong v) {
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return 0;
  (*env)->SetLongArrayRegion(env, arr, idx, 1, &v);
  return !(*env)->ExceptionCheck(env);
}

static int put_int(JNIEnv *env, jintArray arr, jsize idx, jint v) {
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return 0;
  (*env)->SetIntArrayRegion(env, arr, idx, 1, &v);
  return !(*env)->ExceptionCheck(env);
}

JNIEXPORT jint JNICALL JNI(keypair)(JNIEnv *env, jclass cls, jbyteArray priv, jbyteArray pub) {
  (void)cls;
  uint8_t p[32], q[32];
  if (!priv || !pub) return LDMUX_ERR_NULL;
  int rc = ldmux_keypair_generate(p, q);
  if (rc != LDMUX_OK) return rc;
  if (!put_exact(env, priv, p, 32) || !put_exact(env, pub, q, 32)) return LDMUX_ERR_ARG;
  memset(p, 0, sizeof p);
  return LDMUX_OK;
}

JNIEXPORT jint JNICALL JNI(initiatorStart)(JNIEnv *env, jclass cls, jbyteArray priv,
                                           jbyteArray pub, jbyteArray remote, jbyteArray psk,
                                           jlong ts, jlongArray out, jbyteArray msg1) {
  (void)cls;
  uint8_t p[32], q[32], r[32], k[32], m[LDMUX_MSG1_LEN];
  if (!get_exact(env, priv, p, 32) || !get_exact(env, pub, q, 32) ||
      !get_exact(env, remote, r, 32) || !get_exact(env, psk, k, 32))
    return LDMUX_ERR_ARG;
  LdmuxInitiator *hs = NULL;
  int rc = ldmux_initiator_start(p, q, r, k, (uint64_t)ts, &hs, m);
  memset(p, 0, sizeof p);
  memset(k, 0, sizeof k);
  if (rc != LDMUX_OK) return rc;
  if (!put_exact(env, msg1, m, LDMUX_MSG1_LEN) || !put_long(env, out, 0, (jlong)(intptr_t)hs)) {
    ldmux_initiator_free(&hs);
    return LDMUX_ERR_ARG;
  }
  return LDMUX_OK;
}

JNIEXPORT jint JNICALL JNI(initiatorFinish)(JNIEnv *env, jclass cls, jlong hs, jbyteArray msg2,
                                            jint limit, jlong now, jlongArray out) {
  (void)cls;
  LdmuxInitiator *h = (LdmuxInitiator *)(intptr_t)hs;
  uint8_t m[LDMUX_MSG2_LEN];
  LdmuxSession *s = NULL;
  /* The initiator is consumed whatever happens, so a malformed argument still frees it. */
  if (!get_exact(env, msg2, m, LDMUX_MSG2_LEN) || limit < 0) {
    ldmux_initiator_free(&h);
    return LDMUX_ERR_ARG;
  }
  int rc = ldmux_initiator_finish(&h, m, LDMUX_MSG2_LEN, (uint32_t)limit, (uint64_t)now, &s);
  if (rc != LDMUX_OK) return rc;
  if (!put_long(env, out, 0, (jlong)(intptr_t)s)) {
    ldmux_session_free(&s);
    return LDMUX_ERR_ARG;
  }
  return LDMUX_OK;
}

JNIEXPORT void JNICALL JNI(initiatorFree)(JNIEnv *env, jclass cls, jlong hs) {
  (void)env; (void)cls;
  LdmuxInitiator *h = (LdmuxInitiator *)(intptr_t)hs;
  ldmux_initiator_free(&h);
}

/* out[0] = session handle, out[1] = timestamp to remember. */
JNIEXPORT jint JNICALL JNI(responderAccept)(JNIEnv *env, jclass cls, jbyteArray priv,
                                            jbyteArray pub, jbyteArray psk, jbyteArray pinned,
                                            jlong last, jlong now, jbyteArray msg1, jint index,
                                            jint limit, jbyteArray reply, jbyteArray remote,
                                            jlongArray out) {
  (void)cls;
  uint8_t p[32], q[32], k[32], pin[32], m[LDMUX_MSG1_LEN];
  uint8_t rep[LDMUX_MSG2_LEN], rem[32];
  uint64_t ts = 0;
  if (!get_exact(env, priv, p, 32) || !get_exact(env, pub, q, 32) ||
      !get_exact(env, psk, k, 32) || !get_exact(env, msg1, m, LDMUX_MSG1_LEN))
    return LDMUX_ERR_ARG;
  if (index < 0 || index > 255 || limit < 0) return LDMUX_ERR_ARG;
  if (pinned && !get_exact(env, pinned, pin, 32)) return LDMUX_ERR_ARG;
  LdmuxSession *s = NULL;
  int rc = ldmux_responder_accept(p, q, k, pinned ? pin : NULL, (uint64_t)last, (uint64_t)now, m,
                                  LDMUX_MSG1_LEN, (uint8_t)index, (uint32_t)limit, rep, rem, &ts,
                                  &s);
  memset(p, 0, sizeof p);
  memset(k, 0, sizeof k);
  if (rc != LDMUX_OK) return rc;
  if (!put_exact(env, reply, rep, LDMUX_MSG2_LEN) || !put_exact(env, remote, rem, 32) ||
      !put_long(env, out, 0, (jlong)(intptr_t)s) || !put_long(env, out, 1, (jlong)ts)) {
    ldmux_session_free(&s);
    return LDMUX_ERR_ARG;
  }
  return LDMUX_OK;
}

JNIEXPORT jint JNICALL JNI(send)(JNIEnv *env, jclass cls, jlong h, jint shell, jbyteArray data) {
  (void)cls;
  if (shell < 0 || shell > 255 || !data) return LDMUX_ERR_ARG;
  jsize n = (*env)->GetArrayLength(env, data);
  if (n > JNI_BUF) return LDMUX_ERR_ARG;
  jbyte buf[JNI_BUF];
  if (n > 0) {
    (*env)->GetByteArrayRegion(env, data, 0, n, buf);
    if ((*env)->ExceptionCheck(env)) return LDMUX_ERR_ARG;
  }
  return ldmux_session_send(session_of(h), (uint8_t)shell, (const uint8_t *)buf, (size_t)n);
}

JNIEXPORT jint JNICALL JNI(receive)(JNIEnv *env, jclass cls, jlong h, jlong now, jbyteArray rec) {
  (void)cls;
  if (!rec) return LDMUX_ERR_NULL;
  jsize n = (*env)->GetArrayLength(env, rec);
  if (n > LDMUX_RECORD_MAX) return LDMUX_ERR_TOO_LONG;
  jbyte buf[LDMUX_RECORD_MAX];
  if (n > 0) {
    (*env)->GetByteArrayRegion(env, rec, 0, n, buf);
    if ((*env)->ExceptionCheck(env)) return LDMUX_ERR_ARG;
  }
  return ldmux_session_receive(session_of(h), (uint64_t)now, (const uint8_t *)buf, (size_t)n);
}

/* meta[0] = shell, meta[1] = length. Returns 1, 0 or an error. */
JNIEXPORT jint JNICALL JNI(takeDelivered)(JNIEnv *env, jclass cls, jlong h, jbyteArray out,
                                          jintArray meta) {
  (void)cls;
  if (!out) return LDMUX_ERR_NULL;
  uint8_t buf[LDMUX_RECORD_MAX];
  uint8_t shell = 0;
  size_t len = 0;
  jsize cap = (*env)->GetArrayLength(env, out);
  size_t use = (size_t)cap < sizeof buf ? (size_t)cap : sizeof buf;
  int rc = ldmux_session_take_delivered(session_of(h), &shell, buf, use, &len);
  if (rc == LDMUX_ERR_BUFFER && !put_int(env, meta, 1, (jint)len)) return LDMUX_ERR_ARG;
  if (rc != 1) return rc;
  if (!put_exact(env, out, buf, (jsize)len) || !put_int(env, meta, 0, shell) ||
      !put_int(env, meta, 1, (jint)len))
    return LDMUX_ERR_ARG;
  return 1;
}

JNIEXPORT jint JNICALL JNI(poll)(JNIEnv *env, jclass cls, jlong h, jlong now, jint left,
                                 jint slots) {
  (void)env; (void)cls;
  if (left < 0) left = 0;
  if (left > 255) left = 255;
  if (slots < 0) slots = 0;
  if (slots > 255) slots = 255;
  return ldmux_session_poll(session_of(h), (uint64_t)now, (uint8_t)left, (uint8_t)slots);
}

/* meta[0] = length. Returns 1, 0 or an error. */
JNIEXPORT jint JNICALL JNI(takeRecord)(JNIEnv *env, jclass cls, jlong h, jbyteArray out,
                                       jintArray meta) {
  (void)cls;
  if (!out) return LDMUX_ERR_NULL;
  uint8_t buf[LDMUX_RECORD_MAX];
  size_t len = 0;
  jsize cap = (*env)->GetArrayLength(env, out);
  size_t use = (size_t)cap < sizeof buf ? (size_t)cap : sizeof buf;
  int rc = ldmux_session_take_record(session_of(h), buf, use, &len);
  if (rc == LDMUX_ERR_BUFFER && !put_int(env, meta, 0, (jint)len)) return LDMUX_ERR_ARG;
  if (rc != 1) return rc;
  if (!put_exact(env, out, buf, (jsize)len) || !put_int(env, meta, 0, (jint)len))
    return LDMUX_ERR_ARG;
  return 1;
}

/* meta[0] = length, meta[1] = the ARQ sequence number that names the record's slot.
 * Returns 1, 0 or an error. The sequence number is written only on success. */
JNIEXPORT jint JNICALL JNI(takeRecordSeq)(JNIEnv *env, jclass cls, jlong h, jbyteArray out,
                                          jintArray meta) {
  (void)cls;
  if (!out) return LDMUX_ERR_NULL;
  uint8_t buf[LDMUX_RECORD_MAX];
  size_t len = 0;
  uint16_t seq = 0;
  jsize cap = (*env)->GetArrayLength(env, out);
  size_t use = (size_t)cap < sizeof buf ? (size_t)cap : sizeof buf;
  int rc = ldmux_session_take_record_seq(session_of(h), buf, use, &len, &seq);
  if (rc == LDMUX_ERR_BUFFER && !put_int(env, meta, 0, (jint)len)) return LDMUX_ERR_ARG;
  if (rc != 1) return rc;
  if (!put_exact(env, out, buf, (jsize)len) || !put_int(env, meta, 0, (jint)len) ||
      !put_int(env, meta, 1, (jint)seq))
    return LDMUX_ERR_ARG;
  return 1;
}

/* The radio refused that record without transmitting it; give the attempt back. Returns 1 when a
 * slot with that number was in flight, 0 when it was already settled. */
JNIEXPORT jint JNICALL JNI(refuseSeq)(JNIEnv *env, jclass cls, jlong h, jlong now, jint seq) {
  (void)env; (void)cls;
  if (seq < 0) return LDMUX_ERR_ARG;
  if (seq > 65535) return LDMUX_ERR_ARG;
  return ldmux_session_refuse_seq(session_of(h), (uint64_t)now, (uint16_t)seq);
}

JNIEXPORT jint JNICALL JNI(close)(JNIEnv *env, jclass cls, jlong h) {
  (void)env; (void)cls;
  return ldmux_session_close(session_of(h));
}

JNIEXPORT jint JNICALL JNI(status)(JNIEnv *env, jclass cls, jlong h, jlong now) {
  (void)env; (void)cls;
  return ldmux_session_status(session_of(h), (uint64_t)now);
}

JNIEXPORT jint JNICALL JNI(pending)(JNIEnv *env, jclass cls, jlong h) {
  (void)env; (void)cls;
  return ldmux_session_pending(session_of(h));
}

/* out[0] = deadline. Returns 1, 0 or an error. */
JNIEXPORT jint JNICALL JNI(nextDeadline)(JNIEnv *env, jclass cls, jlong h, jlongArray out) {
  (void)cls;
  uint64_t d = 0;
  int rc = ldmux_session_next_deadline(session_of(h), &d);
  if (rc == 1 && !put_long(env, out, 0, (jlong)d)) return LDMUX_ERR_ARG;
  return rc;
}

JNIEXPORT jint JNICALL JNI(peerKey)(JNIEnv *env, jclass cls, jlong h, jbyteArray out) {
  (void)cls;
  uint8_t k[32];
  int rc = ldmux_session_peer_key(session_of(h), k);
  if (rc != LDMUX_OK) return rc;
  return put_exact(env, out, k, 32) ? LDMUX_OK : LDMUX_ERR_ARG;
}

JNIEXPORT jint JNICALL JNI(dropped)(JNIEnv *env, jclass cls, jlong h) {
  (void)env; (void)cls;
  return ldmux_session_dropped(session_of(h));
}

JNIEXPORT void JNICALL JNI(free)(JNIEnv *env, jclass cls, jlong h) {
  (void)env; (void)cls;
  LdmuxSession *s = session_of(h);
  ldmux_session_free(&s);
}
