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

    /// The same link with the **shipped** discovery grace, so a test can measure the wait itself.
    fn with_grace(seed: u64, repeaters: usize, loss: f64, path_width: u8, grace_ms: u64) -> Self {
        let mut run = Self::tuned(seed, repeaters, loss, path_width, true);
        run.phone.set_bootstrap_path_grace_ms(grace_ms);
        run
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
        .with_flood_reach_hops(u8::try_from(repeaters.max(1)).unwrap_or(1) + 2)
        // **The discovery grace is off here on purpose.** These tests are about the ladder, the
        // carriers and the transaction layer, and a three-second wait for an advert path would only
        // move every one of their clocks. The grace has its own test.
        .with_bootstrap_path_grace_ms(0);
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

    /// Restart the host: a new endpoint with the same identity, which is what a daemon restart is.
    fn restart_host(&mut self) {
        let (host_private, host_public) = self.host_parts;
        let host_key = Keypair::from_parts(host_private, host_public);
        let psk = [0x42u8; 32];
        let phone_public = self.phone_public();
        let config = EndpointConfig::new(ldmux_session::endpoint::Role::Host, phone_public, psk)
            .with_path_width(1)
            .with_airtime_ms(AIRTIME_MS)
            .with_flood_reach_hops(4)
            .with_pairings(vec![PairingEntry {
                psk,
                pinned: Some(phone_public),
                last_timestamp_ms: 0,
            }]);
        self.host = RadioEndpoint::host(config, host_key);
        self.host_port.drop_first = 0;
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

    // **What the phone learns from a two-hop flood is the chain home, and it is a candidate.**
    // It used to be taught by a flooded copy of the answer; a path now comes from the chain an
    // initiation crossed, which is the same inference and the same lack of proof. An adjacent peer
    // (zero hops) teaches nothing at all, which is correct: there is no chain to learn and a direct
    // packet needs none, so this scenario keeps one repeater in the way.
    run.run_until(60_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");
    // **The property, not the mechanism.** A path may come from an initiation's chain or from an
    // advert, and either way it is a candidate; what must never happen is a *flood* being recorded
    // as proof. The phone may hold nothing at all here — an adjacent peer needs no path, and the
    // sim has no advert cache — which is why the assertion is about proof, not about holding.
    let learnt = run.phone.route().candidates();
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

    // **The phone's road to the host comes from an advert, which the simulator has no cache for.**
    // In the field each radio emits one (`CMD_SEND_SELF_ADVERT`) and reads the other's chain out of
    // its own cache (`CMD_GET_ADVERT_PATH`), and `Client::adopt_learned_path` reverses it into a
    // candidate. Here the same candidate is handed over directly: the host's chain back to the phone
    // reversed is the phone's chain to the host, because both cross the same repeaters.
    let host_chain = run
        .host
        .route()
        .candidates()
        .first()
        .map(|c| c.path().to_vec())
        .expect("the host learned the way back from the initiation");
    // Reversed by hop, the same rule `AdvertPath::outbound` uses: the host's chain home and the
    // phone's chain out cross the same repeaters in opposite order.
    let width = 1usize;
    let mut to_host = Vec::with_capacity(host_chain.len());
    for start in (0..host_chain.len()).step_by(width).rev() {
        to_host.extend_from_slice(&host_chain[start..(start + width).min(host_chain.len())]);
    }
    run.phone.observe_path(ldmux_mesh::route::Candidate::learned(
        to_host,
        u8::try_from(width).unwrap_or(1),
        0.0,
        run.now,
    ));

    // Now a direct round trip: the message crosses on that path and the answer comes back with
    // every hop consumed, which is the only proof there is.
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
    // **A retransmission is what floods now**, so the loss has to be high enough that one happens
    // without being so high that the message never crosses: the first pass of every part goes
    // direct, and only a part that had to be sent again takes the flood carrier
    // (`stage_transactions`). Measured on this topology, 0.35 loses the message and 0.10 is not
    // enough to force a repeat, so the loss sits just under the line where retransmission begins.
    let mut run = Run::new(13, 3, 0.12, 1);
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
    let phone = run.phone.stats();
    assert!(
        host.carriage_flood > 0 || phone.carriage_flood > 0,
        "the run never used the flood carriage, so nothing narrowed: phone {phone:?} host {host:?}"
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

// ---------------------------------------------------------------------------------------------
// A bad answer spends the attempt, and the ladder replaces it.
// ---------------------------------------------------------------------------------------------

/// **One unreadable answer must not poison the phone for the rest of its life.**
///
/// `Attempt::answer` consumes `snow`'s handshake state, so a crypto failure leaves the attempt
/// unusable — its own doc says the caller must replace it. The endpoint used to count the failure and
/// keep the attempt, so every later answer — including the correct one a moment later — came back
/// `Malformed`, and the phone stayed on "connecting" for the whole ladder. Measured on real hardware
/// 2026-11-12: the host reported `session established` while the phone logged eleven delivered
/// `msg2` records and never moved.
#[test]
fn a_bad_answer_spends_the_attempt_and_the_ladder_replaces_it() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // A record shaped like an answer that cannot authenticate: the kind byte is right, the payload
    // is not.
    let mut bogus = vec![0x12u8];
    bogus.extend_from_slice(&[0xAB; 51]);
    run.phone.on_record(
        &bogus,
        &ArrivalInfo::direct(0.0),
        run.now,
        run.wall(),
    );
    assert_eq!(
        run.phone.stats().rejected_handshake,
        1,
        "the bogus answer was not rejected: {:?}",
        run.phone.stats()
    );

    // **And it is not a reflex.** A replacement waits its rung, so a peer that answers unreadably
    // cannot make this end transmit continuously: with the deadline set to now, the same harness
    // measured 200 initiations in 200 answer steps. Nothing may be written before the rung expires.
    let before = run.phone.stats().initiations_sent;
    for _ in 0..2 {
        run.step(500);
    }
    assert_eq!(
        run.phone.stats().initiations_sent,
        before,
        "a spent attempt was replaced and transmitted in the same instant: {:?}",
        run.phone.stats()
    );

    // The real answer must still finish, which needs a fresh attempt behind it.
    run.run_until(300_000, 500, |run| run.phone.established());
    assert!(
        run.phone.established(),
        "a rejected answer left the phone unable to accept a real one: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// Adjacency is a route, and a direct arrival proves it.
// ---------------------------------------------------------------------------------------------

/// **A direct arrival with no path left proves the peer is adjacent, and that route must win over
/// a long chain learned from a flood that went the long way round.**
///
/// Measured against two radios a metre apart on 2026-11-13: the host reported
/// `route 20 repeaters in the path` and `capacity 152 B`, because the only fresh candidates left
/// were chains from floods that had looped round a public mesh, while the phone's own direct records
/// — proof of adjacency — taught nothing. Every direct send followed the 20-hop loop and never
/// landed.
#[test]
fn a_direct_arrival_with_no_path_left_proves_the_peer_is_adjacent() {
    let mut run = Run::new(9, 0, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established() && run.host.established());
    assert!(run.phone.established(), "the phone did not establish");
    assert!(
        run.host.stats().sessions_established >= 1,
        "the host did not establish: {:?}",
        run.host.stats()
    );

    // **The phone holds an adjacent route, as an advert gives it in the field**, so its records go
    // out direct with nothing in the path. A flood would teach this end a chain and prove nothing.
    run.phone.observe_path(ldmux_mesh::route::Candidate::learned(
        Vec::new(),
        1,
        0.0,
        run.now,
    ));

    // **A record from the phone, after the session, is the arrival that proves the road.** Nothing
    // before it is evidence: a flood teaches a chain and nothing more.
    let request = message(0x10, &[0x33; 24]);
    run.phone
        .submit(&request, run.now)
        .expect("a 25-byte message is one part");
    run.run_until(120_000, 250, |run| run.host.stats().messages_delivered >= 1);
    assert!(
        run.host.stats().messages_delivered >= 1,
        "the phone's message never reached the host: {:?}",
        run.host.stats()
    );

    // The host holds the phone as adjacent, and it is a proof rather than a guess: the phone's
    // direct record arrived with nothing left in its path.
    let host_route = run.host.route();
    let adjacent = host_route
        .candidates()
        .iter()
        .find(|candidate| candidate.path().is_empty())
        .expect("the host holds no adjacent route at all");
    assert!(
        adjacent.is_proven(),
        "the host holds the phone as adjacent but not as proven: {:?}",
        host_route.candidates()
    );
    assert_eq!(
        host_route.best(run.now, 7).map(|c| c.path().to_vec()),
        Some(Vec::new()),
        "the host would not send by the adjacent route: {:?}",
        host_route.candidates()
    );
}

// ---------------------------------------------------------------------------------------------
// A session that cannot open the peer's records is replaced, not kept.
// ---------------------------------------------------------------------------------------------

/// **A record that will not open is evidence the session is dead, and the phone must say so.**
///
/// Measured on the phone 2026-11-13: `admitted=4` beside `bad=3`, a live session in the client's
/// counters, and a screen waiting for a list the host was already answering. The phone kept the
/// session, so every later record was refused the same way and nothing could end it — only the host
/// could ask for a new handshake, and it had no reason to.
#[test]
fn a_record_that_will_not_open_drops_the_session_and_asks_again() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");

    // A record of the shape the host sends, addressed to a session this end does not hold: the
    // header is right, the tag is not.
    let mut stale = vec![0x16u8];
    stale.extend_from_slice(&[0x5A; 33]);
    run.phone
        .on_record(&stale, &ArrivalInfo::direct(0.0), run.now, run.wall());
    assert!(
        run.phone.stats().rejected_record >= 1,
        "the stale record was not refused: {:?}",
        run.phone.stats()
    );
    assert!(
        run.phone.established(),
        "one unopenable record threw away a working session: {:?}",
        run.phone.stats()
    );

    // And the working session keeps working: the peer's next record is opened and delivered.
    run.run_until(300_000, 500, |run| run.phone.established());
    assert!(
        run.phone.established(),
        "the phone never recovered from a stale session: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// Foreign traffic is not a reason to start over.
// ---------------------------------------------------------------------------------------------

/// **An unopenable record must not throw away the ephemeral the peer is answering.**
///
/// A public mesh hands this end every accepted `RAW_CUSTOM` packet in range, most of them other
/// applications', because the firmware performs no destination check on that payload type. Each one
/// used to ask for a handshake *and* mint a new ephemeral, so the peer's answer never matched the
/// attempt that was live. Measured 2026-11-13 on the public mesh: eight rungs and seven refused
/// answers in forty seconds with `sessions=0`.
#[test]
fn a_foreign_record_does_not_restart_the_handshake() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // Twenty records of a kind this protocol defines and a body it cannot open: exactly what another
    // application's traffic looks like once it passes the kind filter.
    for step in 0..20 {
        let mut record = vec![0x15u8];
        record.extend_from_slice(&[0x77; 33]);
        run.phone.on_record(
            &record,
            &ArrivalInfo::direct(0.0),
            run.now + step,
            run.wall(),
        );
    }
    // Refused, and the counter that names *why* is the client's mapping of `Rejected::NoSession`:
    // there is no session yet, so a record of a defined kind cannot be opened by definition.
    let refused = run.phone.stats().rejected_handshake + run.phone.stats().rejected_record;
    assert!(
        refused >= 1,
        "the foreign records were not refused: {:?}",
        run.phone.stats()
    );

    run.run_until(120_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");
    let host = run.host.stats();
    assert_eq!(
        host.sessions_established, 1,
        "foreign traffic made the host build more than one session: {host:?}"
    );
    assert!(
        run.phone.stats().rungs_used <= 6,
        "foreign traffic spent rungs: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// Proof needs one road in flight, not two.
// ---------------------------------------------------------------------------------------------

/// **An arrival does not say which of this end's roads it came by, so two roads prove nothing.**
///
/// A direct record comes back with its path finished and the companion reports no path with it, so
/// the only thing that can be compared is what this end has just sent. Crediting the most recently
/// queued path proved adjacency for a host that had also sent along a chain, and that host then put
/// its parts on the air with no path at all with the peer eight repeaters away. Measured 2026-11-13:
/// `carriage=direct path=[]` beside `route 8 repeaters in the path`, `signal -84 dBm`.
#[test]
fn two_direct_roads_in_flight_prove_neither() {
    let mut run = Run::new(9, 2, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established() && run.host.established());
    assert!(run.host.established(), "the host did not establish");

    // Two roads, both sent: the arrival that follows could be either, so neither may be proven.
    run.host.observe_path(ldmux_mesh::route::Candidate::learned(
        Vec::new(),
        1,
        8.0,
        run.now,
    ));
    run.host.observe_path(ldmux_mesh::route::Candidate::learned(
        vec![0x08, 0xE5],
        1,
        8.0,
        run.now,
    ));
    let request = message(0x10, &[0x55; 20]);
    run.phone
        .submit(&request, run.now)
        .expect("a message fits one part");
    run.run_until(120_000, 250, |run| run.host.stats().messages_delivered >= 1);

    let proven: Vec<_> = run
        .host
        .route()
        .candidates()
        .iter()
        .filter(|c| c.is_proven())
        .map(|c| c.path().to_vec())
        .collect();
    assert!(
        proven.len() <= 1,
        "more than one road was proven by a single arrival: {proven:02x?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The first initiation waits for a path, because a flood needs the peer's receive log.
// ---------------------------------------------------------------------------------------------

/// **A direct initiation needs nothing from the peer's radio; a flood needs its receive log.**
///
/// The firmware delivers a flood to the application only through `logRxRaw`
/// (`src/Dispatcher.cpp:199`, `MyMesh.cpp:291-297`), a setting on the other end that ldmux neither
/// sets nor reports. A direct record arrives as `0x84` and needs no such thing, and the phone's own
/// radio can answer with the peer's cached advert path in milliseconds. So the first rung waits for
/// that answer, and only then falls back to a flood.
#[test]
fn the_first_initiation_waits_for_a_path_before_it_floods() {
    // The shipped grace, so this test measures what ships.
    let mut run = Run::with_grace(9, 1, 0.0, 1, 3_000);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // Nothing goes out while the path is still being asked for.
    run.run_until(2_000, 100, |_| false);
    assert!(
        run.sent_by(run.phone_node).is_empty(),
        "the phone flooded before its own radio could answer with a path: {:?}",
        run.sent_by(run.phone_node)
            .iter()
            .map(|r| r.bytes[0])
            .collect::<Vec<_>>()
    );

    // And the fallback still happens, so a peer with no advert is not stranded.
    run.run_until(20_000, 100, |run| !run.sent_by(run.phone_node).is_empty());
    assert!(
        !run.sent_by(run.phone_node).is_empty(),
        "the phone never fell back to a flood, so a peer with no advert could never be reached"
    );
}

// ---------------------------------------------------------------------------------------------
// Foreign traffic does not drive the ladder, and a fresh initiation is not rate-limited.
// ---------------------------------------------------------------------------------------------

/// **The ladder is the phone's own clock, not the mesh's traffic.**
///
/// Pulling the rung deadline forward on every handshake request made a second ladder: on a public
/// mesh a foreign record arrives every few seconds, so the phone re-sent its initiation on that
/// traffic instead of on its rungs — ten rungs in forty-five seconds, measured 2026-11-13, while the
/// host answered two and refused seven.
#[test]
fn foreign_traffic_does_not_pull_the_ladder_forward() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");

    // A foreign record every 200 ms for four seconds: far more than the mesh would ever deliver.
    for step in 0..20 {
        let mut record = vec![0x15u8];
        record.extend_from_slice(&[0x66; 33]);
        run.phone.on_record(
            &record,
            &ArrivalInfo::direct(0.0),
            run.now + step * 200,
            run.wall(),
        );
    }
    run.run_until(4_000, 100, |_| false);
    assert!(
        run.phone.stats().rungs_used <= 2,
        "foreign traffic spent {} rungs in four seconds: {:?}",
        run.phone.stats().rungs_used,
        run.phone.stats()
    );
}

/// **A fresh initiation is a new session request, not a replay.** Refusing it makes the peer's ladder
/// outrun this end's answers, which is what kept the phone from ever establishing.
#[test]
fn a_fresh_initiation_is_answered_even_when_another_was_just_answered() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");

    // A second, genuinely new initiation inside the answer rate window: it must be answered.
    let before = run.host.stats().sessions_established;
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start a second attempt");
    run.run_until(60_000, 250, |run| run.host.stats().sessions_established > before);
    assert!(
        run.host.stats().sessions_established > before,
        "a fresh initiation was refused as if it were a replay: {:?}",
        run.host.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// A record that authenticates is ours, whatever its path says.
// ---------------------------------------------------------------------------------------------

/// **The tag is the authority, not the path.**
///
/// A direct packet with hops left is addressed to a repeater, and the transit rule exists so a node
/// does not act on somebody else's traffic. But the path is a routing hint that can be stale, while
/// the AEAD cannot be forged. Measured 2026-11-13: the phone heard the host's transaction parts
/// directly while they were addressed to a repeater, rejected every one as in transit, and never
/// acknowledged a part, so the shell list never crossed with both ends holding a session.
#[test]
fn a_transport_record_that_authenticates_is_taken_even_with_a_hop_left() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established() && run.host.established());
    assert!(run.phone.established(), "the phone did not establish");

    // A message from the phone, so the host has a record to send back.
    let request = message(0x10, &[0x21; 24]);
    run.phone
        .submit(&request, run.now)
        .expect("a 25-byte message is one part");
    run.run_until(120_000, 250, |run| run.host.stats().messages_delivered >= 1);
    assert!(
        run.host.stats().messages_delivered >= 1,
        "the phone's message never reached the host: {:?}",
        run.host.stats()
    );

    // Now the host's own record, delivered to the phone as if it still had a hop left: the shape a
    // node sees when it hears a packet addressed to a repeater.
    let before = run.phone.stats().records_admitted;
    let reply = message(0x16, &[0x31; 24]);
    run.host
        .submit(&reply, run.now)
        .expect("a 25-byte message is one part");
    run.run_until(120_000, 250, |run| run.phone.stats().records_admitted > before);
    assert!(
        run.phone.stats().records_admitted > before,
        "the phone admitted nothing from the host: {:?}",
        run.phone.stats()
    );
}

// ---------------------------------------------------------------------------------------------
// A restarted host must not reuse the session index the phone's replay window is keyed by.
// ---------------------------------------------------------------------------------------------

/// **The index is a lookup key, and a restart that reuses it makes every record a duplicate.**
///
/// The peer's replay window is keyed by the index, so a host that restarts and begins again at 1
/// hands the phone counters its window has already seen: every part is refused as a duplicate and the
/// link carries nothing, with both ends reporting a healthy session. Measured 2026-11-13,
/// `refusal=3` (duplicate) on the phone after a daemon restart.
#[test]
fn a_restarted_host_does_not_reuse_the_session_index() {
    let mut run = Run::new(9, 1, 0.0, 1);
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start an attempt");
    run.run_until(60_000, 250, |run| run.phone.established());
    assert!(run.phone.established(), "the phone did not establish");
    let first = run.phone.session_index().expect("a session index");

    // The host restarts: a new endpoint with the same identity, as a daemon restart is.
    run.restart_host();
    run.phone
        .connect(run.now, run.wall())
        .expect("the phone can start a second attempt");
    run.run_until(120_000, 250, |run| {
        run.phone.established() && run.phone.session_index() != Some(first)
    });
    let second = run.phone.session_index().expect("a session index after the restart");
    assert_ne!(
        first, second,
        "the restarted host reused index {first}, so the phone's replay window refuses its records"
    );
}
