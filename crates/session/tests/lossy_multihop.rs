//! **The session over a simulated route, where the radio cannot go on demand.**
//!
//! A record crosses `hops` repeaters, each holding it for up to an airtime, so one way is
//! `hops × airtime` and a round trip is twice that. Nothing here mocks the protocol: both ends do a
//! real handshake and real AEAD, and the only thing this harness fakes is the air — a fixed one-way
//! delay and a deterministic loss pattern.
//!
//! It exists because the timers are the part no amount of reading settles. A retransmission
//! scheduled before its first copy could have crossed doubles the air and, on a half-duplex
//! channel, collides with its own original; that is precisely the failure measured on 2026-11-09
//! and 2026-11-10, and it is invisible in a test that only asserts a configured field.

use ldmux_session::arq::Limits;
use ldmux_session::handshake::{Initiator, Keypair, PairingEntry, Responder};
use ldmux_session::record::application_bytes;
use ldmux_session::session::{Inbound, Session};

const PSK: [u8; 32] = [7; 32];
const NOW: u64 = 1_800_000_000_000;
const LIMITS: Limits = Limits {
    records_left: 255,
    l2_slots: 8,
};

struct Airborne {
    at: u64,
    to_host: bool,
    bytes: Vec<u8>,
}

/// A link with a fixed one-way delay and a deterministic loss pattern.
struct Air {
    one_way_ms: u64,
    airborne: Vec<Airborne>,
    sent: u32,
    lost: u32,
    /// One record in `drop_in` is lost; zero is a perfect link.
    drop_in: u32,
}

impl Air {
    fn new(hops: u64, airtime_ms: u64, drop_in: u32) -> Self {
        Self {
            one_way_ms: hops * airtime_ms,
            airborne: Vec::new(),
            sent: 0,
            lost: 0,
            drop_in,
        }
    }

    fn send(&mut self, now: u64, to_host: bool, bytes: Vec<u8>) {
        self.sent += 1;
        if self.drop_in > 0 && self.sent.is_multiple_of(self.drop_in) {
            self.lost += 1;
            return;
        }
        self.airborne.push(Airborne {
            at: now + self.one_way_ms,
            to_host,
            bytes,
        });
    }

