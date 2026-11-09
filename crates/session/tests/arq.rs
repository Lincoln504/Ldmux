use ldmux_session::arq::{
    Arq, Config, Delivered, Failure, Incoming, Kind, Limits, Outgoing, QueueError,
    REFUSAL_DEADLINE_MS, REFUSED_RETRY_MS,
};

const PLENTY: Limits = Limits {
    records_left: 255,
    l2_slots: 8,
};

fn cfg() -> Config {
    Config {
        heartbeat_ms: 3_000,
        window: 4,
        rto_initial_ms: 1_000,
        rto_max_ms: 8_000,
        max_attempts: 4,
        ack_delay_ms: 100,
        airtime_ms: 0,
        queue_max: 16,
        max_payload: 10,
    }
}

fn inc(o: &Outgoing) -> Incoming {
    Incoming {
        mark: 0,
        kind: o.kind,
        seq: o.seq,
        ack: o.ack,
        shell: o.shell,
        payload: o.payload.clone(),
    }
}

fn bytes(d: &[Delivered]) -> Vec<u8> {
    d.iter().flat_map(|x| x.payload.clone()).collect()
}

#[test]
fn bytes_are_chunked_ordered_and_acknowledged() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(1, b"0123456789abcdefghijKLM").unwrap(); // 23 bytes: 3 chunks of <= 10
    let out = a.poll(0, PLENTY);
    assert_eq!(out.len(), 3);
    assert_eq!(out.iter().map(|o| o.seq).collect::<Vec<_>>(), [0, 1, 2]);
    let mut got = Vec::new();
    for o in &out {
        got.extend(b.on_incoming(0, &inc(o)));
    }
    assert_eq!(bytes(&got), b"0123456789abcdefghijKLM");
    assert!(got.iter().all(|d| d.shell == 1));
    // b owes an ack; after the delay it sends one and a's window empties.
    assert!(
        b.poll(50, PLENTY).is_empty(),
        "the ack may wait for data to ride on"
    );
    let ack = b.poll(100, PLENTY);
    assert_eq!(ack.len(), 1);
    assert_eq!(ack[0].kind, Kind::Ack);
    assert_eq!(ack[0].ack, 3);
    a.on_incoming(100, &inc(&ack[0]));
    assert_eq!(a.pending(), 0);
}

#[test]
fn an_ack_rides_on_outgoing_data_so_no_pure_ack_is_spent() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(0, b"ping").unwrap();
    let o = a.poll(0, PLENTY);
    b.on_incoming(0, &inc(&o[0]));
    b.send(0, b"pong").unwrap();
    let r = b.poll(10, PLENTY);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].kind, Kind::Data);
    assert_eq!(r[0].ack, 1, "the reply acknowledges the request");
    assert_eq!(b.stats().sent, 1);
}

#[test]
fn a_lost_record_is_retransmitted_alone_and_the_gap_fills_in_order() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(0, b"AAAAAAAAAA").unwrap();
    a.send(0, b"BBBBBBBBBB").unwrap();
    a.send(0, b"CCCCCCCCCC").unwrap();
    let out = a.poll(0, PLENTY);
    assert_eq!(out.len(), 3);
    // Record 0 is lost. 1 and 2 arrive first, are held, and nothing is delivered.
    assert!(b.on_incoming(5, &inc(&out[1])).is_empty());
    assert!(b.on_incoming(5, &inc(&out[2])).is_empty());
    // The timer runs out: exactly one retransmission, of record 0.
    let retx = a.poll(1_000, PLENTY);
    assert_eq!(retx.len(), 1);
    assert!(retx[0].retransmit);
    assert_eq!(retx[0].seq, 0);
    let got = b.on_incoming(1_010, &inc(&retx[0]));
    assert_eq!(
        bytes(&got),
        b"AAAAAAAAAABBBBBBBBBBCCCCCCCCCC",
        "in order, once"
    );
}

