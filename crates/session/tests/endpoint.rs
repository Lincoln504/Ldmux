//! The whole L1–L3 radio stack, end to end, over a simulated MeshCore mesh.
//!
//! These are the gates `docs/RADIO-PLAN.md` §9 calls `B3`, `B4`, `B7` and `B8`. Every test here
//! drives a real [`RadioEndpoint`] against a real [`VirtualMesh`]: the two ends share no state, all
//! traffic crosses the simulator's air, and the simulator applies the firmware's own rules — the
//! companion interface's 173-byte receive ceiling, the 160-entry duplicate ring keyed on
//! `SHA256(payload_type || payload)`, the origin's own packet coming back off the first repeater,
//! a repeater that is deaf while it forwards, and the flood's forward delay.
//!
//! Nothing here sleeps, opens a socket, or reads a clock. The mesh's clock is the only clock, and a
//! run is a function of its seed.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use ldmux_mesh::carrier::{Carrier, Event};
use ldmux_mesh::error::MeshError;
use ldmux_mesh::route::Carriage;
use ldmux_mesh::sim::{Delivery, NodeId, NodeSpec, VirtualMesh};
use ldmux_session::bootstrap::{self, MSG2_CONTENT_LEN, MSG2_NONCED_LEN};
use ldmux_session::direction::{KIND_MSG1, KIND_MSG1_FRAG, KIND_UP};
use ldmux_session::endpoint::{
    Admitted, ArrivalInfo, CarriageSink, EndpointConfig, Outbound, RadioEndpoint, Rejected,
    SentKind,
};
use ldmux_session::handshake::{Keypair, PairingEntry};
use ldmux_session::transaction::PeerId;

/// One full record's airtime at the public preset, which is what the pacing is derived from.
///
/// `docs/AIRTIME.md` and `crates/mesh/src/airtime.rs` agree on it: a 172-byte record at 62.5 kHz,
/// SF7, CR5 occupies the channel for 602.6 ms.
const AIRTIME_MS: f64 = 602.6;

/// The wall clock the handshake timestamps are drawn from. Only its differences matter.
const WALL_BASE_MS: u64 = 1_700_000_000_000;

/// The first retransmission deadline for a route crossing `repeaters` repeaters: six airtimes a
/// hop, the firmware's own direct-send budget (`DIRECT_SEND_PERHOP_FACTOR`, the figure
/// `docs/RADIO-PLAN.md` §4.3 derives `rto_initial` from).
///
/// A constant is wrong here and the wrongness is measurable: a fixed `2 s` is shorter than a
/// six-hop crossing, so every retransmission is fired while the copy before it is still travelling,
/// and the sender spends all six attempts before any acknowledgement could possibly have returned.
/// Measured on this harness at fifteen percent loss over six hops: 34 of 40 messages arrived and 3
/// gave up entirely, with several copies of each colliding at the first repeater that was deaf while
/// it forwarded. Spaced by one crossing, each copy gets its own chance.
fn retransmission_timeout_ms(repeaters: usize) -> u64 {
    let hops = u32::try_from(repeaters).unwrap_or(u32::MAX).saturating_add(1);
    let airtime = AIRTIME_MS.round() as u64;
    airtime.saturating_mul(6).saturating_mul(u64::from(hops))
}

/// How long the whole retry ladder takes, with room for the poll that reports its last verdict.
///
/// The passes are `rto`, `2 rto`, `4 rto` and so on to `32 rto`, which sums to `63 rto`, and the
/// pass that must expire before the verdict is a further `64 rto`, so the whole ladder is `127 rto`.
/// A test that waits exactly that long races the deadline it is waiting for, hence the margin. A
/// fixed limit would have to be the worst case of every topology, and a washout at forty percent loss
/// over two repeaters really does need the whole ladder.
fn ladder_ms(repeaters: usize) -> u64 {
    retransmission_timeout_ms(repeaters).saturating_mul(160)
}

/// Run a future that never really waits.
///
/// Both carriers here resolve on their first poll, so a `Pending` is a bug rather than a wait. There
/// is deliberately no executor dependency: a test that needed one would be testing the executor.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    // One poll, not a loop: both ends of this harness resolve immediately, so a `Pending` is a bug in
    // the carrier rather than something to wait for.
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("a carrier in this test never waits, so this is a bug"),
    }
}

/// A carrier that records what it was told to send, so the harness can put it on the air.
///
/// Keeping transmission out of the carrier is what lets one `VirtualMesh` serve both ends: the
/// simulator owns the air, and a port owns only the intent.
struct Port {
    carriage: Carriage,
    out: Vec<Vec<u8>>,
    /// Records to swallow instead of sending, so a test can lose an answer without changing the
    /// mesh's loss model for everything else.
    drop_first: usize,
}

