//! The C ABI over `ldmux_session::client`: the phone's whole radio route, behind a handle.
//!
//! Header: `packages/mobile/native/ldmux_client.h`, which is the contract; read it for ownership
//! rules. This module adds no protocol of its own — it names [`Client`]'s operations in C, copies
//! what the caller asked for into buffers the caller owns, and nothing else.
//!
//! **Pull only.** The engine never calls into the caller. Kotlin hands bytes in
//! (`ldmux_client_on_ble_rx`), drains frames out (`ldmux_client_poll`), drains what happened
//! (`ldmux_client_poll_event`), and sets one timer from `ldmux_client_next_deadline`. There are no
//! upcalls, so no `AttachCurrentThread` and no thread of our own (`docs/RADIO-PLAN.md` §8.1).
//!
//! **One handle, one caller.** The handles are not thread safe; the caller serialises calls on one
//! handle, exactly as the session ABI in `lib.rs` requires.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ldmux_mesh::error::MeshError;
use ldmux_protocol::mesh_term::{self, Message};
use ldmux_session::client::{Client, ClientConfig, ClientError, ClientEvent, FrameOutcome, PageMirror};
use ldmux_session::handshake::Keypair;

use crate::{
    LDMUX_ERR_ARG, LDMUX_ERR_BUFFER, LDMUX_ERR_NULL, LDMUX_ERR_NOT_OPEN, LDMUX_OK, guard,
};

/// One page's screen, as the caller reads it.
///
/// The rows are fetched one at a time with [`ldmux_client_mirror_line`], because a row is variable
/// length and a C struct cannot own it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LdmuxMirror {
    /// Whether there is a screen to draw at all.
    pub painted: u8,
    /// The grid the host painted, in cells.
    pub cols: u8,
    /// Rows in the grid, which is how many rows [`ldmux_client_mirror_line`] answers for.
    pub rows: u8,
    /// The cursor's column.
    pub cursor_x: u8,
    /// The cursor's row.
    pub cursor_y: u8,
    /// A full-screen program owns the pane.
    pub alternate: u8,
    /// The shell's process exited.
    pub dead: u8,
    /// The pane had stopped changing when this screen was captured; clear means a mid-state.
    pub settled: u8,
    /// `1` when `applied_status` is meaningful for this screen.
    pub has_applied: u8,
    /// What the host did with the command this screen answers: `mesh_term::applied`.
    pub applied_status: u8,
    /// The identity of this screen; a `base_id` diff names the screen it was cut against.
    pub snapshot_id: u32,
    /// The absolute line number of row 0.
    pub first_line: u32,
}

/// The zero event: what `ldmux_client_poll_event` reports when it has nothing, and the value every
/// field starts at.
impl Default for LdmuxClientEvent {
    fn default() -> Self {
        Self {
            kind: 0,
            shell_len: 0,
            shell: [0; 64],
            value: 0,
            snapshot_id: 0,
            has_applied: 0,
            applied_status: 0,
            settled: 0,
            cols: 0,
            rows: 0,
            text_len: 0,
        }
    }
}

/// One thing the app has to draw or say.
///
/// `kind` says which fields mean anything, and whether the text buffer carries bytes:
///
/// | kind | name | fields | text |
/// |---|---|---|---|
/// | 1 | `ATTACHED` | `cols`, `rows` | — |
/// | 2 | `SETTLED` | `snapshot_id`, `settled`, `has_applied`, `applied_status` | — |
/// | 3 | `HELD` | — | — |
/// | 4 | `CLOSED` | `value` = the reason (`mesh_term::closed`) | a `Closed` message |
/// | 5 | `SESSIONS` | `value` = 1 when it is the last page | a `Sessions` message |
/// | 6 | `MARKS` | `value` = 1 when it is the last page | a `Marks` message |
/// | 7 | `HISTORY` | `value` = 1 when it is the last page | a `HistoryPage` message |
///
/// The four message kinds are the wire bytes of the message the host sent, so the caller decodes
/// them with the decoder it already has. A second encoding for one fact would be a second thing to
/// get wrong.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LdmuxClientEvent {
    /// Which event this is; see the table above.
    pub kind: u8,
    /// The shell, when the event names one.
    pub shell_len: u8,
    /// The shell's id, `shell_len` bytes of it.
    pub shell: [u8; 64],
    /// A number whose meaning depends on `kind`.
    pub value: u32,
    /// The screen's identity, for `SETTLED`.
    pub snapshot_id: u32,
    /// `1` when `applied_status` is meaningful, for `SETTLED`.
    pub has_applied: u8,
    /// What the host did with the command, for `SETTLED`: `mesh_term::applied`.
    pub applied_status: u8,
    /// Whether the pane had settled, for `SETTLED`.
    pub settled: u8,
    /// The grid, for `ATTACHED`.
    pub cols: u8,
    /// Rows, for `ATTACHED`.
    pub rows: u8,
    /// Bytes written to the caller's text buffer.
    pub text_len: u32,
}

/// What the client needs to start.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LdmuxClientConfig {
    /// The host radio's static public key: what an initiation is addressed to.
    pub peer_static: [u8; 32],
    /// The pairing's pre-shared key.
    pub psk: [u8; 32],
    /// The phone's own static private key.
    pub local_private: [u8; 32],
    /// The phone's own static public key.
    pub local_public: [u8; 32],
    /// Bytes per hop the mesh's paths use, 1..=3.
    pub path_width: u8,
    /// The spreading factor, for the SNR margin table (7..=12); 0 leaves the default.
    pub sf: u8,
    /// How many hops a flood is expected to reach, which prices the flood's path; 0 leaves the
    /// default.
    pub flood_reach_hops: u8,
    /// How many pages the operator may open; 0 leaves the default.
    pub max_open_pages: u8,
    /// One full record's airtime, in milliseconds, at the radio's own tuple.
    pub airtime_ms: f32,
    /// The flood scope's 16-bit transport code, or zero for unscoped.
    pub scope: u16,
}

