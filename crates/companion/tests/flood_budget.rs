//! The record budget's flood arithmetic, pinned against the packet builder that spends it.
//!
//! `ldmux_mesh::budget` computes a flooded record's ceiling as
//! `RECEIVE_MAX − PACKET_HEADER − FLOOD_WRAPPER − scope − path_arrival`. Three of those numbers are
//! facts about the companion protocol rather than about any one crate, so the crate that owns the
//! vocabulary states them — and the crate that actually **builds** the packet is the only thing that
//! can say whether they are true. This file is that check.
//!
//! Why it matters: `RC-6` was a budget that disagreed with the wire by thirteen bytes. A wrapper
//! that grew a field, or a header that stopped being one byte, would make every flood one packet
//! larger than the sender believed, and the loss would be silent on both ends.

use ldmux_companion::codec::{
    FloodScope, build_record_flood_in_scope, parse_record_flood,
};
use ldmux_mesh::budget::{
    COMPANION_FRAME_MAX, FLOOD_WRAPPER, LOG_RX_HEADER, PACKET_HEADER, RecordBudget, SCOPE_BYTES,
};

const PEER: [u8; 32] = [0x11; 32];
const OWN: [u8; 32] = [0x22; 32];

fn scope() -> FloodScope {
    FloodScope {
        name: "test".to_owned(),
        key: [0x33; 16],
    }
}

/// The bytes the builder adds around a record are exactly [`FLOOD_WRAPPER`], unscoped.
#[test]
fn the_flood_wrapper_is_the_ten_bytes_the_budget_deducts() {
    let record = vec![0x5A; 100];
    let nonce = [1, 2, 3, 4];
    let packet = build_record_flood_in_scope(1, &PEER, &OWN, None, &nonce, &record).expect("builds");
    let overhead = packet.len() - record.len();
    assert_eq!(
        overhead,
        PACKET_HEADER + FLOOD_WRAPPER,
        "the builder spends {overhead} bytes on header and wrapper, the budget assumes {}",
        PACKET_HEADER + FLOOD_WRAPPER
    );
}

/// And a scoped flood costs exactly [`SCOPE_BYTES`] more, which is what the four-byte deduction in
/// [`RecordBudget::flood_scoped`] is for.
#[test]
fn a_scoped_flood_costs_exactly_the_four_transport_code_bytes() {
    let record = vec![0x5A; 100];
    let nonce = [1, 2, 3, 4];
    let unscoped =
        build_record_flood_in_scope(1, &PEER, &OWN, None, &nonce, &record).expect("builds");
    let scoped =
        build_record_flood_in_scope(1, &PEER, &OWN, Some(&scope()), &nonce, &record).expect("builds");
    assert_eq!(scoped.len() - unscoped.len(), SCOPE_BYTES);
}

/// The arithmetic closes: a record of [`RecordBudget::flood_scoped`]'s limit at the longest legal
/// arrival path is a packet the receiver will report, and one byte more is not.
///
/// The builder writes the packet with an **empty** path (a flood grows its own), so this drives the
/// receive side by adding the arrival path the way a chain of repeaters would, and then checks the
/// same `len + 3 ≤ 176` filter `MyMesh::logRxRaw` applies.
#[test]
fn a_flood_at_its_budget_passes_the_receive_filter_and_one_byte_more_does_not() {
    let longest = ldmux_mesh::budget::MAX_PATH_BYTES;
    let budget = RecordBudget::flood_scoped(longest);
    let limit = budget.limit();
    assert_eq!(limit, 93);

    let nonce = [1, 2, 3, 4];
    let record = vec![0x5A; limit];
    let packet =
        build_record_flood_in_scope(1, &PEER, &OWN, Some(&scope()), &nonce, &record).expect("builds");
    // The path the flood will have accumulated by the time it arrives: the builder writes an empty
    // one, and each repeater appends a hash.
    let arrived = packet.len() + longest;
    assert!(
        arrived + LOG_RX_HEADER <= COMPANION_FRAME_MAX,
        "a record at the budget becomes a {arrived}-byte packet, past the {} a radio reports",
        COMPANION_FRAME_MAX - LOG_RX_HEADER
    );

    let over = build_record_flood_in_scope(1, &PEER, &OWN, Some(&scope()), &nonce, &vec![0x5A; limit + 1])
        .expect("builds");
    assert!(
        over.len() + longest + LOG_RX_HEADER > COMPANION_FRAME_MAX,
        "one byte over the budget still passes the receive filter"
    );
}

/// The record inside a flood is readable back out, so the wrapper and the parser agree about the
/// same ten bytes.
#[test]
fn the_parser_finds_the_record_the_builder_put_there() {
    let record = vec![0x5A; 93];
    let nonce = [9, 9, 9, 9];
    let packet =
        build_record_flood_in_scope(1, &PEER, &OWN, None, &nonce, &record).expect("builds");
    // `PEER` is the destination the builder addressed it to, which is the key the parser looks for.
    let parsed = parse_record_flood(&packet, &PEER).expect("parses");
    assert_eq!(parsed.record, &record[..]);
}
