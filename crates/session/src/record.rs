//! The transport record (ADR-0020 section 3).
//!
//! ```text
//! offset  size  field                      protected
//! 0       1     kind/version (0x13)        no  (clear)
//! 1       1     session index              no  (clear; lookup only)
//! 2       4     counter, little-endian     no  (it is the AEAD nonce: a wrong one fails the tag)
//! 6       1     inner flags                yes
//! 7       1     shell id                   yes
//! 8       n     application bytes          yes
//! 8+n     16    tag                        never truncated
//! ```
//!
//! Fixed overhead is 24 bytes, so a 172-byte L2 record leaves **148** application bytes. That
//! arithmetic lives here and nowhere else: the carrier exports only the 172.
//!
//! `snow`'s transport API takes no associated data, so the clear header is not bound into the
//! tag. That is the ADR's design (the header is "lookup only"): a flipped kind or index selects
//! a different or no key and fails the tag, and a flipped counter is the nonce.

use ldmux_mesh::constants::RECORD_MAX;
use snow::StatelessTransportState;

pub const CLEAR_HEADER: usize = 6;
pub const INNER_HEADER: usize = 2;
pub const TAG: usize = 16;
/// 6 clear + 2 inner + 16 tag.
pub const OVERHEAD: usize = CLEAR_HEADER + INNER_HEADER + TAG;
/// Application bytes in a record of the carrier's ceiling.
pub const APPLICATION_BYTES: usize = RECORD_MAX - OVERHEAD;

/// The transport record's kind for a record the **phone** sends.
///
/// Defined in [`crate::direction`] as [`KIND_UP`](crate::direction::KIND_UP), because a record's
/// direction is a fact the filter needs before it decrypts anything. This alias is what every caller
/// that predates the direction split uses, and it is the same byte: one definition, two names.
///
/// A host's record opens with [`KIND_DOWN`](crate::direction::KIND_DOWN) instead. Both are transport
/// records and both open here; the split exists so that a half-duplex radio cannot mistake its own
/// transmission come back off a repeater for the peer's (see [`crate::direction`]).
pub use crate::direction::KIND_UP as KIND_TRANSPORT;

/// Whether `kind` is one of the two transport record kinds this layer opens.
#[must_use]
pub const fn is_record_kind(kind: u8) -> bool {
    kind == crate::direction::KIND_UP || kind == crate::direction::KIND_DOWN
}

/// Application bytes in a record of `record_limit` bytes (the size `BIND_OK` advertised).
#[must_use]
pub const fn application_bytes(record_limit: usize) -> usize {
    record_limit.saturating_sub(OVERHEAD)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RecordError {
    /// Shorter than header + inner + tag, or not the transport kind.
    Malformed,
    /// The record is for a different session index.
    WrongIndex,
    /// Counter already accepted.
    Duplicate,
    /// Counter older than the window.
    TooOld,
    /// The tag did not verify. The replay window is untouched.
    BadTag,
    /// The plaintext would not fit the record limit.
    TooLong,
    /// The 32-bit counter is used up. A new handshake is the only way on.
    CounterExhausted,
    /// The transport could not seal the plaintext: a key or nonce state fault, not a sizing one.
    SealFailed,
}

impl RecordError {
    /// A stable code for a log line, so a field failure names its reason instead of "bad record".
    ///
    /// Append-only: a code that changes meaning makes an old log lie.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Malformed => 1,
            Self::WrongIndex => 2,
            Self::Duplicate => 3,
            Self::TooOld => 4,
            Self::BadTag => 5,
            Self::TooLong => 6,
            Self::CounterExhausted => 7,
            Self::SealFailed => 8,
        }
    }

    /// The reason in words, for a status line.
    #[must_use]
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::WrongIndex => "another session's index",
            Self::Duplicate => "a counter already seen",
            Self::TooOld => "a counter older than the window",
            Self::BadTag => "the tag did not verify",
            Self::TooLong => "longer than the record limit",
            Self::CounterExhausted => "the counter is used up",
            Self::SealFailed => "the transport could not seal it",
        }
    }
}