#[test]
fn a_duplicate_is_dropped_but_owes_one_ack_not_one_per_copy() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(0, b"hi").unwrap();
    let o = a.poll(0, PLENTY).remove(0);
    assert_eq!(b.on_incoming(0, &inc(&o)).len(), 1);
    b.poll(100, PLENTY); // the ack goes out (and is "lost")
    for _ in 0..5 {
        assert!(
            b.on_incoming(200, &inc(&o)).is_empty(),
            "a duplicate delivers nothing"
        );
    }
    assert_eq!(b.stats().duplicates, 5);
    let acks = b.poll(300, PLENTY);
    assert_eq!(acks.len(), 1, "five duplicates, one repair ack");
    assert_eq!(acks[0].ack, 1);
}

#[test]
fn the_window_bounds_what_is_in_flight() {
    let mut a = Arq::new(Config { window: 2, ..cfg() });
    for _ in 0..6 {
        a.send(0, b"x").unwrap();
    }
    assert_eq!(a.poll(0, PLENTY).len(), 2);
    assert!(
        a.poll(1, PLENTY).is_empty(),
        "window full, nothing new, timer not due"
    );
    // An ack for one frees one slot.
    a.on_incoming(
        2,
        &Incoming {
            mark: 0,
            kind: Kind::Ack,
            seq: 0,
            ack: 1,
            shell: 0,
            payload: vec![],
        },
    );
    assert_eq!(a.poll(3, PLENTY).len(), 1);
    // The window can never exceed the L2 credit window, whatever the config says.
    let mut big = Arq::new(Config {
        window: 200,
        queue_max: 64,
        ..cfg()
    });
    for _ in 0..20 {
        big.send(0, b"x").unwrap();
    }
    assert_eq!(big.poll(0, PLENTY).len(), 8);
}

#[test]
fn the_hourly_budget_is_never_exceeded_and_a_stall_is_visible() {
    let mut a = Arq::new(cfg());
    for _ in 0..4 {
        a.send(0, b"x").unwrap();
    }
    let out = a.poll(
        0,
        Limits {
            records_left: 2,
            l2_slots: 8,
        },
    );
    assert_eq!(out.len(), 2, "two records left means two records out");
    let none = a.poll(
        1,
        Limits {
            records_left: 0,
            l2_slots: 8,
        },
    );
    assert!(
        none.iter().all(|o| o.kind != Kind::Data),
        "the voluntary cap stops new data"
    );
    assert_eq!(a.stats().budget_stalls, 1);
    // A retransmission is not new traffic: it is the record already paid for. Refusing it throws
    // away airtime already spent and turns a busy minute into a dead session.
    let retx = a.poll(
        5_000,
        Limits {
            records_left: 0,
            l2_slots: 8,
        },
    );
    assert_eq!(retx.len(), 1, "the due retransmission still goes out");
    assert!(retx[0].retransmit);
    assert_eq!(a.stats().retransmits, 1);
}

#[test]
fn l2_slots_cap_the_burst_too() {
    let mut a = Arq::new(cfg());
    for _ in 0..4 {
        a.send(0, b"x").unwrap();
    }
    assert_eq!(
        a.poll(
            0,
            Limits {
                records_left: 255,
                l2_slots: 1
            }
        )
        .len(),
        1
    );
}

#[test]
fn l2_slots_bound_new_data_not_only_retransmissions() {
    // The caller has one free credit slot. Filling the window past it would mark eight records in
    // flight while one reached the radio, and the other seven would be retransmitted as duplicates
    // the radio never asked for — the exact flood the slot count exists to prevent.
    let mut a = Arq::new(cfg());
    for _ in 0..6 {
        a.send(0, b"x").unwrap();
    }
    let out = a.poll(
        0,
        Limits {
            records_left: 255,
            l2_slots: 2,
        },
    );
    assert_eq!(out.len(), 2, "no more records than there is room for");
    // The two that went out are the two in flight; the rest wait for credit, not for a timer.
    assert_eq!(a.stats().sent, 2);
    assert_eq!(a.pending(), 6, "two in flight and four still queued");
    // The window is not full of phantom slots: a later poll with room sends the next two.
    let more = a.poll(
        1,
        Limits {
            records_left: 255,
            l2_slots: 2,
        },
    );
    assert_eq!(more.len(), 2);
    assert_eq!(a.stats().sent, 4);
}