/// The client's own counters, for a log that has to say what happened.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LdmuxClientStats {
    /// Records the engine handed to the radio.
    pub records_sent: u64,
    /// Records the radio handed up.
    pub records_received: u64,
    /// Commands submitted.
    pub commands_sent: u64,
    /// Refreshes sent.
    pub fetches_sent: u64,
    /// Screens applied to a page.
    pub snapshots_applied: u64,
    /// Screens refused because they named a base the mirror never held.
    pub snapshots_refused: u64,
    /// Handshakes accepted.
    pub sessions_established: u64,
    /// Retry rungs spent.
    pub rungs_used: u64,
    /// Bootstrap rungs sent while no path to the peer was held.
    pub blind_asks: u64,
    /// `1` when the mesh answered a blind ask with a path.
    pub route_answered: u8,
    /// Records the engine queued for the radio.
    pub radio_records: u64,
    /// Radio commands that were answered.
    pub radio_answered: u64,
    /// Radio commands whose deadline passed before their answer arrived.
    pub radio_expired: u64,
    /// Records this end admitted: the session layer took them.
    pub records_admitted: u64,
    /// Records dropped as this end's own transmission coming back.
    pub rejected_self_echo: u64,
    /// Records dropped because they were still in transit to another node.
    pub rejected_in_transit: u64,
    /// Answers dropped because the handshake refused them.
    pub rejected_handshake: u64,
    /// Downlinks that decrypted but did not decode as a message.
    pub unknown_downlink: u64,
    /// Lists this end asked for.
    pub lists_sent: u64,
    /// Records whose kind this end does not know.
    pub rejected_unknown_kind: u64,
    /// Records that decrypted to nothing usable.
    pub rejected_record: u64,
    /// The live session's index, or 0 when there is none. The responder assigns it and every record
    /// carries it, so two ends that disagree here cannot read each other at all.
    pub session_index: u8,
    /// Parts of messages handed to the radio, retransmissions included.
    pub parts_sent: u64,
    /// Standalone acknowledgements handed to the radio.
    pub acks_sent: u64,
    /// Commands waiting for the radio's one answer window.
    pub radio_backlog: u32,
    /// Frames the radio has ready for the caller to write.
    pub radio_ready: u32,
    /// Records abandoned after the window accepted them, which is otherwise invisible.
    pub radio_malformed: u64,
    /// Whether the radio's one answer window is held right now.
    pub radio_outstanding: u32,
    /// How many times the caller asked the radio to make progress.
    pub radio_pumps: u64,
    /// Answers that finished a session, repeated one, or were ignored as another initiation's.
    pub answers_finished: u64,
    pub answers_repeated: u64,
    pub answers_ignored: u64,
    /// Records that arrived twice, which the receive log makes routine and which are not failures.
    pub duplicate_records: u64,
    /// Records for another session index: the peer re-handshook and this end followed.
    pub wrong_index_records: u64,
    /// Acknowledgements the transaction layer staged, before anything tried to send them.
    pub acks_staged: u64,
    /// Transport records that arrived while this end held no session.
    pub records_without_a_session: u64,
    /// The kind and length of the last record this end admitted.
    pub last_admitted_kind: u8,
    pub last_admitted_len: u16,
    /// The last record refused by the record layer, as a stable code; 0 when none has been.
    pub last_record_refusal: u8,
    /// The index and counter that record carried, and the index this end holds.
    pub last_refusal_index: u8,
    pub last_refusal_counter: u32,
    pub local_index: u8,
    /// Records the radio refused to put on the air, against the ones it took.
    pub records_refused: u64,
    /// Every push the radio handed up, and the code of the last one.
    pub pushes: u64,
    pub last_push_code: u8,
    pub route_asks: u64,
    pub route_replies: u64,
}

/// What the phone's radio last said about itself.
///
/// Every reading is optional separately, because they arrive from different questions and a radio
/// that answered one may not answer the others: a firmware without `GET_STATS` still reports its
/// tuple, and a radio that has heard nothing reports no RSSI rather than a plausible number.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LdmuxRadioStatus {
    /// `1` once the radio answered `APP_START`, which is the only evidence this end has.
    pub connected: u8,
    /// `1` when `rssi_dbm` and `snr_quarter_db` mean anything.
    pub has_signal: u8,
    /// The RSSI of the last packet **this end** heard, in dBm.
    pub rssi_dbm: i8,
    /// That packet's SNR in quarters of a dB, which is the firmware's own scale.
    pub snr_quarter_db: i8,
    /// `1` when `battery_mv` was read.
    pub has_battery: u8,
    /// The board's battery, in millivolts.
    pub battery_mv: u16,
    /// The radio's own transmit airtime since it booted, in seconds.
    pub tx_air_secs: u32,
    /// And its receive airtime.
    pub rx_air_secs: u32,
    /// The caller's record budget, as last given to `ldmux_client_set_records_left`; 255 = unknown.
    pub ops_left: u8,
    /// `1` when `freq_khz`..`coding_rate` were read.
    pub has_preset: u8,
    /// The frequency the radio is tuned to, in kHz.
    pub freq_khz: u32,
    /// Its bandwidth, in Hz.
    pub bw_hz: u32,
    /// Its spreading factor.
    pub spreading_factor: u8,
    /// Its coding rate, as the firmware's denominator index (5 is 4/5).
    pub coding_rate: u8,
    /// Its transmit power, in dBm.
    pub tx_power_dbm: i8,
    /// The path hash width the firmware reported, in bytes (1..=3); 0 when it did not say.
    pub path_hash_width: u8,
    /// The firmware's own version code, or 0 when it was not read.
    pub firmware_version_code: u8,
    /// `1` when `name` and `max_tx_power_dbm` were read (`SELF_INFO`).
    ///
    /// **Separate from `has_device`, because they arrive from different questions.** A firmware
    /// that answers `SELF_INFO` and not `DEVICE_INFO` is a real radio, and a caller that draws
    /// "not read" for the fields it has is worse than one that draws nothing.
    pub has_identity: u8,
    /// How many bytes of `name` are meaningful; 0 means nothing was read.
    pub name_len: u8,
    /// The node's own name, UTF-8, `name_len` bytes meaningful; the rest is zero.
    ///
    /// **Length-prefixed rather than NUL-terminated**: the header promises no pointer into library
    /// memory and no guessing, and a terminator would be the one place that promise went fuzzy.
    pub name: [u8; 32],
    /// The radio's maximum transmit power in dBm, which is the regulatory ceiling beside the one
    /// in `tx_power_dbm`.
    pub max_tx_power_dbm: i8,
    /// `1` when `firmware`, `max_contacts` and `path_hash_width` were read (`DEVICE_INFO`).
    pub has_device: u8,
    /// How many bytes of `firmware` are meaningful; 0 means nothing was read.
    pub firmware_len: u8,
    /// The firmware's own version string, UTF-8, `firmware_len` bytes meaningful.
    pub firmware: [u8; 24],
    /// The contact capacity the firmware reports, or `u16::MAX` when it did not say.
    pub max_contacts: u16,
}

