//! The C ABI over `ldmux-session`. Header: `packages/mobile/native/ldmux_session.h`, which is
//! the contract; read it for ownership rules.
//!
//! Rules this file keeps, each one checkable:
//!
//! - No global mutable state. Every piece of state is behind a handle the caller owns.
//! - No pointer into Rust memory ever leaves this crate to be interpreted. Handles are opaque
//!   and only passed back; every byte the caller receives is copied into a buffer *it* gave.
//! - Every entry point returns a status code and never unwinds across the boundary (a panic
//!   becomes [`LDMUX_ERR_PANIC`]).
//! - Destroy functions take a pointer *to* the handle and null it, so a second destroy is a
//!   no-op and a use after destroy is [`LDMUX_ERR_NULL`] instead of undefined behaviour.
//!
//! The handles are not thread safe; the caller serialises calls on one handle.
#![allow(clippy::missing_safety_doc)]
// Closures passed to `with_session` run inside the caller's `unsafe` block, so the inner blocks
// are redundant to the compiler; they stay because each one carries its own SAFETY argument.
#![allow(unused_unsafe)]

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;

use ldmux_session::arq::{Failure, Limits, QueueError};
use ldmux_session::handshake::{self, HandshakeError, Initiator, Keypair, PairingEntry, Responder};
use ldmux_session::record::RecordError;
use ldmux_session::session::{Inbound, Session, SessionError};

pub const LDMUX_OK: i32 = 0;
pub const LDMUX_ERR_NULL: i32 = -1;
pub const LDMUX_ERR_PANIC: i32 = -2;
pub const LDMUX_ERR_ARG: i32 = -3;
pub const LDMUX_ERR_BUFFER: i32 = -4;
pub const LDMUX_ERR_CRYPTO: i32 = -10;
pub const LDMUX_ERR_MALFORMED: i32 = -11;
pub const LDMUX_ERR_BAD_FILTER: i32 = -12;
pub const LDMUX_ERR_STALE: i32 = -13;
pub const LDMUX_ERR_REPLAY: i32 = -14;
pub const LDMUX_ERR_PIN_MISMATCH: i32 = -15;
pub const LDMUX_ERR_WRONG_INDEX: i32 = -20;
pub const LDMUX_ERR_DUPLICATE: i32 = -21;
pub const LDMUX_ERR_TOO_OLD: i32 = -22;
pub const LDMUX_ERR_BAD_TAG: i32 = -23;
pub const LDMUX_ERR_TOO_LONG: i32 = -24;
pub const LDMUX_ERR_COUNTER_EXHAUSTED: i32 = -25;
pub const LDMUX_ERR_DECODE: i32 = -26;
pub const LDMUX_ERR_RECORD_LIMIT: i32 = -27;
pub const LDMUX_ERR_QUEUE_FULL: i32 = -30;
pub const LDMUX_ERR_NOT_OPEN: i32 = -31;

/// `ldmux_session_receive` results (non-negative).
pub const LDMUX_RX_QUIET: i32 = 0;
pub const LDMUX_RX_DATA: i32 = 1;
pub const LDMUX_RX_CLOSED: i32 = 2;

/// `ldmux_session_status` bits.
pub const LDMUX_STATUS_FAILED: u32 = 1;
pub const LDMUX_STATUS_PEER_CLOSED: u32 = 2;
pub const LDMUX_STATUS_NEEDS_REKEY: u32 = 4;

pub const LDMUX_KEY_LEN: usize = 32;
pub const LDMUX_MSG1_LEN: usize = handshake::MSG1_LEN;
pub const LDMUX_MSG2_LEN: usize = handshake::MSG2_LEN;
/// The most bytes one delivered chunk or one sealed record can be.
pub const LDMUX_RECORD_MAX: usize = ldmux_mesh::constants::RECORD_MAX;

/// An established session plus the two queues the pull-style API drains.
pub struct LdmuxSession {
    inner: Session,
    delivered: VecDeque<(u8, Vec<u8>)>,
    outbox: VecDeque<Vec<u8>>,
}

/// A started handshake awaiting message 2.
pub struct LdmuxInitiator(Initiator);