#[test]
fn retries_back_off_and_then_fail_instead_of_storming() {
    // A heartbeat would be indistinguishable from a retransmission in this test's timing.
    let mut a = Arq::new(Config {
        heartbeat_ms: 60_000,
        ..cfg()
    }); // rto 1000, 2000, 4000, then fail (max_attempts 4)
    a.send(0, b"x").unwrap();
    assert_eq!(a.poll(0, PLENTY).len(), 1);
    assert!(a.poll(999, PLENTY).is_empty());
    assert_eq!(a.poll(1_000, PLENTY).len(), 1); // attempt 2
    assert!(a.poll(2_999, PLENTY).is_empty());
    assert_eq!(a.poll(3_000, PLENTY).len(), 1); // attempt 3 (rto 2000 after 1000)
    assert!(a.poll(6_999, PLENTY).is_empty());
    assert_eq!(a.poll(7_000, PLENTY).len(), 1); // attempt 4 (rto 4000)
    assert!(a.failure().is_none());
    assert!(
        a.poll(7_000 + 8_000, PLENTY).is_empty(),
        "out of attempts: nothing is sent"
    );
    assert_eq!(a.failure(), Some(Failure::RetriesExhausted));
    assert_eq!(a.stats().sent, 4);
    assert_eq!(a.send(0, b"y"), Err(QueueError::NotOpen));
}

#[test]
fn the_queue_is_bounded_and_a_refused_send_changes_nothing() {
    let mut a = Arq::new(Config {
        queue_max: 3,
        ..cfg()
    });
    assert_eq!(a.send(0, &[0u8; 25]), Ok(())); // 3 chunks
    assert_eq!(a.send(0, b"x"), Err(QueueError::Full));
    assert_eq!(a.pending(), 3);
    let mut b = Arq::new(Config {
        queue_max: 3,
        ..cfg()
    });
    assert_eq!(
        b.send(0, &[0u8; 45]),
        Err(QueueError::Full),
        "5 chunks do not fit, none are taken"
    );
    assert_eq!(b.pending(), 0);
}

#[test]
fn nonsense_acks_are_ignored_and_counted() {
    let mut a = Arq::new(cfg());
    a.send(0, b"x").unwrap();
    a.poll(0, PLENTY);
    // Acknowledging something never sent, and something long past.
    a.on_incoming(
        1,
        &Incoming {
            mark: 0,
            kind: Kind::Ack,
            seq: 0,
            ack: 5,
            shell: 0,
            payload: vec![],
        },
    );
    a.on_incoming(
        1,
        &Incoming {
            mark: 0,
            kind: Kind::Ack,
            seq: 0,
            ack: 0xFFF0,
            shell: 0,
            payload: vec![],
        },
    );
    assert_eq!(a.stats().bad_acks, 2);
    assert_eq!(a.pending(), 1, "the real record is still unacknowledged");
}

#[test]
fn sequence_numbers_wrap_without_losing_order() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    let mut now = 0;
    let mut got = Vec::new();
    for i in 0..70_000u32 {
        a.send(0, &[(i % 251) as u8]).unwrap();
        for o in a.poll(now, PLENTY) {
            got.extend(b.on_incoming(now, &inc(&o)));
        }
        now += 200;
        for o in b.poll(now, PLENTY) {
            a.on_incoming(now, &inc(&o));
        }
    }
    assert_eq!(got.len(), 70_000);
    assert!(
        got.iter()
            .enumerate()
            .all(|(i, d)| d.payload == [(i as u32 % 251) as u8])
    );
}