/// What this end believes about the path to the peer, for the status line and the history deadline.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LdmuxRoute {
    /// `1` when a fresh path is held, so the next attempt goes direct.
    pub known: u8,
    /// `1` when an authenticated direct arrival proved it. A flood's reversed chain never does.
    pub proven: u8,
    /// `1` when it is inside the freshness window.
    pub fresh: u8,
    /// `1` when the next transmission would flood, which is also what "no route" looks like.
    pub flood: u8,
    /// How many repeaters it crosses.
    pub hops: u8,
    /// Bytes per hop it is encoded with.
    pub path_width: u8,
    /// Its length in bytes.
    pub path_len: u8,
    /// The path itself, `path_len` bytes of it.
    pub path: [u8; 64],
    /// The SNR it was heard at, in quarters of a dB.
    pub snr_quarter_db: i16,
}

/// Event kinds, as `LdmuxClientEvent::kind`.
pub const LDMUX_EV_ATTACHED: u8 = 1;
/// A screen arrived for a page.
pub const LDMUX_EV_SETTLED: u8 = 2;
/// Another client holds the shell.
pub const LDMUX_EV_HELD: u8 = 3;
/// The host ended this client's view of the shell.
pub const LDMUX_EV_CLOSED: u8 = 4;
/// One page of the shell list.
pub const LDMUX_EV_SESSIONS: u8 = 5;
/// One page of the marks the host holds.
pub const LDMUX_EV_MARKS: u8 = 6;
/// One page of scrollback.
pub const LDMUX_EV_HISTORY: u8 = 7;
/// The host made a shell this client asked for. `text` carries its id as UTF-8 — **not** a wire
/// message, because a `Create`'s answer is a name, not something the app draws.
pub const LDMUX_EV_CREATED: u8 = 8;

/// The most bytes a shell id may be, which is `mesh_term::MAX_SELECTOR`.
pub const LDMUX_SHELL_MAX: usize = 40;

/// A `send` item that is written as it is.
pub const LDMUX_ITEM_TEXT: u8 = 0;
/// The same bytes, marked as a paste so the host wraps them for the pane's bracketed-paste mode.
pub const LDMUX_ITEM_PASTE: u8 = 1;
/// Key names, space separated — the same names `mesh_term`'s `Key` message takes (`Up`, `S-Tab`,
/// `C-c`). The engine encodes them for the pane's live cursor-key and Kitty mode, which only the
/// host can read, so the phone sends names rather than escape sequences it would have to guess.
pub const LDMUX_ITEM_KEYS: u8 = 2;
/// A whole command: an **item stream**, `[kind u8][len u16 LE][bytes]...` for text and pastes and
/// the `Keys` form for names — the same bytes `mesh_term`'s `Send` carries, so the phone builds an
/// item stream once and the library folds it into one message.
///
/// Use this whenever a command has more than one item. Sending a draft's text and its `Enter` as
/// two `LDMUX_ITEM_*` calls is two messages, which is `RC-8` (`docs/RADIO-PLAN.md` §12).
pub const LDMUX_ITEM_STREAM: u8 = 3;

/// A phone's radio route, from connect to screen.
pub struct LdmuxClient {
    /// The whole engine: the link, the radio window and the frames it owes.
    client: Client,
    /// Events taken from the engine and not yet handed to the caller.
    pending: VecDeque<Ready>,
}

/// One event, already encoded, so a caller whose buffer was too small loses nothing.
struct Ready {
    event: ClientEvent,
    value: u32,
    text: Vec<u8>,
}

/// Run a future that never really waits.
///
/// The engine's own carrier resolves on its first poll — it composes a packet and queues a frame —
/// so `Pending` means a bug rather than a wait. There is deliberately no executor: the engine is
/// synchronous state, and an executor would be a runtime the phone's library does not have.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the phone's carrier never waits, so this is a bug"),
    }
}

fn client_err(e: &ClientError) -> i32 {
    use ldmux_session::transaction::BeginError;
    match e {
        ClientError::TooManyPages { .. } => crate::LDMUX_ERR_TOO_MANY_PAGES,
        ClientError::AlreadyOpen(_) => crate::LDMUX_ERR_ALREADY_OPEN,
        ClientError::NoPage(_) => crate::LDMUX_ERR_NO_PAGE,
        ClientError::Encode(_) => crate::LDMUX_ERR_DECODE,
        ClientError::Begin(BeginError::TooLarge) => crate::LDMUX_ERR_TOO_LONG,
        ClientError::Begin(_) => crate::LDMUX_ERR_QUEUE_FULL,
    }
}

/// A caller-supplied shell id, refused unless it is a non-empty UTF-8 string of at most
/// [`LDMUX_SHELL_MAX`] bytes.
///
/// SAFETY (callers): `p` points to `len` readable bytes.
unsafe fn shell<'a>(p: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 || len > LDMUX_SHELL_MAX || p.is_null() {
        return None;
    }
    // SAFETY: non-null, and the caller promised `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(p, len) };
    std::str::from_utf8(bytes).ok()
}

/// Copy `src` into the caller's buffer, reporting the length needed even when it does not fit.
fn put(src: &[u8], out: *mut u8, cap: usize, out_len: *mut usize) -> i32 {
    if out_len.is_null() || (out.is_null() && cap != 0) {
        return LDMUX_ERR_NULL;
    }
    // SAFETY: out_len is non-null and the caller promised it is writable.
    unsafe { *out_len = src.len() };
    if src.len() > cap {
        return LDMUX_ERR_BUFFER;
    }
    if !src.is_empty() {
        // SAFETY: `out` is non-null with `cap >= src.len()` writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), out, src.len()) };
    }
    LDMUX_OK
}

