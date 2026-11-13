//! The tests the harness exists for.
//!
//! Each one pins a field failure that no unit test could see, because each is a property of the air
//! and the repeaters rather than of a function. The name of the defect is in the test's name and in
//! its first line, so a failure says which behaviour broke rather than which assertion fired.

use super::*;

/// The ten bytes a flooded session record wraps around its record:
/// `[dest hash][source hash]["LDMR"][nonce]` (`build_record_flood_in_scope`,
/// `crates/companion/src/codec.rs`).
const FLOOD_WRAPPER: usize = 10;

/// A flood payload of the size a real one has when it carries a `record_len`-byte record.
fn flood_payload(record_len: usize) -> Vec<u8> {
    vec![0x5A; FLOOD_WRAPPER + record_len]
}

/// The mesh three nodes make: a companion, a repeater, a companion.
fn companion_repeater_companion(seed: u64) -> (VirtualMesh, NodeId, NodeId, NodeId) {
    let mut mesh = VirtualMesh::new(seed);
    let a = mesh.add_node(NodeSpec::companion());
    let r = mesh.add_node(NodeSpec::repeater());
    let b = mesh.add_node(NodeSpec::companion());
    mesh.link(a, r, 8.0);
    mesh.link(r, b, 8.0);
    (mesh, a, r, b)
}

/// `RC-6`: a flood longer than 173 bytes is transmitted, forwarded, heard, and never handed up —
/// because the receive log refuses the frame, not because a flood has no delivery path. A flood that
/// fits is delivered as `0x88` (`src/Dispatcher.cpp:199`, `MyMesh.cpp:291-297`).
///
/// The 162-byte record the old sender believed was legal is 174 bytes on the air unscoped
/// (`2 + 0 path + 10 wrapper + 162`), and `MyMesh::logRxRaw` refuses the frame when
/// `len + 3 > 176`. Nothing reports it on either end: the sender was told the record went, and the
/// receiver's application simply never sees it. One byte shorter arrives.
#[test]
fn rc6_a_flood_past_the_receive_ceiling_is_silently_lost() {
    let mut mesh = VirtualMesh::new(1);
    let a = mesh.add_node(NodeSpec::companion());
    let b = mesh.add_node(NodeSpec::companion());
    mesh.link(a, b, 10.0);

    // 162-byte record: 174 bytes on the air, three over what the interface will report.
    mesh.send_flood(a, &flood_payload(162));
    mesh.advance(2_000);
    let too_long = mesh.log().to_vec();
    assert!(
        too_long
            .iter()
            .any(|t| t.outcome == Outcome::DroppedCeiling),
        "a 174-byte flood was accepted; the log says {:?}",
        too_long.iter().map(|t| t.outcome).collect::<Vec<_>>()
    );
    assert!(
        mesh.deliveries().is_empty(),
        "the oversized flood reached an application anyway"
    );

    // 161 bytes of record: exactly 173 on the air, the last size that fits.
    mesh.send_flood(a, &flood_payload(161));
    mesh.advance(2_000);
    assert_eq!(
        mesh.deliveries().len(),
        1,
        "a 173-byte flood, which is the last size the interface reports, was lost too"
    );
}

