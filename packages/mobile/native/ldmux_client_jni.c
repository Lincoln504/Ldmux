/*
 * ldmux_client_jni.c — the Android/JVM binding over the phone's whole radio route
 * (ldmux_client.h, docs/RADIO-PLAN.md §8.1).
 *
 * Same rule as ldmux_session_jni.c and ldmux_jni.c: every function turns a Kotlin value into a C
 * one, calls the core, and turns the answer back. **No protocol lives here.** If you find yourself
 * writing a byte layout in this file, it belongs in Rust.
 *
 * Two conventions that are not cosmetic:
 *
 *   - **A handle crosses as an element of a LongArray, not as a jlong.** The C contract is
 *     `ldmux_client_free(LdmuxClient **)`: it nulls the caller's variable so a second free is a
 *     no-op and a use after free is a null, not a dereference of freed memory. A `jlong` is a copy,
 *     so a shim that took one could not honour that, and Kotlin would have to invent its own
 *     liveness rule for a pointer it cannot see. With a one-element array the variable the C side
 *     nulls is the same variable Kotlin reads, so there is exactly one liveness rule and it is the
 *     library's.
 *   - **A struct comes back as an int/long array, field by field.** Kotlin never sees a C layout, so
 *     a field cannot be read at the wrong offset and a struct cannot grow without a Kotlin change
 *     being a compile error rather than a silent misread.
 *
 * The Kotlin declarations these implement are in
 * `androidMain/.../mesh/NativeRadioClient.kt`; the names must match exactly.
 */
#include <jni.h>
#include <stdint.h>
#include <string.h>

#include "ldmux_client.h"

#define JNI(name) Java_io_ldmux_mobile_mesh_NativeRadioClient_##name

/* The largest array accepted from Java in one call. A BLE notification is one companion frame
 * (at most 179 bytes) and a command is at most 16 KiB, so this is a ceiling on a lie rather than a
 * working size. */
#define JNI_BUF 65536

/* Field counts of the two packed structs. A Kotlin array shorter than this is refused rather than
 * partly written. */
#define MIRROR_FIELDS 12
#define EVENT_FIELDS 8
#define STATS_FIELDS 42
#define RADIO_FIELDS 23
/* name at [0, name_len), firmware at [NAME_MAX, NAME_MAX + firmware_len). */
#define NAME_MAX 32
#define FIRMWARE_MAX 24
#define RADIO_TEXT (NAME_MAX + FIRMWARE_MAX)
#define ROUTE_FIELDS 8
#define PATH_MAX 64

static LdmuxClient *client_of(jlong h) { return (LdmuxClient *)(intptr_t)h; }

static int put_i32(JNIEnv *env, jintArray arr, jsize idx, jint v) {
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return 0;
  (*env)->SetIntArrayRegion(env, arr, idx, 1, &v);
  return !(*env)->ExceptionCheck(env);
}

static int put_i64(JNIEnv *env, jlongArray arr, jsize idx, jlong v) {
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return 0;
  (*env)->SetLongArrayRegion(env, arr, idx, 1, &v);
  return !(*env)->ExceptionCheck(env);
}

static jlong get_i64(JNIEnv *env, jlongArray arr, jsize idx) {
  jlong v = 0;
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return 0;
  (*env)->GetLongArrayRegion(env, arr, idx, 1, &v);
  if ((*env)->ExceptionCheck(env)) return 0;
  return v;
}

static void set_i64(JNIEnv *env, jlongArray arr, jsize idx, jlong v) {
  if (!arr || (*env)->GetArrayLength(env, arr) <= idx) return;
  (*env)->SetLongArrayRegion(env, arr, idx, 1, &v);
}

/* Copy a Java array into `dst` (capacity `cap`) and report its length in `*n`. A null array is
 * length zero, which the library reads as "no bytes" rather than an error. */
static int get_bytes(JNIEnv *env, jbyteArray arr, uint8_t *dst, size_t cap, size_t *n) {
  *n = 0;
  if (!arr) return 1;
  jsize len = (*env)->GetArrayLength(env, arr);
  if (len < 0 || (size_t)len > cap) return 0;
  if (len > 0) (*env)->GetByteArrayRegion(env, arr, 0, len, (jbyte *)dst);
  if ((*env)->ExceptionCheck(env)) return 0;
  *n = (size_t)len;
  return 1;
}