fn guard(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(LDMUX_ERR_PANIC)
}

fn hs_err(e: HandshakeError) -> i32 {
    match e {
        HandshakeError::Malformed => LDMUX_ERR_MALFORMED,
        HandshakeError::BadFilter => LDMUX_ERR_BAD_FILTER,
        HandshakeError::Stale => LDMUX_ERR_STALE,
        HandshakeError::Replay => LDMUX_ERR_REPLAY,
        HandshakeError::PinMismatch => LDMUX_ERR_PIN_MISMATCH,
        HandshakeError::Crypto => LDMUX_ERR_CRYPTO,
    }
}

fn rec_err(e: RecordError) -> i32 {
    match e {
        RecordError::Malformed => LDMUX_ERR_MALFORMED,
        RecordError::WrongIndex => LDMUX_ERR_WRONG_INDEX,
        RecordError::Duplicate => LDMUX_ERR_DUPLICATE,
        RecordError::TooOld => LDMUX_ERR_TOO_OLD,
        RecordError::BadTag => LDMUX_ERR_BAD_TAG,
        RecordError::TooLong => LDMUX_ERR_TOO_LONG,
        RecordError::CounterExhausted => LDMUX_ERR_COUNTER_EXHAUSTED,
    }
}

fn sess_err(e: SessionError) -> i32 {
    match e {
        SessionError::Record(r) => rec_err(r),
        SessionError::Decode(_) => LDMUX_ERR_DECODE,
        SessionError::RecordLimitTooSmall => LDMUX_ERR_RECORD_LIMIT,
    }
}

/// A caller-supplied fixed array, or `None` if the pointer is null.
///
/// SAFETY (callers): the header requires `p` to point to `N` readable bytes.
unsafe fn arr<'a, const N: usize>(p: *const u8) -> Option<&'a [u8; N]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: non-null, and the caller promised N readable bytes.
        Some(unsafe { &*p.cast::<[u8; N]>() })
    }
}

/// A caller-supplied byte slice. A null pointer is only valid with length 0.
unsafe fn bytes<'a>(p: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        Some(&[])
    } else if p.is_null() {
        None
    } else {
        // SAFETY: non-null, and the caller promised `len` readable bytes.
        Some(unsafe { slice::from_raw_parts(p, len) })
    }
}

/// Copy `src` into the caller's buffer. `cap` is its size; the length written goes to `out_len`.
unsafe fn put(src: &[u8], out: *mut u8, cap: usize, out_len: *mut usize) -> i32 {
    if out_len.is_null() || (out.is_null() && cap != 0) {
        return LDMUX_ERR_NULL;
    }
    // SAFETY: out_len is non-null and the caller promised it is writable.
    unsafe { *out_len = src.len() };
    if src.len() > cap {
        return LDMUX_ERR_BUFFER;
    }
    if !src.is_empty() {
        // SAFETY: `out` is non-null with `cap >= src.len()` writable bytes, and does not
        // overlap `src`, which is Rust-owned memory.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), out, src.len()) };
    }
    LDMUX_OK
}

fn wrap(inner: Session) -> *mut LdmuxSession {
    Box::into_raw(Box::new(LdmuxSession {
        inner,
        delivered: VecDeque::new(),
        outbox: VecDeque::new(),
    }))
}

/// `priv_out` and `pub_out` are each 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_keypair_generate(priv_out: *mut u8, pub_out: *mut u8) -> i32 {
    guard(|| {
        if priv_out.is_null() || pub_out.is_null() {
            return LDMUX_ERR_NULL;
        }
        match Keypair::generate() {
            Ok(kp) => {
                // SAFETY: both pointers are non-null and the header requires 32 writable bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(kp.private.as_ptr(), priv_out, 32);
                    std::ptr::copy_nonoverlapping(kp.public.as_ptr(), pub_out, 32);
                }
                LDMUX_OK
            }
            Err(e) => hs_err(e),
        }
    })
}

