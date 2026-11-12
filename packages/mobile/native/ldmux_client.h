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
} LdmuxClientStats;

/* ---------------------------------------------------------------------------------------------
 * Life cycle
 * ------------------------------------------------------------------------------------------- */

/* Create a client from `config` and write the handle to `*out`.
 * Returns LDMUX_ERR_NULL for a null argument, LDMUX_ERR_ARG for a path width outside 1..=3, or
 * LDMUX_ERR_PANIC for a panic inside. */
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

/* Hand one BLE notification to the engine: the whole companion frame, byte for byte.
 * Returns 1 when it carried a session record offered to the link, 0 when it carried nothing of ours
 * (an advert, a route request, a stranger's channel traffic), or a negative error. */
int32_t ldmux_client_on_ble_rx(LdmuxClient *c, const uint8_t *frame, size_t len, uint64_t now_ms);

/* Write **at most one** frame to `out`, and its length to `*out_len`.
 * Returns 1 when a frame was written, 0 when there is nothing to send, or a negative error; a `cap`
 * too small writes nothing, keeps the frame and reports the length it needed in *out_len. */
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

/* Send one item to `shell` as a single command, and write its operation id to `*op_id` when `op_id`
 * is not null. The answer is one LDMUX_EV_SETTLED carrying what the host did with it.
 *
 * `kind` is LDMUX_ITEM_TEXT for text typed at a prompt, or LDMUX_ITEM_PASTE for anything that must
 * not run as it arrives (a multi-line paste, and a key's own escape sequence). */
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

#ifdef __cplusplus
}
#endif

#endif /* LDMUX_CLIENT_H */