/// Call `f` on the handle behind `c`, containing a panic.
unsafe fn with_client(c: *mut LdmuxClient, f: impl FnOnce(&mut LdmuxClient) -> i32) -> i32 {
    guard(|| {
        if c.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: non-null, from `ldmux_client_new`, not freed (free nulls the caller's slot), and
        // the caller serialises calls on one handle (header).
        f(unsafe { &mut *c })
    })
}

/// Turn one `ClientEvent` into the value the caller reads, and the bytes it carries.
fn encode_event(event: &ClientEvent) -> Ready {
    let mut ready = Ready {
        event: event.clone(),
        value: 0,
        text: Vec::new(),
    };
    let message = match event {
        ClientEvent::Closed { reason } => {
            ready.value = u32::from(*reason);
            Some(Message::Closed { reason: *reason })
        }
        ClientEvent::Sessions { last, entries } => {
            ready.value = u32::from(*last);
            Some(Message::Sessions {
                last: *last,
                // Every entry carries all of it: the decoder filled these in from the wire, and a
                // re-encode that dropped a field would tell the app less than the host did.
                cwd: true,
                origin: true,
                owner: true,
                shell: true,
                entries: entries.clone(),
            })
        }
        ClientEvent::Marks {
            first,
            last,
            entries,
        } => {
            ready.value = u32::from(*last);
            Some(Message::Marks {
                first: *first,
                last: *last,
                entries: entries.clone(),
            })
        }
        ClientEvent::History { from, last, older, lines } => {
            ready.value = u32::from(*last);
            Some(Message::HistoryPage {
                from: *from,
                last: *last,
                older: *older,
                lines: lines.clone(),
            })
        }
        ClientEvent::Created { id } => {
            // The id is the text; there is no message to encode, because the host's `Created` is a
            // name rather than something the app draws.
            ready.text = id.as_bytes().to_vec();
            None
        }
        ClientEvent::Attached { .. } | ClientEvent::Settled { .. } | ClientEvent::Held { .. } => None,
    };
    if let Some(message) = message {
        // A message that will not encode cannot happen for one this crate decoded, and losing the
        // event silently would be worse than dropping the bytes: the struct fields still say what
        // happened.
        ready.text = message.encode().unwrap_or_default();
    }
    ready
}

/// Fill the caller's event struct from one event and its encoded text.
fn fill(ready: &Ready, text_len: u32) -> LdmuxClientEvent {
    let mut out = LdmuxClientEvent {
        value: ready.value,
        text_len,
        ..LdmuxClientEvent::default()
    };
    let set_shell = |out: &mut LdmuxClientEvent, shell: &str| {
        let bytes = shell.as_bytes();
        let n = bytes.len().min(out.shell.len());
        out.shell[..n].copy_from_slice(&bytes[..n]);
        out.shell_len = u8::try_from(n).unwrap_or(0);
    };
    match &ready.event {
        ClientEvent::Attached { shell, cols, rows } => {
            out.kind = LDMUX_EV_ATTACHED;
            out.cols = *cols;
            out.rows = *rows;
            set_shell(&mut out, shell);
        }
        ClientEvent::Settled {
            shell,
            snapshot_id,
            applied,
            settled,
        } => {
            out.kind = LDMUX_EV_SETTLED;
            out.snapshot_id = *snapshot_id;
            out.settled = u8::from(*settled);
            if let Some(applied) = applied {
                out.has_applied = 1;
                out.applied_status = applied.status;
            }
            set_shell(&mut out, shell);
        }
        ClientEvent::Held { shell } => {
            out.kind = LDMUX_EV_HELD;
            set_shell(&mut out, shell);
        }
        ClientEvent::Closed { .. } => out.kind = LDMUX_EV_CLOSED,
        ClientEvent::Sessions { .. } => out.kind = LDMUX_EV_SESSIONS,
        ClientEvent::Marks { .. } => out.kind = LDMUX_EV_MARKS,
        ClientEvent::History { .. } => out.kind = LDMUX_EV_HISTORY,
        ClientEvent::Created { id } => {
            out.kind = LDMUX_EV_CREATED;
            // A created id is a name, so it goes in the text buffer; a caller that asked for one
            // reads it there. It is not a message and nothing decodes it.
            out.text_len = u32::try_from(id.len()).unwrap_or(0);
        }
    }
    out
}

/// Fill `priv_out` and `pub_out` with a fresh device keypair (32 bytes each).
///
/// The one call the app needs outside a client handle: a pairing starts before there is a session
/// to put it in. It lives here rather than in a second library so the app links one `.so` and the
/// suite has one implementation (`docs/RADIO-PLAN.md` §10a).
///
/// # Errors
///
/// [`LDMUX_ERR_NULL`] for a null pointer, [`LDMUX_ERR_PANIC`] for a panic inside.
///
/// # Safety
///
/// `priv_out` and `pub_out` must each be valid for writes of 32 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_keypair(priv_out: *mut u8, pub_out: *mut u8) -> i32 {
    guard(|| {
        if priv_out.is_null() || pub_out.is_null() {
            return LDMUX_ERR_NULL;
        }
        let Ok(pair) = Keypair::generate() else {
            return LDMUX_ERR_ARG;
        };
        // SAFETY: both pointers are non-null (checked above) and the caller guarantees room for the
        // 32 bytes `as_bytes` writes.
        unsafe {
            std::ptr::copy_nonoverlapping(pair.private.as_ptr(), priv_out, 32);
            std::ptr::copy_nonoverlapping(pair.public.as_ptr(), pub_out, 32);
        }
        LDMUX_OK
    })
}

/// Build a client from `config` and write the handle to `out`.
///
/// # Errors
///
/// [`LDMUX_ERR_NULL`] for a null argument, [`LDMUX_ERR_ARG`] for key material that is not a usable
/// point (an all-zero scalar, say), [`LDMUX_ERR_PANIC`] for a panic inside.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_new(
    config: *const LdmuxClientConfig,
    out: *mut *mut LdmuxClient,
) -> i32 {
    guard(|| {
        if config.is_null() || out.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: non-null, and the header promises one readable config.
        let cfg = unsafe { *config };
        if cfg.path_width == 0 || cfg.path_width > 3 {
            return LDMUX_ERR_ARG;
        }
        let local = Keypair::from_parts(cfg.local_private, cfg.local_public);
        let mut config = ClientConfig::new(cfg.peer_static, cfg.psk)
            .with_path_width(cfg.path_width)
            .with_airtime_ms(f64::from(cfg.airtime_ms));
        if (7..=12).contains(&cfg.sf) {
            config = config.with_sf(cfg.sf);
        }
        if cfg.flood_reach_hops > 0 {
            config = config.with_flood_reach_hops(cfg.flood_reach_hops);
        }
        if cfg.max_open_pages > 0 {
            config = config.with_max_open_pages(usize::from(cfg.max_open_pages));
        }
        if cfg.scope != 0 {
            config = config.with_scope(Some(cfg.scope));
        }
        let handle = Box::new(LdmuxClient {
            client: Client::new(config, local),
            pending: VecDeque::new(),
        });
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = Box::into_raw(handle) };
        LDMUX_OK
    })
}

