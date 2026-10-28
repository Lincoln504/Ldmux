use ldmux_session::handshake::{
    Established, HandshakeError, Initiator, Keypair, MSG1_LEN, MSG2_LEN, PairingEntry, Responder,
    TIMESTAMP_WINDOW_MS,
};
use ldmux_session::record::{self, APPLICATION_BYTES, OVERHEAD, RecordError, ReplayWindow};

const PSK: [u8; 32] = [0x42; 32];
const NOW: u64 = 1_800_000_000_000;

fn pairing() -> PairingEntry {
    PairingEntry {
        psk: PSK,
        pinned: None,
        last_timestamp_ms: 0,
    }
}

struct Pair {
    phone: Established,
    host: Established,
}

fn connect() -> Pair {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let responder = Responder::new(gw);
    let phone_key = Keypair::generate().unwrap();
    let (init, m1) = Initiator::start(&phone_key, &gw_pub, &PSK, NOW).unwrap();
    let acc = responder.accept(&[pairing()], NOW, &m1, 7).unwrap();
    let phone = init.finish(&acc.reply).unwrap();
    Pair {
        phone,
        host: acc.established,
    }
}

/// Independent known-answer check of the exact suite: the Cacophony vector for
/// `Noise_IKpsk2_25519_ChaChaPoly_SHA256`, shipped in snow's own test tree and checked in
/// here. It runs snow (the library we depend on) against a vector it did not generate.
#[test]
fn the_noise_suite_matches_the_cacophony_known_answer_vector() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("vectors/noise_ikpsk2_cacophony.json")).unwrap();
    assert_eq!(v["protocol_name"], ldmux_session::handshake::SUITE);
    let h = |k: &str| hex(v[k].as_str().unwrap());
    let psk: [u8; 32] = hex(v["init_psks"][0].as_str().unwrap()).try_into().unwrap();
    let params: snow::params::NoiseParams = ldmux_session::handshake::SUITE.parse().unwrap();
    let prologue = h("init_prologue");
    let mut i = snow::Builder::new(params.clone())
        .local_private_key(&h("init_static"))
        .unwrap()
        .remote_public_key(&h("init_remote_static"))
        .unwrap()
        .psk(2, &psk)
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(&h("init_ephemeral"))
        .build_initiator()
        .unwrap();
    let mut r = snow::Builder::new(params)
        .local_private_key(&h("resp_static"))
        .unwrap()
        .psk(2, &psk)
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(&h("resp_ephemeral"))
        .build_responder()
        .unwrap();
    let msgs = v["messages"].as_array().unwrap();
    let mut buf = [0u8; 1024];
    let mut out = [0u8; 1024];
    for (n, m) in msgs.iter().take(2).enumerate() {
        let (w, rd) = if n == 0 {
            (&mut i, &mut r)
        } else {
            (&mut r, &mut i)
        };
        let payload = hex(m["payload"].as_str().unwrap());
        let len = w.write_message(&payload, &mut buf).unwrap();
        assert_eq!(
            &buf[..len],
            hex(m["ciphertext"].as_str().unwrap()).as_slice(),
            "message {n}"
        );
        let got = rd.read_message(&buf[..len], &mut out).unwrap();
        assert_eq!(&out[..got], payload.as_slice());
    }
    assert_eq!(i.get_handshake_hash(), h("handshake_hash").as_slice());
    assert!(i.is_handshake_finished() && r.is_handshake_finished());
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

#[test]
fn handshake_messages_are_the_adr_sizes() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone = Keypair::generate().unwrap();
    let (init, m1) = Initiator::start(&phone, &gw_pub, &PSK, NOW).unwrap();
    assert_eq!(m1.len(), MSG1_LEN);
    assert_eq!(m1.len(), 121);
    assert_eq!(m1[0], 0x11);
    let acc = Responder::new(gw)
        .accept(&[pairing()], NOW, &m1, 3)
        .unwrap();
    assert_eq!(acc.reply.len(), MSG2_LEN);
    assert_eq!(acc.reply.len(), 50);
    assert_eq!(acc.reply[0], 0x12);
    let est = init.finish(&acc.reply).unwrap();
    assert_eq!(est.index, 3, "the responder assigns the index in message 2");
    assert_eq!(acc.remote_static, phone.public);
    assert_eq!(est.remote_static, gw_pub);
}

