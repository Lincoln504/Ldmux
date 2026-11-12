//! How many bytes of record fit in one packet, on this carrier, right now.
//!
//! The record budget is **not a constant**, and treating it as one is the defect behind `RC-6`: a
//! 162-byte record was published to the session, a 174-byte packet was transmitted, every repeater
//! forwarded it, and the receiving radio never handed it to the application — silently, on both
//! ends. The budget is two arithmetic facts that must both hold:
//!
//! 1. the **sender's** frame fits the companion's write path, and
//! 2. the **receiver's** frame fits the companion's read path, *after* the path that the packet
//!    accumulated in transit.
//!
//! Both are derived here from the firmware's own numbers rather than from a remembered figure:
//!
//! | Number | Value | Source |
//! |---|---|---|
//! | Companion frame ceiling | 176 | `MAX_FRAME_SIZE`, `src/helpers/BaseSerialInterface.h:5` |
//! | Receive-log header | 3 | `PUSH_CODE_LOG_RX_DATA`, `snr`, `rssi` |
//! | Longest packet a radio hands up | 173 | `MyMesh::logRxRaw` refuses when `len + 3 > 176` (`examples/companion_radio/MyMesh.cpp:291`) |
//! | Command + priority in a write | 2 | `CMD_SEND_RAW_PACKET` then the priority byte (`MyMesh.cpp:2047-2051`) |
//! | Longest packet a write can carry | 174 | `176 − 2`; the serial reader truncates beyond it (`ArduinoSerialInterface.cpp:56-59`) |
//! | Packet header + path-length byte | 2 | `Packet::getRawLength` (`src/Packet.cpp:49`) |
//! | Flooded-record wrapper | 10 | `[dest hash][source hash]["LDMR"][nonce]` (`crates/companion/src/codec.rs`) |
//! | Transport codes on a scoped flood | 4 | two 16-bit codes (`Packet::hasTransportCodes`) |
//!
//! ## Direct
//!
//! A raw send is `[0x41][priority][header][path_len][path][record]`, so at the sender
//! `2 + path_out + record ≤ 174`. Every repeater consumes its own hash on the way
//! (`removeSelfFromPath`, `src/Mesh.cpp`), so the receiver sees an **empty** path and its limit is
//! `2 + record ≤ 173`, independent of how far the record travelled. The record limit is the smaller
//! of the two.
//!
//! ## Flood
//!
//! A flooded record is `[header][codes?][path_len][wrapper 10][record]`, and a flood's path **grows**
//! by one hash per hop, so the receiver's limit shrinks with distance:
//! `2 + scope + path_arrival + 10 + record ≤ 173`.
//!
//! That is the arithmetic `docs/RADIO-PLAN.md` §4.1 states as `record ≤ 161 − S − P`, and
//! [`FLOOD_RECORD_BASE`] is where the 161 comes from.

use crate::constants::{RECORD_FLOOR, RECORD_MAX, RECORD_RECEIVED_MAX};

/// The companion's serial/BLE frame ceiling, `MAX_FRAME_SIZE`
/// (`src/helpers/BaseSerialInterface.h:5`).
pub const COMPANION_FRAME_MAX: usize = 176;

/// The three bytes a receive-log frame puts in front of the packet: the push code and the two
/// readings. `MyMesh::logRxRaw` filters on `len + 3` (`examples/companion_radio/MyMesh.cpp:291`).
pub const LOG_RX_HEADER: usize = 3;

/// The longest packet a companion radio will **hand up**: 173 bytes.
pub const RECEIVE_MAX: usize = COMPANION_FRAME_MAX - LOG_RX_HEADER;

/// The command byte and the priority byte a raw send spends before the packet starts.
pub const SEND_FRAME_OVERHEAD: usize = 2;

/// The longest packet one write frame can carry: 174 bytes. Beyond it the serial reader truncates
/// the frame (`ArduinoSerialInterface.cpp:56-59`), turning an oversized record into a parse
/// failure rather than a send.
pub const SEND_PACKET_MAX: usize = COMPANION_FRAME_MAX - SEND_FRAME_OVERHEAD;

/// The packet header byte and the path-length byte.
pub const PACKET_HEADER: usize = 2;