/// Destroy a client and null the caller's slot. A second destroy is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_free(c: *mut *mut LdmuxClient) {
    let _ = guard(|| {
        if c.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: `c` points at a handle slot the caller owns.
        let raw = unsafe { *c };
        if !raw.is_null() {
            // SAFETY: non-null, from `ldmux_client_new`, and nulled below so it is freed once.
            drop(unsafe { Box::from_raw(raw) });
            // SAFETY: the slot is writable.
            unsafe { *c = std::ptr::null_mut() };
        }
        LDMUX_OK
    });
}

/// Start an attempt at the host. `wall_ms` is the phone's wall clock, which the handshake stamps.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_connect(c: *mut LdmuxClient, now_ms: u64, wall_ms: u64) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| match h.client.connect(now_ms, wall_ms) {
            Ok(()) => LDMUX_OK,
            Err(e) => crate::hs_err(e),
        })
    }
}

/// Whether an L3 session is established.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_established(c: *mut LdmuxClient) -> i32 {
    // SAFETY: see `with_client`.
    unsafe { with_client(c, |h| i32::from(h.client.established())) }
}

/// Tell the engine how many records the radio says are left this hour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_set_records_left(c: *mut LdmuxClient, left: u32) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            h.client.set_records_left(u8::try_from(left).unwrap_or(u8::MAX));
            LDMUX_OK
        })
    }
}

/// Hand one BLE notification to the engine.
///
/// Returns `1` when the frame carried a session record that was offered to the link, `0` when it
/// carried nothing of ours (an advert, a route request, a stranger's channel traffic), or a
/// negative error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_on_ble_rx(
    c: *mut LdmuxClient,
    frame: *const u8,
    len: usize,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; the caller promised `len` readable bytes at `frame`.
    unsafe {
        with_client(c, |h| {
            if len == 0 || frame.is_null() {
                return LDMUX_ERR_ARG;
            }
            // SAFETY: non-null with `len` readable bytes.
            let bytes = unsafe { std::slice::from_raw_parts(frame, len) };
            // **The engine decides what a payload is**, not this shim: a receive-log push, an answer
            // to a command, or a push about somebody else's traffic. Splitting that decision here
            // is what gave the caller a protocol to reimplement.
            match h.client.on_frame(bytes, now_ms, now_ms) {
                FrameOutcome::Record => 1,
                FrameOutcome::Ignored
                | FrameOutcome::Answered(_)
                | FrameOutcome::Unexpected
                | FrameOutcome::Discarded => 0,
            }
        })
    }
}

/// Write **at most one** frame to `out` and its length to `out_len`.
///
/// Returns `1` when a frame was written, `0` when there is nothing to send, or a negative error. A
/// `cap` too small writes nothing, keeps the frame for the next call, and reports the length it
/// needed in `out_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_poll(
    c: *mut LdmuxClient,
    now_ms: u64,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out_len.is_null() {
                return LDMUX_ERR_NULL;
            }
            if let Err(e) = block_on(h.client.poll(now_ms)) {
                return match e {
                    MeshError::Closed => LDMUX_ERR_NOT_OPEN,
                    _ => LDMUX_ERR_ARG,
                };
            }
            let Some(frame) = h.client.take_frame() else {
                // SAFETY: out_len is non-null.
                unsafe { *out_len = 0 };
                return 0;
            };
            let rc = put(&frame, out, cap, out_len);
            if rc == LDMUX_OK {
                return 1;
            }
            // Too small a buffer keeps the frame: dropping it here would lose a record. The engine
            // handed it over once, so the caller must ask again for the same bytes.
            h.client.put_back(frame);
            rc
        })
    }
}

/// When the caller should look again even if nothing arrives: `1` and `*out` set, or `0`.
///
/// `now_ms` is needed because a **queued command is a deadline of its own**: a record waiting for the
/// radio's one answer window has no instant of its own and would otherwise be looked at again only
/// when an unrelated status poll or bootstrap rung came due.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_next_deadline(
    c: *mut LdmuxClient,
    now_ms: u64,
    out: *mut u64,
) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            match h.client.next_deadline_ms(now_ms) {
                Some(deadline) => {
                    // SAFETY: `out` is non-null and writable.
                    unsafe { *out = deadline };
                    1
                }
                None => 0,
            }
        })
    }
}