/// `RC-3`: a sender that routes through one repeater hears the repeater's own retransmission back,
/// with the path consumed, and cannot tell it from a genuine zero-hop arrival.
///
/// `CMD_SEND_RAW_PACKET` (`examples/companion_radio/MyMesh.cpp:2047`) calls `sendPacket` without
/// `markSeen`, so the origin's deduplication table never holds its own packet. The repeater consumes
/// its hash from the front of the path (`removeSelfFromPath`, `src/Mesh.cpp:334`) before
/// retransmitting, so what comes back has an **empty** path — indistinguishable from a packet that
/// crossed nothing at all. Every multi-hop direct send therefore looks like a working zero-hop link.
#[test]
fn rc3_a_senders_own_packet_returns_as_a_zero_hop_arrival() {
    let (mut mesh, a, r, b) = companion_repeater_companion(2);
    let path = mesh.path_through(&[r]);
    mesh.send_direct(a, &path, &[0x11; 40]);
    mesh.advance(5_000);

    let back = mesh
        .deliveries()
        .iter()
        .find(|d| d.to == a)
        .unwrap_or_else(|| {
            panic!(
                "the sender never heard its own packet back; deliveries were {:?}",
                mesh.deliveries()
                    .iter()
                    .map(|d| (d.to, d.hops))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        back.hops, 0,
        "the echo arrived with a path, so it could be told apart from a direct one"
    );
    assert!(
        back.path.is_empty(),
        "the repeater left its hash in the path instead of consuming it"
    );
    assert!(
        mesh.deliveries().iter().any(|d| d.to == b),
        "the far end did not receive the packet at all, so there is no echo to mistake"
    );
}

/// `RC-1`: the packet after another one is lost at the first repeater, because the repeater is still
/// transmitting the first one's retransmission when the second arrives.
///
/// This is the self-jamming that costs a connect: the phone fires a precursor flood and then the
/// handshake twenty milliseconds later, its own radio serialises them into back-to-back
/// transmissions, and the first repeater is deaf for the second — exactly the half that had to
/// arrive. The control at the end proves the loss is the collision and not the loss model.
#[test]
fn rc1_the_packet_after_another_is_swallowed_at_the_first_repeater() {
    // A pinned forward delay, because `getRetransmitDelay` draws from a window and a test that has
    // to prove one specific collision cannot hinge on the draw. 100 ms is inside the firmware's
    // own range for a packet of this size at this preset.
    let mut mesh = VirtualMesh::new(3);
    let a = mesh.add_node(NodeSpec::companion());
    let r = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(100));
    let b = mesh.add_node(NodeSpec::companion());
    mesh.link(a, r, 8.0);
    mesh.link(r, b, 8.0);

    let first = flood_payload(30);
    // The two packets in the field are different packets — a precursor flood and then the
    // handshake — so they are different payloads here. Identical ones would be deduplicated and the
    // collision would never be reached, which would test the wrong mechanism.
    let second = flood_payload(31);
    let airtime = mesh.airtime_of(2 + first.len()).round() as u64;
    assert!(airtime > 20, "the preset is wrong: one packet is {airtime} ms");

    mesh.send_flood(a, &first);
    // "Twenty milliseconds later" is what the app asks for; the radio serialises it into the first
    // slot it has free, which is the instant the first packet left.
    mesh.advance(20);
    mesh.send_flood(a, &second);
    mesh.advance(20 * airtime);

    let deaf = mesh
        .log()
        .iter()
        .any(|t| t.to == r && t.outcome == Outcome::DeafWhileTransmitting);
    assert!(
        deaf,
        "the repeater was listening throughout, so nothing collided; outcomes at the repeater were {:?}",
        mesh.log()
            .iter()
            .filter(|t| t.to == r)
            .map(|t| t.outcome)
            .collect::<Vec<_>>()
    );

    // Control: the same two packets with the second held back beyond the repeater's whole forward
    // window. Both arrive, so the loss above was the collision.
    let mut spaced = VirtualMesh::new(3);
    let sa = spaced.add_node(NodeSpec::companion());
    let sr = spaced.add_node(NodeSpec::repeater().with_forward_delay_ms(100));
    let sb = spaced.add_node(NodeSpec::companion());
    spaced.link(sa, sr, 8.0);
    spaced.link(sr, sb, 8.0);
    spaced.send_flood(sa, &first);
    spaced.advance(3 * airtime);
    spaced.send_flood(sa, &second);
    spaced.advance(20 * airtime);
    assert!(
        spaced.log().iter().all(|t| t.outcome != Outcome::DeafWhileTransmitting),
        "spacing the packets apart did not help, so the harness is losing them for another reason"
    );
    // Counted at the far companion only: the origin also hears its own flood come back off the
    // first repeater, because `CMD_SEND_RAW_PACKET` never marks the packet seen. That is `RC-3`, and
    // it is the harness working rather than a second delivery.
    let far: Vec<&Delivery> = spaced
        .deliveries()
        .iter()
        .filter(|d| d.to == sb)
        .collect();
    assert_eq!(
        far.len(),
        2,
        "both spaced packets should reach the far end; got {:?}",
        spaced
            .deliveries()
            .iter()
            .map(|d| (d.to, d.hops))
            .collect::<Vec<_>>()
    );
}

/// `RC-5`: the same bytes sent twice is one packet. The second is dropped by the deduplication ring,
/// at the repeater and at the far end, which is why every retransmission has to carry a fresh nonce.
#[test]
fn rc5_byte_identical_copies_are_dropped_as_duplicates() {
    let (mut mesh, a, _r, _b) = companion_repeater_companion(4);
    let payload = flood_payload(80);

    mesh.send_flood(a, &payload);
    mesh.advance(5_000);
    let after_first = mesh.deliveries().len();

    mesh.send_flood(a, &payload);
    mesh.advance(5_000);

    assert!(
        mesh.log()
            .iter()
            .any(|t| t.outcome == Outcome::DroppedDedup),
        "the second identical copy was forwarded again; deduplication did not happen"
    );
    assert_eq!(
        mesh.deliveries().len(),
        after_first,
        "the duplicate reached an application as a second arrival"
    );
}

/// The property the whole harness rests on: a script and a seed decide the run, so a failure
/// reproduces and a passing test means something.
#[test]
fn a_run_is_a_function_of_its_seed() {
    let script = |seed: u64| {
        let (mut mesh, a, r, _b) = companion_repeater_companion(seed);
        mesh.set_loss(0.4);
        let path = mesh.path_through(&[r]);
        for round in 0..6usize {
            mesh.send_direct(a, &path, &vec![0x22; 60 + round]);
            mesh.send_flood(a, &flood_payload(90 + round));
            mesh.advance(2_000);
        }
        mesh.log().to_vec()
    };
    assert_eq!(script(9), script(9), "the same seed produced a different run");

    let differing = script(10);
    let baseline = script(9);
    assert_ne!(
        baseline, differing,
        "two seeds produced identical runs with a quarter of packets lost, so the seed is not wired in"
    );
}

/// A flood's path is the chain of hashes every repeater appended, so its **reverse** is the route
/// home. Anchoring that here is what lets a routing decision trust a flood's path instead of
/// guessing at it.
#[test]
fn a_floods_path_is_the_chain_it_travelled() {
    let mut mesh = VirtualMesh::new(5);
    let a = mesh.add_node(NodeSpec::companion());
    let r1 = mesh.add_node(NodeSpec::repeater());
    let r2 = mesh.add_node(NodeSpec::repeater());
    let b = mesh.add_node(NodeSpec::companion());
    mesh.link(a, r1, 8.0);
    mesh.link(r1, r2, 8.0);
    mesh.link(r2, b, 8.0);

    mesh.send_flood(a, &flood_payload(40));
    mesh.advance(10_000);

    let at_b = mesh
        .deliveries()
        .iter()
        .find(|d| d.to == b)
        .expect("the flood never reached the far end");
    assert_eq!(at_b.hops, 2, "two repeaters is two hashes");
    assert_eq!(
        at_b.path,
        mesh.path_through(&[r1, r2]),
        "the received path is not the chain the packet travelled"
    );
}

/// A scoped flood is admitted only by a repeater that carries its region. The sender is told
/// nothing, which is why a site's private scope is indistinguishable from distance.
#[test]
fn a_scoped_flood_is_dropped_by_a_repeater_without_its_region() {
    let mut mesh = VirtualMesh::new(6);
    let a = mesh.add_node(NodeSpec::companion());
    let without = mesh.add_node(NodeSpec::repeater());
    let with = mesh.add_node(NodeSpec::repeater().with_regions(&[7]));
    mesh.link(a, without, 8.0);
    mesh.link(a, with, 8.0);

    mesh.send_flood_scoped(a, PAYLOAD_TYPE_RAW_CUSTOM, 7, &flood_payload(40));
    mesh.advance(10_000);

    assert!(
        mesh.log()
            .iter()
            .any(|t| t.to == without && t.outcome == Outcome::DroppedScope),
        "a repeater forwarded a scoped flood it cannot resolve"
    );
    assert!(
        mesh.log()
            .iter()
            .any(|t| t.to == with && t.outcome == Outcome::Delivered),
        "the repeater that carries the region refused the flood too"
    );

    // The same bytes unscoped are admitted by both, so the scope is the whole of the difference.
    let mut unscoped = VirtualMesh::new(6);
    let ua = unscoped.add_node(NodeSpec::companion());
    let ub = unscoped.add_node(NodeSpec::repeater());
    unscoped.link(ua, ub, 8.0);
    unscoped.send_flood(ua, &flood_payload(40));
    unscoped.advance(10_000);
    assert!(
        unscoped
            .log()
            .iter()
            .all(|t| t.outcome != Outcome::DroppedScope),
        "an unscoped flood was refused for scope"
    );
}

/// A direct packet goes only to the next hash in its path. A node that is not that hash forwards
/// nothing, which is what keeps a direct send a single chain rather than a broadcast.
#[test]
fn a_direct_packet_is_not_forwarded_by_a_node_that_is_not_the_next_hop() {
    let mut mesh = VirtualMesh::new(7);
    let a = mesh.add_node(NodeSpec::companion());
    let byway = mesh.add_node(NodeSpec::repeater());
    let onward = mesh.add_node(NodeSpec::repeater());
    let b = mesh.add_node(NodeSpec::companion());
    mesh.link(a, byway, 8.0);
    mesh.link(a, onward, 8.0);
    mesh.link(onward, b, 8.0);

    // The path names `onward`, so `byway` hears the packet and must not relay it.
    let path = mesh.path_through(&[onward]);
    mesh.send_direct(a, &path, &[0x33; 40]);
    mesh.advance(10_000);

    assert!(
        mesh.log()
            .iter()
            .any(|t| t.to == byway && t.outcome == Outcome::NotThisHop),
        "the byway node relayed a packet that named another node"
    );
    assert!(
        mesh.deliveries().iter().any(|d| d.to == b),
        "the packet never reached its named next hop"
    );
}

/// An advert is capped at eight hops while an ordinary flood is capped at sixty-four, so an advert
/// cannot be the way a distant peer is found. That asymmetry is the reason discovery is a request
/// flood and not an advert.
#[test]
fn an_advert_stops_at_its_own_hop_cap() {
    let mut mesh = VirtualMesh::new(8);
    let a = mesh.add_node(NodeSpec::companion());
    let mut previous = a;
    // Nine repeaters in a line: an advert is forwarded while its hop count is below eight, so it
    // reaches the eighth repeater and stops there.
    for _ in 0..9 {
        let r = mesh.add_node(NodeSpec::repeater());
        mesh.link(previous, r, 8.0);
        previous = r;
    }
    let far = mesh.add_node(NodeSpec::companion());
    mesh.link(previous, far, 8.0);

    mesh.send_flood_as(a, PAYLOAD_TYPE_ADVERT, &flood_payload(40));
    mesh.advance(60_000);

    let stopped = mesh
        .log()
        .iter()
        .any(|t| t.outcome == Outcome::DroppedHopLimit);
    assert!(
        stopped,
        "an advert crossed nine hops; the eight-hop cap is not modelled"
    );
    assert!(
        mesh.deliveries().iter().all(|d| d.to != far),
        "an advert reached the far end past its hop cap"
    );
}