    /// Deliver everything that has landed by `now`, returning the application bytes that reached
    /// the host.
    fn deliver(&mut self, now: u64, phone: &mut Session, host: &mut Session) -> Vec<u8> {
        self.airborne.sort_by_key(|a| a.at);
        let mut delivered = Vec::new();
        while self.airborne.first().is_some_and(|a| a.at <= now) {
            let a = self.airborne.remove(0);
            if a.to_host {
                match host.receive(a.at, &a.bytes) {
                    Ok(Inbound::Data(chunks)) => {
                        for c in chunks {
                            delivered.extend_from_slice(&c.payload);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => panic!("a delivered record failed to open: {e:?}"),
                }
            } else if let Err(e) = phone.receive(a.at, &a.bytes) {
                panic!("a delivered record failed to open: {e:?}");
            }
        }
        delivered
    }

    fn next_at(&self) -> Option<u64> {
        self.airborne.iter().map(|a| a.at).min()
    }
}

fn pair(record_limit: usize, hops: u8, airtime_ms: f64) -> (Session, Session) {
    let gw = Keypair::generate().expect("keypair");
    let gw_pub = gw.public;
    let phone_key = Keypair::generate().expect("keypair");
    let (init, m1) = Initiator::start(&phone_key, &gw_pub, &PSK, NOW).expect("initiator");
    let acc = Responder::new(gw)
        .accept(
            &[PairingEntry {
                psk: PSK,
                pinned: None,
                last_timestamp_ms: 0,
            }],
            NOW,
            &m1,
            1,
        )
        .expect("responder");
    let phone = init.finish(&acc.reply).expect("handshake");
    let mut phone = Session::with_airtime(phone, record_limit, NOW, Some(airtime_ms))
        .expect("phone session");
    let mut host = Session::with_airtime(acc.established, record_limit, NOW, Some(airtime_ms))
        .expect("host session");
    // The route the timers are sized for: one record crosses `hops` repeaters.
    phone.set_hops(hops);
    host.set_hops(hops);
    (phone, host)
}

/// One tick of the whole system: both ends send, then time advances to whatever is next — a
/// landing record **or a timer deadline**, whichever comes first.
///
/// Advancing only to arrivals was the first version of this harness, and it hid the very defect it
/// was written to catch: a retransmission due at 30 s on a route whose round trip is 34 s simply
/// never got polled, because nothing landed at 30 s.
fn tick(phone: &mut Session, host: &mut Session, air: &mut Air, now: &mut u64) -> Vec<u8> {
    for rec in phone.poll(*now, LIMITS).expect("phone poll") {
        air.send(*now, true, rec);
    }
    for rec in host.poll(*now, LIMITS).expect("host poll") {
        air.send(*now, false, rec);
    }
    let timer = phone
        .arq()
        .next_deadline()
        .into_iter()
        .chain(host.arq().next_deadline())
        .min();
    let next = match (air.next_at(), timer) {
        (Some(a), Some(t)) => Some(a.min(t.max(*now))),
        (a, t) => a.or(t),
    };
    match next {
        Some(at) => {
            *now = at.max(*now);
            air.deliver(*now, phone, host)
        }
        None => Vec::new(),
    }
}

/// **Two kilobytes cross a lossy twenty-eight-repeater route, and the link does not grind.**
///
/// Twenty-eight hops is the chain measured on 2026-11-10, and one record in five lost is the shape
/// of the real air at range. A round trip here is 34 s, which is *above* the thirty-second ceiling
/// the retransmission timer used to have: every attempt then fired before its first copy could
/// have arrived, the duplicate collided with the original it was duplicating on a half-duplex
/// channel, and five attempts ended a session that was working.
#[test]
fn two_kilobytes_cross_a_lossy_twenty_eight_hop_route() {
    let hops = 28u8;
    let (mut phone, mut host) = pair(172, hops, 603.0);
    let mut air = Air::new(u64::from(hops), 603, 5);
    let mut now = 0u64;

    let payload: Vec<u8> = (0..2_000u16).map(|i| i as u8).collect();
    phone.send(1, &payload).expect("room for the payload");

    let mut got = Vec::new();
    for _ in 0..40_000 {
        got.extend(tick(&mut phone, &mut host, &mut air, &mut now));
        if got.len() >= payload.len() {
            break;
        }
        // Nothing in the air means every record is waiting on a timer: step to the next whole
        // second and let the timers speak.
        if air.next_at().is_none() {
            now += 1_000;
        }
    }
    assert_eq!(
        payload.len(),
        got.len(),
        "2 KB across twenty-eight lossy hops ({} transmitted, {} lost)",
        air.sent,
        air.lost
    );
    assert_eq!(payload, got, "in order, every byte");
    // **Not a grind.** The ideal is ceil(2000 / 143) = 14 records; with one in five lost and its
    // retransmissions, a working ladder lands well inside a small multiple.
    assert!(
        air.sent < 200,
        "the link spent {} transmissions on 2 KB at 20 % loss",
        air.sent
    );
}

/// **A perfect route costs almost nothing.** The control for the test above: no loss, same delay,
/// and the record count must be close to the payload's own arithmetic.
#[test]
fn a_clean_route_costs_about_one_record_per_chunk() {
    let hops = 28u8;
    let (mut phone, mut host) = pair(172, hops, 603.0);
    let mut air = Air::new(u64::from(hops), 603, 0);
    let mut now = 0u64;
    let payload: Vec<u8> = (0..2_000u16).map(|i| i as u8).collect();
    phone.send(1, &payload).expect("room for the payload");

    let mut got = Vec::new();
    for _ in 0..40_000 {
        got.extend(tick(&mut phone, &mut host, &mut air, &mut now));
        if got.len() >= payload.len() {
            break;
        }
        if air.next_at().is_none() {
            now += 1_000;
        }
    }
    assert_eq!(payload, got);
    let chunks = payload.len().div_ceil(application_bytes(172) - 5);
    assert!(
        air.sent <= chunks as u32 * 2 + 4,
        "a clean route sent {} records for {} chunks",
        air.sent,
        chunks
    );
    // **And nothing was sent twice for a link that never lost anything.** A retransmission is only
    // ever due if its first copy could not have been answered: at twenty-eight hops the round trip
    // is 34 s, so a ceiling under that schedules a duplicate of every record on a perfect link —
    // invisible in the outcome, and half the air spent.
    assert_eq!(
        0,
        phone.arq().stats().retransmits + host.arq().stats().retransmits,
        "a lossless route retransmitted ({} sent)", air.sent
    );
}
