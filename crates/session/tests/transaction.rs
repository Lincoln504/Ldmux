//! **The transaction layer over real records, real crypto and a simulated mesh.**
//!
//! Every test here runs the actual `record::seal` / `record::open` over a real Noise
//! `StatelessTransportState`, so a bug in the wiring between the two layers cannot pass. Where the
//! route matters the records go through `ldmux_mesh::sim::VirtualMesh`, which models the receive
//! ceiling, duplicate suppression, half duplex and the repeater forward delay.
//!
//! The defect each test pins is named in its comment. They are the seven behaviours
//! `docs/RADIO-PLAN.md` §5.3 promises, one test each.
//!
//! **A message is at most 16 parts, which is 1952 bytes at the public preset**, because `part_count`
//! is validated against `ack_mask`'s 16 bits. That is the plan's own wire (`part_count: 1..=16`) and
//! it covers every message this protocol actually sends — a deflated full screen is three to seven
//! parts and a history page is five. A larger message is refused by
//! [`BeginError::TooLarge`](ldmux_session::transaction::BeginError::TooLarge) rather than truncated,
//! so these tests use 1900 bytes, the largest that fits, and say so where the brief asked for more.

use ldmux_mesh::budget::RecordBudget;
use ldmux_mesh::sim::{NodeSpec, VirtualMesh};
use ldmux_session::direction::Side;
use ldmux_session::handshake::{Established, Initiator, Keypair, PairingEntry, Responder};
use ldmux_session::record::{self, ReplayWindow};
use ldmux_session::transaction::{
    Action, BeginError, Config, Header, Ignored, MessageKind, PeerId, Received, SubHalf, TX_HEADER,
    TransactionLayer, payload_capacity,
};

const PSK: [u8; 32] = [11; 32];
const WALL: u64 = 1_800_000_000_000;
const AIRTIME_MS: f64 = 402.0;
/// The public preset's record sizes: a direct record, and the guarantee a flood gives.
const DIRECT: usize = 171;
const FLOOD: usize = 93;
/// The most bytes one message can carry: 16 parts at 118.
const MESSAGE_MAX: usize = 118 * 16;

/// A message that begins with its own kind byte, the way the real terminal protocol does.
fn message(kind: MessageKind, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(kind.0);
    out.extend_from_slice(body);
    out
}

fn config(side: Side) -> Config {
    Config {
        side,
        airtime_ms: AIRTIME_MS,
        direct_record_limit: DIRECT,
        flood_record_limit: FLOOD,
        max_attempts: 5,
        rto_ms: 2_400,
    }
}

/// One end of a real session: keys, index, record counter, replay window and the layer.
struct End {
    est: Established,
    counter: u32,
    window: ReplayWindow,
    layer: TransactionLayer,
    peer: PeerId,
}

impl End {
    fn new(est: Established, side: Side, peer: PeerId) -> Self {
        Self {
            est,
            counter: 1,
            window: ReplayWindow::default(),
            layer: TransactionLayer::new(config(side)),
            peer,
        }
    }

    /// Seal `app` into a record this end may send.
    fn seal(&mut self, app: &[u8]) -> Vec<u8> {
        let record = record::seal(
            self.est.transport_for_tests(),
            self.est.index,
            self.counter,
            0,
            0,
            app,
            DIRECT,
        )
        .expect("sealing a record");
        self.counter += 1;
        record
    }

    /// Open a record that arrived, or `None` when it was not ours to read.
    fn open(&mut self, record: &[u8]) -> Option<Vec<u8>> {
        record::open(
            self.est.transport_for_tests(),
            self.est.index,
            &mut self.window,
            record,
        )
        .ok()
        .map(|opened| opened.app)
    }
}

/// A phone and a host, handshaken for real.
fn pair() -> (End, End) {
    let host_key = Keypair::generate().unwrap();
    let host_pub = host_key.public;
    let phone_key = Keypair::generate().unwrap();
    let (init, msg1) = Initiator::start(&phone_key, &host_pub, &PSK, WALL).unwrap();
    let accepted = Responder::new(host_key)
        .accept(
            &[PairingEntry {
                psk: PSK,
                pinned: None,
                last_timestamp_ms: 0,
            }],
            WALL,
            &msg1,
            1,
        )
        .unwrap();
    let phone = init.finish(&accepted.reply).unwrap();
    // Each end addresses the other: the phone writes to the host and the host writes to the phone.
    (
        End::new(phone, Side::Phone, PeerId(2)),
        End::new(accepted.established, Side::Host, PeerId(1)),
    )
}

