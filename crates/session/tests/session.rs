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
fn each_record_carries_up_to_144_bytes_of_payload_at_the_ceiling() {
    let (mut phone, _host) = pair(172);
    phone.send(0, &[7u8; 144]).unwrap();
    let out = phone.poll(0, PLENTY).unwrap();
    assert_eq!(
        out.len(),
        1,
        "144 = 148 application bytes - the 4-byte ARQ header"
    );
    assert_eq!(out[0].len(), 172);
    phone.send(0, &[7u8; 145]).unwrap();
    assert_eq!(
        phone.poll(1, PLENTY).unwrap().len(),
        2,
        "145 bytes need two records"
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