/// Open a page for `shell`. The answer is a `Snapshot`, which arrives as `SETTLED`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_open(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    cols: u8,
    rows: u8,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.open(id, cols, rows, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Let go of `shell`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_close(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.close(id) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Send one command to `shell`. The op id is written to `op_id` when it is not null.
///
/// `kind` is [`LDMUX_ITEM_TEXT`], [`LDMUX_ITEM_PASTE`], [`LDMUX_ITEM_KEYS`] or
/// [`LDMUX_ITEM_STREAM`]. **`LDMUX_ITEM_STREAM` is what a composed draft uses**: the buffer is an
/// item stream (`[kind u8][len u16 LE][bytes]...`, and the `Keys` form for names), decoded by
/// `mesh_term::decode_item_stream` — the same function the wire uses — and sent as **one** message.
/// Anything else would put a command's text and its `Enter` in two messages, which is `RC-8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_send(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    kind: u8,
    items: *const u8,
    items_len: usize,
    cols: u8,
    rows: u8,
    now_ms: u64,
    op_id: *mut u32,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` and the slice below check their arguments.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            if items_len == 0 || items.is_null() {
                return LDMUX_ERR_ARG;
            }
            // SAFETY: non-null with `items_len` readable bytes.
            let bytes = unsafe { std::slice::from_raw_parts(items, items_len) }.to_vec();
            let item = match kind {
                LDMUX_ITEM_TEXT => mesh_term::SendItem::Text(bytes),
                LDMUX_ITEM_PASTE => mesh_term::SendItem::Paste(bytes),
                LDMUX_ITEM_KEYS => {
                    // The `Key` message's own convention: names, single-space separated. Splitting
                    // here rather than in Kotlin keeps one spelling of a key name in the tree.
                    let Ok(text) = String::from_utf8(bytes) else {
                        return LDMUX_ERR_ARG;
                    };
                    let names: Vec<String> =
                        text.split(' ').filter(|n| !n.is_empty()).map(str::to_owned).collect();
                    if names.is_empty() {
                        return LDMUX_ERR_ARG;
                    }
                    mesh_term::SendItem::Keys(names)
                }
                LDMUX_ITEM_STREAM => {
                    // The same decoder the wire uses, so an item means one thing on both sides.
                    match mesh_term::decode_item_stream(&bytes) {
                        Ok(items) => {
                            return match h.client.send(id, &items, cols, rows, now_ms) {
                                Ok(id) => {
                                    if !op_id.is_null() {
                                        // SAFETY: the caller said `op_id` is a writable `u32`.
                                        unsafe { *op_id = id };
                                    }
                                    LDMUX_OK
                                }
                                Err(e) => client_err(&e),
                            };
                        }
                        Err(_) => return LDMUX_ERR_ARG,
                    }
                }
                _ => return LDMUX_ERR_ARG,
            };
            match h.client.send(id, &[item], cols, rows, now_ms) {
                Ok(id) => {
                    if !op_id.is_null() {
                        // SAFETY: the caller said `op_id`, when non-null, is a writable `u32`.
                        unsafe { *op_id = id };
                    }
                    LDMUX_OK
                }
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask for `shell`'s screen again, at the grid the phone draws with. **Spends no draft.**
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_fetch(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    cols: u8,
    rows: u8,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.fetch(id, cols, rows, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask the host to start a shell in `cwd` (empty means its default directory).
///
/// The answer is a `CREATED` event whose text is the id to pass to [`ldmux_client_open`]. The phone
/// attaches to the name it is given rather than guessing one out of a list another client may have
/// changed in between.
///
/// # Errors
///
/// [`LDMUX_ERR_NULL`], [`LDMUX_ERR_ARG`] for a directory that is not UTF-8 or too long to say, or
/// the transaction layer's refusal.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_create(
    c: *mut LdmuxClient,
    cwd: *const u8,
    cwd_len: usize,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; the bytes are checked below.
    unsafe {
        with_client(c, |h| {
            let cwd = if cwd.is_null() || cwd_len == 0 {
                String::new()
            } else {
                // SAFETY: non-null with `cwd_len` readable bytes.
                match String::from_utf8(unsafe { std::slice::from_raw_parts(cwd, cwd_len) }.to_vec())
                {
                    Ok(text) => text,
                    Err(_) => return LDMUX_ERR_ARG,
                }
            };
            match h.client.create(&cwd, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Take `shell` from whoever holds it, displacing them. A plain open is refused while a desk window
/// holds a shell, so taking one is always something the operator chose.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_takeover(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.takeover(id, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask the host to open `shell` in a desk window: **send to desktop**.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_make_live(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.make_live(id, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask the host for a page of `shell`'s own scrollback from absolute line `from`, `rows` at most.
///
/// The answer is one or more `HISTORY` events. **The keeper has the history; the link never sends
/// it**, so this is the only way a screen the phone never saw can be read back.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_history(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    from_line: u32,
    rows: u8,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.want_history(id, from_line, rows, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask the host to end `shell`, or to put it down when a desk window holds it, and forget the page.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_end(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    now_ms: u64,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            match h.client.end(id, now_ms) {
                Ok(()) => LDMUX_OK,
                Err(e) => client_err(&e),
            }
        })
    }
}

/// Ask which shells the host is running. The answer is one or more `SESSIONS` events.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_list(c: *mut LdmuxClient, now_ms: u64) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| match h.client.list(now_ms) {
            Ok(()) => LDMUX_OK,
            Err(e) => client_err(&e),
        })
    }
}

/// Read one page's screen into `out`.
///
/// Returns `1` when the shell is open, `0` when it is not, or a negative error. A page that is open
/// but has no screen yet reports `painted = 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_mirror(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    out: *mut LdmuxMirror,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            let Some(mirror) = h.client.mirror(id) else {
                return 0;
            };
            // SAFETY: `out` is non-null and writable; the struct is plain data.
            unsafe { *out = mirror_of(mirror) };
            1
        })
    }
}

/// Copy one row of a page's screen into `out`, and its length to `out_len`.
///
/// Returns `1` when the row exists, `0` when it does not, or [`LDMUX_ERR_BUFFER`] with the length it
/// needed in `out_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_mirror_line(
    c: *mut LdmuxClient,
    shell_bytes: *const u8,
    shell_len: usize,
    row: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: see `with_client`; `shell()` checks the pointer and length.
    unsafe {
        with_client(c, |h| {
            if out_len.is_null() {
                return LDMUX_ERR_NULL;
            }
            let Some(id) = shell(shell_bytes, shell_len) else {
                return LDMUX_ERR_ARG;
            };
            let Some(mirror) = h.client.mirror(id) else {
                return 0;
            };
            let Some(line) = mirror.lines.get(usize::try_from(row).unwrap_or(usize::MAX)) else {
                // SAFETY: out_len is non-null.
                unsafe { *out_len = 0 };
                return 0;
            };
            match put(line.as_bytes(), out, cap, out_len) {
                LDMUX_OK => 1,
                other => other,
            }
        })
    }
}