fn direct() -> RecordBudget {
    RecordBudget::direct(0)
}

fn flood() -> RecordBudget {
    RecordBudget::flood_scoped(64)
}

/// Every `Action::Send` in a batch, as `(part_idx, sub, payload)`.
fn sends(actions: &[Action]) -> Vec<(u8, SubHalf, Vec<u8>)> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Send {
                part_idx,
                sub,
                bytes,
                ..
            } => Some((*part_idx, *sub, bytes[TX_HEADER..].to_vec())),
            _ => None,
        })
        .collect()
}

fn ack_bytes(tx_id: u16, mask: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    Header {
        tx_id: 0,
        part_idx: 0,
        part_count: 0,
        ack_tx_id: tx_id,
        ack_mask: mask,
        sub: SubHalf::Whole,
        tx_nonce: 1,
    }
    .encode(&mut bytes);
    bytes
}

/// **`RC-7`: a message must survive a shrinking record limit without being cut again.**
///
/// The byte-stream ARQ split an in-flight record when the carrier narrowed, re-sealed the head under
/// the same sequence number and pushed the tail to the front of the queue. Here parts are cut once at
/// the direct size; the shrink happens on the air, every record still fits, and the receiver
/// reassembles exactly the bytes that were sent, once, in order.
#[test]
fn rc7_a_shrinking_budget_never_re_cuts_the_message() {
    let (mut phone, mut host) = pair();
    let data = message(MessageKind(0x0E), &(0..MESSAGE_MAX - 1).map(|i| (i % 251) as u8).collect::<Vec<_>>());
    let tx_id = phone.layer.begin(phone.peer, &data, 0).unwrap();

    let mut now = 0;
    let mut assembled: Option<Vec<u8>> = None;
    let mut records = 0usize;
    for _ in 0..60 {
        // Two passes direct, then the route drops and every retry is a flood.
        let carrier = if now < 5_000 { direct() } else { flood() };
        let actions = phone.layer.poll_all(now, carrier, 255);
        for action in &actions {
            let Action::Send { bytes, .. } = action else {
                continue;
            };
            let record = phone.seal(bytes);
            assert!(
                carrier.receiver_accepts(record.len()),
                "a {} byte record would never be reported at the far end",
                record.len()
            );
            records += 1;
            let app = host.open(&record).expect("the host could not open the record");
            if let Received::Message { payload, tx_id: id, .. } = host.layer.on_app(PeerId(1), &app, now)
            {
                assert_eq!(id, tx_id);
                assembled = Some(payload);
            }
        }
        // The host answers what it has, and the phone applies it.
        for action in host.layer.poll_all(now, direct(), 255) {
            if let Action::StandaloneAck { bytes, .. } = action {
                let record = host.seal(&bytes);
                let app = phone.open(&record).expect("the phone could not open the ack");
                let _ = phone.layer.on_app(PeerId(2), &app, now);
            }
        }
        if assembled.is_some() {
            break;
        }
        now += 3_000;
    }

    let payload = assembled.expect("the message crossed while the budget shrank");
    assert_eq!(payload, data, "the message arrived changed after {records} records");
}

/// **Selective repeat: only the parts the mask says are missing go out again.**
#[test]
fn only_the_missing_parts_are_retransmitted() {
    let mut phone = TransactionLayer::new(config(Side::Phone));
    let data = message(MessageKind(0x0E), &vec![7u8; 1_200]);
    let tx_id = phone.begin(PeerId(2), &data, 0).expect("ten parts");

    let first = sends(&phone.poll_all(0, direct(), 255));
    assert_eq!(first.len(), 11, "1201 bytes at 118 a part is eleven parts");
    assert_eq!(phone.stats().parts_sent, 11);

    // Parts 0, 4 and 7 arrived; everything else is missing.
    let received: u16 = 0b1 | 0b1_0000 | 0b1000_0000;
    let _ = phone.on_app(PeerId(2), &ack_bytes(tx_id, received), 100);

    // The pass deadline is its own airtime: eleven parts paced 2.5 airtimes apart. A retransmission
    // pass is collected by polling rather than by `poll_all`, because `poll_all` deliberately stops at
    // the first retransmission it meets.
    let second = sends(&phone.poll_all(phone.pace_ms() * 2 * 11, direct(), 255));
    assert_eq!(
        second.len(),
        8,
        "a full replay would be 11; only the unacknowledged parts go out"
    );
    assert_eq!(phone.stats().parts_retransmitted, 8);
    for (idx, _, _) in &second {
        assert!(
            ![0u8, 4, 7].contains(idx),
            "part {idx} was acknowledged and must not be sent again"
        );
    }
}

