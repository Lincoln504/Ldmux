/*
 * ldmux_client.h — the C ABI over the phone's whole radio route (crates/session-ffi,
 * docs/RADIO-PLAN.md §8.1).
 *
 * This is the phone's engine, in Rust: the handshake, the record layer, the transaction layer, the
 * carriage decision, the terminal protocol and the per-page screen mirror. What is left in Kotlin is
 * a BLE pipe: it writes the frames this library hands it and feeds back the notifications it hears.
 * Nothing here knows a GATT characteristic, a coroutine or an Android `Context`.
 *
 * PULL ONLY. The library never calls back into the caller and never starts a thread. One loop in the
 * caller is enough:
 *
 *     poll frames out ──► write them to the radio
 *     radio notification ──► ldmux_client_on_ble_rx
 *     poll events out ──► draw
 *     ldmux_client_next_deadline ──► set one timer
 *
 * OWNERSHIP
 *   - `LdmuxClient` is created by ldmux_client_new and owned by the caller until ldmux_client_free.
 *     Handles are opaque: never read through them.
 *   - ldmux_client_free takes a pointer to the caller's handle variable and nulls it. Freeing twice
 *     through the same variable is a no-op; using a nulled handle returns LDMUX_ERR_NULL.
 *   - No pointer into library memory is ever returned. Every byte the library produces is copied
 *     into a buffer the caller supplied, so there is nothing for the caller to free or keep alive.
 *   - There is no global state. A handle is not thread safe; serialise calls on one handle.
 *   - Every function returns a status; no function unwinds. A Rust panic is LDMUX_ERR_PANIC.
 *
 * BUFFERS
 *   - Variable-length outputs take (buf, cap, &len). *len is always set, so a LDMUX_ERR_BUFFER tells
 *     the caller what it needed. **Nothing is lost by a short buffer**: the frame or the event stays
 *     queued and the next call returns it.
 *   - A shell id is 1..=LDMUX_SHELL_MAX bytes of UTF-8.
 */
#ifndef LDMUX_CLIENT_H
#define LDMUX_CLIENT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LdmuxClient LdmuxClient;

/* Status codes. 0 is success; failures are negative. The first four are shared with
 * ldmux_session.h and mean the same thing there. */
#define LDMUX_OK 0
#define LDMUX_ERR_NULL (-1)
#define LDMUX_ERR_PANIC (-2)
#define LDMUX_ERR_ARG (-3)
#define LDMUX_ERR_BUFFER (-4)
/* A shell id or a command that cannot be built. */
#define LDMUX_ERR_DECODE (-26)
#define LDMUX_ERR_TOO_LONG (-24)
#define LDMUX_ERR_QUEUE_FULL (-30)
#define LDMUX_ERR_NOT_OPEN (-31)
/* The page cap is reached: close one first. */
#define LDMUX_ERR_TOO_MANY_PAGES (-32)
/* That shell is already open. */
#define LDMUX_ERR_ALREADY_OPEN (-33)
/* That shell is not open, so there is nothing to send to or refresh. */
#define LDMUX_ERR_NO_PAGE (-34)

#define LDMUX_SHELL_MAX 40

/* What one `send` item is. */
#define LDMUX_ITEM_TEXT 0
#define LDMUX_ITEM_PASTE 1
/* Key names, space separated — the same names mesh_term's `Key` message takes (`Up`, `S-Tab`,
 * `C-c`). The engine encodes them for the pane's live cursor-key and Kitty mode, which only the
 * host can read, so the phone sends names rather than escape sequences it would have to guess. */
#define LDMUX_ITEM_KEYS 2
/* A whole command: an item stream, `[kind u8][len u16 LE][bytes]...` for text and pastes and the
 * Keys form for names — the same bytes mesh_term's `Send` carries. Use this whenever a command has
 * more than one item; sending a draft's text and its Enter as two calls is two messages, which is
 * RC-8 (docs/RADIO-PLAN.md §12). */
#define LDMUX_ITEM_STREAM 3

/* Event kinds, as LdmuxClientEvent.kind. */
#define LDMUX_EV_ATTACHED 1
#define LDMUX_EV_SETTLED 2
#define LDMUX_EV_HELD 3
#define LDMUX_EV_CLOSED 4
#define LDMUX_EV_SESSIONS 5
#define LDMUX_EV_MARKS 6
#define LDMUX_EV_HISTORY 7
/* The host made the shell a Create asked for. `text` carries its id as UTF-8 — not a wire message,
 * because a Create's answer is a name rather than something the app draws. */