/// A sliding replay window over a 32-bit counter, 64 records wide (ADR-0020: judgement call,
/// tens of seconds of history at the planned rate).
///
/// `check` and `commit` are separate on purpose: the window advances only after the tag
/// verifies, so a forged packet cannot move it.
#[derive(Debug, Default, Clone)]
pub struct ReplayWindow {
    highest: Option<u32>,
    /// Bit `i` is set when counter `highest - i` has been accepted.
    seen: u64,
}

impl ReplayWindow {
    pub const WIDTH: u32 = 64;

    pub fn check(&self, counter: u32) -> Result<(), RecordError> {
        let Some(highest) = self.highest else {
            return Ok(());
        };
        if counter > highest {
            return Ok(());
        }
        let behind = highest - counter;
        if behind >= Self::WIDTH {
            return Err(RecordError::TooOld);
        }
        if self.seen >> behind & 1 == 1 {
            return Err(RecordError::Duplicate);
        }
        Ok(())
    }

    /// Call only after the tag verified.
    pub fn commit(&mut self, counter: u32) {
        match self.highest {
            Some(highest) if counter <= highest => {
                let behind = highest - counter;
                if behind < Self::WIDTH {
                    self.seen |= 1 << behind;
                }
            }
            _ => {
                let shift = self.highest.map_or(0, |h| counter - h);
                self.seen = if shift >= Self::WIDTH {
                    0
                } else {
                    self.seen << shift
                };
                self.seen |= 1;
                self.highest = Some(counter);
            }
        }
    }
}

/// An opened record.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened {
    pub flags: u8,
    pub shell: u8,
    pub app: Vec<u8>,
    pub counter: u32,
}

/// Encrypt one record the **phone** sends. `record_limit` is the size the carrier allows.
///
/// A host's record must open with [`crate::direction::KIND_DOWN`], so a host calls [`seal_as`]; this
/// form is kept for the callers that predate the direction split and is exactly `seal_as` with the
/// phone's kind.
///
/// # Errors
///
/// [`RecordError::TooLong`] when the payload does not fit, [`RecordError::SealFailed`] on a key or
/// nonce fault, [`RecordError::CounterExhausted`] when the 32-bit counter is spent.
pub fn seal(
    transport: &StatelessTransportState,
    index: u8,
    counter: u32,
    flags: u8,
    shell: u8,
    app: &[u8],
    record_limit: usize,
) -> Result<Vec<u8>, RecordError> {
    seal_as(
        transport,
        KIND_TRANSPORT,
        index,
        counter,
        flags,
        shell,
        app,
        record_limit,
    )
}

