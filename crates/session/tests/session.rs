use ldmux_session::arq::Limits;
use ldmux_session::handshake::{Initiator, Keypair, PairingEntry, Responder};
use ldmux_session::record::RecordError;
use ldmux_session::session::{Inbound, REKEY_AGE_MS, REKEY_RECORDS};
use ldmux_session::{Session, SessionError};

const PSK: [u8; 32] = [3; 32];
const NOW: u64 = 1_800_000_000_000;
const PLENTY: Limits = Limits {
    records_left: 255,
    l2_slots: 8,
};

fn pair(record_limit: usize) -> (Session, Session) {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone_key = Keypair::generate().unwrap();
    let (init, m1) = Initiator::start(&phone_key, &gw_pub, &PSK, NOW).unwrap();
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
        .unwrap();
    let phone = init.finish(&acc.reply).unwrap();
    (
        Session::new(phone, record_limit, 0).unwrap(),
        Session::new(acc.established, record_limit, 0).unwrap(),
    )
}

#[test]
fn bytes_cross_in_both_directions_encrypted_and_in_order() {
    let (mut phone, mut host) = pair(172);
    let msg: Vec<u8> = (0..500u16).map(|i| i as u8).collect();
    phone.send(4, &msg).unwrap();
    let mut got = Vec::new();
    for rec in phone.poll(0, PLENTY).unwrap() {
        assert!(rec.len() <= 172, "never above the advertised record size");
        assert!(
            !rec.windows(8).any(|w| msg.windows(8).any(|m| m == w)),
            "plaintext does not appear on the wire"
        );
        if let Inbound::Data(d) = host.receive(0, &rec).unwrap() {
            got.extend(d.into_iter().flat_map(|x| x.payload));
        }
    }
    // Window 4 means the first four records; the rest follow after acks.
    let mut now = 10_000;
    for _ in 0..10 {
        for rec in host.poll(now, PLENTY).unwrap() {
            phone.receive(now, &rec).unwrap();
        }
        for rec in phone.poll(now, PLENTY).unwrap() {
            if let Inbound::Data(d) = host.receive(now, &rec).unwrap() {
                got.extend(d.into_iter().flat_map(|x| x.payload));
            }
        }
        now += 10_000;
    }
    assert_eq!(got, msg);
}

#[test]
fn each_record_carries_up_to_143_bytes_of_payload_at_the_ceiling() {
    let (mut phone, _host) = pair(172);
    phone.send(0, &[7u8; 143]).unwrap();
    let out = phone.poll(0, PLENTY).unwrap();
    assert_eq!(
        out.len(),
        1,
        "143 = 148 application bytes - the 5-byte ARQ header"
    );
    assert_eq!(out[0].len(), 172);
    phone.send(0, &[7u8; 144]).unwrap();
    assert_eq!(
        phone.poll(1, PLENTY).unwrap().len(),
        2,
        "144 bytes need two records"
    );
}

#[test]
fn a_replayed_record_is_refused_and_a_forged_one_changes_nothing() {
    let (mut phone, mut host) = pair(172);
    phone.send(0, b"x").unwrap();
    let rec = phone.poll(0, PLENTY).unwrap().remove(0);
    host.receive(0, &rec).unwrap();
    assert_eq!(
        host.receive(0, &rec),
        Err(SessionError::Record(RecordError::Duplicate))
    );
    let mut forged = rec.clone();
    forged[10] ^= 1;
    forged[2] = 9;
    assert_eq!(
        host.receive(0, &forged),
        Err(SessionError::Record(RecordError::BadTag))
    );
    assert_eq!(host.dropped, 2);
}

#[test]
fn a_shorter_advertised_record_is_honoured_and_a_tiny_one_is_refused() {
    let (mut phone, _h) = pair(160);
    phone.send(0, &[1u8; 200]).unwrap();
    assert!(
        phone
            .poll(0, PLENTY)
            .unwrap()
            .iter()
            .all(|r| r.len() <= 160)
    );
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let k = Keypair::generate().unwrap();
    let (init, m1) = Initiator::start(&k, &gw_pub, &PSK, NOW).unwrap();
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
        .unwrap();
    let est = init.finish(&acc.reply).unwrap();
    assert!(matches!(
        Session::new(est, 28, 0),
        Err(SessionError::RecordLimitTooSmall)
    ));
}

#[test]
fn the_budget_limits_sealed_records_and_a_rekey_is_signalled() {
    let (mut phone, _h) = pair(172);
    phone.send(0, &[0u8; 600]).unwrap();
    assert_eq!(
        phone
            .poll(
                0,
                Limits {
                    records_left: 1,
                    l2_slots: 8
                }
            )
            .unwrap()
            .len(),
        1
    );
    assert!(!phone.needs_rekey(1));
    assert!(phone.needs_rekey(REKEY_AGE_MS));
    assert_eq!(REKEY_RECORDS, 65_536);
}

#[test]
fn a_session_closes_cleanly() {
    let (mut phone, mut host) = pair(172);
    phone.arq_mut().close();
    let rec = phone.poll(0, PLENTY).unwrap().remove(0);
    assert_eq!(host.receive(0, &rec).unwrap(), Inbound::Closed);
}

