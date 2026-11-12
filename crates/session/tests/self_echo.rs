//! The self-echo filter, run against a real mesh rather than against a hand-built record.
//!
//! `RC-3` was not a parsing bug and could not have been caught by one: it required a repeater to
//! forward a packet **back to the node that sent it**, which is what half-duplex hardware and a
//! broadcast forward do.
//!
//! What makes it visible to an application at all is the receive tap. `Dispatcher::checkRecv` calls
//! `logRxRaw(...)` with the raw bytes of **every** packet the radio demodulated
//! (`src/Dispatcher.cpp:199`), before parsing, before deduplication, and before any routing decision
//! — and it is that `0x88` push, not the router, that ldmux reads records from. So a node's own echo
//! and a packet in transit to a neighbour both arrive at the application as ordinary records, and
//! both have to be refused by this end because the firmware will never refuse them for us.
//!
//! That is why these tests read [`VirtualMesh::log`] rather than [`VirtualMesh::deliveries`]: the log
//! is one entry per reception at one node, whatever the router then did with it, which is the
//! application's own view.
//!
//! The other firmware detail the harness reproduces and these tests depend on: a direct packet's
//! path lists the **repeaters to cross**, and each repeater consumes its own hash
//! (`routeRecvPacket` + `removeSelfFromPath`, `src/Mesh.cpp:78-120`). So a sender's own echo comes
//! back either with every hop consumed (the last repeater's forward) or with hops still left (an
//! earlier repeater's), and only the two rules together reject both.

use ldmux_mesh::sim::{NodeId, Transmission, VirtualMesh, NodeSpec};
use ldmux_session::direction::{Arrival, KIND_DOWN, KIND_UP, Side, Verdict, admit};

/// A record's first bytes, enough to be recognisable in a log entry's length.
fn record(kind: u8) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    out
}

/// Every reception at `node`, in the order it happened: what the radio handed to the application.
fn heard(mesh: &VirtualMesh, node: NodeId) -> Vec<&Transmission> {
    mesh.log().iter().filter(|t| t.to == node).collect()
}

/// The arrival shape an application builds from a reception, which is all it knows: the carrier's hop
/// count and whether it came by flood.
fn arrival(reception: &Transmission) -> Arrival {
    if reception.flood {
        Arrival::Flood {
            hops: reception.hops,
        }
    } else {
        Arrival::Direct {
            hops: reception.hops,
        }
    }
}

/// The phone sends a direct record across one repeater. Both the repeater's forward to the host and
/// its forward back to the phone carry the same bytes at **zero hops**, so only the direction rule
/// separates them.
#[test]
fn a_one_repeater_echo_arrives_at_zero_hops_and_is_caught_by_kind() {
    let mut mesh = VirtualMesh::new(7);
    let phone = mesh.add_node(NodeSpec::companion());
    // A pinned forward delay keeps the run independent of the seed; the firmware draws one, and a
    // test that depended on the draw would pass or fail for a reason it is not about.
    let repeater = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(50));
    let host = mesh.add_node(NodeSpec::companion());
    mesh.link(phone, repeater, 9.0);
    mesh.link(repeater, host, 9.0);

    let path = mesh.path_through(&[repeater]);
    let payload = record(KIND_UP);
    mesh.send_direct(phone, &path, &payload);
    mesh.advance(5_000);

    let back_to_phone = heard(&mesh, phone);
    assert_eq!(
        back_to_phone.len(),
        1,
        "the repeater did not forward the phone's own packet back to it: {back_to_phone:?}"
    );
    let echo = back_to_phone[0];
    assert_eq!(echo.hops, 0, "the echo arrived with a path: {echo:?}");
    assert_eq!(
        admit(Side::Phone, KIND_UP, arrival(echo)),
        Verdict::SelfEcho,
        "the phone read its own transmission back as a peer's record"
    );

    // The same bytes at the host are the genuine arrival the rule exists to let through.
    let at_host = heard(&mesh, host);
    assert_eq!(at_host.len(), 1, "the record never reached the host: {at_host:?}");
    let genuine = at_host[0];
    assert_eq!(genuine.hops, 0);
    assert_eq!(
        admit(Side::Host, KIND_UP, arrival(genuine)),
        Verdict::Admit
    );
}