#[test]
fn a_lossy_reordering_link_still_delivers_everything_once_in_order() {
    // A deterministic pseudo-random link: drops ≈1/3, delays by one tick sometimes.
    let (mut a, mut b) = (
        Arq::new(Config {
            max_attempts: 200,
            queue_max: 64,
            ..cfg()
        }),
        Arq::new(cfg()),
    );
    let msg: Vec<u8> = (0..400u32).map(|i| (i * 7 % 256) as u8).collect();
    a.send(2, &msg).unwrap();
    let mut seed: u32 = 12345;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed >> 16
    };
    let (mut to_b, mut to_a): (Vec<Outgoing>, Vec<Outgoing>) = (vec![], vec![]);
    let mut got = Vec::new();
    let mut now = 0u64;
    while got.len() < msg.len() && now < 2_000_000 {
        for o in a.poll(now, PLENTY) {
            if next() % 3 != 0 {
                to_b.push(o);
            }
        }
        for o in b.poll(now, PLENTY) {
            if next() % 3 != 0 {
                to_a.push(o);
            }
        }
        // Deliver in a scrambled order.
        while !to_b.is_empty() {
            let i = next() as usize % to_b.len();
            let o = to_b.remove(i);
            for d in b.on_incoming(now, &inc(&o)) {
                got.extend(d.payload);
            }
        }
        while let Some(o) = to_a.pop() {
            a.on_incoming(now, &inc(&o));
        }
        now += 250;
    }
    assert_eq!(got, msg, "every byte, once, in order");
    assert!(a.failure().is_none());
    assert!(
        a.stats().retransmits > 0,
        "the link really did lose records"
    );
}

#[test]
fn a_reserved_flag_bit_is_not_half_understood() {
    assert_eq!(ldmux_session::arq::Kind::from_flags(0), Some(Kind::Data));
    assert_eq!(ldmux_session::arq::Kind::from_flags(1), Some(Kind::Ack));
    assert_eq!(ldmux_session::arq::Kind::from_flags(2), Some(Kind::Close));
    assert_eq!(ldmux_session::arq::Kind::from_flags(3), None);
    assert_eq!(ldmux_session::arq::Kind::from_flags(0b100), None);
    assert!(Incoming::decode(0b1000_0000, 0, &[0; 4]).is_err());
    assert!(Incoming::decode(0, 0, &[0; 3]).is_err());
}

#[test]
fn close_is_sent_once_after_the_queue_drains() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(0, b"last").unwrap();
    a.close();
    let out = a.poll(0, PLENTY);
    assert_eq!(out.iter().map(|o| o.kind).collect::<Vec<_>>(), [Kind::Data]);
    // Not while the data is unacknowledged: a close must not overtake it.
    assert!(a.poll(1, PLENTY).is_empty());
    b.on_incoming(2, &inc(&out[0]));
    let ack = b.poll(200, PLENTY);
    a.on_incoming(200, &inc(&ack[0]));
    let out2 = a.poll(201, PLENTY);
    assert_eq!(
        out2.iter().map(|o| o.kind).collect::<Vec<_>>(),
        [Kind::Close]
    );
    assert!(
        a.poll(300, PLENTY).iter().all(|o| o.kind != Kind::Close),
        "once"
    );
    b.on_incoming(301, &inc(&out2[0]));
    assert!(b.peer_closed());
}

/// An acknowledgement is control traffic, and refusing it is how a link dies: the peer
/// retransmits a full record for ever, then gives up and the session fails. The voluntary cap
/// bounds what the operator chooses to send, not what it takes to stay in step.
#[test]
fn an_ack_is_sent_even_with_no_budget_left() {
    let mut a = Arq::new(cfg());
    let mut b = Arq::new(cfg());
    b.send(0, b"x").unwrap();
    let record = b.poll(0, PLENTY).pop().unwrap();
    let incoming = Incoming::decode(record.kind.flags(), record.shell, &record.app_bytes()).unwrap();
    a.on_incoming(0, &incoming);
    let out = a.poll(
        1_000,
        Limits {
            records_left: 0,
            l2_slots: 8,
        },
    );
    assert_eq!(out.len(), 1, "the ack is not optional");
    assert_eq!(out[0].kind, Kind::Ack);
}