/// One event, or `0` when there is none.
///
/// For a kind that carries a message (`CLOSED`, `SESSIONS`, `MARKS`, `HISTORY`) the wire bytes go to
/// `text`; a `cap` too small writes nothing, keeps the event for the next call, and reports the
/// length it needed in `text_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_poll_event(
    c: *mut LdmuxClient,
    out: *mut LdmuxClientEvent,
    text: *mut u8,
    cap: usize,
    text_len: *mut usize,
) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out.is_null() || text_len.is_null() {
                return LDMUX_ERR_NULL;
            }
            if h.pending.is_empty() {
                for event in h.client.take_events() {
                    h.pending.push_back(encode_event(&event));
                }
            }
            let Some(ready) = h.pending.front() else {
                // SAFETY: text_len is non-null.
                unsafe { *text_len = 0 };
                return 0;
            };
            let rc = put(&ready.text, text, cap, text_len);
            if rc != LDMUX_OK {
                // Too small a buffer keeps the event: a fact the app has not drawn is not ours to
                // drop.
                return rc;
            }
            // SAFETY: `out` is non-null and writable; `text_len` was set by `put`.
            let written = unsafe { *text_len };
            let filled = fill(ready, u32::try_from(written).unwrap_or(u32::MAX));
            // SAFETY: `out` is non-null and writable.
            unsafe { *out = filled };
            h.pending.pop_front();
            1
        })
    }
}

/// The client's own counters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_stats(c: *mut LdmuxClient, out: *mut LdmuxClientStats) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let stats = h.client.stats();
            let link = h.client.link_stats();
            let radio = h.client.radio().counters();
            let backlog = h.client.radio().backlog();
            let filled = LdmuxClientStats {
                records_sent: link.records_sent,
                records_received: link.records_received,
                commands_sent: stats.commands_sent,
                fetches_sent: stats.fetches_sent,
                snapshots_applied: stats.snapshots_applied,
                snapshots_refused: stats.snapshots_refused,
                sessions_established: link.sessions_established,
                rungs_used: link.rungs_used,
                blind_asks: stats.blind_asks,
                route_answered: u8::from(stats.route_answered),
                radio_records: radio.records,
                radio_answered: radio.answered,
                radio_expired: radio.expired,
                records_admitted: link.records_admitted,
                rejected_self_echo: link.rejected_self_echo,
                rejected_in_transit: link.rejected_in_transit,
                rejected_handshake: link.rejected_handshake,
                unknown_downlink: stats.unknown_downlink,
                lists_sent: stats.lists_sent,
                rejected_unknown_kind: link.rejected_unknown_kind,
                rejected_record: link.rejected_record,
                session_index: h.client.session_index().unwrap_or(0),
                parts_sent: stats.parts_sent,
                acks_sent: stats.acks_sent,
                radio_backlog: u32::try_from(backlog.0).unwrap_or(u32::MAX),
                radio_ready: u32::try_from(backlog.1).unwrap_or(u32::MAX),
                radio_malformed: h.client.radio().malformed(),
                radio_outstanding: u32::try_from(h.client.radio().outstanding()).unwrap_or(u32::MAX),
                radio_pumps: radio.pumps,
                answers_finished: link.answers_finished,
                answers_repeated: link.answers_repeated,
                answers_ignored: link.answers_ignored,
                duplicate_records: link.duplicate_records,
                wrong_index_records: link.wrong_index_records,
                acks_staged: link.acks_staged,
                records_without_a_session: link.records_without_a_session,
                last_admitted_kind: link.last_admitted_kind,
                last_admitted_len: link.last_admitted_len,
                last_record_refusal: link.last_record_refusal,
                last_refusal_index: link.last_refusal_index,
                last_refusal_counter: link.last_refusal_counter,
                local_index: link.local_index,
                records_refused: radio.sends_refused,
                pushes: stats.pushes,
                last_push_code: stats.last_push_code,
                route_asks: radio.route_asks,
                route_replies: radio.route_replies,
            };
            // SAFETY: `out` is non-null and writable; the struct is plain data.
            unsafe { *out = filled };
            LDMUX_OK
        })
    }
}

impl Default for LdmuxRoute {
    fn default() -> Self {
        Self {
            known: 0,
            proven: 0,
            fresh: 0,
            flood: 1,
            hops: 0,
            path_width: 1,
            path_len: 0,
            path: [0; 64],
            snr_quarter_db: 0,
        }
    }
}

/// What the phone's radio last said about itself.
///
/// Read after `ldmux_client_poll` has handed answers back through `ldmux_client_on_ble_rx`: the
/// engine updates it from `SELF_INFO`, `DEVICE_INFO`, the radio's stats and the battery answer, and
/// from the readings on every receive-log push.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_radio(c: *mut LdmuxClient, out: *mut LdmuxRadioStatus) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let status = h.client.radio().status();
            let (tx_air_secs, rx_air_secs) = status.airtime_secs();
            let mut filled = LdmuxRadioStatus {
                connected: u8::from(status.connected),
                ops_left: status.ops_left.unwrap_or(u8::MAX),
                tx_air_secs,
                rx_air_secs,
                ..LdmuxRadioStatus::default()
            };
            if let Some(rssi) = status.last_rssi_dbm {
                filled.has_signal = 1;
                filled.rssi_dbm = rssi;
                filled.snr_quarter_db = status.last_snr_quarter_db.unwrap_or(0);
            }
            if let Some(battery) = status.battery {
                filled.has_battery = 1;
                filled.battery_mv = battery.millivolts;
            }
            if let Some((freq_khz, bw_hz, sf, cr)) = status.preset() {
                filled.has_preset = 1;
                filled.freq_khz = freq_khz;
                filled.bw_hz = bw_hz;
                filled.spreading_factor = sf;
                filled.coding_rate = cr;
                filled.tx_power_dbm = status
                    .self_info
                    .as_ref()
                    .map_or(0, |info| info.tx_power as i8);
            }
            filled.path_hash_width = status.path_hash_width().unwrap_or(0);
            filled.max_contacts = u16::MAX;
            if let Some(info) = status.self_info.as_ref() {
                filled.has_identity = 1;
                filled.name_len = copy_text(&mut filled.name, &info.name);
                filled.max_tx_power_dbm = info.max_tx_power as i8;
            }
            if let Some(info) = status.device_info.as_ref() {
                filled.has_device = 1;
                filled.firmware_version_code = info.firmware_version_code;
                filled.firmware_len = copy_text(&mut filled.firmware, &info.firmware_version);
                filled.max_contacts = info.max_contacts.unwrap_or(u16::MAX);
            }
            // SAFETY: `out` is non-null and writable; the struct is plain data.
            unsafe { *out = filled };
            LDMUX_OK
        })
    }
}

