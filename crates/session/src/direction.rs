//! Who may send which record kind, and when an arriving packet is ours to read at all.
//!
//! The radio is half duplex and a repeater forwards to **every** neighbour it hears from, including
//! the node that originated the packet (`forward`, `src/Mesh.cpp`). So an originating radio hears
//! its own transmission come back, once per repeater in range, and the firmware offers it to the
//! application like any other packet. Two things follow, and the field session of 2026-11-10 broke
//! on both:
//!
//! * **A direct packet with bytes left in its path is still travelling.** `ROUTE_TYPE_DIRECT` names
//!   the next hops it must cross, and a repeater consumes only its own hash
//!   (`removeSelfFromPath`, `src/Mesh.cpp:344`), so a packet heard with a non-empty path is in
//!   transit to somebody else. Accepting it is `RC-3`: every multi-hop direct send was read back by
//!   its own sender as an inbound peer record, which stamped `directWorks`, `last_peer_record` and
//!   the route *before* any cryptographic check, and produced `Record(BadTag)` and
//!   `unknown record kind` from the echoed bytes.
//! * **And a single-repeater echo arrives with an empty path**, so the transit rule alone cannot
//!   catch it. What catches that is direction: a record's kind says which end originates it, and an
//!   end that receives its own kind is hearing itself.
//!
//! ## The kinds
//!
//! Six, in two groups of three. The numbers are the record's first byte, on the wire and inside the
//! `LDMR` wrapper alike, so the filter runs before any decryption:
//!
//! | Kind | Value | Sent by | Answers |
//! |---|---|---|---|
//! | `KIND_MSG1` | `0x11` | phone | the Noise initiation, `e` and the sealed static |
//! | `KIND_MSG1_FRAG` | `0x14` | phone | the same 121 bytes in two parts, when the carrier cannot hold it whole |
//! | `KIND_UP` | `0x13` | phone | a transaction header and payload |
//! | `KIND_MSG2` | `0x12` | host | the Noise response |
//! | `KIND_NEED_HANDSHAKE` | `0x15` | host | a PSK-tagged request to start again |
//! | `KIND_DOWN` | `0x16` | host | a transaction header and payload |
//!
//! Splitting the transport kind in two is what makes the direction rule total. With one `0x13` for
//! both directions, a host could not tell its own downlink record from the phone's uplink one by
//! kind alone, and a single-repeater echo of a downlink record would have been indistinguishable
//! from a real uplink record at zero hops.
//!
//! **This is where the three numbers that already existed are defined.** `handshake.rs` re-exports
//! [`KIND_MSG1`] and [`KIND_MSG2`] rather than declaring them, and `record.rs` re-exports
//! [`KIND_UP`] as its `KIND_TRANSPORT`, so there is one definition of each and every existing path
//! still compiles.

/// The Noise initiation, phone to host. 121 bytes: a 16-byte PSK filter tag then the 104-byte
/// message.
pub const KIND_MSG1: u8 = 0x11;

/// The Noise response, host to phone.
pub const KIND_MSG2: u8 = 0x12;

/// A transport record the phone sends: transaction header and payload.
pub const KIND_UP: u8 = 0x13;

/// The same initiation in two parts, for a carrier that cannot hold 121 bytes.
pub const KIND_MSG1_FRAG: u8 = 0x14;

/// The host asking the phone to handshake again, PSK-tagged so a stranger cannot force one.
pub const KIND_NEED_HANDSHAKE: u8 = 0x15;

/// A transport record the host sends.
pub const KIND_DOWN: u8 = 0x16;

/// Which end of the link is reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// The phone, which originates up-records and initiates the handshake.
    Phone,
    /// The host, which originates down-records and answers the handshake.
    Host,
}

impl Side {
    /// The side that originates records of `kind`.
    ///
    /// `None` for a kind this protocol does not define: a newer peer's kind, or a stranger's packet
    /// that happens to be addressed to us. Such a record is not filtered here — the session layer
    /// rejects it by kind when it dispatches — because inventing a direction for it would hide a
    /// version mismatch instead of reporting one.
    #[must_use]
    pub fn originator(kind: u8) -> Option<Self> {
        match kind {
            KIND_MSG1 | KIND_MSG1_FRAG | KIND_UP => Some(Self::Phone),
            KIND_MSG2 | KIND_NEED_HANDSHAKE | KIND_DOWN => Some(Self::Host),
            _ => None,
        }
    }
}

/// How the packet that carried a record reached this end.
///
/// `hops` is the path length the carrier reported, in hops — not bytes. For a direct packet it is
/// how many hashes are **left** in its path, which is what says whether it is still travelling; for
/// a flood it is how many the path accumulated, which says nothing about whether the packet is ours.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arrival {
    /// A `RAW_CUSTOM` direct packet, with `hops` hashes left in its path.
    Direct {
        /// The unconsumed path length in hops.
        hops: u8,
    },
    /// A flood (`LDMR`), with `hops` hashes accumulated in transit.
    Flood {
        /// The accumulated path length in hops.
        hops: u8,
    },
}

impl Arrival {
    /// The reported hop count, whichever carrier it came by.
    #[must_use]
    pub fn hops(self) -> u8 {
        match self {
            Self::Direct { hops } | Self::Flood { hops } => hops,
        }
    }

