/*
 * ldmux_session.h — the C ABI over the L3 session (crates/session-ffi, ADR-0023).
 *
 * One implementation of Noise IKpsk2_25519_ChaChaPoly_SHA256, the 6+2+16 record and the ARQ,
 * written once in Rust and called from the phone. Nothing here knows a radio, a preset, a
 * contact or an airtime figure: the only numbers it takes are the record size the gateway
 * advertised and the count of records the gateway says may still be sent.
 *
 * OWNERSHIP
 *   - Every handle (LdmuxInitiator, LdmuxSession) is created by this library and owned by the
 *     caller until it is passed to its *_free. Handles are opaque: never read through them.
 *   - Free functions take a pointer to the caller's handle variable and null it. Freeing twice
 *     through the same variable is a no-op; using a nulled handle returns LDMUX_ERR_NULL.
 *   - ldmux_initiator_finish consumes the initiator whatever the outcome and nulls it.
 *   - No pointer into library memory is ever returned. Every byte the library produces is
 *     copied into a buffer the caller supplied, so there is nothing for the caller to free
 *     or to keep alive.
 *   - There is no global state. A handle is not thread safe; serialise calls on one handle.
 *   - Every function returns a status; no function unwinds. A Rust panic is LDMUX_ERR_PANIC.
 *
 * BUFFERS
 *   - Keys and the PSK are exactly 32 bytes, message 1 exactly 121, message 2 exactly 50.
 *   - Variable-length outputs take (buf, cap, &len). *len is always set, so a
 *     LDMUX_ERR_BUFFER tells the caller what it needed; the item stays queued.
 *   - A sealed record or delivered chunk is at most LDMUX_RECORD_MAX (172) bytes.
 */
#ifndef LDMUX_SESSION_H
#define LDMUX_SESSION_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LdmuxInitiator LdmuxInitiator;
typedef struct LdmuxSession LdmuxSession;

#define LDMUX_KEY_LEN 32
#define LDMUX_MSG1_LEN 121
#define LDMUX_MSG2_LEN 50
#define LDMUX_RECORD_MAX 172

/* Status codes. 0 is success; failures are negative. */
#define LDMUX_OK 0
#define LDMUX_ERR_NULL (-1)
#define LDMUX_ERR_PANIC (-2)
#define LDMUX_ERR_ARG (-3)
#define LDMUX_ERR_BUFFER (-4)
#define LDMUX_ERR_CRYPTO (-10)
#define LDMUX_ERR_MALFORMED (-11)
#define LDMUX_ERR_BAD_FILTER (-12)
#define LDMUX_ERR_STALE (-13)
#define LDMUX_ERR_REPLAY (-14)
#define LDMUX_ERR_PIN_MISMATCH (-15)
#define LDMUX_ERR_WRONG_INDEX (-20)
#define LDMUX_ERR_DUPLICATE (-21)
#define LDMUX_ERR_TOO_OLD (-22)
#define LDMUX_ERR_BAD_TAG (-23)
#define LDMUX_ERR_TOO_LONG (-24)
#define LDMUX_ERR_COUNTER_EXHAUSTED (-25)
#define LDMUX_ERR_DECODE (-26)
#define LDMUX_ERR_RECORD_LIMIT (-27)
#define LDMUX_ERR_QUEUE_FULL (-30)
#define LDMUX_ERR_NOT_OPEN (-31)

/* ldmux_session_receive results (>= 0). */
#define LDMUX_RX_QUIET 0
#define LDMUX_RX_DATA 1
#define LDMUX_RX_CLOSED 2

/* ldmux_session_status bits. */
#define LDMUX_STATUS_FAILED 1
#define LDMUX_STATUS_PEER_CLOSED 2
#define LDMUX_STATUS_NEEDS_REKEY 4

/* Generate an X25519 keypair into two 32-byte buffers. */
int32_t ldmux_keypair_generate(uint8_t *priv_out, uint8_t *pub_out);

/* Phone side. Writes the 121-byte message 1 and a new initiator. *out is untouched on failure.
 * timestamp_ms is wall-clock Unix milliseconds. */
int32_t ldmux_initiator_start(const uint8_t *local_priv, const uint8_t *local_pub,
                              const uint8_t *remote_pub, const uint8_t *psk,
                              uint64_t timestamp_ms, LdmuxInitiator **out, uint8_t *msg1);

/* Consume message 2. *hs is consumed and nulled whatever the result. record_limit is the
 * gateway's advertised uplinkRecordMax. On success *out is the established session. */
int32_t ldmux_initiator_finish(LdmuxInitiator **hs, const uint8_t *msg2, size_t msg2_len,
                               uint32_t record_limit, uint64_t now_ms, LdmuxSession **out);