#define LDMUX_EV_CREATED 8

/*
 * What the client needs to start. Every field is a fact the caller already has: the pairing's keys
 * (from the QR code), the radio's tuple, and the mesh's path width.
 */
typedef struct {
    uint8_t peer_static[32];  /* the host radio's static public key */
    uint8_t psk[32];          /* the pairing's pre-shared key */
    uint8_t local_private[32];/* this phone's own static private key */
    uint8_t local_public[32]; /* and its public half */
    uint8_t path_width;       /* bytes per hop, 1..=3 */
    uint8_t sf;               /* spreading factor 7..=12 for the SNR margin table; 0 = default */
    uint8_t flood_reach_hops; /* how far a flood is expected to reach; 0 = default */
    uint8_t max_open_pages;   /* how many pages the operator may open; 0 = default (5) */
    float airtime_ms;         /* one full record's airtime at this tuple */
    uint16_t scope;           /* the flood scope's transport code, 0 = unscoped */
} LdmuxClientConfig;

/*
 * One page's screen, as the caller reads it. The rows are fetched one at a time with
 * ldmux_client_mirror_line, because a row is variable length and a C struct cannot own it.
 */
typedef struct {
    uint8_t painted;       /* whether there is a screen to draw at all */
    uint8_t cols;          /* the grid the host painted */
    uint8_t rows;          /* how many rows ldmux_client_mirror_line will answer for */
    uint8_t cursor_x;
    uint8_t cursor_y;
    uint8_t alternate;     /* a full-screen program owns the pane */
    uint8_t dead;          /* the shell's process exited */
    uint8_t settled;       /* clear means the screen is a mid-state, not the end of the command */
    uint8_t has_applied;   /* 1 when applied_status is meaningful */
    uint8_t applied_status;/* what the host did with the command: mesh_term's `applied` vocabulary */
    uint32_t snapshot_id;  /* the identity of this screen */
    uint32_t first_line;   /* the absolute line number of row 0 */
} LdmuxMirror;

/*
 * One thing the app has to draw or say. `kind` says which fields mean anything and whether the text
 * buffer carries bytes:
 *
 *   kind                 fields                                        text
 *   LDMUX_EV_ATTACHED    cols, rows                                    -
 *   LDMUX_EV_SETTLED     snapshot_id, settled, has_applied,            -
 *                        applied_status
 *   LDMUX_EV_HELD        -                                             -
 *   LDMUX_EV_CLOSED      value = the reason (mesh_term's `closed`)      a Closed message
 *   LDMUX_EV_SESSIONS    value = 1 when it is the last page             a Sessions message
 *   LDMUX_EV_MARKS       value = 1 when it is the last page             a Marks message
 *   LDMUX_EV_HISTORY     value = 1 when it is the last page             a HistoryPage message
 *   LDMUX_EV_CREATED     -                                             the new shell's id, UTF-8
 *
 * The four message kinds are the wire bytes of the message the host sent, so the caller decodes them
 * with the decoder it already has. A second encoding for one fact would be a second thing to be
 * wrong in.
 */
typedef struct {
    uint8_t kind;
    uint8_t shell_len;
    uint8_t shell[64];     /* the shell's id, shell_len bytes of it */
    uint32_t value;        /* meaning depends on kind */
    uint32_t snapshot_id;  /* LDMUX_EV_SETTLED */
    uint8_t has_applied;   /* LDMUX_EV_SETTLED */
    uint8_t applied_status;/* LDMUX_EV_SETTLED */
    uint8_t settled;       /* LDMUX_EV_SETTLED */
    uint8_t cols;          /* LDMUX_EV_ATTACHED */
    uint8_t rows;          /* LDMUX_EV_ATTACHED */
    uint32_t text_len;     /* bytes written to the caller's text buffer */
} LdmuxClientEvent;