#[test]
fn a_wrong_psk_is_dropped_by_the_filter_before_any_dh() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone = Keypair::generate().unwrap();
    let (_i, m1) = Initiator::start(&phone, &gw_pub, &[9u8; 32], NOW).unwrap();
    let err = Responder::new(gw).accept(&[pairing()], NOW, &m1, 0).err();
    assert_eq!(err, Some(HandshakeError::BadFilter));
}

#[test]
fn a_flipped_bit_anywhere_in_message_1_is_refused() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone = Keypair::generate().unwrap();
    let (_i, m1) = Initiator::start(&phone, &gw_pub, &PSK, NOW).unwrap();
    let responder = Responder::new(gw);
    for at in 0..m1.len() {
        let mut bad = m1.clone();
        bad[at] ^= 0x01;
        assert!(
            responder.accept(&[pairing()], NOW, &bad, 0).is_err(),
            "byte {at}"
        );
    }
}

#[test]
fn stale_replayed_and_wrong_length_handshakes_are_refused() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone = Keypair::generate().unwrap();
    let responder = Responder::new(gw);

    let (_i, old) = Initiator::start(&phone, &gw_pub, &PSK, NOW - TIMESTAMP_WINDOW_MS - 1).unwrap();
    assert_eq!(
        responder.accept(&[pairing()], NOW, &old, 0).err(),
        Some(HandshakeError::Stale)
    );

    let (_i, fresh) = Initiator::start(&phone, &gw_pub, &PSK, NOW).unwrap();
    let mut p = pairing();
    let acc = responder.accept(&[p.clone()], NOW, &fresh, 0).unwrap();
    p.last_timestamp_ms = acc.timestamp_ms;
    assert_eq!(
        responder.accept(&[p], NOW, &fresh, 0).err(),
        Some(HandshakeError::Replay)
    );

    assert_eq!(
        responder.accept(&[pairing()], NOW, &fresh[..100], 0).err(),
        Some(HandshakeError::Malformed)
    );
}

#[test]
fn a_pinned_pairing_refuses_a_different_phone_key() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let responder = Responder::new(gw);
    let first = Keypair::generate().unwrap();
    let (_i, m1) = Initiator::start(&first, &gw_pub, &PSK, NOW).unwrap();
    let acc = responder.accept(&[pairing()], NOW, &m1, 0).unwrap();
    let mut p = pairing();
    p.pinned = Some(acc.remote_static);
    p.last_timestamp_ms = acc.timestamp_ms;

    let other = Keypair::generate().unwrap();
    let (_i, m1) = Initiator::start(&other, &gw_pub, &PSK, NOW + 1).unwrap();
    assert_eq!(
        responder.accept(&[p.clone()], NOW, &m1, 0).err(),
        Some(HandshakeError::PinMismatch)
    );
    let (_i, m1) = Initiator::start(&first, &gw_pub, &PSK, NOW + 1).unwrap();
    assert!(
        responder.accept(&[p], NOW, &m1, 0).is_ok(),
        "the pinned key itself still gets in"
    );
}

#[test]
fn the_right_pairing_is_found_among_several() {
    let gw = Keypair::generate().unwrap();
    let gw_pub = gw.public;
    let phone = Keypair::generate().unwrap();
    let other = PairingEntry {
        psk: [1; 32],
        pinned: None,
        last_timestamp_ms: 0,
    };
    let (_i, m1) = Initiator::start(&phone, &gw_pub, &PSK, NOW).unwrap();
    let acc = Responder::new(gw)
        .accept(&[other, pairing()], NOW, &m1, 0)
        .unwrap();
    assert_eq!(acc.pairing, 1);
}