/// A heartbeat, when one is configured at all, is a pure ack that carries the receive state.
///
/// It is **off by default** (`heartbeat_ms: 0`): an idle session with nothing in flight has
/// nobody waiting on an answer, and on the mesh carrier a heartbeat costs a record of a budget
/// measured in dozens per hour. This test pins the mechanism, not the policy.
#[test]
fn an_idle_link_heartbeats_when_asked_to() {
    // A long retry timer, so the only thing that could fire is the heartbeat.
    let mut a = Arq::new(Config {
        rto_initial_ms: 60_000,
        heartbeat_ms: 3_000,
        ..cfg()
    });
    a.send(0, b"x").unwrap();
    assert_eq!(a.poll(0, PLENTY).len(), 1);
    assert!(a.poll(2_999, PLENTY).is_empty(), "not yet");
    let beat = a.poll(3_000, PLENTY);
    assert_eq!(beat.len(), 1, "the idle link says something");
    assert_eq!(beat[0].kind, Kind::Ack);
    // And it is a wake reason, so a caller that only sleeps until `next_deadline` will send it.
    assert!(a.next_deadline().is_some());
}

/// MeshCore suppresses duplicates by packet hash, first copy wins, at every receiver and
/// repeater. A retransmission carrying the same bytes as the original is therefore dropped on
/// arrival: the sender waits for an ack the peer never had a chance to send, retries until the
/// attempts run out, and the session dies with both radios healthy - "host stopped answering".
/// The per-record mark makes each transmission a different packet.
#[test]
fn a_retransmission_is_a_different_packet() {
    let mut a = Arq::new(Config {
        heartbeat_ms: 60_000,
        ..cfg()
    });
    a.send(0, b"hello").unwrap();
    let first = a.poll(0, PLENTY).pop().unwrap();
    let again = a.poll(1_000, PLENTY).pop().unwrap();
    assert!(again.retransmit);
    assert_ne!(
        first.app_bytes(),
        again.app_bytes(),
        "the same bytes would be suppressed by the mesh's own duplicate check"
    );
}

/// Two heartbeats carry the same ack and the same sequence: without the mark they would be the
/// same packet, and the second would never arrive.
#[test]
fn two_heartbeats_are_different_packets() {
    let mut a = Arq::new(Config {
        rto_initial_ms: 60_000,
        ..cfg()
    });
    a.send(0, b"x").unwrap();
    let _ = a.poll(0, PLENTY);
    let one = a.poll(3_000, PLENTY).pop().unwrap();
    let two = a.poll(6_000, PLENTY).pop().unwrap();
    assert_eq!(one.kind, Kind::Ack);
    assert_eq!(two.kind, Kind::Ack);
    assert_eq!(one.ack, two.ack, "the same receive state");
    assert_ne!(one.app_bytes(), two.app_bytes(), "but not the same packet");
}

/// A path that genuinely takes longer than the configured first timeout must not be retried at
/// that timeout. Two hops at 500 kHz SF8 answer in ≈3.7 s, which is above the 1 s this
/// config starts with, and the old code put a duplicate record on the air for every expiry.
#[test]
fn a_slow_ack_raises_the_retry_floor_to_the_measured_round_trip() {
    // No heartbeat, so the deadline under test is the retry and not a 3 s beat.
    let mut a = Arq::new(Config {
        heartbeat_ms: 0,
        ..cfg()
    });
    a.send(1, b"hello").unwrap();
    assert_eq!(a.poll(0, PLENTY).len(), 1);
    let ack = Incoming {
        kind: Kind::Ack,
        seq: 0,
        ack: 1,
        shell: 0,
        mark: 0,
        payload: Vec::new(),
    };
    a.on_incoming(3_700, &ack);
    assert_eq!(a.smoothed_rtt_ms(), Some(3_700));
    a.send(1, b"again").unwrap();
    assert_eq!(a.poll(3_700, PLENTY).len(), 1);
    assert_eq!(
        a.next_deadline(),
        Some(3_700 + 3_700),
        "the retry waits one measured round trip, not the configured 1 s"
    );
}