/* Copy `n` bytes into a Java array. A short array is an argument error, never a truncation. */
static int put_bytes(JNIEnv *env, jbyteArray arr, const uint8_t *src, size_t n) {
  if (!arr || (size_t)(*env)->GetArrayLength(env, arr) < n) return 0;
  if (n > 0) (*env)->SetByteArrayRegion(env, arr, 0, (jsize)n, (const jbyte *)src);
  return !(*env)->ExceptionCheck(env);
}

/* ---------------------------------------------------------------------------------------------
 * Life cycle
 * ------------------------------------------------------------------------------------------- */

/* out[0] = the handle, 0 when it failed. */
JNIEXPORT jint JNICALL JNI(create)(JNIEnv *env, jclass cls, jbyteArray peer_static, jbyteArray psk,
                                   jbyteArray local_private, jbyteArray local_public, jint path_width,
                                   jint sf, jint flood_reach_hops, jint max_open_pages, jfloat airtime_ms,
                                   jint scope, jlongArray out) {
  (void)cls;
  if (!out || (*env)->GetArrayLength(env, out) < 1) return LDMUX_ERR_ARG;
  LdmuxClientConfig cfg;
  size_t n = 0;
  memset(&cfg, 0, sizeof cfg);
  if (!get_bytes(env, peer_static, cfg.peer_static, sizeof cfg.peer_static, &n) || n != 32) return LDMUX_ERR_ARG;
  if (!get_bytes(env, psk, cfg.psk, sizeof cfg.psk, &n) || n != 32) return LDMUX_ERR_ARG;
  if (!get_bytes(env, local_private, cfg.local_private, sizeof cfg.local_private, &n) || n != 32)
    return LDMUX_ERR_ARG;
  if (!get_bytes(env, local_public, cfg.local_public, sizeof cfg.local_public, &n) || n != 32)
    return LDMUX_ERR_ARG;
  if (path_width < 0 || path_width > 255 || sf < 0 || sf > 255 || flood_reach_hops < 0 ||
      flood_reach_hops > 255 || max_open_pages < 0 || max_open_pages > 255 || scope < 0 ||
      scope > 65535)
    return LDMUX_ERR_ARG;
  cfg.path_width = (uint8_t)path_width;
  cfg.sf = (uint8_t)sf;
  cfg.flood_reach_hops = (uint8_t)flood_reach_hops;
  cfg.max_open_pages = (uint8_t)max_open_pages;
  cfg.airtime_ms = airtime_ms;
  cfg.scope = (uint16_t)scope;

  LdmuxClient *c = NULL;
  int rc = ldmux_client_new(&cfg, &c);
  /* The keys are the caller's; this copy is ours and it goes with the frame. */
  memset(&cfg, 0, sizeof cfg);
  if (rc != LDMUX_OK) {
    set_i64(env, out, 0, 0);
    return rc;
  }
  set_i64(env, out, 0, (jlong)(intptr_t)c);
  return LDMUX_OK;
}

/* Free the client and null out[0]. A second call is a no-op, because the library nulls the slot. */
JNIEXPORT void JNICALL JNI(free)(JNIEnv *env, jclass cls, jlongArray slot) {
  (void)cls;
  if (!slot || (*env)->GetArrayLength(env, slot) < 1) return;
  jlong v = get_i64(env, slot, 0);
  LdmuxClient *c = client_of(v);
  ldmux_client_free(&c);
  set_i64(env, slot, 0, (jlong)(intptr_t)c);
}

/*
 * A fresh device keypair, for a pairing that happens before there is a handle. Two 32-byte arrays
 * filled field by field, the same way every other out-struct crosses: Kotlin never sees a C layout.
 */
JNIEXPORT jint JNICALL JNI(keypair)(JNIEnv *env, jclass cls, jbyteArray priv, jbyteArray pub) {
  (void)cls;
  if (!priv || !pub) return LDMUX_ERR_NULL;
  if ((*env)->GetArrayLength(env, priv) < 32 || (*env)->GetArrayLength(env, pub) < 32) {
    return LDMUX_ERR_BUFFER;
  }
  uint8_t k_priv[32], k_pub[32];
  int32_t rc = ldmux_client_keypair(k_priv, k_pub);
  if (rc == LDMUX_OK) {
    (*env)->SetByteArrayRegion(env, priv, 0, 32, (const jbyte *)k_priv);
    (*env)->SetByteArrayRegion(env, pub, 0, 32, (const jbyte *)k_pub);
  }
  return rc;
}