/// With two repeaters the sender hears the first one's forward with a hop **left in its path**, which
/// is the other shape of the same defect: a packet still in transit, read as an arrival.
///
/// The firmware's router discards it (`NotThisHop`), and the application is told about it anyway,
/// because the receive tap runs first. The field log's `unknown record kind` came from exactly this.
#[test]
fn a_two_repeater_echo_arrives_in_transit_and_is_caught_by_the_path() {
    let mut mesh = VirtualMesh::new(11);
    let phone = mesh.add_node(NodeSpec::companion());
    let first = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(40));
    let second = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(40));
    let host = mesh.add_node(NodeSpec::companion());
    mesh.link(phone, first, 8.0);
    mesh.link(first, second, 8.0);
    mesh.link(second, host, 8.0);
    // The phone is linked to the first repeater only, so the one thing it can hear is that
    // repeater's forward of its own packet. The host is linked to both repeaters, which is an
    // ordinary mesh: it hears the first repeater's forward (still in transit) as well as the
    // second's (the one addressed to it).
    mesh.link(first, host, 8.0);

    let path = mesh.path_through(&[first, second]);
    mesh.send_direct(phone, &path, &record(KIND_UP));
    mesh.advance(5_000);

    let back_to_phone = heard(&mesh, phone);
    assert_eq!(
        back_to_phone.len(),
        1,
        "the first repeater's forward never came back to the phone: {back_to_phone:?}"
    );
    let echo = back_to_phone[0];
    assert_eq!(echo.hops, 1, "expected one hop left in the echoed path: {echo:?}");
    // The kind rule settles the phone's case before the hop count is even consulted, which is the
    // point of having it: direction is a cheaper question than transit.
    assert_eq!(
        admit(Side::Phone, KIND_UP, arrival(echo)),
        Verdict::SelfEcho
    );

    // The destination is where the transit rule earns its place. A host in the same mesh hears the
    // first repeater's forward too — bytes identical to the record addressed to it, one hop short of
    // arrival — and only the path can tell the two apart. This is `RC-3` in its exact field shape:
    // the record was stamped as an inbound peer record before anything looked at its path.
    let at_host = heard(&mesh, host);
    let in_transit = at_host
        .iter()
        .find(|t| t.hops == 1)
        .expect("the host never heard the first repeater's forward");
    assert_eq!(
        admit(Side::Host, KIND_UP, arrival(in_transit)),
        Verdict::InTransit,
        "a packet still crossing its path was read as an arrival"
    );

    // And the arrival itself, with every hop consumed by the repeater that forwarded it.
    let genuine = at_host
        .iter()
        .find(|t| t.hops == 0)
        .expect("the host never saw a fully consumed path");
    assert_eq!(
        admit(Side::Host, KIND_UP, arrival(genuine)),
        Verdict::Admit
    );
}

/// The mirror case on the other side: a host's downlink record, echoed by a repeater, is a kind the
/// host originates, so the direction rule catches it there too — before the host could mistake it for
/// an uplink record that happens to share a byte value.
#[test]
fn a_host_reading_its_own_downlink_record_is_caught_by_kind() {
    let mut mesh = VirtualMesh::new(3);
    let host = mesh.add_node(NodeSpec::companion());
    let repeater = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(30));
    let phone = mesh.add_node(NodeSpec::companion());
    mesh.link(host, repeater, 9.0);
    mesh.link(repeater, phone, 9.0);

    let path = mesh.path_through(&[repeater]);
    mesh.send_direct(host, &path, &record(KIND_DOWN));
    mesh.advance(5_000);

    let back_to_host = heard(&mesh, host);
    assert_eq!(
        back_to_host.len(),
        1,
        "the repeater did not forward the host's own packet back to it: {back_to_host:?}"
    );
    assert_eq!(
        admit(Side::Host, KIND_DOWN, arrival(back_to_host[0])),
        Verdict::SelfEcho
    );

    let at_phone = heard(&mesh, phone);
    assert!(!at_phone.is_empty(), "the downlink record never reached the phone");
    assert_eq!(
        admit(Side::Phone, KIND_DOWN, arrival(at_phone[0])),
        Verdict::Admit
    );
}