/// What this end believes about the path to the peer, as of `now_ms`.
///
/// `hops` is the reason this exists: `answerDeadlineMs(HISTORY_WAIT_MS, hops)` sizes a scrollback
/// wait against the route, so a flat deadline on a twenty-hop route is the up-key losing a page that
/// was still arriving.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_route(
    c: *mut LdmuxClient,
    now_ms: u64,
    out: *mut LdmuxRoute,
) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let route = h.client.route();
            let sf = 0; // `best` reads the margin table; zero keeps every copy eligible.
            let mut filled = LdmuxRoute::default();
            if let Some(best) = route.best(now_ms, sf) {
                filled.known = 1;
                filled.proven = u8::from(best.is_proven());
                filled.fresh = u8::from(best.is_fresh(now_ms));
                filled.hops = best.hops();
                filled.path_width = best.path_width();
                let path = best.path();
                let n = path.len().min(filled.path.len());
                filled.path[..n].copy_from_slice(&path[..n]);
                filled.path_len = u8::try_from(n).unwrap_or(0);
                #[allow(clippy::cast_possible_truncation)]
                let quarter = (best.snr_db() * 4.0).round() as i16;
                filled.snr_quarter_db = quarter;
            }
            // The carrier the *first* transmission of the next part would take, which is the one a
            // status line has to name: a later attempt always floods, so asking about it would say
            // "flood" for every link that ever missed once.
            filled.flood = u8::from(route.carrier_for(1, now_ms, sf).is_flood());
            // SAFETY: `out` is non-null and writable; the struct is plain data.
            unsafe { *out = filled };
            LDMUX_OK
        })
    }
}

/// Set the region key this radio's floods are scoped with, or clear it.
///
/// A scoped flood carries `HMAC-SHA-256(key, payload_type || payload)` cut to two bytes, so the key
/// is 16 bytes and the 16-bit code in the config is **not** it — the code is an HMAC *of* it. A
/// caller with no key sends unscoped, which most meshes forward.
#[unsafe(no_mangle)]
/// Tune the radio to a tuple, and optionally to a path width and a power.
///
/// `path_width` 0 and `power_dbm` `LDMUX_UNSET_POWER` (-128) mean "leave it as the radio has it",
/// which is why they are sentinels rather than `bool` flags: a caller that has a number passes it,
/// and a caller that does not omits nothing by accident.
///
/// The writes are queued on the radio's one command window, so a preset change cannot overlap a
/// raw send. The firmware's answer arrives on the next `ldmux_client_on_ble_rx`, like every other
/// command's.
///
/// # Safety
/// `c` must be a handle from [`ldmux_client_new`] that has not been freed.
pub unsafe extern "C" fn ldmux_client_set_preset(
    c: *mut LdmuxClient,
    freq_khz: u32,
    bw_hz: u32,
    sf: u8,
    cr: u8,
    path_width: u8,
    power_dbm: i8,
) -> i32 {
    // SAFETY: see `with_client`.
    unsafe {
        with_client(c, |h| {
            let width = (path_width != 0).then_some(path_width);
            let power = (power_dbm != crate::LDMUX_UNSET_POWER).then_some(power_dbm);
            h.client
                .set_preset(freq_khz, bw_hz, sf, cr, width, power);
            LDMUX_OK
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_client_set_scope(
    c: *mut LdmuxClient,
    key: *const u8,
    key_len: usize,
) -> i32 {
    // SAFETY: see `with_client`; the caller promised `key_len` readable bytes at `key`.
    unsafe {
        with_client(c, |h| {
            if key_len == 0 {
                h.client.set_scope(None);
                return LDMUX_OK;
            }
            if key.is_null() {
                return LDMUX_ERR_NULL;
            }
            if key_len != 16 {
                return LDMUX_ERR_ARG;
            }
            // SAFETY: non-null with 16 readable bytes.
            let bytes = unsafe { std::slice::from_raw_parts(key, 16) };
            let mut region = [0u8; 16];
            region.copy_from_slice(bytes);
            h.client.set_scope(Some(region));
            LDMUX_OK
        })
    }
}

/// The screen, as plain data.
fn mirror_of(mirror: &PageMirror) -> LdmuxMirror {
    LdmuxMirror {
        painted: u8::from(mirror.painted()),
        cols: mirror.cols,
        rows: mirror.rows,
        cursor_x: mirror.cursor.0,
        cursor_y: mirror.cursor.1,
        alternate: u8::from(mirror.alternate),
        dead: u8::from(mirror.dead),
        settled: u8::from(mirror.settled),
        has_applied: u8::from(mirror.applied.is_some()),
        applied_status: mirror.applied.map(|a| a.status).unwrap_or(0),
        snapshot_id: mirror.snapshot_id,
        first_line: mirror.first_line,
    }
}

/// Copy `text` into `dst` at a character boundary and answer how many bytes are meaningful.
///
/// **A character boundary, not a byte count.** The name a user gives a radio can be any UTF-8, and
/// cutting mid-sequence would put a broken character in the caller's buffer — which is invisible
/// until a screen draws a replacement glyph where a name should be.
fn copy_text<const N: usize>(dst: &mut [u8; N], text: &str) -> u8 {
    let mut end = text.len().min(N);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    dst[..end].copy_from_slice(&text.as_bytes()[..end]);
    end as u8
}

#[cfg(test)]
mod copy_text_tests {
    use super::copy_text;

    #[test]
    fn a_name_is_cut_at_a_character_boundary() {
        let mut dst = [0u8; 8];
        // Four two-byte characters fill exactly eight bytes.
        assert_eq!(copy_text(&mut dst, "÷÷÷÷"), 8);
        assert_eq!(&dst, "÷÷÷÷".as_bytes());
        // A fifth would need a ninth byte, and the cut lands before it rather than inside it.
        let mut dst = [0u8; 8];
        assert_eq!(copy_text(&mut dst, "÷÷÷÷÷"), 8);
        // Three bytes of room holds one two-byte character, not one and a half.
        let mut dst = [0u8; 3];
        assert_eq!(copy_text(&mut dst, "÷÷"), 2);
        assert_eq!(&dst[..2], "÷".as_bytes());
        // Nothing read is zero bytes, which a caller draws as nothing.
        let mut dst = [0u8; 8];
        assert_eq!(copy_text(&mut dst, ""), 0);
    }
}