/* The client's own counters, for a log that has to say what happened. */
typedef struct {
    uint64_t records_sent;
    uint64_t records_received;
    uint64_t commands_sent;
    uint64_t fetches_sent;
    uint64_t snapshots_applied;
    uint64_t snapshots_refused;
    uint64_t sessions_established;
    uint64_t rungs_used;
    /* Bootstrap rungs sent while no path to the peer was held, and whether the mesh answered one. */
    uint64_t blind_asks;
    uint8_t route_answered;
    /* What the phone's own radio answered: records queued, commands answered, deadlines missed. */
    uint64_t radio_records;
    uint64_t radio_answered;
    uint64_t radio_expired;
    /*
     * What happened to what arrived, one counter per decision the session layer makes. They exist
     * because "a record arrived and the session refused it" and "a record never arrived" look
     * identical on screen: measured 2026-11-13, a phone with a live session, thirteen records
     * received and no message in the log.
     */
    uint64_t records_admitted;
    uint64_t rejected_self_echo;
    uint64_t rejected_in_transit;
    uint64_t rejected_handshake;
    uint64_t unknown_downlink;
    uint64_t lists_sent;
} LdmuxClientStats;

/*
 * What the phone's radio last said about itself.
 *
 * Every reading is optional separately, because they arrive from different questions and a radio
 * that answered one may not answer the others: a firmware without GET_STATS still reports its
 * tuple, and a radio that has heard nothing reports no RSSI rather than a plausible number. A field
 * whose `has_` byte is 0 has not been read.
 */
typedef struct {
    uint8_t connected;        /* 1 once the radio answered APP_START */
    uint8_t has_signal;       /* 1 when rssi_dbm and snr_quarter_db mean anything */
    int8_t rssi_dbm;          /* the last packet **this end** heard */
    int8_t snr_quarter_db;    /* its SNR, in quarters of a dB */
    uint8_t has_battery;
    uint16_t battery_mv;
    uint32_t tx_air_secs;     /* the radio's own transmit airtime since boot */
    uint32_t rx_air_secs;
    uint8_t ops_left;         /* the caller's record budget; 255 = not reported */
    uint8_t has_preset;
    uint32_t freq_khz;
    uint32_t bw_hz;
    uint8_t spreading_factor;
    uint8_t coding_rate;      /* the firmware's denominator index: 5 is 4/5 */
    int8_t tx_power_dbm;
    uint8_t path_hash_width;  /* bytes per hop, 1..=3; 0 when the firmware did not say */
    uint8_t firmware_version_code;
    /*
     * Identity, from SELF_INFO. `has_identity` is separate from `has_device` because the two
     * arrive from different questions: a firmware that answers one and not the other is a real
     * radio, and a caller draws nothing for what was not read rather than a zero.
     */
    uint8_t has_identity;
    uint8_t name_len;         /* meaningful bytes of `name`; 0 = not read */
    uint8_t name[32];         /* UTF-8, not NUL-terminated */
    int8_t max_tx_power_dbm;  /* the radio's own ceiling, beside the tx_power_dbm in use */
    /* Device description, from DEVICE_INFO. */
    uint8_t has_device;
    uint8_t firmware_len;     /* meaningful bytes of `firmware`; 0 = not read */
    uint8_t firmware[24];     /* UTF-8, not NUL-terminated */
    uint16_t max_contacts;    /* 0xFFFF when the firmware did not say */
} LdmuxRadioStatus;

/*
 * What this end believes about the path to the peer.
 *
 * `hops` is why this exists: the scrollback wait is sized against the route, so a flat deadline on a
 * twenty-hop route loses a page that was still arriving.
 */
typedef struct {
    uint8_t known;        /* 1 when a fresh path is held, so the next attempt goes direct */
    uint8_t proven;       /* 1 when an authenticated direct arrival proved it */
    uint8_t fresh;
    uint8_t flood;        /* 1 when the next first attempt would flood */
    uint8_t hops;
    uint8_t path_width;
    uint8_t path_len;
    uint8_t path[64];
    int16_t snr_quarter_db;
} LdmuxRoute;

/* ---------------------------------------------------------------------------------------------
 * Life cycle
 * ------------------------------------------------------------------------------------------- */

/* Create a client from `config` and write the handle to `*out`.
 * Returns LDMUX_ERR_NULL for a null argument, LDMUX_ERR_ARG for a path width outside 1..=3, or
 * LDMUX_ERR_PANIC for a panic inside. */
/*
 * Fill `priv_out` and `pub_out` with a fresh device keypair (32 bytes each).
 *
 * A pairing starts before there is a session to put it in, so this is the one call the app needs
 * outside a client handle. One library, one implementation of the suite.
 *
 * Returns LDMUX_OK, LDMUX_ERR_NULL for a null pointer, or LDMUX_ERR_PANIC.
 */
int32_t ldmux_client_keypair(uint8_t *priv_out, uint8_t *pub_out);