/// **Sub-halving: a part too wide for a flood goes as two halves under the same index.**
#[test]
fn a_part_too_wide_for_a_flood_is_sent_as_two_halves() {
    let (mut phone, mut host) = pair();
    let data = message(MessageKind(0x0E), &[9u8; 200]);
    let tx_id = phone.layer.begin(phone.peer, &data, 0).unwrap();

    let actions = phone.layer.poll_all(0, flood(), 255);
    let parts = sends(&actions);
    assert_eq!(parts.len(), 4, "201 bytes at 118 is two parts, halved is four");
    let mut halves = std::collections::BTreeMap::<u8, Vec<SubHalf>>::new();
    for (idx, sub, payload) in &parts {
        assert!(
            payload.len() <= payload_capacity(FLOOD),
            "a {} byte half does not fit a {FLOOD} byte record",
            payload.len()
        );
        halves.entry(*idx).or_default().push(*sub);
    }
    for (idx, subs) in &halves {
        assert_eq!(
            subs,
            &vec![SubHalf::A, SubHalf::B],
            "part {idx} was not carried as two halves of the same index"
        );
    }

    let mut delivered = None;
    for action in &actions {
        if let Action::Send { bytes, .. } = action {
            let record = phone.seal(bytes);
            let app = host.open(&record).expect("the host could not open the record");
            if let Received::Message { payload, .. } = host.layer.on_app(PeerId(1), &app, 0) {
                delivered = Some(payload);
            }
        }
    }
    assert_eq!(
        delivered.as_deref(),
        Some(data.as_slice()),
        "the halves reassemble into the original part {tx_id}"
    );
}

/// **`RC-7`: acknowledgement is held inside a burst, so it cannot collide with the data behind it.**
#[test]
fn a_burst_holds_its_acknowledgement_until_the_burst_is_over() {
    // The receiver holds no message of its own, so everything `poll` returns is an ack.
    let mut host = TransactionLayer::new(config(Side::Host));
    let data = message(MessageKind(0x0E), &vec![3u8; 1_000]);
    let mut sender = TransactionLayer::new(config(Side::Phone));
    let sender_tx = sender.begin(PeerId(2), &data, 0).expect("nine parts");

    let burst = sends(&sender.poll_all(0, direct(), 255));
    assert_eq!(burst.len(), 9, "1001 bytes at 118 is nine parts");
    let last = burst.len() - 1;
    for (part_idx, _, payload) in &burst[..last] {
        let app = part_app(sender_tx, *part_idx, burst.len(), payload);
        match host.on_app(PeerId(1), &app, 0) {
            Received::Part { .. } => {}
            Received::Message { .. } => panic!("the message completed before its last part"),
            other => panic!("unexpected {other:?}"),
        }
    }

    // Inside the burst tail nothing is emitted: the message is incomplete, and the tail has not
    // passed the first repeater yet.
    assert!(
        host.poll(0, direct(), 255).is_none(),
        "an acknowledgement must not go out mid-burst"
    );
    assert!(
        host.stats().acks_suppressed >= burst.len() as u64 - 1,
        "{} acks were held",
        host.stats().acks_suppressed
    );

    // The last part completes the message, and its ack is due at once.
    let (part_idx, _, payload) = &burst[last];
    let app = part_app(sender_tx, *part_idx, burst.len(), payload);
    let completed = host.on_app(PeerId(1), &app, 600);
    assert!(
        matches!(completed, Received::Message { .. }),
        "the last part completes the message: {completed:?}"
    );
    let action = host.poll(600, direct(), 255).expect("the ack is due");
    assert!(
        matches!(action, Action::StandaloneAck { .. }),
        "a whole message releases its ack at once: {action:?}"
    );
}

fn part_app(tx_id: u16, part_idx: u8, part_count: usize, payload: &[u8]) -> Vec<u8> {
    let mut app = Vec::with_capacity(TX_HEADER + payload.len());
    Header {
        tx_id,
        part_idx,
        part_count: u8::try_from(part_count).unwrap(),
        ack_tx_id: 0,
        ack_mask: 0,
        sub: SubHalf::Whole,
        tx_nonce: 1,
    }
    .encode(&mut app);
    app.extend_from_slice(payload);
    app
}