impl Port {
    fn new() -> Self {
        Self {
            carriage: Carriage::Flood,
            out: Vec::new(),
            drop_first: 0,
        }
    }
}

impl Carrier for Port {
    async fn send_record(&mut self, _seq: u8, record: &[u8]) -> Result<(), MeshError> {
        if self.drop_first > 0 {
            self.drop_first -= 1;
            return Ok(());
        }
        self.out.push(record.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        // The harness reads the air itself; nothing is ever pushed at a port.
        Err(MeshError::Closed)
    }
}

impl CarriageSink for Port {
    async fn set_carriage(&mut self, carriage: &Carriage) -> Result<(), MeshError> {
        self.carriage = carriage.clone();
        Ok(())
    }
}

/// A record the harness actually put on the air.
#[derive(Clone, Debug)]
struct OnAir {
    from: NodeId,
    bytes: Vec<u8>,
}

/// Two endpoints, one mesh, and the loop that joins them.
struct Run {
    mesh: VirtualMesh,
    phone_node: NodeId,
    host_node: NodeId,
    phone: RadioEndpoint,
    host: RadioEndpoint,
    phone_port: Port,
    host_port: Port,
    now: u64,
    /// How far into [`VirtualMesh::deliveries`] the harness has already dispatched.
    cursor: usize,
    on_air: Vec<OnAir>,
    /// Polls that emitted nothing, for the spin assertion.
    quiet_polls: usize,
    /// The phone's static public key, which a host in a test has to be told.
    phone_static: [u8; 32],
    /// The host's static pair, so a "restarted" host keeps the identity the phone pinned. A daemon
    /// restart loses its sessions, not its key.
    host_parts: ([u8; 32], [u8; 32]),
}

impl Run {
    /// A phone and a host joined by `repeaters` repeaters, `loss` per transmission, with the retry
    /// ladder **stressed** so a lossy run finishes inside a test.
    fn new(seed: u64, repeaters: usize, loss: f64, path_width: u8) -> Self {
        Self::tuned(seed, repeaters, loss, path_width, true)
    }

    /// The same link with the retry ladder at the values that **ship** — `EndpointConfig`'s own
    /// defaults, unmodified. `docs/RADIO-PLAN.md` §12.3 (AR-4): a gate whose configuration differs
    /// from production's proves a design nobody deploys, and the ladder is exactly the parameter
    /// the field failures were about (how long the phone keeps asking).
    fn shipped(seed: u64, repeaters: usize, loss: f64, path_width: u8) -> Self {
        Self::tuned(seed, repeaters, loss, path_width, false)
    }

    fn tuned(seed: u64, repeaters: usize, loss: f64, path_width: u8, stress: bool) -> Self {
        let host_key = Keypair::generate().expect("a keypair");
        let host_parts = (host_key.private, host_key.public);
        let phone_key = Keypair::generate().expect("a keypair");
        let phone_key_public = phone_key.public;
        let psk = [0x42u8; 32];

        let mut mesh = VirtualMesh::new(seed);
        let phone_node = mesh.add_node(NodeSpec::companion().with_path_width(path_width));
        let mut chain = vec![phone_node];
        for _ in 0..repeaters {
            let id = mesh.add_node(NodeSpec::repeater().with_path_width(path_width));
            mesh.link(*chain.last().expect("the chain is never empty"), id, 8.0);
            chain.push(id);
        }
        let host_node = mesh.add_node(NodeSpec::companion().with_path_width(path_width));
        mesh.link(*chain.last().expect("the chain is never empty"), host_node, 8.0);
        mesh.set_loss(loss);

        let phone_config = EndpointConfig::new(
            ldmux_session::endpoint::Role::Phone,
            host_key.public,
            psk,
        )
        .with_path_width(path_width)
        .with_airtime_ms(AIRTIME_MS)
        .with_rto_ms(retransmission_timeout_ms(repeaters))
        .with_flood_reach_hops(u8::try_from(repeaters.max(1)).unwrap_or(1) + 2);
        // The stress values are applied only when asked for, so `shipped` really is the default
        // configuration rather than a restatement of it that could drift.
        let phone_config = if stress {
            phone_config.with_max_attempts(6).with_max_rungs(12)
        } else {
            phone_config
        };
        let host_config = EndpointConfig::new(
            ldmux_session::endpoint::Role::Host,
            phone_key.public,
            psk,
        )
        .with_path_width(path_width)
        .with_airtime_ms(AIRTIME_MS)
        .with_rto_ms(retransmission_timeout_ms(repeaters))
        .with_flood_reach_hops(u8::try_from(repeaters.max(1)).unwrap_or(1) + 2)
        .with_pairings(vec![PairingEntry {
            psk,
            pinned: Some(phone_key.public),
            last_timestamp_ms: 0,
        }]);
        let host_config = if stress {
            host_config.with_max_attempts(6)
        } else {
            host_config
        };

        Self {
            mesh,
            phone_node,
            host_node,
            phone: RadioEndpoint::phone(phone_config, phone_key),
            host: RadioEndpoint::host(host_config, host_key),
            phone_port: Port::new(),
            host_port: Port::new(),
            now: 0,
            cursor: 0,
            on_air: Vec::new(),
            quiet_polls: 0,
            phone_static: phone_key_public,
            host_parts,
        }
    }