#[test]
fn the_record_layout_is_six_clear_two_inner_sixteen_tag() {
    let p = connect();
    let rec = record::seal(
        p.phone.transport_for_tests(),
        7,
        0x0403_0201,
        0,
        5,
        b"abc",
        172,
    )
    .unwrap();
    assert_eq!(rec[0], 0x13, "kind/version");
    assert_eq!(rec[1], 7, "session index");
    assert_eq!(
        &rec[2..6],
        &[0x01, 0x02, 0x03, 0x04],
        "counter, little-endian"
    );
    assert_eq!(rec.len(), 6 + 2 + 3 + 16);
    assert_eq!(OVERHEAD, 24);
    let mut w = ReplayWindow::default();
    let o = record::open(p.host.transport_for_tests(), 7, &mut w, &rec).unwrap();
    assert_eq!(
        (o.flags, o.shell, o.app.as_slice(), o.counter),
        (0, 5, &b"abc"[..], 0x0403_0201)
    );
}

#[test]
fn a_full_record_is_172_bytes_and_one_more_application_byte_is_refused() {
    let p = connect();
    let t = p.phone.transport_for_tests();
    let rec = record::seal(t, 7, 0, 0, 0, &[0xAA; APPLICATION_BYTES], 172).unwrap();
    assert_eq!(APPLICATION_BYTES, 148);
    assert_eq!(rec.len(), 172);
    assert_eq!(
        record::seal(t, 7, 1, 0, 0, &[0xAA; APPLICATION_BYTES + 1], 172).err(),
        Some(RecordError::TooLong)
    );
    // A shorter advertised limit (a longer path) is honoured, not the ceiling.
    assert_eq!(
        record::seal(t, 7, 2, 0, 0, &[0; 137], 160).err(),
        Some(RecordError::TooLong)
    );
    assert_eq!(
        record::seal(t, 7, 3, 0, 0, &[0; 136], 160).unwrap().len(),
        160
    );
}

#[test]
fn tampering_fails_the_tag_and_leaves_the_replay_window_alone() {
    let p = connect();
    let rec = record::seal(p.phone.transport_for_tests(), 7, 0, 0, 0, b"hello", 172).unwrap();
    let t = p.host.transport_for_tests();
    let mut w = ReplayWindow::default();
    for at in 6..rec.len() {
        let mut bad = rec.clone();
        bad[at] ^= 0x80;
        assert_eq!(
            record::open(t, 7, &mut w, &bad).err(),
            Some(RecordError::BadTag),
            "byte {at}"
        );
    }
    // A forged counter fails too, and never moves the window.
    let mut bad = rec.clone();
    bad[2] = 0xFF;
    assert_eq!(
        record::open(t, 7, &mut w, &bad).err(),
        Some(RecordError::BadTag)
    );
    assert!(
        record::open(t, 7, &mut w, &rec).is_ok(),
        "the genuine record 0 is still fresh"
    );
    assert_eq!(
        record::open(t, 7, &mut w, &rec).err(),
        Some(RecordError::Duplicate)
    );
    assert_eq!(
        record::open(t, 8, &mut w, &rec).err(),
        Some(RecordError::WrongIndex)
    );
    assert_eq!(
        record::open(t, 7, &mut w, &rec[..10]).err(),
        Some(RecordError::Malformed)
    );
}

#[test]
fn the_counter_is_the_nonce_so_a_reordered_record_still_opens() {
    let p = connect();
    let s = p.phone.transport_for_tests();
    let a = record::seal(s, 7, 0, 0, 0, b"first", 172).unwrap();
    let b = record::seal(s, 7, 1, 0, 0, b"second", 172).unwrap();
    let t = p.host.transport_for_tests();
    let mut w = ReplayWindow::default();
    assert_eq!(record::open(t, 7, &mut w, &b).unwrap().app, b"second");
    assert_eq!(record::open(t, 7, &mut w, &a).unwrap().app, b"first");
}

#[test]
fn the_last_counter_value_is_refused_not_wrapped() {
    let p = connect();
    assert_eq!(
        record::seal(p.phone.transport_for_tests(), 7, u32::MAX, 0, 0, b"x", 172).err(),
        Some(RecordError::CounterExhausted)
    );
}