/// **A timeout ends the transaction and never the session.**
///
/// The byte-stream ARQ reported `Failure::RetriesExhausted` and dropped its `StatelessTransportState`
/// with it. Here a message may give up while the records still decrypt and a later message works.
#[test]
fn a_timeout_leaves_the_session_and_every_peer_intact() {
    let (mut phone, mut host) = pair();
    let doomed = message(MessageKind(0x0E), &vec![0xABu8; 300]);
    let tx_id = phone.layer.begin(phone.peer, &doomed, 0).unwrap();

    let mut now = 0;
    let mut timed_out = None;
    for _ in 0..400 {
        for action in phone.layer.poll_all(now, direct(), 255) {
            if let Action::TimedOut { tx_id: id, .. } = action {
                timed_out = Some(id);
            }
        }
        if timed_out.is_some() {
            break;
        }
        now += 2_400;
    }
    assert_eq!(timed_out, Some(tx_id), "the message had to give up");

    // The very next record still decrypts, and a new message still crosses.
    let fresh = message(MessageKind(0x0E), &[0xCDu8; 120]);
    let new_id = phone.layer.begin(phone.peer, &fresh, now).unwrap();
    let mut delivered = None;
    for action in phone.layer.poll_all(now, direct(), 255) {
        if let Action::Send { bytes, tx_id: id, .. } = action {
            assert_eq!(id, new_id);
            let record = phone.seal(&bytes);
            let app = host
                .open(&record)
                .expect("the session survived the timeout and still decrypts");
            if let Received::Message { payload, .. } = host.layer.on_app(PeerId(1), &app, now) {
                delivered = Some(payload);
            }
        }
    }
    assert_eq!(delivered.as_deref(), Some(fresh.as_slice()));
}

/// **Supersession: a newer snapshot replaces an older incomplete one, and nothing is joined.**
#[test]
fn a_newer_snapshot_abandons_the_older_one_without_joining_anything() {
    const SNAPSHOT: MessageKind = MessageKind(0x8E);
    let mut host = TransactionLayer::new(config(Side::Host));
    host.set_superseding(&[SNAPSHOT]);
    let mut sender = TransactionLayer::new(config(Side::Phone));
    sender.set_superseding(&[SNAPSHOT]);

    let older = message(SNAPSHOT, &vec![0x11; 400]);
    let newer = message(SNAPSHOT, &vec![0x22; 400]);
    let old_tx = sender.begin(PeerId(2), &older, 0).unwrap();
    let old_parts = sends(&sender.poll_all(0, direct(), 255));

    // The first two parts of the old snapshot arrive.
    for (part_idx, _, payload) in &old_parts[..2] {
        let app = part_app(old_tx, *part_idx, old_parts.len(), payload);
        assert!(matches!(
            host.on_app(PeerId(1), &app, 0),
            Received::Part { .. }
        ));
    }

    // A newer snapshot begins and its parts arrive. No byte of the old one may appear.
    let new_tx = sender.begin(PeerId(2), &newer, 1_000).unwrap();
    assert_ne!(new_tx, old_tx);
    let new_parts = sends(&sender.poll_all(1_000, direct(), 255));
    let mut delivered = None;
    for (part_idx, _, payload) in &new_parts {
        let app = part_app(new_tx, *part_idx, new_parts.len(), payload);
        if let Received::Message { payload, .. } = host.on_app(PeerId(1), &app, 1_000) {
            delivered = Some(payload);
        }
    }
    let payload = delivered.expect("the newer snapshot completes on its own");
    assert_eq!(payload[0], SNAPSHOT.0, "the kind byte survives reassembly");
    assert!(
        payload[1..].iter().all(|b| *b == 0x22),
        "the delivered snapshot contains bytes of the abandoned one"
    );
    assert_eq!(payload, newer);
    assert!(host.stats().messages_superseded >= 1);
}