/// The bytes a flooded record wraps around its record: destination hash, source hash, `"LDMR"`,
/// and a four-byte nonce that a retransmission varies so the firmware's deduplication hash does not
/// swallow it (`crates/companion/src/codec.rs`, `build_record_flood_in_scope`).
pub const FLOOD_WRAPPER: usize = 10;

/// The two 16-bit transport codes a scoped flood carries, and nothing else.
pub const SCOPE_BYTES: usize = 4;

/// The firmware's path ceiling, `MAX_PATH_SIZE` (`src/MeshCore.h:22`). A longer path is refused by
/// `Packet::isValidPathLen` on every receiver.
pub const MAX_PATH_BYTES: usize = 64;

/// `161`: the largest record a flood could carry with no scope and an empty arrival path.
///
/// Derived, not remembered: the receive ceiling less the packet header and the flood wrapper.
pub const FLOOD_RECORD_BASE: usize = RECEIVE_MAX - PACKET_HEADER - FLOOD_WRAPPER;

/// **93 bytes** — the flooded record size `docs/RADIO-PLAN.md` §4.1 guarantees at the longest legal
/// path and a scoped flood. A record at or below this crosses at every hop count and hash width.
pub const FLOOD_RECORD_GUARANTEED: usize = FLOOD_RECORD_BASE - SCOPE_BYTES - MAX_PATH_BYTES;

/// **125 bytes** — the flooded record size that holds to a 32-byte arrival path under a scoped
/// flood: thirty-two one-byte hops, or sixteen two-byte ones.
pub const FLOOD_RECORD_NEAR: usize = FLOOD_RECORD_BASE - SCOPE_BYTES - 32;

// The three figures the rest of the tree publishes are the same arithmetic, and a change to any of
// them that this module does not follow would be a silent loss of exactly the kind `RC-6` was.
const _: () = {
    assert!(RECORD_MAX == SEND_PACKET_MAX - PACKET_HEADER);
    assert!(RECORD_RECEIVED_MAX == RECEIVE_MAX - PACKET_HEADER);
    assert!(FLOOD_RECORD_GUARANTEED == 93);
    assert!(FLOOD_RECORD_NEAR == 125);
    // The floor is the direct limit at the longest legal path, which is why it is a *direct* test
    // and not a universal one: 93 bytes of flood is legal and below it.
    assert!(RECORD_FLOOR == RECORD_MAX - MAX_PATH_BYTES);
};

/// What carrier a record is about to be handed to, and what that carrier costs it.
///
/// Built from what the caller knows — the outbound path for a direct send, the arrival path for a
/// flood — so [`RecordBudget::limit`] answers with a number for *this* packet rather than for a
/// remembered one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordBudget {
    /// A `RAW_CUSTOM` direct packet along `path_out` bytes of path.
    Direct {
        /// The outbound path length in bytes, `0..=64`.
        path_out: usize,
    },
    /// A flooded record inside the `LDMR` wrapper.
    Flood {
        /// The transport codes the flood carries: [`SCOPE_BYTES`] when it is scoped, `0` when not.
        scope_bytes: usize,
        /// The path the flood will have accumulated when it arrives, in bytes.
        path_arrival: usize,
    },
}

impl RecordBudget {
    /// A direct send along a path of `path_out` bytes.
    #[must_use]
    pub const fn direct(path_out: usize) -> Self {
        Self::Direct { path_out }
    }

    /// A flood that will have travelled `path_arrival` bytes when it is received, scoped.
    #[must_use]
    pub const fn flood_scoped(path_arrival: usize) -> Self {
        Self::Flood {
            scope_bytes: SCOPE_BYTES,
            path_arrival,
        }
    }

    /// The same, unscoped, which four bytes more record buys.
    #[must_use]
    pub const fn flood_unscoped(path_arrival: usize) -> Self {
        Self::Flood {
            scope_bytes: 0,
            path_arrival,
        }
    }

    /// The largest record this carrier can carry **and have reported**, in bytes.
    ///
    /// This is the number to build a record to. It is the smaller of the two bounds, so a record of
    /// exactly this size is accepted by both ends — [`RecordBudget::sender_accepts`] and
    /// [`RecordBudget::receiver_accepts`] both say so for it, and one byte more fails the binding
    /// one.
    #[must_use]
    pub const fn limit(&self) -> usize {
        match *self {
            Self::Direct { path_out } => {
                let send = RECORD_MAX.saturating_sub(path_out);
                if send < RECORD_RECEIVED_MAX {
                    send
                } else {
                    RECORD_RECEIVED_MAX
                }
            }
            Self::Flood {
                scope_bytes,
                path_arrival,
            } => FLOOD_RECORD_BASE.saturating_sub(scope_bytes + path_arrival),
        }
    }