    /// Whether it arrived as a flood.
    #[must_use]
    pub fn is_flood(self) -> bool {
        matches!(self, Self::Flood { .. })
    }
}

/// What an end does with an arriving record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Ours to read: hand it to the session layer, which decides what it means.
    Admit,
    /// This end originates this kind, so the packet is this end's own transmission coming back off
    /// a repeater. Dropped before anything is parsed and before any routing state is touched.
    SelfEcho,
    /// A direct packet with hops left in its path, on its way to another node.
    InTransit,
}

impl Verdict {
    /// Whether the record may be handed upward.
    #[must_use]
    pub fn admitted(self) -> bool {
        matches!(self, Self::Admit)
    }
}

/// Whether `side` may read a record of `kind` that arrived as `arrival`.
///
/// Three questions, in the order that costs least:
///
/// 1. **Direction.** A kind this end originates is this end's echo, whatever the path says.
/// 2. **Transit**, for a direct packet only. A direct packet exists to cross a named path, and one
///    that still carries hashes has not finished crossing it. A flood carries the path it *has*
///    crossed, so its hop count is not a transit test.
/// 3. Otherwise it is ours.
///
/// A direct record that arrives with **zero** hops left and the peer's kind passes all three. That
/// is the only shape that can be a genuine arrival, and it is also the only shape allowed to be
/// evidence about a route ([`crate::direction`] says nothing about proof; `ldmux_mesh::route` does).
#[must_use]
pub fn admit(side: Side, kind: u8, arrival: Arrival) -> Verdict {
    if Side::originator(kind) == Some(side) {
        return Verdict::SelfEcho;
    }
    if let Arrival::Direct { hops } = arrival
        && hops > 0
    {
        return Verdict::InTransit;
    }
    Verdict::Admit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind, from the side that may receive it and the side that may not.
    #[test]
    fn a_kind_is_read_only_by_the_end_that_does_not_originate_it() {
        let table = [
            (KIND_MSG1, Side::Host, Side::Phone),
            (KIND_MSG1_FRAG, Side::Host, Side::Phone),
            (KIND_UP, Side::Host, Side::Phone),
            (KIND_MSG2, Side::Phone, Side::Host),
            (KIND_NEED_HANDSHAKE, Side::Phone, Side::Host),
            (KIND_DOWN, Side::Phone, Side::Host),
        ];
        for (kind, reader, originator) in table {
            assert_eq!(Side::originator(kind), Some(originator), "kind {kind:#04x}");
            assert_eq!(
                admit(reader, kind, Arrival::Flood { hops: 3 }),
                Verdict::Admit,
                "kind {kind:#04x} was refused by its reader"
            );
            assert_eq!(
                admit(originator, kind, Arrival::Flood { hops: 3 }),
                Verdict::SelfEcho,
                "kind {kind:#04x} was read back by the end that sent it"
            );
        }
        assert_eq!(Side::originator(0x7F), None);
    }

    /// A direct record that has finished crossing its path is the one shape a session can read: the
    /// repeaters that carried it each removed their own hash.
    #[test]
    fn a_fully_consumed_direct_record_is_admitted() {
        assert_eq!(
            admit(Side::Host, KIND_UP, Arrival::Direct { hops: 0 }),
            Verdict::Admit
        );
        assert_eq!(
            admit(Side::Phone, KIND_DOWN, Arrival::Direct { hops: 0 }),
            Verdict::Admit
        );
    }

    /// `RC-3`: a direct record with hops left is in transit to somebody else. This is the rule that
    /// was missing, and without it every multi-hop direct send was read back by its sender.
    #[test]
    fn rc3_a_direct_record_with_hops_left_is_in_transit() {
        for hops in 1..=8 {
            assert_eq!(
                admit(Side::Host, KIND_UP, Arrival::Direct { hops }),
                Verdict::InTransit,
                "{hops} hops left was treated as an arrival"
            );
        }
    }

    /// A flood is not a transit candidate at any hop count: its path is the road it travelled, not
    /// hops still to come, and the record on it is addressed to us by the wrapper. Hop count is not
    /// a filter here — the host answered a flooded request that arrived at **29 hops** (measured
    /// 2026-11-09), which is why the ceiling is the firmware's 64 and not a figure of ours.
    #[test]
    fn a_flood_is_admitted_at_every_hop_count() {
        assert_eq!(
            admit(Side::Host, KIND_UP, Arrival::Flood { hops: 30 }),
            Verdict::Admit
        );
        assert_eq!(
            admit(Side::Phone, KIND_DOWN, Arrival::Flood { hops: 63 }),
            Verdict::Admit
        );
    }

    /// A kind this protocol does not define is not filtered by direction: the session layer reports
    /// it by kind, which is how a version mismatch is noticed rather than silently absorbed.
    #[test]
    fn an_unknown_kind_is_not_attributed_to_a_side() {
        assert_eq!(
            admit(Side::Host, 0x42, Arrival::Direct { hops: 0 }),
            Verdict::Admit
        );
        assert_eq!(Side::originator(0x42), None);
    }

    #[test]
    fn an_arrival_reports_its_own_shape() {
        assert_eq!(Arrival::Direct { hops: 4 }.hops(), 4);
        assert_eq!(Arrival::Flood { hops: 4 }.hops(), 4);
        assert!(!Arrival::Direct { hops: 4 }.is_flood());
        assert!(Arrival::Flood { hops: 4 }.is_flood());
    }
}