/// A superseded message on the sending side is reported, not silently forgotten.
#[test]
fn beginning_a_new_snapshot_reports_the_one_it_replaced() {
    const SNAPSHOT: MessageKind = MessageKind(0x8E);
    let mut tx = TransactionLayer::new(config(Side::Phone));
    tx.set_superseding(&[SNAPSHOT]);
    let first = tx.begin(PeerId(2), &message(SNAPSHOT, &vec![1u8; 300]), 0).unwrap();
    let _ = tx.poll_all(0, direct(), 255);
    let second = tx.begin(PeerId(2), &message(SNAPSHOT, &vec![2u8; 300]), 10).unwrap();
    let events = tx.poll_all(10, direct(), 255);
    let replaced = events.iter().find_map(|action| match action {
        Action::Superseded { tx_id, .. } => Some(*tx_id),
        _ => None,
    });
    assert_eq!(replaced, Some(first));
    assert_ne!(second, first);
}

/// A malformed or oversized message is refused with a named reason, and never merged.
#[test]
fn a_record_this_layer_cannot_use_is_named_rather_than_merged() {
    let mut tx = TransactionLayer::new(config(Side::Phone));
    assert_eq!(
        tx.on_app(PeerId(2), &[1, 2, 3], 0),
        Received::Ignored(Ignored::Malformed)
    );
    // `part_count` zero is not a message.
    let mut app = part_app(4, 0, 1, b"x");
    app[3] = 0;
    assert_eq!(
        tx.on_app(PeerId(2), &app, 0),
        Received::Ignored(Ignored::BadPartCount)
    );
    // A part index past the count is refused, not clamped.
    let app = part_app(4, 5, 4, b"x");
    assert_eq!(
        tx.on_app(PeerId(2), &app, 0),
        Received::Ignored(Ignored::BadPartCount)
    );
    // A message with no bytes has no kind to send.
    assert_eq!(
        tx.begin(PeerId(2), &[], 0),
        Err(BeginError::Empty)
    );
    // A message too large to number is refused rather than truncated.
    assert_eq!(
        tx.begin(PeerId(2), &message(MessageKind(1), &vec![0u8; MESSAGE_MAX]), 0),
        Err(BeginError::TooLarge)
    );
    // A part of a message already delivered is named stale, and changes nothing.
    let data = message(MessageKind(0x0E), b"short");
    let tx_id = tx.begin(PeerId(2), &data, 0).unwrap();
    let one = sends(&tx.poll_all(0, direct(), 255));
    assert_eq!(one.len(), 1);
    let app = part_app(tx_id, 0, 1, &one[0].2);
    let mut receiver = TransactionLayer::new(config(Side::Host));
    assert!(matches!(
        receiver.on_app(PeerId(1), &app, 0),
        Received::Message { .. }
    ));
    assert_eq!(
        receiver.on_app(PeerId(1), &app, 10),
        Received::Ignored(Ignored::StalePart)
    );
}