JNIEXPORT jint JNICALL JNI(connect)(JNIEnv *env, jclass cls, jlong handle, jlong now_ms, jlong wall_ms) {
  (void)env; (void)cls;
  return ldmux_client_connect(client_of(handle), (uint64_t)now_ms, (uint64_t)wall_ms);
}

JNIEXPORT jint JNICALL JNI(established)(JNIEnv *env, jclass cls, jlong handle) {
  (void)env; (void)cls;
  return ldmux_client_established(client_of(handle));
}

JNIEXPORT jint JNICALL JNI(setRecordsLeft)(JNIEnv *env, jclass cls, jlong handle, jint left) {
  (void)env; (void)cls;
  if (left < 0) return LDMUX_ERR_ARG;
  return ldmux_client_set_records_left(client_of(handle), (uint32_t)left);
}

/* ---------------------------------------------------------------------------------------------
 * The air
 * ------------------------------------------------------------------------------------------- */

JNIEXPORT jint JNICALL JNI(onBleRx)(JNIEnv *env, jclass cls, jlong handle, jbyteArray frame,
                                    jlong now_ms) {
  (void)cls;
  uint8_t buf[JNI_BUF];
  size_t n = 0;
  if (!get_bytes(env, frame, buf, sizeof buf, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_on_ble_rx(client_of(handle), buf, n, (uint64_t)now_ms);
}

/* outLen[0] = the length the library needed, which is set even when it did not fit. */
JNIEXPORT jint JNICALL JNI(poll)(JNIEnv *env, jclass cls, jlong handle, jlong now_ms, jbyteArray out,
                                 jintArray out_len) {
  (void)cls;
  static uint8_t buf[JNI_BUF];
  size_t cap = out ? (size_t)(*env)->GetArrayLength(env, out) : 0;
  size_t n = 0;
  if (cap > sizeof buf) cap = sizeof buf;
  int rc = ldmux_client_poll(client_of(handle), (uint64_t)now_ms, buf, cap, &n);
  if (out_len) put_i32(env, out_len, 0, (jint)n);
  if (rc == 1 && n > 0) {
    if (!put_bytes(env, out, buf, n)) return LDMUX_ERR_BUFFER;
    return 1;
  }
  return rc;
}

JNIEXPORT jint JNICALL JNI(nextDeadline)(JNIEnv *env, jclass cls, jlong handle, jlong now_ms,
                                         jlongArray out) {
  (void)cls;
  uint64_t when = 0;
  int rc = ldmux_client_next_deadline(client_of(handle), (uint64_t)now_ms, &when);
  if (rc == 1) set_i64(env, out, 0, (jlong)when);
  return rc;
}

/* ---------------------------------------------------------------------------------------------
 * Pages
 * ------------------------------------------------------------------------------------------- */

JNIEXPORT jint JNICALL JNI(open)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell, jint cols,
                                 jint rows, jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  if (cols < 0 || cols > 255 || rows < 0 || rows > 255) return LDMUX_ERR_ARG;
  return ldmux_client_open(client_of(handle), id, n, (uint8_t)cols, (uint8_t)rows, (uint64_t)now_ms);
}

/* `createShell`, not `create`: the object already has a `create` for the client itself, and two
 * JNI functions of one name do not link. */
JNIEXPORT jint JNICALL JNI(createShell)(JNIEnv *env, jclass cls, jlong handle, jbyteArray cwd,
                                        jlong now_ms) {
  (void)cls;
  static uint8_t dir[LDMUX_SHELL_MAX * 8];
  size_t n = 0;
  if (!get_bytes(env, cwd, dir, sizeof dir, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_create(client_of(handle), dir, n, (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(takeover)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                     jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_takeover(client_of(handle), id, n, (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(makeLive)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                     jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_make_live(client_of(handle), id, n, (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(history)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                    jint from_line, jint rows, jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  if (from_line < 0 || rows < 0 || rows > 255) return LDMUX_ERR_ARG;
  return ldmux_client_history(client_of(handle), id, n, (uint32_t)from_line, (uint8_t)rows,
                              (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(end)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_end(client_of(handle), id, n, (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(close)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_close(client_of(handle), id, n);
}

/* opId[0] = the operation id, when the caller asked for it. */
JNIEXPORT jint JNICALL JNI(send)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell, jint kind,
                                 jbyteArray items, jint cols, jint rows, jlong now_ms,
                                 jintArray op_id) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  static uint8_t body[JNI_BUF];
  size_t shell_len = 0, items_len = 0;
  if (!get_bytes(env, shell, id, sizeof id, &shell_len)) return LDMUX_ERR_ARG;
  if (!get_bytes(env, items, body, sizeof body, &items_len)) return LDMUX_ERR_ARG;
  if (kind < 0 || kind > 255 || cols < 0 || cols > 255 || rows < 0 || rows > 255)
    return LDMUX_ERR_ARG;
  uint32_t op = 0;
  int rc = ldmux_client_send(client_of(handle), id, shell_len, (uint8_t)kind, body, items_len,
                             (uint8_t)cols, (uint8_t)rows, (uint64_t)now_ms,
                             op_id ? &op : NULL);
  if (op_id && rc == LDMUX_OK) put_i32(env, op_id, 0, (jint)op);
  return rc;
}

JNIEXPORT jint JNICALL JNI(fetch)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell, jint cols,
                                  jint rows, jlong now_ms) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  if (cols < 0 || cols > 255 || rows < 0 || rows > 255) return LDMUX_ERR_ARG;
  return ldmux_client_fetch(client_of(handle), id, n, (uint8_t)cols, (uint8_t)rows, (uint64_t)now_ms);
}

JNIEXPORT jint JNICALL JNI(list)(JNIEnv *env, jclass cls, jlong handle, jlong now_ms) {
  (void)env; (void)cls;
  return ldmux_client_list(client_of(handle), (uint64_t)now_ms);
}


/* ---------------------------------------------------------------------------------------------
 * The phone's own radio
 * ------------------------------------------------------------------------------------------- */

/* The reading, as RADIO_FIELDS ints in the order NativeRadioClient.RadioStatus reads them.
 *
 * The `has_` bytes are kept beside every optional field: a radio that has heard nothing reports no
 * RSSI, and a Kotlin `Int` cannot say "not read" on its own. */
JNIEXPORT jint JNICALL JNI(radio)(JNIEnv *env, jclass cls, jlong handle, jintArray out,
                                  jbyteArray text) {
  (void)cls;
  if (!out || (*env)->GetArrayLength(env, out) < RADIO_FIELDS) return LDMUX_ERR_ARG;
  LdmuxRadioStatus r;
  memset(&r, 0, sizeof r);
  int rc = ldmux_client_radio(client_of(handle), &r);
  if (rc != LDMUX_OK) return rc;
  put_i32(env, out, 0, (jint)r.connected);
  put_i32(env, out, 1, (jint)r.has_signal);
  put_i32(env, out, 2, (jint)r.rssi_dbm);
  put_i32(env, out, 3, (jint)r.snr_quarter_db);
  put_i32(env, out, 4, (jint)r.has_battery);
  put_i32(env, out, 5, (jint)r.battery_mv);
  /* **Every field of this array is an `int`, including the airtime.** The counters are seconds
   * since boot, which the firmware keeps in 32 bits, so an `int` holds any value a radio can
   * report -- and writing them as `long` into an `int[]` aborted the VM at the first settings
   * page (`JNI DETECTED ERROR: incompatible array type int[] expected long[]`, measured
   * 2026-11-12). Kotlin reads index 6 and 7 as `Int` (`RadioStatus.from`).
   *
   * `u32` can exceed `i32::MAX` in principle, so the cast is clamped rather than wrapped: a
   * wrapped airtime would read as a *smaller* number, which is the direction that under-reports
   * the band's duty cycle. */
  put_i32(env, out, 6, (jint)(r.tx_air_secs > 0x7FFFFFFFull ? 0x7FFFFFFF : r.tx_air_secs));
  put_i32(env, out, 7, (jint)(r.rx_air_secs > 0x7FFFFFFFull ? 0x7FFFFFFF : r.rx_air_secs));
  put_i32(env, out, 8, (jint)r.ops_left);
  put_i32(env, out, 9, (jint)r.has_preset);
  put_i32(env, out, 10, (jint)r.freq_khz);
  put_i32(env, out, 11, (jint)r.bw_hz);
  put_i32(env, out, 12, (jint)r.spreading_factor);
  put_i32(env, out, 13, (jint)r.coding_rate);
  put_i32(env, out, 14, (jint)r.tx_power_dbm);
  put_i32(env, out, 15, (jint)r.path_hash_width);
  put_i32(env, out, 16, (jint)r.firmware_version_code);
  put_i32(env, out, 17, (jint)r.has_identity);
  put_i32(env, out, 18, (jint)r.max_tx_power_dbm);
  put_i32(env, out, 19, (jint)r.name_len);
  put_i32(env, out, 20, (jint)r.has_device);
  put_i32(env, out, 21, (jint)r.firmware_len);
  put_i32(env, out, 22, (jint)r.max_contacts);
  /* The two strings, in one buffer: name first, then firmware. A caller that gets 0 bytes for
   * either draws nothing, rather than an empty box where a fact should be. */
  if (text && (*env)->GetArrayLength(env, text) >= RADIO_TEXT) {
    uint8_t both[RADIO_TEXT];
    memset(both, 0, sizeof both);
    memcpy(both, r.name, (size_t)r.name_len);
    memcpy(both + NAME_MAX, r.firmware, (size_t)r.firmware_len);
    if (!put_bytes(env, text, both, RADIO_TEXT)) return LDMUX_ERR_ARG;
  }
  return (*env)->ExceptionCheck(env) ? LDMUX_ERR_ARG : LDMUX_OK;
}

/* The route, as ROUTE_FIELDS ints in the order NativeRadioClient.Route reads them, and the path
 * bytes in path_out (its length in `path_len[0]`). */
JNIEXPORT jint JNICALL JNI(route)(JNIEnv *env, jclass cls, jlong handle, jlong now_ms, jintArray out,
                                  jbyteArray path_out, jintArray path_len) {
  (void)cls;
  if (!out || (*env)->GetArrayLength(env, out) < ROUTE_FIELDS) return LDMUX_ERR_ARG;
  LdmuxRoute r;
  memset(&r, 0, sizeof r);
  int rc = ldmux_client_route(client_of(handle), (uint64_t)now_ms, &r);
  if (rc != LDMUX_OK) return rc;
  put_i32(env, out, 0, (jint)r.known);
  put_i32(env, out, 1, (jint)r.proven);
  put_i32(env, out, 2, (jint)r.fresh);
  put_i32(env, out, 3, (jint)r.flood);
  put_i32(env, out, 4, (jint)r.hops);
  put_i32(env, out, 5, (jint)r.path_width);
  put_i32(env, out, 6, (jint)r.snr_quarter_db);
  put_i32(env, out, 7, (jint)r.path_len);
  if (path_len) put_i32(env, path_len, 0, (jint)r.path_len);
  if (path_out && r.path_len > 0) {
    if ((size_t)(*env)->GetArrayLength(env, path_out) < r.path_len) return LDMUX_ERR_BUFFER;
    if (!put_bytes(env, path_out, r.path, r.path_len)) return LDMUX_ERR_BUFFER;
  }
  return (*env)->ExceptionCheck(env) ? LDMUX_ERR_ARG : LDMUX_OK;
}

JNIEXPORT jint JNICALL JNI(setScope)(JNIEnv *env, jclass cls, jlong handle, jbyteArray key) {
  (void)cls;
  uint8_t buf[32];
  size_t n = 0;
  if (!get_bytes(env, key, buf, sizeof buf, &n)) return LDMUX_ERR_ARG;
  return ldmux_client_set_scope(client_of(handle), buf, n);
}

JNIEXPORT jint JNICALL JNI(setPreset)(JNIEnv *env, jclass cls, jlong handle, jint freq_khz,
                                    jint bw_hz, jint sf, jint cr, jint path_width,
                                    jint power_dbm) {
  (void)env;
  (void)cls;
  return ldmux_client_set_preset(client_of(handle), (uint32_t)freq_khz, (uint32_t)bw_hz,
                                 (uint8_t)sf, (uint8_t)cr, (uint8_t)path_width,
                                 (int8_t)power_dbm);
}

/* ---------------------------------------------------------------------------------------------
 * Reading back
 * ------------------------------------------------------------------------------------------- */

JNIEXPORT jint JNICALL JNI(mirror)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                   jintArray out) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  size_t n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &n)) return LDMUX_ERR_ARG;
  if (!out || (*env)->GetArrayLength(env, out) < MIRROR_FIELDS) return LDMUX_ERR_ARG;
  LdmuxMirror m;
  memset(&m, 0, sizeof m);
  int rc = ldmux_client_mirror(client_of(handle), id, n, &m);
  if (rc != 1) return rc;
  put_i32(env, out, 0, (jint)m.painted);
  put_i32(env, out, 1, (jint)m.cols);
  put_i32(env, out, 2, (jint)m.rows);
  put_i32(env, out, 3, (jint)m.cursor_x);
  put_i32(env, out, 4, (jint)m.cursor_y);
  put_i32(env, out, 5, (jint)m.alternate);
  put_i32(env, out, 6, (jint)m.dead);
  put_i32(env, out, 7, (jint)m.settled);
  put_i32(env, out, 8, (jint)m.has_applied);
  put_i32(env, out, 9, (jint)m.applied_status);
  put_i32(env, out, 10, (jint)m.snapshot_id);
  put_i32(env, out, 11, (jint)m.first_line);
  return (*env)->ExceptionCheck(env) ? LDMUX_ERR_ARG : 1;
}

/* outLen[0] = the row's length, set even when it did not fit. */
JNIEXPORT jint JNICALL JNI(mirrorLine)(JNIEnv *env, jclass cls, jlong handle, jbyteArray shell,
                                       jint row, jbyteArray out, jintArray out_len) {
  (void)cls;
  uint8_t id[LDMUX_SHELL_MAX];
  static uint8_t line[JNI_BUF];
  size_t shell_len = 0, n = 0;
  if (!get_bytes(env, shell, id, sizeof id, &shell_len)) return LDMUX_ERR_ARG;
  if (row < 0) return LDMUX_ERR_ARG;
  size_t cap = out ? (size_t)(*env)->GetArrayLength(env, out) : 0;
  if (cap > sizeof line) cap = sizeof line;
  int rc = ldmux_client_mirror_line(client_of(handle), id, shell_len, (uint32_t)row, line, cap, &n);
  if (out_len) put_i32(env, out_len, 0, (jint)n);
  if (rc == 1 && n > 0) {
    if (!put_bytes(env, out, line, n)) return LDMUX_ERR_BUFFER;
  }
  return rc;
}

/* One event, or 0 when there is none. `out` carries the kinds and the scalars, `shell_out` the
 * shell id, `text_out` the wire bytes of a message kind, and each length lands in its int[1]. */
JNIEXPORT jint JNICALL JNI(pollEvent)(JNIEnv *env, jclass cls, jlong handle, jintArray out,
                                      jbyteArray shell_out, jintArray shell_len,
                                      jbyteArray text_out, jintArray text_len) {
  (void)cls;
  if (!out || (*env)->GetArrayLength(env, out) < EVENT_FIELDS) return LDMUX_ERR_ARG;
  LdmuxClientEvent ev;
  static uint8_t text[JNI_BUF];
  memset(&ev, 0, sizeof ev);
  size_t cap = text_out ? (size_t)(*env)->GetArrayLength(env, text_out) : 0;
  if (cap > sizeof text) cap = sizeof text;
  size_t written = 0;
  int rc = ldmux_client_poll_event(client_of(handle), &ev, text, cap, &written);
  if (rc != 1) return rc;
  put_i32(env, out, 0, (jint)ev.kind);
  put_i32(env, out, 1, (jint)ev.value);
  put_i32(env, out, 2, (jint)ev.snapshot_id);
  put_i32(env, out, 3, (jint)ev.has_applied);
  put_i32(env, out, 4, (jint)ev.applied_status);
  put_i32(env, out, 5, (jint)ev.settled);
  put_i32(env, out, 6, (jint)ev.cols);
  put_i32(env, out, 7, (jint)ev.rows);
  if (shell_len) put_i32(env, shell_len, 0, (jint)ev.shell_len);
  if (shell_out && ev.shell_len > 0) put_bytes(env, shell_out, ev.shell, ev.shell_len);
  if (text_len) put_i32(env, text_len, 0, (jint)ev.text_len);
  if (text_out && written > 0) {
    if (!put_bytes(env, text_out, text, written)) return LDMUX_ERR_BUFFER;
  }
  return (*env)->ExceptionCheck(env) ? LDMUX_ERR_ARG : 1;
}

JNIEXPORT jint JNICALL JNI(stats)(JNIEnv *env, jclass cls, jlong handle, jlongArray out) {
  (void)cls;
  if (!out || (*env)->GetArrayLength(env, out) < STATS_FIELDS) return LDMUX_ERR_ARG;
  LdmuxClientStats s;
  memset(&s, 0, sizeof s);
  int rc = ldmux_client_stats(client_of(handle), &s);
  if (rc != LDMUX_OK) return rc;
  put_i64(env, out, 0, (jlong)s.records_sent);
  put_i64(env, out, 1, (jlong)s.records_received);
  put_i64(env, out, 2, (jlong)s.commands_sent);
  put_i64(env, out, 3, (jlong)s.fetches_sent);
  put_i64(env, out, 4, (jlong)s.snapshots_applied);
  put_i64(env, out, 5, (jlong)s.snapshots_refused);
  put_i64(env, out, 6, (jlong)s.sessions_established);
  put_i64(env, out, 7, (jlong)s.rungs_used);
  put_i64(env, out, 8, (jlong)s.blind_asks);
  put_i64(env, out, 9, (jlong)s.route_answered);
  put_i64(env, out, 10, (jlong)s.radio_records);
  put_i64(env, out, 11, (jlong)s.radio_answered);
  put_i64(env, out, 12, (jlong)s.radio_expired);
  put_i64(env, out, 13, (jlong)s.records_admitted);
  put_i64(env, out, 14, (jlong)s.rejected_self_echo);
  put_i64(env, out, 15, (jlong)s.rejected_in_transit);
  put_i64(env, out, 16, (jlong)s.rejected_handshake);
  put_i64(env, out, 17, (jlong)s.unknown_downlink);
  put_i64(env, out, 18, (jlong)s.lists_sent);
  put_i64(env, out, 19, (jlong)s.rejected_unknown_kind);
  put_i64(env, out, 20, (jlong)s.rejected_record);
  put_i64(env, out, 21, (jlong)s.session_index);
  put_i64(env, out, 22, (jlong)s.parts_sent);
  put_i64(env, out, 23, (jlong)s.acks_sent);
  put_i64(env, out, 24, (jlong)s.radio_backlog);
  put_i64(env, out, 25, (jlong)s.radio_ready);
  put_i64(env, out, 26, (jlong)s.radio_malformed);
  put_i64(env, out, 27, (jlong)s.radio_outstanding);
  put_i64(env, out, 28, (jlong)s.radio_pumps);
  put_i64(env, out, 29, (jlong)s.answers_finished);
  put_i64(env, out, 30, (jlong)s.answers_repeated);
  put_i64(env, out, 31, (jlong)s.answers_ignored);
  put_i64(env, out, 32, (jlong)s.last_record_refusal);
  put_i64(env, out, 33, (jlong)s.last_refusal_index);
  put_i64(env, out, 34, (jlong)s.last_refusal_counter);
  put_i64(env, out, 35, (jlong)s.local_index);
  put_i64(env, out, 36, (jlong)s.duplicate_records);
  put_i64(env, out, 37, (jlong)s.wrong_index_records);
  put_i64(env, out, 38, (jlong)s.acks_staged);
  put_i64(env, out, 39, (jlong)s.records_without_a_session);
  put_i64(env, out, 40, (jlong)s.last_admitted_kind);
  put_i64(env, out, 41, (jlong)s.last_admitted_len);
  put_i64(env, out, 42, (jlong)s.records_refused);
  return (*env)->ExceptionCheck(env) ? LDMUX_ERR_ARG : LDMUX_OK;
}