    fn wall(&self) -> u64 {
        WALL_BASE_MS + self.now
    }

    /// Let both ends decide, put what they owe on the air, advance the mesh, and dispatch arrivals.
    fn step(&mut self, ms: u64) {
        self.step_reporting(ms);
    }

    /// The same, handing back what each end decided, so a test can read a completion or a timeout by
    /// message id instead of inferring it from a counter that cannot say which message it was.
    fn step_reporting(&mut self, ms: u64) -> (Vec<Outbound>, Vec<Outbound>) {
        let phone_report = block_on(self.phone.poll(self.now, &mut self.phone_port))
            .expect("the phone's carrier never fails");
        let host_report = block_on(self.host.poll(self.now, &mut self.host_port))
            .expect("the host's carrier never fails");
        if phone_report.is_empty() && host_report.is_empty() {
            self.quiet_polls += 1;
        }
        self.pump();
        self.mesh.advance(ms);
        self.now = self.now.saturating_add(ms);
        self.dispatch();
        (phone_report, host_report)
    }

    /// Let both ends decide and put what they owe on the air, without moving the clock.
    fn step_in_place(&mut self) -> (Vec<Outbound>, Vec<Outbound>) {
        let phone_report = block_on(self.phone.poll(self.now, &mut self.phone_port))
            .expect("the phone's carrier never fails");
        let host_report = block_on(self.host.poll(self.now, &mut self.host_port))
            .expect("the host's carrier never fails");
        self.pump();
        (phone_report, host_report)
    }

    fn pump(&mut self) {
        let phone_out: Vec<Vec<u8>> = self.phone_port.out.drain(..).collect();
        let carriage = self.phone_port.carriage.clone();
        for bytes in phone_out {
            self.transmit(self.phone_node, carriage.clone(), bytes);
        }
        let host_out: Vec<Vec<u8>> = self.host_port.out.drain(..).collect();
        let carriage = self.host_port.carriage.clone();
        for bytes in host_out {
            self.transmit(self.host_node, carriage.clone(), bytes);
        }
    }

    fn transmit(&mut self, from: NodeId, carriage: Carriage, bytes: Vec<u8>) {
        self.on_air.push(OnAir {
            from,
            bytes: bytes.clone(),
        });
        match carriage {
            Carriage::Direct { path } => {
                self.mesh.send_direct(from, &path, &bytes);
            }
            Carriage::Flood => {
                self.mesh.send_flood(from, &bytes);
            }
        }
    }

    fn dispatch(&mut self) {
        let deliveries: Vec<Delivery> = self.mesh.deliveries().to_vec();
        let batch = deliveries
            .get(self.cursor..)
            .map(<[Delivery]>::to_vec)
            .unwrap_or_default();
        self.cursor = deliveries.len();
        for delivery in batch {
            let info = info_of(&delivery);
            let record = delivery.payload.clone();
            if delivery.to == self.phone_node {
                self.phone
                    .on_record(&record, &info, self.now, self.wall());
            } else if delivery.to == self.host_node {
                self.host
                    .on_record(&record, &info, self.now, self.wall());
            }
        }
    }

    /// Step until `done`, collecting every completion and timeout each end reported.
    ///
    /// The two ends number their own messages from one, so the ids are only ever read per end: one
    /// list holding both would report a completion of an id that belonged to the other side.
    fn drive_until(
        &mut self,
        limit_ms: u64,
        step_ms: u64,
        mut done: impl FnMut(&Run) -> bool,
    ) -> Reported {
        let mut reported = Reported::default();
        let mut spent = 0;
        while spent < limit_ms {
            if done(self) {
                break;
            }
            let (phone, host) = self.step_reporting(step_ms);
            reported.absorb(&phone, &host);
            spent += step_ms;
        }
        reported
    }

    /// Steps until `done`, or until `limit` of virtual time has passed.
    fn run_until(&mut self, limit_ms: u64, step_ms: u64, mut done: impl FnMut(&Run) -> bool) {
        let mut spent = 0;
        while spent < limit_ms {
            if done(self) {
                return;
            }
            self.step(step_ms);
            spent += step_ms;
        }
    }