int32_t ldmux_client_new(const LdmuxClientConfig *config, LdmuxClient **out);

/* Destroy a client and null the caller's handle. A second call is a no-op. */
void ldmux_client_free(LdmuxClient **c);

/* Start an attempt at the host. `wall_ms` is the phone's wall clock, which the handshake stamps;
 * `now_ms` is a monotonic clock, which every deadline is measured on. They are separate because a
 * phone's wall clock can jump and its monotonic one cannot. */
int32_t ldmux_client_connect(LdmuxClient *c, uint64_t now_ms, uint64_t wall_ms);

/* 1 when an L3 session is established, 0 when not. */
int32_t ldmux_client_established(LdmuxClient *c);

/* Tell the engine how many records the radio says are left this hour (the radio's own budget
 * report). Never a guess: a caller with no report leaves this alone. */
int32_t ldmux_client_set_records_left(LdmuxClient *c, uint32_t left);

/* ---------------------------------------------------------------------------------------------
 * The air
 * ------------------------------------------------------------------------------------------- */

/* Hand one BLE notification to the engine: one companion **payload**, opcode first.
 *
 * A characteristic carries the payload and no serial framing; the engine decides whether it is a
 * receive-log push carrying a record, the answer to a command it asked, or a push about somebody
 * else's traffic. Returns 1 when it carried a record offered to the link, 0 for anything else, or a
 * negative error. */
int32_t ldmux_client_on_ble_rx(LdmuxClient *c, const uint8_t *frame, size_t len, uint64_t now_ms);

/* Write **at most one** frame to `out`, and its length to `*out_len`.
 *
 * The frame is a companion **payload**, ready for the characteristic: `[opcode][args...]`. It is not
 * a serial frame — no `0x3C` and no length — because a BLE write carries the payload and a serial
 * caller wraps it once.
 *
 * Returns 1 when a frame was written, 0 when there is nothing to send, or a negative error; a `cap`
 * too small writes nothing, keeps the frame and reports the length it needed in *out_len. Call in a
 * loop until it returns 0: a command queued behind another becomes writable the moment the first is
 * answered. */
int32_t ldmux_client_poll(LdmuxClient *c, uint64_t now_ms, uint8_t *out, size_t cap, size_t *out_len);

/* When the caller should look again even if nothing arrives: 1 and *out set, or 0 for no deadline.
 * The engine never wakes the caller, so this is the only timer the app needs. */
int32_t ldmux_client_next_deadline(LdmuxClient *c, uint64_t *out);

/* ---------------------------------------------------------------------------------------------
 * Pages
 * ------------------------------------------------------------------------------------------- */

/* Open a page for `shell`. The answer is a screen, which arrives as LDMUX_EV_ATTACHED and then
 * LDMUX_EV_SETTLED. Refused with LDMUX_ERR_TOO_MANY_PAGES when the cap is reached. */
int32_t ldmux_client_open(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint8_t cols,
                          uint8_t rows, uint64_t now_ms);

/* Let go of `shell`. */
int32_t ldmux_client_close(LdmuxClient *c, const uint8_t *shell, size_t shell_len);

/* Send one command to `shell`, and write its operation id to `*op_id` when `op_id` is not null. The
 * answer is one LDMUX_EV_SETTLED carrying what the host did with it.
 *
 * `kind` is LDMUX_ITEM_TEXT for text typed at a prompt, LDMUX_ITEM_PASTE for anything that must not
 * run as it arrives (a multi-line paste), LDMUX_ITEM_KEYS for key names, or LDMUX_ITEM_STREAM for a
 * composed draft — which is what a command with more than one item uses, so that `ls -la` and its
 * Enter are ONE message. */
int32_t ldmux_client_send(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint8_t kind,
                          const uint8_t *items, size_t items_len, uint8_t cols, uint8_t rows,
                          uint64_t now_ms, uint32_t *op_id);

/* Ask the host to start a shell in `cwd` (empty = its default directory). The answer is one
 * LDMUX_EV_CREATED whose text is the id to pass to ldmux_client_open: the phone attaches to the name
 * it is given rather than guessing one out of a list another client may have changed. */
int32_t ldmux_client_create(LdmuxClient *c, const uint8_t *cwd, size_t cwd_len, uint64_t now_ms);

/* Take `shell` from whoever holds it. A plain open is refused while a desk window holds a shell, so
 * taking one is always something the operator chose. */
int32_t ldmux_client_takeover(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint64_t now_ms);