/// Encrypt one record under a chosen transport kind.
///
/// # Errors
///
/// The same as [`seal`].
// Eight parameters, and every one of them is a field of the record being built: the transport kind,
// the session index, the counter, the two inner header bytes, the payload and the ceiling. Folding
// them into a struct would only move the same eight values into a struct literal at each call site.
#[allow(clippy::too_many_arguments)]
pub fn seal_as(
    transport: &StatelessTransportState,
    kind: u8,
    index: u8,
    counter: u32,
    flags: u8,
    shell: u8,
    app: &[u8],
    record_limit: usize,
) -> Result<Vec<u8>, RecordError> {
    if counter == u32::MAX {
        return Err(RecordError::CounterExhausted);
    }
    if app.len() > application_bytes(record_limit.min(RECORD_MAX)) {
        return Err(RecordError::TooLong);
    }
    let mut plain = Vec::with_capacity(INNER_HEADER + app.len());
    plain.push(flags);
    plain.push(shell);
    plain.extend_from_slice(app);
    let mut ct = vec![0u8; plain.len() + TAG];
    // **A crypto fault is not a sizing fault.** Reporting both as `TooLong` made a broken key or a
    // spent nonce indistinguishable from "the route grew a hop" in every log, in `mesh status`, and
    // in the ABI's `LDMUX_ERR_TOO_LONG`.
    let n = transport
        .write_message(u64::from(counter), &plain, &mut ct)
        .map_err(|_| RecordError::SealFailed)?;
    ct.truncate(n);
    let mut out = Vec::with_capacity(CLEAR_HEADER + ct.len());
    out.push(kind);
    out.push(index);
    out.extend_from_slice(&counter.to_le_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt one record. Order of checks (ADR-0020 section 3): kind, index, replay window, AEAD
/// open; the window advances only on success.
pub fn open(
    transport: &StatelessTransportState,
    index: u8,
    window: &mut ReplayWindow,
    record: &[u8],
) -> Result<Opened, RecordError> {
    if record.len() < OVERHEAD || !is_record_kind(record[0]) {
        return Err(RecordError::Malformed);
    }
    if record[1] != index {
        return Err(RecordError::WrongIndex);
    }
    let counter = u32::from_le_bytes([record[2], record[3], record[4], record[5]]);
    window.check(counter)?;
    let ct = &record[CLEAR_HEADER..];
    let mut plain = vec![0u8; ct.len()];
    let n = transport
        .read_message(u64::from(counter), ct, &mut plain)
        .map_err(|_| RecordError::BadTag)?;
    if n < INNER_HEADER {
        return Err(RecordError::Malformed);
    }
    window.commit(counter);
    Ok(Opened {
        flags: plain[0],
        shell: plain[1],
        app: plain[INNER_HEADER..n].to_vec(),
        counter,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_arithmetic_is_the_adr_numbers() {
        assert_eq!(CLEAR_HEADER + INNER_HEADER + TAG, 24);
        assert_eq!(APPLICATION_BYTES, 148);
        assert_eq!(application_bytes(160), 136);
        assert_eq!(application_bytes(10), 0);
    }

    #[test]
    fn replay_window_accepts_new_rejects_duplicates_and_old() {
        let mut w = ReplayWindow::default();
        for c in [0u32, 1, 2, 5] {
            w.check(c).unwrap();
            w.commit(c);
        }
        assert_eq!(w.check(5), Err(RecordError::Duplicate));
        assert_eq!(w.check(1), Err(RecordError::Duplicate));
        // Reordered but unseen is fine.
        w.check(3).unwrap();
        w.commit(3);
        assert_eq!(w.check(3), Err(RecordError::Duplicate));
        w.commit(100);
        assert_eq!(w.check(5), Err(RecordError::TooOld));
        assert_eq!(w.check(100 - 63), Ok(()));
        assert_eq!(w.check(100 - 64), Err(RecordError::TooOld));
    }

    #[test]
    fn a_checked_but_uncommitted_counter_does_not_move_the_window() {
        let mut w = ReplayWindow::default();
        w.commit(10);
        w.check(1_000_000).unwrap(); // a forged packet passed `check`...
        assert_eq!(w.check(9), Ok(())); // ...and, never committed, did not push 9 out.
    }

    #[test]
    fn a_big_jump_clears_the_bitmap() {
        let mut w = ReplayWindow::default();
        w.commit(1);
        w.commit(500);
        assert_eq!(w.check(1), Err(RecordError::TooOld));
        assert_eq!(w.check(499), Ok(()));
    }
    /// Some firmware forks classify a raw-custom packet as media by its first byte alone and
    /// route `0x56`, `0x57`, `0x72` and `0x53` to a second frequency most nodes do not listen on.
    /// The radio still answers `OK`, so a collision would look like a peer that never replies.
    ///
    /// Every LDMUX record starts with [KIND_TRANSPORT] (`encode`), so no LDMUX record can match
    /// that heuristic. This test is what keeps a future record kind from quietly joining the set.
    #[test]
    fn the_record_kind_cannot_be_mistaken_for_a_media_packet() {
        for kind in [KIND_TRANSPORT, crate::direction::KIND_DOWN] {
            assert!(is_record_kind(kind));
            for routed_away in [0x53u8, 0x56, 0x57, 0x72] {
                assert_ne!(
                    kind, routed_away,
                    "a record beginning {routed_away:#04x} would be routed to the second radio profile"
                );
            }
        }
        assert!(!is_record_kind(crate::direction::KIND_MSG1));
        assert!(!is_record_kind(0x00));
    }
}