/// Start a handshake. On success `*out` is a new initiator and `msg1` holds the 121-byte
/// message 1. On failure `*out` is untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_initiator_start(
    local_priv: *const u8,
    local_pub: *const u8,
    remote_pub: *const u8,
    psk: *const u8,
    timestamp_ms: u64,
    out: *mut *mut LdmuxInitiator,
    msg1: *mut u8,
) -> i32 {
    guard(|| {
        if out.is_null() || msg1.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: the header requires each key pointer to give 32 readable bytes.
        let (Some(lp), Some(lu), Some(rp), Some(k)) = (unsafe {
            (
                arr::<32>(local_priv),
                arr::<32>(local_pub),
                arr::<32>(remote_pub),
                arr::<32>(psk),
            )
        }) else {
            return LDMUX_ERR_NULL;
        };
        let local = Keypair::from_parts(*lp, *lu);
        match Initiator::start(&local, rp, k, timestamp_ms) {
            Ok((hs, m1)) => {
                // SAFETY: `msg1` is non-null with 121 writable bytes (header); `out` is writable.
                unsafe {
                    std::ptr::copy_nonoverlapping(m1.as_ptr(), msg1, m1.len());
                    *out = Box::into_raw(Box::new(LdmuxInitiator(hs)));
                }
                LDMUX_OK
            }
            Err(e) => hs_err(e),
        }
    })
}

/// Consume message 2. The initiator is consumed whatever the outcome and `*hs` is nulled.
/// On success `*out` is the established session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_initiator_finish(
    hs: *mut *mut LdmuxInitiator,
    msg2: *const u8,
    msg2_len: usize,
    record_limit: u32,
    now_ms: u64,
    out: *mut *mut LdmuxSession,
) -> i32 {
    guard(|| {
        if hs.is_null() || out.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: `hs` is non-null and points at a handle slot the caller owns.
        let raw = unsafe { *hs };
        if raw.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: a non-null handle came from `ldmux_initiator_start` and has not been freed,
        // because every consuming call nulls the slot it was given.
        let boxed = unsafe { Box::from_raw(raw) };
        // SAFETY: the slot is writable; nulling it now makes a retry or a free a no-op.
        unsafe { *hs = std::ptr::null_mut() };
        // SAFETY: header: `msg2` has `msg2_len` readable bytes.
        let Some(m2) = (unsafe { bytes(msg2, msg2_len) }) else {
            return LDMUX_ERR_NULL;
        };
        let est = match boxed.0.finish(m2) {
            Ok(e) => e,
            Err(e) => return hs_err(e),
        };
        match Session::new(est, record_limit as usize, now_ms) {
            Ok(s) => {
                // SAFETY: `out` is non-null and writable.
                unsafe { *out = wrap(s) };
                LDMUX_OK
            }
            Err(e) => sess_err(e),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_initiator_free(hs: *mut *mut LdmuxInitiator) {
    let _ = guard(|| {
        if hs.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: `hs` points at a handle slot the caller owns.
        let raw = unsafe { *hs };
        if !raw.is_null() {
            // SAFETY: non-null and not yet freed (the slot is nulled right after).
            drop(unsafe { Box::from_raw(raw) });
            // SAFETY: the slot is writable.
            unsafe { *hs = std::ptr::null_mut() };
        }
        LDMUX_OK
    });
}

/// The gateway's side for one pairing. `pinned` is null or 32 bytes. On success `reply` holds the
/// 50-byte message 2, `remote_static_out` the phone key to pin, `*timestamp_out` the timestamp
/// to remember, and `*out` the established session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_responder_accept(
    local_priv: *const u8,
    local_pub: *const u8,
    psk: *const u8,
    pinned: *const u8,
    last_timestamp_ms: u64,
    now_ms: u64,
    msg1: *const u8,
    msg1_len: usize,
    index: u8,
    record_limit: u32,
    reply: *mut u8,
    remote_static_out: *mut u8,
    timestamp_out: *mut u64,
    out: *mut *mut LdmuxSession,
) -> i32 {
    guard(|| {
        if reply.is_null()
            || remote_static_out.is_null()
            || timestamp_out.is_null()
            || out.is_null()
        {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: the header requires each key pointer to give 32 readable bytes.
        let (Some(lp), Some(lu), Some(k)) =
            (unsafe { (arr::<32>(local_priv), arr::<32>(local_pub), arr::<32>(psk)) })
        else {
            return LDMUX_ERR_NULL;
        };
        // SAFETY: `pinned` is null or 32 readable bytes (header).
        let pin = unsafe { arr::<32>(pinned) }.copied();
        // SAFETY: header: `msg1` has `msg1_len` readable bytes.
        let Some(m1) = (unsafe { bytes(msg1, msg1_len) }) else {
            return LDMUX_ERR_NULL;
        };
        let responder = Responder::new(Keypair::from_parts(*lp, *lu));
        let entry = PairingEntry {
            psk: *k,
            pinned: pin,
            last_timestamp_ms,
        };
        let acc = match responder.accept(&[entry], now_ms, m1, index) {
            Ok(a) => a,
            Err(e) => return hs_err(e),
        };
        let sess = match Session::new(acc.established, record_limit as usize, now_ms) {
            Ok(s) => s,
            Err(e) => return sess_err(e),
        };
        // SAFETY: all four out pointers are non-null; `reply` has 50 and `remote_static_out`
        // 32 writable bytes (header).
        unsafe {
            std::ptr::copy_nonoverlapping(acc.reply.as_ptr(), reply, acc.reply.len());
            std::ptr::copy_nonoverlapping(acc.remote_static.as_ptr(), remote_static_out, 32);
            *timestamp_out = acc.timestamp_ms;
            *out = wrap(sess);
        }
        LDMUX_OK
    })
}

/// Run `f` on the session behind `*s`, or `LDMUX_ERR_NULL` if the slot or handle is null.
unsafe fn with_session(s: *mut LdmuxSession, f: impl FnOnce(&mut LdmuxSession) -> i32) -> i32 {
    guard(|| {
        if s.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: non-null, from `wrap`, not freed (free nulls the caller's slot), and the
        // caller serialises calls on one handle (header).
        f(unsafe { &mut *s })
    })
}

/// Queue application bytes for `shell`. All or nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_send(
    s: *mut LdmuxSession,
    shell: u8,
    data: *const u8,
    len: usize,
) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            // SAFETY: header: `data` has `len` readable bytes.
            let Some(d) = (unsafe { bytes(data, len) }) else {
                return LDMUX_ERR_NULL;
            };
            match s.inner.send(shell, d) {
                Ok(()) => LDMUX_OK,
                Err(QueueError::Full) => LDMUX_ERR_QUEUE_FULL,
                Err(QueueError::NotOpen) => LDMUX_ERR_NOT_OPEN,
            }
        })
    }
}