/// Karn's rule: a retransmitted slot cannot say which transmission was acknowledged, so it must
/// teach the estimator nothing. Otherwise a timeout's own delay is folded in as round-trip time
/// and the estimate drifts upward forever.
#[test]
fn a_retransmission_teaches_no_round_trip_sample() {
    let mut a = Arq::new(cfg());
    a.send(1, b"lost").unwrap();
    assert_eq!(a.poll(0, PLENTY).len(), 1);
    assert!(a.poll(1_000, PLENTY).pop().unwrap().retransmit);
    let ack = Incoming {
        kind: Kind::Ack,
        seq: 0,
        ack: 1,
        shell: 0,
        mark: 0,
        payload: Vec::new(),
    };
    a.on_incoming(2_000, &ack);
    assert_eq!(a.smoothed_rtt_ms(), None);
}

/// `heartbeat_ms: 0` means "never", and the default config uses it. The deadline must then be
/// the retry time, not `last_sent_at`: the daemon sleeps for `deadline - now`, so a past
/// deadline makes the session loop spin with no work to do.
#[test]
fn a_disabled_heartbeat_does_not_make_the_deadline_due_now() {
    let mut a = Arq::new(Config {
        heartbeat_ms: 0,
        ..cfg()
    });
    a.send(0, b"x").unwrap();
    assert_eq!(a.poll(0, PLENTY).len(), 1);
    assert_eq!(a.next_deadline(), Some(1_000), "one first timeout, not zero");
}

/// A refusal never reached the air, so it must not count against the retry ladder. A radio that
/// has spent its hourly budget refuses every send; counting those as attempts ended the session
/// after five, and a session end costs a handshake that the budget also has to pay for.
#[test]
fn a_refused_record_gets_its_attempt_back_and_waits_a_full_interval() {
    let mut a = Arq::new(cfg());
    a.send(0, b"hello").unwrap();
    let first = a.poll(0, PLENTY);
    assert_eq!(first.len(), 1);
    let seq = first[0].seq;

    // The radio says no. The slot must keep its single attempt and restart its timer.
    assert!(a.on_refused(seq, 10));
    assert_eq!(a.poll(10, PLENTY).len(), 0, "not due straight after a refusal");
    assert_eq!(a.failure(), None);

    // Four refusals in a row would have exhausted `max_attempts` before this rule existed.
    for t in [20, 30, 40, 50] {
        assert!(a.on_refused(seq, t));
    }
    assert_eq!(a.failure(), None, "refusals are not transmissions");

    // A refusal is "not yet", not "lost": the retry follows the firmware's transmit-table pace,
    // not the retry interval for a packet that went out and was not answered.
    let refused_at = 50;
    assert_eq!(
        a.poll(refused_at + REFUSED_RETRY_MS - 1, PLENTY).len(),
        0,
        "not before the table could have drained"
    );
    let again = a.poll(refused_at + REFUSED_RETRY_MS, PLENTY);
    assert_eq!(again.len(), 1);
    assert!(again[0].retransmit);
    assert_eq!(again[0].seq, seq);
}

/// A radio that keeps refusing one record must fail the session, not retry it at the transmit-table
/// pace forever. A full table drains; a wedged radio, a wrong preset or a record the firmware will
/// never parse does not, and the caller needs a named failure rather than silence.
#[test]
fn a_refusal_that_outlives_the_deadline_fails_the_session() {
    // A heartbeat would be extra traffic this test does not mean to exercise.
    let mut a = Arq::new(Config {
        heartbeat_ms: 60_000,
        ..cfg()
    });
    a.send(0, b"x").unwrap();
    let seq = a.poll(0, PLENTY).remove(0).seq;

    // Refused once a second: plenty of refusals, none long enough to trip the deadline.
    let mut now = 0;
    for _ in 0..10 {
        now += 1_000;
        assert!(a.on_refused(seq, now));
        assert_eq!(a.poll(now, PLENTY).len(), 0);
    }
    assert_eq!(a.failure(), None, "a busy radio is not a failure");

    // The run started at the first refusal, so the deadline is measured from then, not extended.
    assert_eq!(a.poll(1_000 + REFUSAL_DEADLINE_MS, PLENTY).len(), 0);
    assert_eq!(a.failure(), Some(Failure::RadioRefused));
    assert_eq!(a.stats().sent, 1, "one record went out; the rest were refusals");
}