/// **The whole layer over a real 8-hop lossy mesh, with real records and real crypto.**
///
/// This is the end-to-end proof `B8` asks for: a sixteen-part message crosses eight repeaters with
/// one link in six lossy, and arrives byte-exact.
#[test]
fn a_sixteen_part_message_crosses_an_eight_hop_lossy_mesh() {
    let (mut phone, mut host) = pair();

    // Phone - r1 .. r8 - host, at the public preset. One link in fifty is lossy, which is a
    // marginal-but-working route: nine hops at 15 % a hop is a link that does not work at all.
    let mut mesh = VirtualMesh::new(20_191);
    let phone_node = mesh.add_node(NodeSpec::companion());
    let mut chain = vec![phone_node];
    for _ in 0..8 {
        let repeater = mesh.add_node(NodeSpec::repeater());
        let previous = *chain.last().expect("the chain is never empty");
        mesh.link(previous, repeater, 9.0);
        chain.push(repeater);
    }
    let host_node = mesh.add_node(NodeSpec::companion());
    mesh.link(*chain.last().expect("the chain is never empty"), host_node, 9.0);
    mesh.set_loss(0.02);
    // The path the phone must use to reach the host, built from the repeaters' own hashes.
    let mut path = Vec::new();
    for hop in &chain[1..] {
        path.extend_from_slice(&mesh.path_through(&[*hop]));
    }
    assert_eq!(path.len(), 8, "eight repeaters, one byte of hash each");
    // The budget must be the one this carrier really is: eight bytes of path cost eight record
    // bytes, and a budget that ignored them would build records the far end never reports.
    let carrier = RecordBudget::direct(path.len());
    let mut back = path.clone();
    back.reverse();

    let data = message(MessageKind(0x0E), &vec![0x5Au8; MESSAGE_MAX - 1]);
    let tx_id = phone.layer.begin(phone.peer, &data, 0).unwrap();

    let pace = phone.layer.pace_ms();
    let mut delivered = None;
    let mut now = 0u64;
    let mut seen = 0usize;
    // One part at a time, `pace` apart, because that is what the radio allows: a repeater
    // forwarding the previous part is deaf to this one.
    for _ in 0..600 {
        if let Some(action) = phone.layer.poll(now, carrier, 255) {
            match action {
                Action::Send { bytes, .. } => {
                    let record = phone.seal(&bytes);
                    assert!(
                        carrier.receiver_accepts(record.len()),
                        "a {} byte record would be transmitted and never reported at the far end",
                        record.len()
                    );
                    mesh.send_direct(phone_node, &path, &record);
                }
                Action::StandaloneAck { bytes, .. } => {
                    let record = phone.seal(&bytes);
                    mesh.send_direct(phone_node, &path, &record);
                }
                _ => {}
            }
        }
        // The host answers what it has, along the reverse of the same route.
        if let Some(Action::StandaloneAck { bytes, .. }) = host.layer.poll(now, carrier, 255) {
            let record = host.seal(&bytes);
            mesh.send_direct(host_node, &back, &record);
        }

        // Let the air move, then hand every arrival to the end it reached. Nothing is delivered
        // until the clock advances, because a repeater holds a packet before forwarding it.
        mesh.advance(pace);
        let fresh: Vec<_> = mesh.deliveries()[seen..].to_vec();
        seen = mesh.deliveries().len();
        for arrival in fresh {
            if arrival.to == host_node {
                if let Some(app) = host.open(&arrival.payload)
                    && let Received::Message { payload, tx_id: id, .. } =
                        host.layer.on_app(PeerId(1), &app, now)
                {
                    assert_eq!(id, tx_id);
                    delivered = Some(payload);
                }
            } else if arrival.to == phone_node
                && let Some(app) = phone.open(&arrival.payload)
            {
                let _ = phone.layer.on_app(PeerId(2), &app, now);
            }
        }
        if delivered.is_some() {
            break;
        }
        now += pace;
    }

    let payload = delivered.expect("the message crossed eight lossy hops");
    assert_eq!(payload.len(), data.len());
    assert_eq!(payload, data, "the message arrived changed at the far end");
    assert!(
        mesh.log().len() > 20,
        "the harness only recorded {} transmissions, so it did not really cross",
        mesh.log().len()
    );
}

/// The record kind is the one this end is allowed to send, so a peer can reject its own echo.
#[test]
fn each_end_sends_its_own_record_kind() {
    let phone = TransactionLayer::new(config(Side::Phone));
    let host = TransactionLayer::new(config(Side::Host));
    assert_eq!(phone.record_kind(), ldmux_session::direction::KIND_UP);
    assert_eq!(host.record_kind(), ldmux_session::direction::KIND_DOWN);
}

/// **With no allowance left, the engine starts nothing.**
///
/// The band's duty cycle is enforced by the caller pushing `records_left`, and the layer is where
/// it becomes real: `poll` must return no `Send` at all. This is the assertion the app's metering
/// depends on — a value that arrives and is ignored would read as metering while the radio
/// transmitted, which in 869.4-869.65 MHz is outside the band's condition of use.
#[test]
fn no_allowance_means_no_record_starts() {
    let (_host, mut phone) = pair();
    let peer = PeerId(7);
    phone
        .layer
        .begin(peer, &message(MessageKind(0x10), &[0xAB; 40]), 0)
        .expect("a small message fits");

    // A part that would have gone out with an allowance, so the silence below is the gate and not
    // an empty queue.
    let would_send = phone.layer.poll(0, RecordBudget::direct(0), 1);
    assert!(
        matches!(would_send, Some(Action::Send { .. })),
        "the message was never sendable, so this test would prove nothing: {would_send:?}"
    );

    // Same instant, no allowance: nothing new starts. (The first call consumed the ready part, so
    // a second call with an allowance is the control.)
    let mut fresh = TransactionLayer::new(config(Side::Phone));
    fresh
        .begin(peer, &message(MessageKind(0x10), &[0xCD; 40]), 0)
        .expect("a small message fits");
    assert!(
        fresh.poll(0, RecordBudget::direct(0), 0).is_none(),
        "a record was handed out with no allowance left"
    );
    assert!(
        matches!(
            fresh.poll(0, RecordBudget::direct(0), 255),
            Some(Action::Send { .. })
        ),
        "the allowance gate swallowed the message instead of holding it"
    );
}