    /// The length this packet has **when the receiver sees it**, which is what `logRxRaw` filters.
    ///
    /// A direct packet's path has been consumed by the repeaters that carried it, so the arrival
    /// length does not grow with distance; a flood's does.
    #[must_use]
    pub const fn packet_len(&self, record: usize) -> usize {
        match *self {
            Self::Direct { .. } => PACKET_HEADER + record,
            Self::Flood {
                scope_bytes,
                path_arrival,
            } => PACKET_HEADER + scope_bytes + path_arrival + FLOOD_WRAPPER + record,
        }
    }

    /// Whether the receiving radio will report a packet carrying `record` bytes.
    ///
    /// False means the record is transmitted, forwarded by every repeater, heard — and never handed
    /// to the application, with no error on either end. That is `RC-6`, so no caller may ignore it.
    #[must_use]
    pub const fn receiver_accepts(&self, record: usize) -> bool {
        self.packet_len(record) + LOG_RX_HEADER <= COMPANION_FRAME_MAX
    }

    /// Whether one write frame can carry a packet with `record` bytes.
    #[must_use]
    pub const fn sender_accepts(&self, record: usize) -> bool {
        let sent = match *self {
            Self::Direct { path_out } => PACKET_HEADER + path_out + record,
            // A flood is written with an empty path: it grows in transit.
            Self::Flood { scope_bytes, .. } => PACKET_HEADER + scope_bytes + FLOOD_WRAPPER + record,
        };
        sent <= SEND_PACKET_MAX
    }