/// Feed one record from the tunnel. Returns `LDMUX_RX_*` (>= 0) or a negative error. Data is
/// then drained with `ldmux_session_take_delivered`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_receive(
    s: *mut LdmuxSession,
    now_ms: u64,
    record: *const u8,
    len: usize,
) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            // SAFETY: header: `record` has `len` readable bytes.
            let Some(r) = (unsafe { bytes(record, len) }) else {
                return LDMUX_ERR_NULL;
            };
            match s.inner.receive(now_ms, r) {
                Ok(Inbound::Data(chunks)) => {
                    for c in chunks {
                        s.delivered.push_back((c.shell, c.payload));
                    }
                    LDMUX_RX_DATA
                }
                Ok(Inbound::Quiet) => LDMUX_RX_QUIET,
                Ok(Inbound::Closed) => LDMUX_RX_CLOSED,
                Err(e) => sess_err(e),
            }
        })
    }
}

/// Pop one delivered chunk. Returns 1 and fills the buffer, 0 if none is waiting, or an error
/// (a too-small buffer leaves the chunk queued; `*out_len` says what is needed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_take_delivered(
    s: *mut LdmuxSession,
    shell_out: *mut u8,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            if shell_out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let Some((shell, payload)) = s.delivered.front() else {
                return 0;
            };
            // SAFETY: the out pointers are validated inside `put`; `shell_out` is non-null.
            let rc = unsafe { put(payload, out, cap, out_len) };
            if rc != LDMUX_OK {
                return rc;
            }
            // SAFETY: `shell_out` is non-null and writable.
            unsafe { *shell_out = *shell };
            s.delivered.pop_front();
            1
        })
    }
}