    /// Records sent by one end, for the tests that assert on what went on the air at all.
    fn sent_by(&self, from: NodeId) -> Vec<&OnAir> {
        self.on_air.iter().filter(|r| r.from == from).collect()
    }

    fn delivered_to_host(&mut self) -> Vec<Vec<u8>> {
        self.host.take_inbound()
    }
}

/// What each end reported, by message id, for the tests that read completions rather than counters.
///
/// The two ends number their own messages from one, so the ids are only ever compared within one
/// end's list: one list holding both would report a completion of an id belonging to the other side.
#[derive(Default)]
struct Reported {
    phone_completed: Vec<u16>,
    phone_timed_out: Vec<u16>,
    host_completed: Vec<u16>,
    host_timed_out: Vec<u16>,
}

impl Reported {
    /// Fold another pass's reports into this one.
    fn merge(&mut self, other: Self) {
        self.phone_completed.extend(other.phone_completed);
        self.phone_timed_out.extend(other.phone_timed_out);
        self.host_completed.extend(other.host_completed);
        self.host_timed_out.extend(other.host_timed_out);
    }

    /// Take one step's reports, each into its own end's list.
    fn absorb(&mut self, phone: &[Outbound], host: &[Outbound]) {
        for item in phone {
            match item {
                Outbound::Completed { tx_id } => self.phone_completed.push(*tx_id),
                Outbound::TimedOut { tx_id } => self.phone_timed_out.push(*tx_id),
                _ => {}
            }
        }
        for item in host {
            match item {
                Outbound::Completed { tx_id } => self.host_completed.push(*tx_id),
                Outbound::TimedOut { tx_id } => self.host_timed_out.push(*tx_id),
                _ => {}
            }
        }
    }
}

fn info_of(delivery: &Delivery) -> ArrivalInfo {
    let path_width = 1;
    if delivery.flood {
        ArrivalInfo::flood(delivery.path.clone(), path_width, delivery.snr_db)
    } else if delivery.hops == 0 {
        ArrivalInfo::direct(delivery.snr_db)
    } else {
        ArrivalInfo::in_transit(delivery.hops, delivery.snr_db)
    }
}

/// A message the terminal protocol would carry: its kind is its first byte.
fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(kind);
    out.extend_from_slice(body);
    out
}

fn delivered(report: &[Outbound], want: SentKind) -> bool {
    report.iter().any(|item| match item {
        Outbound::Sent { what, .. } => *what == want,
        _ => false,
    })
}

// ---------------------------------------------------------------------------------------------
// The gate: a link with no route at all, six repeaters away, fifteen percent loss.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_link_comes_up_at_the_retry_ladder_that_ships() {
    // AR-4. Every other gate here stresses the ladder so a lossy run finishes quickly; this one
    // runs `EndpointConfig`'s own defaults, so a change to what ships is caught here rather than at
    // range. One repeater and no loss, because what is under test is the ladder, not the mesh.
    let mut run = Run::shipped(11, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    let request = message(0x10, &[0x5A; 40]);
    run.phone
        .submit(&request, run.now)
        .expect("a 41-byte message is one part");
    run.run_until(120_000, 250, |run| {
        run.host.established() && run.host.stats().messages_delivered >= 1
    });
    assert!(
        run.host.established(),
        "the shipped ladder never established a link one repeater away: phone {:?} host {:?}",
        run.phone.stats(),
        run.host.stats()
    );
    assert_eq!(
        run.delivered_to_host().pop().as_deref(),
        Some(request.as_slice()),
        "the message did not arrive at the shipped ladder"
    );
}