/* Ask the host to open `shell` in a desk window: send to desktop. */
int32_t ldmux_client_make_live(LdmuxClient *c, const uint8_t *shell, size_t shell_len,
                               uint64_t now_ms);

/* Ask the host for a page of `shell`'s own scrollback from absolute line `from_line`, `rows` at
 * most. The answer is one or more LDMUX_EV_HISTORY. **The keeper has the history; the link never
 * sends it**, so this is the only way a screen the phone never saw can be read back. */
int32_t ldmux_client_history(LdmuxClient *c, const uint8_t *shell, size_t shell_len,
                             uint32_t from_line, uint8_t rows, uint64_t now_ms);

/* Ask the host to end `shell`, or to put it down when a desk window holds it, and forget the page. */
int32_t ldmux_client_end(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint64_t now_ms);

/* Ask for `shell`'s screen again, at the grid the phone draws with. **Spends no draft**: looking at
 * a shell can never run what is typed in it. */
int32_t ldmux_client_fetch(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint8_t cols,
                           uint8_t rows, uint64_t now_ms);

/* Ask which shells the host is running. The answer is one or more LDMUX_EV_SESSIONS. */
int32_t ldmux_client_list(LdmuxClient *c, uint64_t now_ms);

/* ---------------------------------------------------------------------------------------------
 * Reading back
 * ------------------------------------------------------------------------------------------- */

/* Read one page's screen into `*out`. Returns 1 when the shell is open, 0 when it is not. An open
 * page with no screen yet reports painted = 0. */
int32_t ldmux_client_mirror(LdmuxClient *c, const uint8_t *shell, size_t shell_len, LdmuxMirror *out);

/* Copy one row of a page's screen into `out`. Returns 1 when the row exists, 0 when it does not, or
 * LDMUX_ERR_BUFFER with the length it needed in *out_len. */
int32_t ldmux_client_mirror_line(LdmuxClient *c, const uint8_t *shell, size_t shell_len, uint32_t row,
                                 uint8_t *out, size_t cap, size_t *out_len);

/* One event, or 0 when there is none. For a kind that carries a message the wire bytes go to `text`;
 * a `cap` too small writes nothing, keeps the event and reports the length in *text_len. */
int32_t ldmux_client_poll_event(LdmuxClient *c, LdmuxClientEvent *out, uint8_t *text, size_t cap,
                                size_t *text_len);

/* The client's own counters. */
int32_t ldmux_client_stats(LdmuxClient *c, LdmuxClientStats *out);

/* ---------------------------------------------------------------------------------------------
 * The phone's own radio
 * ------------------------------------------------------------------------------------------- */

/* What the radio last said about itself. Read after answers have been handed back through
 * ldmux_client_on_ble_rx: the engine updates it from SELF_INFO, DEVICE_INFO, the radio's stats and
 * the battery answer, and from the readings on every receive-log push. */
int32_t ldmux_client_radio(LdmuxClient *c, LdmuxRadioStatus *out);

/* What this end believes about the path to the peer, as of `now_ms`. */
int32_t ldmux_client_route(LdmuxClient *c, uint64_t now_ms, LdmuxRoute *out);

/* Set the region key this radio's floods are scoped with, or clear it with key_len 0.
 * A scoped flood carries HMAC-SHA-256(key, payload_type || payload) cut to two bytes, so the key is
 * 16 bytes — the 16-bit code in the config is an HMAC *of* the key and is not it. */
int32_t ldmux_client_set_scope(LdmuxClient *c, const uint8_t *key, size_t key_len);

/**
 * Tune the radio: frequency (kHz), bandwidth (Hz), spreading factor, coding rate, and optionally a
 * path-hash width (1..3) and a transmit power in dBm.
 *
 * `path_width` 0 and `power_dbm` LDMUX_UNSET_POWER (-128) mean "leave what the radio has". The
 * writes go through the radio's one command window, so a preset change cannot overlap a raw send;
 * the firmware's answer arrives on the next ldmux_client_on_ble_rx like any other.
 *
 * Returns LDMUX_OK, or LDMUX_ERR_NULL.
 */
int32_t ldmux_client_set_preset(LdmuxClient *c, uint32_t freq_khz, uint32_t bw_hz, uint8_t sf,
                                uint8_t cr, uint8_t path_width, int8_t power_dbm);

#ifdef __cplusplus
}
#endif

#endif /* LDMUX_CLIENT_H */