/// A retransmission must not be byte-identical to the original. MeshCore radios deduplicate by
/// payload, so an identical retry is eaten by the first relay that forwarded the original and a
/// multi-hop transfer stalls at its tail. The ARQ hands back the same plaintext; the seal makes it
/// a different packet on the wire. This test is what keeps a future "cache the sealed bytes"
/// optimisation from quietly reintroducing the stall.
#[test]
fn a_retransmission_is_not_byte_identical_to_the_original() {
    let (mut phone, mut host) = pair(172);
    phone.send(4, b"the same plaintext twice").unwrap();
    let first = phone.poll(0, PLENTY).unwrap().remove(0);
    // No ack: the record stays in flight and its timer runs out.
    let retry = phone.poll(2_001, PLENTY).unwrap().remove(0);
    assert_ne!(
        first, retry,
        "the radio would drop the second packet as a duplicate"
    );
    // Both are the same application record once decrypted, so the peer sees one delivery and a
    // duplicate, which is what the ARQ expects.
    match host.receive(0, &first).unwrap() {
        Inbound::Data(d) => assert_eq!(d[0].payload, b"the same plaintext twice"),
        other => panic!("first: {other:?}"),
    }
    match host.receive(0, &retry).unwrap() {
        Inbound::Data(d) => assert_eq!(d[0].payload, b"the same plaintext twice"),
        // A duplicate is filtered by sequence number and reported as quiet, which is also correct.
        Inbound::Quiet => {}
        other => panic!("retry: {other:?}"),
    }
}

/// The ARQ sequence must travel with the sealed bytes, or a caller cannot say which record the
/// radio refused. Before this, every caller kept its own counter and guessed the mapping, which is
/// wrong as soon as a retransmission or a handshake reply shares the tunnel.
#[test]
fn a_sealed_record_keeps_the_arq_sequence_that_names_it() {
    let (mut phone, mut host) = pair(172);
    phone.send(4, b"one").unwrap();
    phone.send(4, b"two").unwrap();
    let sealed = phone.poll_with_seq(0, PLENTY).unwrap();
    assert_eq!(sealed.len(), 2);
    // Sequence numbers are the ARQ's, starting at zero and distinct per record.
    assert_eq!(sealed[0].seq, Some(0));
    assert_eq!(sealed[1].seq, Some(1));
    // The bytes are the sealed records, and the plain API still yields exactly them.
    let (mut other_phone, _) = pair(172);
    other_phone.send(4, b"one").unwrap();
    assert_eq!(
        other_phone.poll(0, PLENTY).unwrap()[0].len(),
        sealed[0].bytes.len(),
        "poll and poll_with_seq seal the same way"
    );
    // Both are readable by the peer.
    for s in &sealed {
        match host.receive(0, &s.bytes).unwrap() {
            Inbound::Data(_) => {}
            other => panic!("expected data, got {other:?}"),
        }
    }
}

/// A refusal must reach the slot by its ARQ number and make the retry follow the radio's pace.
#[test]
fn refusing_a_sealed_record_by_its_sequence_retries_at_the_radio_pace() {
    let (mut phone, _host) = pair(172);
    phone.send(4, b"refused").unwrap();
    let sealed = phone.poll_with_seq(0, PLENTY).unwrap();
    let seq = sealed[0].seq.expect("a data record has a slot");
    // The radio refused it without transmitting, so the slot gives its attempt back and waits the
    // transmit-table pace, not the retry interval.
    assert!(phone.arq_mut().on_refused(seq, 10));
    assert!(
        phone
            .poll_with_seq(10 + ldmux_session::arq::REFUSED_RETRY_MS - 1, PLENTY)
            .unwrap()
            .is_empty(),
        "not before the table could have drained"
    );
    let retry = phone
        .poll_with_seq(10 + ldmux_session::arq::REFUSED_RETRY_MS, PLENTY)
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].seq, Some(seq), "the same slot, not a new record");
    assert_ne!(
        retry[0].bytes, sealed[0].bytes,
        "resealed, so the radio does not drop it as a duplicate"
    );
}

/// **A route that grows under an in-flight record must not end the session.**
///
/// A slot keeps the payload it was split at, so a smaller record limit makes the front record
/// unsealable. That used to discard the whole batch — acks and perfectly good records with it — and
/// charge an attempt per poll until the session died. Measured shape: the path gains one repeater
/// mid-screen, the record limit drops by one byte, and a working link tears itself down.
#[test]
fn a_route_that_grows_mid_record_splits_it_instead_of_failing() {
    let (mut phone, mut host) = pair(172);
    // A payload that fills a 172-byte record exactly, queued at the wide size.
    let wide = ldmux_session::record::application_bytes(172) - ldmux_session::arq::HEADER;
    let msg: Vec<u8> = (0..u16::try_from(wide).expect("small")).map(|i| i as u8).collect();
    phone.send(1, &msg).unwrap();
    let first = phone.poll(0, PLENTY).expect("the wide record seals");
    assert_eq!(1, first.len());

    // The path grows: the host resizes the link to a record four bytes smaller.
    phone.set_record_limit(168);
    let next = phone
        .poll(10_000, PLENTY)
        .expect("a record the new size cannot hold is split, not fatal");
    assert_eq!(1, next.len(), "the retransmission goes out anyway");
    assert!(next[0].len() <= 168, "and it fits the smaller record");

    // Nothing was lost: the tail is queued, and the peer reassembles the original bytes in order.
    let mut got = Vec::new();
    let mut now = 20_000;
    for _ in 0..24 {
        for rec in phone.poll(now, PLENTY).unwrap() {
            if let Inbound::Data(d) = host.receive(now, &rec).unwrap() {
                got.extend(d.into_iter().flat_map(|x| x.payload));
            }
        }
        for rec in host.poll(now, PLENTY).unwrap() {
            phone.receive(now, &rec).unwrap();
        }
        now += 5_000;
        if got.len() >= msg.len() {
            break;
        }
    }
    assert_eq!(msg, got, "every byte arrives, in order, after the split");
}