/// Ask the ARQ what to transmit now, bounded by `records_left` and `l2_slots`. Sealed records
/// are queued inside the handle. Returns how many are now waiting, or an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_poll(
    s: *mut LdmuxSession,
    now_ms: u64,
    records_left: u8,
    l2_slots: u8,
) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            match s.inner.poll(
                now_ms,
                Limits {
                    records_left,
                    l2_slots,
                },
            ) {
                Ok(v) => {
                    s.outbox.extend(v);
                    i32::try_from(s.outbox.len()).unwrap_or(i32::MAX)
                }
                Err(e) => sess_err(e),
            }
        })
    }
}

/// Pop one sealed record (at most `LDMUX_RECORD_MAX` bytes). Returns 1, 0 if none, or an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_take_record(
    s: *mut LdmuxSession,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            let Some(front) = s.outbox.front() else {
                return 0;
            };
            // SAFETY: the out pointers are validated inside `put`.
            let rc = unsafe { put(front, out, cap, out_len) };
            if rc != LDMUX_OK {
                return rc;
            }
            s.outbox.pop_front();
            1
        })
    }
}

/// End the session after everything queued is acknowledged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_close(s: *mut LdmuxSession) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            s.inner.arq_mut().close();
            LDMUX_OK
        })
    }
}

/// `LDMUX_STATUS_*` bits, or a negative error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_status(s: *mut LdmuxSession, now_ms: u64) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            let mut bits = 0u32;
            if s.inner.arq().failure() == Some(Failure::RetriesExhausted) {
                bits |= LDMUX_STATUS_FAILED;
            }
            if s.inner.arq().peer_closed() {
                bits |= LDMUX_STATUS_PEER_CLOSED;
            }
            if s.inner.needs_rekey(now_ms) {
                bits |= LDMUX_STATUS_NEEDS_REKEY;
            }
            i32::try_from(bits).unwrap_or(0)
        })
    }
}

/// Chunks queued and not yet acknowledged, as the ARQ counts them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_pending(s: *mut LdmuxSession) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            i32::try_from(s.inner.arq().pending()).unwrap_or(i32::MAX)
        })
    }
}

/// Write the next retransmit deadline (ms) to `*out` and return 1, or return 0 if none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_next_deadline(s: *mut LdmuxSession, out: *mut u64) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            match s.inner.arq().next_deadline() {
                Some(d) => {
                    // SAFETY: `out` is non-null and writable.
                    unsafe { *out = d };
                    1
                }
                None => 0,
            }
        })
    }
}

/// The peer's static key, 32 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_peer_key(s: *mut LdmuxSession, out: *mut u8) -> i32 {
    // SAFETY: see `with_session`.
    unsafe {
        with_session(s, |s| {
            if out.is_null() {
                return LDMUX_ERR_NULL;
            }
            let k = s.inner.peer_key();
            // SAFETY: `out` is non-null with 32 writable bytes (header).
            unsafe { std::ptr::copy_nonoverlapping(k.as_ptr(), out, 32) };
            LDMUX_OK
        })
    }
}

/// Records dropped because they failed a check (tag, replay, decode).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_dropped(s: *mut LdmuxSession) -> i32 {
    // SAFETY: see `with_session`.
    unsafe { with_session(s, |s| i32::try_from(s.inner.dropped).unwrap_or(i32::MAX)) }
}

/// Destroy a session and null the caller's slot. A null slot or a null handle is a no-op, so
/// freeing twice through the same variable is safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ldmux_session_free(s: *mut *mut LdmuxSession) {
    let _ = guard(|| {
        if s.is_null() {
            return LDMUX_ERR_NULL;
        }
        // SAFETY: `s` points at a handle slot the caller owns.
        let raw = unsafe { *s };
        if !raw.is_null() {
            // SAFETY: non-null, from `wrap`, and nulled below so it is freed once.
            drop(unsafe { Box::from_raw(raw) });
            // SAFETY: the slot is writable.
            unsafe { *s = std::ptr::null_mut() };
        }
        LDMUX_OK
    });
}