/* Abandon a started handshake. */
void ldmux_initiator_free(LdmuxInitiator **hs);

/* Daemon side, for one pairing. pinned is NULL or 32 bytes. Writes the 50-byte message 2, the
 * phone key to pin, the timestamp to remember, and the established session. */
int32_t ldmux_responder_accept(const uint8_t *local_priv, const uint8_t *local_pub,
                               const uint8_t *psk, const uint8_t *pinned,
                               uint64_t last_timestamp_ms, uint64_t now_ms,
                               const uint8_t *msg1, size_t msg1_len, uint8_t index,
                               uint32_t record_limit, uint8_t *reply,
                               uint8_t *remote_static_out, uint64_t *timestamp_out,
                               LdmuxSession **out);

/* Queue bytes for a shell. All or nothing: LDMUX_ERR_QUEUE_FULL leaves the queue as it was. */
int32_t ldmux_session_send(LdmuxSession *s, uint8_t shell, const uint8_t *data, size_t len);

/* Feed one record from the tunnel. Returns LDMUX_RX_* or a negative error; a record that fails
 * any check changes no state. Drain data with ldmux_session_take_delivered. */
int32_t ldmux_session_receive(LdmuxSession *s, uint64_t now_ms, const uint8_t *record,
                              size_t len);

/* Pop one in-order chunk. Returns 1 and fills the buffer, 0 if none, or a negative error. */
int32_t ldmux_session_take_delivered(LdmuxSession *s, uint8_t *shell_out, uint8_t *out,
                                     size_t cap, size_t *out_len);

/* Ask the ARQ what to send now. NEVER more than records_left (the gateway's
 * recordsLeftThisHour) nor l2_slots (free L2 credit). Sealed records queue inside the handle;
 * returns how many are waiting. */
int32_t ldmux_session_poll(LdmuxSession *s, uint64_t now_ms, uint8_t records_left,
                           uint8_t l2_slots);

/* Pop one sealed record, at most LDMUX_RECORD_MAX bytes. Returns 1, 0 if none, or an error. */
int32_t ldmux_session_take_record(LdmuxSession *s, uint8_t *out, size_t cap, size_t *out_len);

/* Pop one sealed record and the ARQ sequence number that names its slot. Use this when the caller
 * can observe the radio's send result: `seq_out` is what ldmux_session_refuse_seq needs, and it is
 * written only on success. Returns 1, 0 if none, or an error. */
int32_t ldmux_session_take_record_seq(LdmuxSession *s, uint8_t *out, size_t cap, size_t *out_len,
                                      uint16_t *seq_out);

/* Tell the session the radio refused that record without transmitting it. The attempt is given
 * back and the retry follows the radio's transmit-table pace instead of a full retry interval.
 * Returns 1 when the slot was in flight, 0 when it was already settled. */
int32_t ldmux_session_refuse_seq(LdmuxSession *s, uint64_t now_ms, uint16_t seq);

/* End the session once everything queued is acknowledged. */
int32_t ldmux_session_close(LdmuxSession *s);

/* LDMUX_STATUS_* bits, or a negative error. */
int32_t ldmux_session_status(LdmuxSession *s, uint64_t now_ms);

/* Retransmission timing for this radio: one full record's airtime at the active preset, in ms.
 * Zero or less keeps the interactive defaults. Call once the preset is known. */
int32_t ldmux_session_set_airtime(LdmuxSession *s, double airtime_ms);

/* Repeaters on the route to the peer: the first retransmission is six airtimes per hop, which is
   MeshCore's own direct-send budget. Zero or one keeps the direct-neighbour timing. */
int32_t ldmux_session_set_hops(LdmuxSession *s, uint32_t hops);

/* Re-size the record because the route changed: a repeater's hops come out of the same 176-byte
   radio frame as the record. Queued chunks keep the size they were split at. */
int32_t ldmux_session_set_record_limit(LdmuxSession *s, uint32_t bytes);

/* Chunks queued or in flight. */
int32_t ldmux_session_pending(LdmuxSession *s);

/* Writes the next retransmit deadline (ms) and returns 1, or returns 0 if none. */
int32_t ldmux_session_next_deadline(LdmuxSession *s, uint64_t *out);

/* The peer's static public key, 32 bytes. */
int32_t ldmux_session_peer_key(LdmuxSession *s, uint8_t *out);

/* Records dropped because they failed a check. */
int32_t ldmux_session_dropped(LdmuxSession *s);

/* Destroy and null. Safe to call again through the same variable. */
void ldmux_session_free(LdmuxSession **s);

#ifdef __cplusplus
}
#endif
#endif