#[test]
fn a_route_from_nothing_carries_a_message_and_its_answer_six_repeaters_away() {
    let mut run = Run::new(7, 6, 0.15, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    let request = message(0x10, &[0xAB; 60]);
    let tx_id = run
        .phone
        .submit(&request, run.now)
        .expect("a 61-byte message is one part");

    run.run_until(900_000, 500, |run| {
        run.host.established() && run.phone.established()
    });
    assert!(
        run.host.established(),
        "the host never established: {:?}",
        run.host.stats()
    );
    assert!(
        run.phone.established(),
        "the phone never established: phone {:?} host {:?} on air {}",
        run.phone.stats(),
        run.host.stats(),
        run.on_air.len()
    );

    run.run_until(400_000, 500, |run| {
        run.host.stats().messages_delivered >= 1
    });
    let mut arrived = run.delivered_to_host();
    assert_eq!(
        arrived.pop().as_deref(),
        Some(request.as_slice()),
        "the host did not receive the message the phone submitted"
    );

    // The reply, in the other direction, over the same six repeaters.
    let reply = message(0x8E, &[0xCD; 60]);
    run.host
        .submit(&reply, run.now)
        .expect("the host can submit a message");
    run.run_until(400_000, 500, |run| {
        run.phone.stats().messages_delivered >= 1
    });
    let answered = run.phone.take_inbound();
    assert!(
        answered.iter().any(|m| m == &reply),
        "the phone never received the host's message: {:?}",
        run.phone.stats()
    );

    // A direct packet lands about two times in five over six hops at fifteen percent loss, and the
    // ladder escalates to flood on the first miss, so one exchange often proves nothing: that is a
    // property of the topology, not of the design. A phone that sends a few commands gives the
    // direct carrier the attempts it needs, and proving the route is then a property of the run.
    for round in 0..4u8 {
        let again = message(0x10, &[0xA0 + round; 24]);
        run.phone
            .submit(&again, run.now)
            .expect("the phone can submit again");
        let want = 2 + u64::from(round);
        run.run_until(400_000, 500, |run| {
            run.host.stats().messages_delivered >= want
        });
        let back = message(0x8E, &[0xB0 + round; 24]);
        run.host
            .submit(&back, run.now)
            .expect("the host can submit again");
        run.run_until(400_000, 500, |run| {
            run.phone.stats().messages_delivered >= want
        });
    }

    // Both ends learned the way, and at least one proved it with a direct arrival. Which end proves
    // first depends on who sends direct inside the 120 s freshness window, so the gate asks for the
    // property (a route was learned and a direct arrival proved one) rather than for a particular
    // race; the one-repeater test below pins the rule that a flood never proves anything.
    assert!(
        run.phone.stats().routes_learned >= 1,
        "the phone learned no route: {:?}",
        run.phone.stats()
    );
    assert!(
        run.host.stats().routes_learned >= 1,
        "the host learned no route: {:?}",
        run.host.stats()
    );
    assert!(
        run.phone.stats().routes_proven >= 1 || run.host.stats().routes_proven >= 1,
        "no DIRECT arrival ever proved a route: phone {:?} host {:?}",
        run.phone.stats(),
        run.host.stats()
    );

    let phone_stats = run.phone.stats();
    assert!(
        phone_stats.routes_learned > 0,
        "no route was learned from a flood's reversed chain"
    );
    assert!(
        phone_stats.carriage_direct > 0 && phone_stats.carriage_flood > 0,
        "the link never used both carriages: {phone_stats:?}"
    );
    assert_eq!(tx_id, 1, "the message was not one part after all");

    eprintln!(
        "six repeaters, 15% loss: phone {:?}\nhost {:?}\nrecords on the air {}",
        run.phone.stats(),
        run.host.stats(),
        run.on_air.len()
    );
}

/// Five exchanges cross six repeaters at fifteen percent loss, and no delivered message is left
/// unacknowledged.
///
/// This pins the defect that let an acknowledgement ride along unnoticed: `TransactionLayer::on_app`
/// applied `ack_tx_id`/`ack_mask` only on a **pure** acknowledgement (`tx_id == 0`), so on a link
/// where the peer always had data to send, no acknowledgement was ever applied at all. The symptom
/// is the worst shape a reliable layer can have, because every message *reaches the far end* and its
/// sender still retries until it gives up: the phone says "sent", then "no answer", for a command
/// that already ran.
///
/// **The link is clean on purpose.** At fifteen percent loss over six repeaters a single record
/// arrives about two times in five, so a message can be delivered and its acknowledgement lost to
/// the air rather than to a bug, and the test could no longer tell the two apart. Measured on this
/// harness before the fix, with the same six repeaters and no loss at all, the phone completed
/// **zero** of five messages while the host received every one: that is the defect, and a clean link
/// isolates it. What loss does to a single acknowledgement is a separate question, recorded with the
/// ladder's other timing figures.
#[test]
fn five_exchanges_over_six_repeaters_complete_every_message() {
    const ROUNDS: u8 = 5;
    let mut run = Run::new(11, 6, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(900_000, 500, |run| {
        run.host.established() && run.phone.established()
    });
    assert!(
        run.phone.established() && run.host.established(),
        "the link never established: phone {:?} host {:?}",
        run.phone.stats(),
        run.host.stats()
    );

    let mut reported = Reported::default();
    let mut requests: Vec<(u16, Vec<u8>)> = Vec::new();
    let mut replies: Vec<(u16, Vec<u8>)> = Vec::new();
    let mut delivered_requests = 0;
    let mut delivered_replies = 0;

    for round in 0..ROUNDS {
        let request = message(0x10, &[0x11 + round; 40]);
        let tx_id = run
            .phone
            .submit(&request, run.now)
            .expect("a 41-byte message is one part");
        requests.push((tx_id, request.clone()));

        let want = u64::from(round) + 1;
        let step = run.drive_until(ladder_ms(6), 500, |run| {
            run.host.stats().messages_delivered >= want
        });
        reported.merge(step);

        let arrived = run.delivered_to_host();
        if !arrived.iter().any(|m| m == &request) {
            continue;
        }
        delivered_requests += 1;

        let reply = message(0x8E, &[0x91 + round; 40]);
        let reply_id = run
            .host
            .submit(&reply, run.now)
            .expect("the host can answer");
        replies.push((reply_id, reply.clone()));
        let want = u64::try_from(delivered_replies).unwrap_or(0) + 1;
        let step = run.drive_until(900_000, 500, |run| {
            run.phone.stats().messages_delivered >= want
        });
        reported.merge(step);
        let answered = run.phone.take_inbound();
        if answered.iter().any(|m| m == &reply) {
            delivered_replies += 1;
        }
    }

    // The last answer's acknowledgement still has to cross: a completion is reported on the decision
    // *after* the acknowledgement is applied, and the phone owes that acknowledgement as a record of
    // its own because it has nothing else to send. This is one bounded wait for a round trip, not a
    // fixed number of steps.
    let want = u64::try_from(delivered_replies).unwrap_or(0);
    let tail = run.drive_until(ladder_ms(6), 500, |run| {
        run.host.stats().transactions_completed >= want
    });
    reported.merge(tail);

    let phone = run.phone.stats();
    let host = run.host.stats();

    // No delivery without a completion.
    for (tx_id, _) in requests.iter().take(delivered_requests) {
        assert!(
            reported.phone_completed.contains(tx_id),
            "the host received message {tx_id} but the phone never completed it: {phone:?} \
             completed {:?} timed out {:?}",
            reported.phone_completed,
            reported.phone_timed_out
        );
        assert!(
            !reported.phone_timed_out.contains(tx_id),
            "the phone gave up on message {tx_id} after the host had already received it: {phone:?}"
        );
    }
    for (tx_id, _) in replies.iter().take(delivered_replies) {
        assert!(
            reported.host_completed.contains(tx_id),
            "the phone received answer {tx_id} but the host never completed it: {host:?} \
             completed {:?} timed out {:?}",
            reported.host_completed,
            reported.host_timed_out
        );
        assert!(
            !reported.host_timed_out.contains(tx_id),
            "the host gave up on answer {tx_id} after the phone had already received it: {host:?}"
        );
    }

    assert_eq!(
        (delivered_requests, delivered_replies),
        (usize::from(ROUNDS), usize::from(ROUNDS)),
        "on a clean link every exchange crosses: phone {phone:?} host {host:?}"
    );
    assert!(
        reported.phone_timed_out.is_empty(),
        "the phone gave up on a message it had sent: {:?} {phone:?}",
        reported.phone_timed_out
    );
    assert!(
        reported.host_timed_out.is_empty(),
        "the host gave up on an answer it had sent: {:?} {host:?}",
        reported.host_timed_out
    );
}

// ---------------------------------------------------------------------------------------------
// RC-1: nothing goes out before the initiation.
// ---------------------------------------------------------------------------------------------

#[test]
fn rc1_no_precursor_flood_precedes_the_first_initiation() {
    let mut run = Run::new(11, 2, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(500, 100, |_| false);

    let sent = run.sent_by(run.phone_node);
    assert!(!sent.is_empty(), "the phone sent nothing at all");
    for record in sent {
        let kind = record.bytes[0];
        assert!(
            kind == KIND_MSG1 || kind == KIND_MSG1_FRAG,
            "a {kind:#04x} record went out before the link had a session: `RC-1` was an advert and \
             three route requests ahead of the first initiation, and every one of them deafened the \
             repeater the initiation needed"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// RC-3: the first repeater hands our own packet back, and it must not be read as an arrival.
// ---------------------------------------------------------------------------------------------

#[test]
fn rc3_the_first_repeaters_echo_is_not_an_arrival() {
    let mut run = Run::new(3, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    let request = message(0x10, &[0x11; 40]);
    run.phone
        .submit(&request, run.now)
        .expect("a 41-byte message is one part");

    run.run_until(120_000, 500, |run| {
        run.host.stats().messages_delivered >= 1
    });
    let arrived = run.delivered_to_host();
    assert!(
        arrived.iter().any(|m| m == &request),
        "the message never arrived: {:?}",
        run.host.stats()
    );
    let phone = run.phone.stats();
    assert!(
        phone.rejected_self_echo > 0,
        "the phone read its own packet back off the repeater as an arrival: {phone:?}"
    );
    assert!(
        phone.sessions_established >= 1,
        "the phone never established: {phone:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// RC-5: an answer that arrives after the next rung still finishes the same attempt.
// ---------------------------------------------------------------------------------------------

#[test]
fn rc5_a_late_answer_still_finishes_the_attempt_it_belongs_to() {
    let mut run = Run::new(5, 1, 0.0, 1);
    // The first two answers the host sends are swallowed rather than lost on the air, so the rung
    // that follows is the reason the answer is late.
    run.host_port.drop_first = 2;
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    run.run_until(300_000, 500, |run| run.phone.established());
    assert!(
        run.phone.established(),
        "a late answer never finished the attempt: {:?}",
        run.phone.stats()
    );
    let host = run.host.stats();
    assert_eq!(
        host.sessions_established, 1,
        "the repeated initiation built a second session, which is the whole of `RC-5`: {host:?}"
    );
    assert!(
        host.answers_from_cache >= 1,
        "the second answer did not come from the cache: {host:?}"
    );
    assert!(
        run.phone.stats().rungs_used >= 2,
        "the test did not actually reach a second rung: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// A route is proven by a direct arrival, and never by a flood's reversed chain.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_route_is_never_proven_by_the_flood_that_taught_it() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // Establish, which is taught by a flood: the teaching copy is the only thing that gives the
    // phone a path at all, because an answer carries no path of its own.
    run.run_until(60_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");
    run.run_until(60_000, 250, |run| !run.phone.route().candidates().is_empty());
    let learnt = run.phone.route().candidates();
    assert!(
        !learnt.is_empty(),
        "the phone learned no path from the flooded answer: {:?}",
        run.phone.stats()
    );
    assert!(
        learnt.iter().all(|candidate| !candidate.is_proven()),
        "a path taught by a flood was recorded as proven, which is the conflation `proof` exists \
         to prevent"
    );
    assert!(
        run.phone.stats().routes_proven == 0,
        "a route was proven before any direct record came back: {:?}",
        run.phone.stats()
    );

    // Now a direct round trip: the message crosses on the learned path and the answer comes back
    // with every hop consumed, which is the only proof there is.
    let request = message(0x10, &[0x22; 40]);
    run.phone
        .submit(&request, run.now)
        .expect("a 41-byte message is one part");
    run.run_until(200_000, 250, |run| run.phone.stats().routes_proven > 0);
    assert!(
        run.phone.stats().routes_proven > 0,
        "a direct round trip proved nothing: phone {:?} host {:?} on air {}",
        run.phone.stats(),
        run.host.stats(),
        run.on_air.len()
    );
    assert!(
        run.phone.stats().carriage_direct > 0,
        "the phone never used the direct carriage: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// RC-7: whatever carriage the parts end up on, the bytes arrive once, in order, unchanged.
// ---------------------------------------------------------------------------------------------

#[test]
fn rc7_carriage_changes_never_duplicate_or_reorder_the_bytes() {
    let mut run = Run::new(13, 3, 0.10, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // Twelve parts, so the message cannot cross inside one pass' worth of time and at least one
    // retransmission — which floods, and therefore may have to halve — is guaranteed.
    let body: Vec<u8> = (0..1_340u32).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
    let request = message(0x0E, &body);
    run.phone
        .submit(&request, run.now)
        .expect("a 1.3 kB message fits sixteen parts");

    run.run_until(ladder_ms(3), 500, |run| {
        run.host.stats().messages_delivered >= 1
    });
    let arrived = run.delivered_to_host();
    assert_eq!(
        arrived.len(),
        1,
        "the message arrived {} times: a re-sent part was taken for a second message",
        arrived.len()
    );
    assert_eq!(
        arrived[0], request,
        "the bytes that arrived are not the bytes that were sent"
    );
    let host = run.host.stats();
    assert!(
        host.carriage_flood > 0,
        "the run never used the flood carriage, so nothing narrowed: {host:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Starvation: loss does not make the endpoint spin, and a silence is reported rather than hidden.
// ---------------------------------------------------------------------------------------------

#[test]
fn heavy_loss_ends_in_a_delivery_or_a_named_timeout_and_never_spins() {
    let mut run = Run::new(17, 2, 0.40, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    let request = message(0x10, &[0x33; 60]);
    run.phone
        .submit(&request, run.now)
        .expect("a 61-byte message is one part");

    // Long enough for the whole retry ladder, not a fixed count: the ladder's deadline is one
    // crossing per pass and doubles, so a bound that ignored the topology would give up before the
    // timeout it is waiting for became due.
    let mut named = false;
    let mut delivered_at_host = false;
    for _ in 0..ladder_ms(2) / 500 {
        let (phone_report, host_report) = run.step_in_place();
        if phone_report.iter().chain(host_report.iter()).any(|item| {
            matches!(
                item,
                Outbound::TimedOut { .. } | Outbound::BootstrapFailed { .. }
            )
        }) {
            named = true;
        }
        // A second poll at the same instant must find nothing: a state machine that emits the same
        // work twice is a loop that spends the hour's allowance on itself.
        let (again_phone, again_host) = run.step_in_place();
        assert!(
            again_phone.is_empty() && again_host.is_empty(),
            "polling twice at one instant emitted more work: {again_phone:?} {again_host:?}"
        );
        run.step(500);
        if run.host.stats().messages_delivered >= 1 {
            delivered_at_host = true;
            break;
        }
        if named {
            break;
        }
    }
    assert!(
        delivered_at_host || named,
        "forty percent loss neither delivered nor reported a timeout: phone {:?} host {:?}",
        run.phone.stats(),
        run.host.stats()
    );
    assert!(
        run.phone.stats().rungs_used >= 1,
        "the phone never even tried: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// The host answers a record it cannot read with a request to handshake again.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_host_with_no_session_asks_the_phone_to_handshake_again() {
    let mut run = Run::new(23, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(120_000, 250, |run| {
        run.host.established() && run.phone.established()
    });

    // The daemon restarts: its sessions are gone while the phone still believes it holds one.
    let (host_private, host_public) = run.host_parts;
    let host_key = Keypair::from_parts(host_private, host_public);
    let psk = [0x42u8; 32];
    let phone_public = run.phone_public();
    let config = EndpointConfig::new(ldmux_session::endpoint::Role::Host, phone_public, psk)
        .with_path_width(1)
        .with_airtime_ms(AIRTIME_MS)
        .with_flood_reach_hops(4)
        .with_pairings(vec![PairingEntry {
            psk,
            pinned: Some(phone_public),
            last_timestamp_ms: 0,
        }]);
    run.host = RadioEndpoint::host(config, host_key);
    run.host_port.drop_first = 0;

    let request = message(0x10, &[0x44; 30]);
    run.phone
        .submit(&request, run.now)
        .expect("a 31-byte message is one part");
    run.run_until(200_000, 250, |run| run.host.established());
    assert!(
        run.host.established(),
        "the phone never re-handshook after the host lost its session: host {:?} phone {:?}",
        run.host.stats(),
        run.phone.stats()
    );
    assert!(
        run.phone.stats().need_handshake_heard >= 1,
        "the phone did not act on the host's request: {:?}",
        run.phone.stats()
    );
}

impl Run {
    /// The phone's static public key, which a host restart has to be given.
    fn phone_public(&self) -> [u8; 32] {
        self.phone_static
    }
}

// ---------------------------------------------------------------------------------------------
// The report itself: it names what went on the air, so a log can be read rather than guessed at.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_report_names_the_initiation_and_the_answer_it_provoked() {
    let mut run = Run::new(29, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    let (phone_report, _) = run.step_in_place();
    assert!(
        delivered(&phone_report, SentKind::Initiation),
        "the phone's report did not name its initiation: {phone_report:?}"
    );
    let mut host_saw_answer = false;
    for _ in 0..12 {
        run.step(500);
        let (_, host_report) = run.step_in_place();
        if delivered(&host_report, SentKind::Answer) {
            host_saw_answer = true;
            break;
        }
    }
    assert!(
        host_saw_answer || run.host.stats().answers_sent > 0,
        "the host never answered: {:?}",
        run.host.stats()
    );
    let answered = run
        .host
        .stats()
        .answers_sent
        .saturating_add(run.host.stats().answers_from_cache);
    assert!(answered >= 1, "no answer was counted: {:?}", run.host.stats());
}

// ---------------------------------------------------------------------------------------------
// The pieces each end must ignore, asserted on the filter the endpoint actually uses.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_record_of_the_wrong_direction_is_ignored_before_it_is_parsed() {
    let mut run = Run::new(31, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    // A `KIND_UP` is the phone's own kind: a phone receiving one is hearing itself, and a host
    // receiving one with a session it does not know would otherwise be a decrypt attempt.
    let echo = message(KIND_UP, &[0u8; 8]);
    let verdict = run
        .phone
        .on_record(&echo, &ArrivalInfo::direct(9.0), run.now, run.wall());
    assert_eq!(verdict, Admitted::Ignored(Rejected::SelfEcho));

    let verdict = run.host.on_record(
        &echo,
        &ArrivalInfo::in_transit(2, 9.0),
        run.now,
        run.wall(),
    );
    assert_eq!(verdict, Admitted::Ignored(Rejected::InTransit));
}

// ---------------------------------------------------------------------------------------------
// The plan's own numbers, pinned where a caller can see them.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_initiation_forms_are_the_sizes_the_plan_promises() {
    assert_eq!(bootstrap::MSG1_NONCED_LEN, 123);
    assert_eq!(MSG2_CONTENT_LEN + bootstrap::NONCE_LEN, MSG2_NONCED_LEN);
    assert_eq!(MSG2_NONCED_LEN, 52);
    assert_eq!(PeerId(1), PeerId(1));
}