/// An answer for a sequence number that is not in flight is ignored, not mis-applied to the
/// oldest slot. The daemon keys its credit accounting on the same number.
#[test]
fn a_refusal_for_an_unknown_sequence_does_nothing() {
    let mut a = Arq::new(cfg());
    a.send(0, b"hello").unwrap();
    let _ = a.poll(0, PLENTY);
    assert!(!a.on_refused(999, 10));
    // The real slot is still due at its own interval, not immediately.
    assert_eq!(a.poll(10, PLENTY).len(), 0);
}

/// **The bytes on the wire are subtracted as they are delivered.**
///
/// The daemon used to count its own `send` calls and reset only when the queue was *completely*
/// empty. On a link that always has something in flight that never happens, so the count climbed
/// until it hit the ceiling and every send was refused with "the outbound queue is full" while the
/// session was healthy — a frozen screen with no error the user could act on. The count belongs to
/// the queue that holds the bytes, and it has to fall as acks arrive.
#[test]
fn queued_bytes_fall_as_records_are_delivered() {
    let (mut a, mut b) = (Arq::new(cfg()), Arq::new(cfg()));
    a.send(1, b"0123456789abcdefghijKLM").unwrap(); // 23 bytes, 3 chunks
    assert_eq!(a.queued_bytes(), 23, "queued before anything is sent");
    let out = a.poll(0, PLENTY);
    assert_eq!(a.queued_bytes(), 23, "still held while in flight");
    for o in &out {
        b.on_incoming(0, &inc(o));
    }
    let ack = b.poll(100, PLENTY);
    for m in &ack {
        a.on_incoming(100, &inc(m));
    }
    assert_eq!(a.queued_bytes(), 0, "delivered bytes are no longer held");
    assert_eq!(a.pending(), 0);
}

/// A config whose airtime is known, so the fast-retransmit floor is real rather than a default.
fn cfg_air() -> Config {
    Config {
        airtime_ms: 600,
        ..cfg()
    }
}

/// **A cumulative ack that does not advance retransmits the front without waiting for the timer.**
///
/// The peer only owes an ack because a record arrived; if that record were the front, the ack would
/// have advanced. So a non-advancing ack is proof the front is missing, and the timer is a die that
/// has already fallen — 0.7 s of the 4.3 s a gap cost at the public preset.
#[test]
fn a_non_advancing_ack_retransmits_the_front_before_the_timer() {
    let (mut a, mut b) = (Arq::new(cfg_air()), Arq::new(cfg_air()));
    a.send(0, b"AAAAAAAAAA").unwrap();
    a.send(0, b"BBBBBBBBBB").unwrap();
    let out = a.poll(0, PLENTY);
    // Record 0 is lost; record 1 arrives and is held, so the peer's ack stays at 0.
    let gap_ack = b.on_incoming(5, &inc(&out[1]));
    assert!(gap_ack.is_empty());
    let held = b.poll(5, PLENTY);
    assert_eq!(held.len(), 1, "the gap owes an ack");
    assert_eq!(held[0].ack, 0, "cumulative: nothing advanced");
    // The floor here is one airtime (600 ms), because no RTT has been sampled yet.
    a.on_incoming(700, &inc(&held[0]));
    let fast = a.poll(700, PLENTY);
    assert_eq!(fast.len(), 1, "the front is re-sent before its 1 s timer: {fast:?}");
    assert!(fast[0].retransmit);
    assert_eq!(fast[0].seq, 0);
    assert_eq!(a.stats().fast_retransmits, 1);
    // And the gap fills in order.
    let got = b.on_incoming(1_200, &inc(&fast[0]));
    assert_eq!(bytes(&got), b"AAAAAAAAAABBBBBBBBBB");
}

