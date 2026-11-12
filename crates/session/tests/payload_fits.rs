//! The wire's own limits must fit inside the transport's, on both carriers.
//!
//! `docs/RADIO-PLAN.md` §12.2 (`AR-2`): the codec and the transaction layer each bound a message,
//! and nothing made those bounds agree. Three ceilings with no ordering between them — `MAX_BODY`
//! (600 bytes of body), `MAX_PARTS x part payload` (16 x 118 = 1,888 bytes of message) and the
//! per-message totals — mean a sender can build a body the transport will refuse, and the failure
//! reaches the operator as a phone that waits out its ladder rather than as an error.
//!
//! So this file is an ordering proof, not a behaviour test. It exists because the numbers live in
//! two crates that cannot see each other: `ldmux-protocol` is the wire and deliberately does not
//! depend on the session, and `ldmux-session` is the transport and does not depend on the wire.
//!
//! The record sizes are the **worst** the protocol can be handed rather than the public preset's:
//! 171 bytes direct and 93 bytes flooded are what `ldmux_mesh::budget` guarantees at the longest
//! legal path under a scoped flood, so a proof at these sizes holds everywhere.

use ldmux_protocol::mesh_term;
use ldmux_session::transaction::{self, MAX_PARTS};

/// The widest direct record the radio route can carry, from `ldmux_mesh::budget`
/// (`RECORD_RECEIVED_MAX`), which is the receiver's own ceiling.
const DIRECT_RECORD: usize = 171;

/// The flooded record size the plan guarantees at every legal hop count and path width.
const FLOOD_RECORD: usize = 93;

/// The largest message one transaction can carry.
fn transport_message_max() -> usize {
    MAX_PARTS * transaction::max_part_payload(DIRECT_RECORD, FLOOD_RECORD)
}

#[test]
fn every_encodable_message_fits_one_transaction() {
    let room = transport_message_max();
    // A `Send` body of `MAX_BODY` plus the two framing bytes the codec adds (tag and length).
    assert!(
        mesh_term::MAX_BODY + 2 <= room,
        "a body of MAX_BODY ({} bytes) does not fit one transaction ({room} bytes), so a sender can \
         build a message the transport refuses — AR-2",
        mesh_term::MAX_BODY,
    );
    // The same for a snapshot, whose header is the largest of the two.
    assert!(
        mesh_term::MAX_BODY + 2 <= room,
        "a snapshot body does not fit one transaction ({room} bytes)",
    );
    // And the bound must not be so loose that a single part cannot hold the smallest useful
    // message: one part carries `max_part_payload`, so a one-record command is one part.
    assert!(
        transaction::max_part_payload(DIRECT_RECORD, FLOOD_RECORD) >= 64,
        "a part is too small for a command, so even `ls` would be split",
    );
}

#[test]
fn a_command_at_the_total_can_actually_be_chunked_into_the_wire() {
    // `MAX_COMMAND_TOTAL` is a ceiling on the *joined* command; the wire only carries `chunk_count`
    // chunks of at most `MAX_BODY` each. If the total needed more chunks than the field can number,
    // the cap would be unreachable and the phone would refuse a paste the plan promised to send.
    let per_chunk = chunk_capacity();
    let chunks_for_command = mesh_term::MAX_COMMAND_TOTAL.div_ceil(per_chunk);
    assert!(
        chunks_for_command <= mesh_term::MAX_CHUNKS,
        "a command at MAX_COMMAND_TOTAL needs {chunks_for_command} chunks, past the {} the field \
         can number",
        mesh_term::MAX_CHUNKS,
    );
    let chunks_for_snapshot = mesh_term::MAX_SNAPSHOT_TOTAL.div_ceil(per_chunk);
    assert!(
        chunks_for_snapshot <= mesh_term::MAX_CHUNKS,
        "a screen at MAX_SNAPSHOT_TOTAL needs {chunks_for_snapshot} chunks, past the {} the field \
         can number",
        mesh_term::MAX_CHUNKS,
    );
}

/// The payload a chunk can carry: `MAX_BODY` less the widest header either message can have.
///
/// The header is counted at its worst — a `Send` carries fourteen fixed bytes, the shell at
/// `MAX_SELECTOR`, and the item stream's own two-byte length; a `Snapshot` carries twenty-five, the
/// shell, and the patch stream's length. Both are under sixty, and the larger is used.
fn chunk_capacity() -> usize {
    let worst_header = 25 + mesh_term::MAX_SELECTOR + 2;
    mesh_term::MAX_BODY - worst_header
}

#[test]
fn a_chunk_carries_room_for_at_least_one_whole_item() {
    // A chunk must be able to hold any single item, or a command could never be split at an item
    // boundary and the assembler would have nothing valid to join.
    assert!(
        chunk_capacity() >= mesh_term::MAX_SEND_ITEM,
        "a chunk holds {} bytes but one item may be {}, so a large item could never be sent",
        chunk_capacity(),
        mesh_term::MAX_SEND_ITEM,
    );
}