    /// The size to build a record to, or `None` when a **direct** path is too short to be worth
    /// sending on.
    ///
    /// `None` is a direct-path verdict and the caller's instruction is in it: the path has more
    /// bytes than the firmware's own ceiling allows for a record, so there is no usable route and
    /// the right move is to ask for a fresh one rather than to send something shorter. A shorter
    /// record is not an option because the session's record size is fixed at the handshake — a
    /// small record would be a protocol change, not a response to the path.
    ///
    /// **A flood never returns `None` for being small.** [`RECORD_FLOOR`] is
    /// `RECORD_MAX − MAX_PATH_BYTES`, the direct limit at the longest legal path; the flood promise
    /// is [`FLOOD_RECORD_GUARANTEED`] = 93 bytes, which is below it by construction. Clamping a
    /// flood up to the floor would produce a packet the receiver drops, which is the defect this
    /// module exists to prevent.
    #[must_use]
    pub const fn usable_limit(&self) -> Option<usize> {
        let limit = self.limit();
        match *self {
            Self::Direct { .. } if limit < RECORD_FLOOR => None,
            _ => Some(limit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every direct path the firmware can encode, against both ends' arithmetic.
    ///
    /// The property that matters is not the number: it is that a record of [`RecordBudget::limit`]
    /// passes the receiver's filter, and that one byte more is refused by **one of the two ends**.
    /// A budget one byte optimistic is a record that is transmitted and never arrives.
    ///
    /// Which end refuses depends on the path. At zero hops it is the receiver (171 bytes, because a
    /// direct packet's path is consumed and its length does not grow); from two hops on it is the
    /// sender, whose packet carries the path. Both are the same arithmetic and both must fail closed.
    #[test]
    fn a_direct_budget_is_the_smaller_of_the_two_frames_at_every_path_length() {
        for path_out in 0..=MAX_PATH_BYTES {
            let budget = RecordBudget::direct(path_out);
            let expected = (RECORD_MAX - path_out).min(RECORD_RECEIVED_MAX);
            assert_eq!(budget.limit(), expected, "path_out {path_out}");
            assert!(
                budget.receiver_accepts(budget.limit()),
                "the receiver would drop a record of its own advertised size at path_out {path_out}"
            );
            let one_more = budget.limit() + 1;
            assert!(
                !(budget.sender_accepts(one_more) && budget.receiver_accepts(one_more)),
                "one byte past the limit is accepted by both ends at path_out {path_out}"
            );
        }
    }

    /// At zero hops the **receiver's** ceiling binds, not the sender's: 171 bytes, not 172.
    ///
    /// This is the single byte that a `RECORD_MAX`-sized record gets wrong, and it fails on the
    /// very first packet of a session rather than at range.
    #[test]
    fn a_pathless_direct_record_is_171_bytes_not_172() {
        let budget = RecordBudget::direct(0);
        assert_eq!(budget.limit(), RECORD_RECEIVED_MAX);
        assert!(budget.receiver_accepts(171));
        assert!(!budget.receiver_accepts(172));
    }

    /// A flood's budget shrinks by exactly one hash per hop, and by four bytes for a scope.
    #[test]
    fn a_flood_budget_shrinks_with_the_path_it_accumulates() {
        for path_arrival in 0..=MAX_PATH_BYTES {
            for scope_bytes in [0, SCOPE_BYTES] {
                let budget = RecordBudget::Flood {
                    scope_bytes,
                    path_arrival,
                };
                assert_eq!(
                    budget.limit(),
                    FLOOD_RECORD_BASE - scope_bytes - path_arrival,
                    "scope {scope_bytes}, path {path_arrival}"
                );
                assert!(budget.receiver_accepts(budget.limit()));
                assert!(
                    !budget.receiver_accepts(budget.limit() + 1),
                    "a flood one byte over its budget still passes the receive filter"
                );
            }
        }
    }

    /// The two sizes `docs/RADIO-PLAN.md` §4.1 promises, at the top of the path range.
    #[test]
    fn the_promised_flood_sizes_hold_at_the_longest_legal_path() {
        assert_eq!(FLOOD_RECORD_BASE, 161);
        assert_eq!(RecordBudget::flood_scoped(MAX_PATH_BYTES).limit(), 93);
        assert_eq!(RecordBudget::flood_scoped(32).limit(), 125);
        // Unscoped, the same path buys the four bytes the transport codes would have taken.
        assert_eq!(RecordBudget::flood_unscoped(MAX_PATH_BYTES).limit(), 97);
    }

    /// A flood is usable well below [`RECORD_FLOOR`], and a direct path below it is not usable at
    /// all.
    ///
    /// The floor is `RECORD_MAX − MAX_PATH_BYTES`, so it is exactly the direct limit at the longest
    /// legal path: no legal direct path can fall under it, and one that does is not a path. A flood
    /// at 93 bytes is the plan's own promise, so clamping it up to 108 would build a packet the
    /// receiver drops.
    #[test]
    fn the_floor_is_a_direct_verdict_and_never_clamps_a_flood() {
        assert_eq!(RecordBudget::direct(MAX_PATH_BYTES).limit(), RECORD_FLOOR);
        assert_eq!(
            RecordBudget::direct(MAX_PATH_BYTES).usable_limit(),
            Some(RECORD_FLOOR)
        );
        for path_out in 0..=MAX_PATH_BYTES {
            assert!(RecordBudget::direct(path_out).usable_limit().is_some());
        }
        // A path beyond the firmware's ceiling cannot happen; the answer if one is handed in anyway
        // is "no usable path", never a silently truncated record.
        assert_eq!(RecordBudget::direct(MAX_PATH_BYTES + 1).usable_limit(), None);
        assert_eq!(
            RecordBudget::flood_scoped(MAX_PATH_BYTES).usable_limit(),
            Some(FLOOD_RECORD_GUARANTEED)
        );
    }

    /// Every budget this module can produce is one the firmware will carry and report; the check is
    /// that the two acceptance tests agree with the limit everywhere, which is what makes `limit`
    /// safe to hand to a builder.
    #[test]
    fn the_limit_is_always_accepted_by_both_ends() {
        for path_out in 0..=MAX_PATH_BYTES {
            let budget = RecordBudget::direct(path_out);
            assert!(budget.sender_accepts(budget.limit()));
        }
        for path_arrival in 0..=MAX_PATH_BYTES {
            for budget in [
                RecordBudget::flood_scoped(path_arrival),
                RecordBudget::flood_unscoped(path_arrival),
            ] {
                assert!(
                    budget.sender_accepts(budget.limit()),
                    "a flood at arrival path {path_arrival} cannot be written"
                );
            }
        }
    }
}