/// **A late record is not a lost one.** Inside the floor, the same evidence must not pull the front:
/// the record may still be in the air, and a second copy would be airtime spent twice.
#[test]
fn a_merely_late_record_is_not_retransmitted_before_the_floor() {
    let (mut a, mut b) = (Arq::new(cfg_air()), Arq::new(cfg_air()));
    a.send(0, b"AAAAAAAAAA").unwrap();
    a.send(0, b"BBBBBBBBBB").unwrap();
    let out = a.poll(0, PLENTY);
    b.on_incoming(5, &inc(&out[1]));
    let held = b.poll(5, PLENTY);
    // 599 ms after the front went out: one millisecond short of one airtime.
    a.on_incoming(599, &inc(&held[0]));
    assert!(
        a.poll(599, PLENTY).is_empty(),
        "a record that may still be in the air is not re-sent"
    );
    // The timer still repairs it, exactly as before.
    let retx = a.poll(1_000, PLENTY);
    assert_eq!(retx.len(), 1);
    assert_eq!(retx[0].seq, 0);
}

/// **One pull per proof.** A duplicate of the same ack inside the floor must not send a second copy;
/// the fast retransmission restarts the clock, so the floor has to be cleared again.
#[test]
fn a_stale_duplicate_ack_does_not_retransmit_twice_within_the_floor() {
    let (mut a, mut b) = (Arq::new(cfg_air()), Arq::new(cfg_air()));
    a.send(0, b"AAAAAAAAAA").unwrap();
    a.send(0, b"BBBBBBBBBB").unwrap();
    let out = a.poll(0, PLENTY);
    b.on_incoming(5, &inc(&out[1]));
    let held = b.poll(5, PLENTY);
    a.on_incoming(700, &inc(&held[0]));
    assert_eq!(a.poll(700, PLENTY).len(), 1, "the first pull");
    // The same ack again, immediately: nothing has been re-sent long enough to be missing again.
    a.on_incoming(750, &inc(&held[0]));
    assert!(a.poll(750, PLENTY).is_empty(), "one retransmission per gap");
    assert_eq!(a.stats().fast_retransmits, 1);
}

/// **A gap's ack does not wait for outgoing data**; an in-order record's ack still does.
#[test]
fn an_ack_owed_by_a_gap_goes_out_at_once() {
    let (mut a, mut b) = (Arq::new(cfg_air()), Arq::new(cfg_air()));
    a.send(0, b"AAAAAAAAAA").unwrap();
    a.send(0, b"BBBBBBBBBB").unwrap();
    let out = a.poll(0, PLENTY);
    // In order: the ack waits `ack_delay_ms` (100 in this config), as it always did.
    b.on_incoming(0, &inc(&out[0]));
    assert!(b.poll(1, PLENTY).is_empty(), "an in-order ack rides the next record");
    assert_eq!(b.poll(100, PLENTY).len(), 1);
    // Out of order: the ack goes at once, because it is the sender's only evidence of the gap.
    let mut b2 = Arq::new(cfg_air());
    b2.on_incoming(0, &inc(&out[0]));
    b2.poll(100, PLENTY); // the first ack has gone
    b2.on_incoming(5, &inc(&out[0])); // a duplicate, in order
    assert!(b2.poll(6, PLENTY).is_empty(), "a duplicate is not a gap");
    let mut b3 = Arq::new(cfg_air());
    b3.on_incoming(0, &inc(&out[1])); // record 1 before record 0: a gap
    let urgent = b3.poll(1, PLENTY);
    assert_eq!(urgent.len(), 1, "the gap's ack does not wait: {urgent:?}");
    assert_eq!(urgent[0].kind, Kind::Ack);
    assert_eq!(urgent[0].ack, 0);
}

