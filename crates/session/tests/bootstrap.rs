//! The 1-round-trip bootstrap, against a real handshake and a real simulated mesh.
//!
//! Every test here pins a defect that reached the field:
//!
//! * `RC-5` — three copies of one initiation built three sessions, and a retry that timed out
//!   abandoned its ephemeral so the answer which finally landed could not be decrypted.
//! * `RC-1` — a precursor route request went out before the initiation and collided with it.
//! * `RC-6` — an initiation too large for a long path was transmitted and never reported, so the
//!   fragments exist at all.
//! * the dedup ring — a byte-identical retry is discarded by every repeater before anything looks
//!   at it, which is why every transmission carries a nonce.

use ldmux_mesh::budget::RecordBudget;
use ldmux_mesh::sim::{NodeSpec, VirtualMesh};
use ldmux_session::bootstrap::{
    self, Answer, AnswerCache, Answered, Attempt, NeedHandshakeError, NeedHandshakeSender, Refusal,
    FRAG_LEN0, FRAG_LEN1, MSG1_CONTENT_LEN, MSG1_NONCED_LEN, MSG2_CONTENT_LEN, MSG2_NONCED_LEN,
    NEED_HANDSHAKE_LEN,
};
use ldmux_session::handshake::{Keypair, PairingEntry, Responder, TIMESTAMP_WINDOW_MS};
use ldmux_session::direction::{KIND_MSG1_FRAG, KIND_NEED_HANDSHAKE};

const PSK: [u8; 32] = [7u8; 32];
/// Unix milliseconds; any value inside the timestamp window works.
const WALL: u64 = 1_700_000_000_000;

fn pairing() -> PairingEntry {
    PairingEntry {
        psk: PSK,
        pinned: None,
        last_timestamp_ms: 0,
    }
}

/// A host, a phone key and the phone's attempt, which is the trio every test needs.
fn setup() -> (Responder, Keypair, Attempt) {
    let host_key = Keypair::generate().expect("a host key");
    let host_pub = host_key.public;
    let responder = Responder::new(host_key);
    let phone = Keypair::generate().expect("a phone key");
    let attempt = Attempt::start(&phone, &host_pub, &PSK, WALL).expect("an initiation");
    (responder, phone, attempt)
}

/// `RC-5`: one initiation, sent three times, is one session and three answers.
///
/// The copies are spaced by the rung floor rather than by milliseconds, because one answer per peer
/// per five seconds is the rule ([`AnswerCache`]): three copies inside one burst are one answer, and
/// the three retries a bootstrap actually gets are the three rungs of the ladder.
#[test]
fn rc5_three_copies_of_one_initiation_build_one_session_and_three_answers() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    let content = *attempt.content();

    let mut replies = Vec::new();
    for (n, at) in [0u64, 6_000, 12_000].into_iter().enumerate() {
        let record = attempt.direct();
        let answered = cache
            .answer(&responder, &entries, 1, at, WALL, &record)
            .expect("the initiation is answerable");
        match answered {
            Answered::Fresh { reply, .. } => {
                assert_eq!(n, 0, "copy {n} rebuilt the session");
                replies.push(reply);
            }
            Answered::Cached { reply } => {
                assert_ne!(n, 0, "the first copy was not fresh");
                replies.push(reply);
            }
            Answered::Held => panic!("a whole initiation is never held"),
        }
    }

    assert_eq!(cache.len(), 1, "one initiation, {n} entries", n = cache.len());
    for reply in &replies {
        assert_eq!(reply.len(), MSG2_NONCED_LEN);
        assert_eq!(
            reply[..MSG2_CONTENT_LEN],
            replies[0][..MSG2_CONTENT_LEN],
            "a repeat answered with different Noise content, so it is a second session"
        );
    }
    let nonces: Vec<u16> = replies
        .iter()
        .map(|r| u16::from_le_bytes([r[MSG2_CONTENT_LEN], r[MSG2_CONTENT_LEN + 1]]))
        .collect();
    assert_eq!(
        nonces.len(),
        nonces.iter().collect::<std::collections::HashSet<_>>().len(),
        "two copies were byte-identical, so a repeater would drop the second: {nonces:?}"
    );
    assert!(cache.is_live(&content), "the accepted initiation is not the live one");
}

/// `RC-5`: the rung that timed out must not have thrown its ephemeral away.
///
/// The answer to rung 1 arrives after rung 2 has already gone out. It still decrypts, because the
/// attempt's Noise state was never rebuilt.
#[test]
fn rc5_an_answer_that_arrives_after_a_retry_still_finishes() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];

    let first = attempt.direct();
    let Answered::Fresh { reply, .. } = cache
        .answer(&responder, &entries, 1, 0, WALL, &first)
        .expect("answered")
    else {
        panic!("the first copy must be fresh");
    };
    // Rung 2 goes out without hearing anything.
    let _second = attempt.direct();
    // Now rung 1's answer lands.
    match attempt.answer(&reply).expect("rung 1's answer is ours") {
        Answer::Finished(established) => assert_eq!(established.index, 1),
        other => panic!("expected a finished attempt, got {other:?}"),
    }
    // And the copy the responder may retransmit is not a failure.
    assert!(matches!(attempt.answer(&reply), Ok(Answer::Repeated)));
}

/// An answer that belongs to no attempt of ours is ignored rather than failing the attempt.
#[test]
fn a_foreign_answer_after_a_finished_attempt_is_ignored() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    let Answered::Fresh { reply, .. } = cache
        .answer(&responder, &entries, 1, 0, WALL, &attempt.direct())
        .expect("answered")
    else {
        panic!("fresh expected")
    };
    assert!(matches!(attempt.answer(&reply), Ok(Answer::Finished(_))));
    let mut other = reply.clone();
    other[MSG2_CONTENT_LEN - 1] ^= 0xFF;
    assert!(matches!(attempt.answer(&other), Ok(Answer::Ignored)));
}

/// A replay from inside the cache window costs an answer; from outside it costs a refusal, and the
/// reason is the timestamp window rather than a silent rebuild.
#[test]
fn a_replay_inside_the_window_is_answered_and_outside_it_is_refused() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    let record = attempt.direct();

    assert!(matches!(
        cache.answer(&responder, &entries, 1, 0, WALL, &record),
        Ok(Answered::Fresh { .. })
    ));
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 6_000, WALL + 60_000, &record),
        Ok(Answered::Cached { .. })
    ));
    // Past the timestamp window the initiation is not merely forgotten, it is stale: an old capture
    // must not become a new session.
    assert_eq!(
        cache
            .answer(&responder, &entries, 1, 12_000, WALL + TIMESTAMP_WINDOW_MS + 1_000, &record)
            .err(),
        Some(Refusal::Stale)
    );
}

/// A captured initiation that a newer one replaced is ignored **without touching the live session**.
///
/// This is the half of `RC-5` that made a replay an unauthenticated session kill.
#[test]
fn a_superseded_initiation_is_refused_and_the_live_session_survives() {
    let (responder, phone, mut old) = setup();
    let host_pub = responder.static_key.public;
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 0, WALL, &old.direct()),
        Ok(Answered::Fresh { .. })
    ));
    let mut newer = Attempt::start(&phone, &host_pub, &PSK, WALL + 1_000).expect("a new attempt");
    assert!(matches!(
        cache.answer(&responder, &entries, 2, 6_000, WALL + 1_000, &newer.direct()),
        Ok(Answered::Fresh { .. })
    ));
    assert_eq!(
        cache
            .answer(&responder, &entries, 1, 12_000, WALL + 1_000, &old.direct())
            .err(),
        Some(Refusal::Superseded)
    );
    assert!(cache.is_live(newer.content()));
    assert_eq!(cache.len(), 2, "the replay changed what is remembered");
}

/// The five-second answer rate, which is what stops a captured initiation being replayed as a
/// transmission amplifier.
#[test]
fn one_answer_per_peer_per_rate_window() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    let record = attempt.direct();
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 0, WALL, &record),
        Ok(Answered::Fresh { .. })
    ));
    match cache.answer(&responder, &entries, 1, 1_000, WALL, &record) {
        Err(Refusal::RateLimited { retry_after_ms }) => assert_eq!(retry_after_ms, 4_000),
        other => panic!("a second answer went out inside the window: {other:?}"),
    }
    assert!(
        cache
            .answer(&responder, &entries, 1, bootstrap::ANSWER_RATE_MS, WALL, &record)
            .is_ok(),
        "the rate window never reopened"
    );
}

/// `RC-6`: an initiation that does not fit the carrier's budget goes as two fragments, and the mesh
/// is what carries them.
#[test]
fn a_long_path_carries_the_initiation_in_two_fragments() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];

    // 93 bytes is the flood guarantee at the firmware's longest legal path, and it is far below the
    // 123 the whole initiation needs.
    let budget = RecordBudget::flood_scoped(64);
    assert_eq!(budget.limit(), 93);
    assert!(
        budget.limit() < MSG1_NONCED_LEN,
        "the whole initiation fits, so this test proves nothing"
    );

    let parts = attempt.parts();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].len(), FRAG_LEN0);
    assert_eq!(parts[1].len(), FRAG_LEN1);
    for part in &parts {
        assert!(
            budget.receiver_accepts(part.len()),
            "a {} byte fragment exceeds the receive ceiling",
            part.len()
        );
    }

    let mut mesh = VirtualMesh::new(5);
    let phone = mesh.add_node(NodeSpec::companion());
    let repeater = mesh.add_node(NodeSpec::repeater().with_forward_delay_ms(40));
    let host = mesh.add_node(NodeSpec::companion());
    mesh.link(phone, repeater, 9.0);
    mesh.link(repeater, host, 9.0);

    // The second part waits out the first hop's forward window; sending them together loses the
    // second at the repeater, which is why `part_gap_ms` exists.
    mesh.send_flood(phone, &parts[0]);
    mesh.advance(bootstrap::part_gap_ms(mesh.airtime_of(FRAG_LEN0)));
    mesh.send_flood(phone, &parts[1]);
    mesh.advance(5_000);

    let arrived: Vec<Vec<u8>> = mesh
        .deliveries()
        .iter()
        .filter(|d| d.to == host)
        .map(|d| d.payload.clone())
        .collect();
    assert_eq!(arrived.len(), 2, "the mesh did not carry both parts: {arrived:?}");

    assert!(matches!(
        cache.answer(&responder, &entries, 1, 0, WALL, &arrived[0]),
        Ok(Answered::Held)
    ));
    match cache.answer(&responder, &entries, 1, 1_000, WALL, &arrived[1]) {
        Ok(Answered::Fresh { initiation, .. }) => {
            assert_eq!(initiation, *attempt.content(), "the parts did not reform the initiation");
        }
        other => panic!("the joined initiation was not answered: {other:?}"),
    }
}

/// A repeated part is harmless, a part whose twin never arrives is held, and two attempts are never
/// merged.
#[test]
fn fragments_are_joined_by_their_own_id_only() {
    let (responder, phone, mut first) = setup();
    let host_pub = responder.static_key.public;
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];

    let parts = first.parts();
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 0, WALL, &parts[1]),
        Ok(Answered::Held)
    ));
    // The same part again changes nothing.
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 10, WALL, &parts[1]),
        Ok(Answered::Held)
    ));
    // A part of another attempt does not complete this one.
    let mut other = Attempt::start(&phone, &host_pub, &PSK, WALL + 1).expect("a second attempt");
    let other_parts = other.parts();
    assert!(matches!(
        cache.answer(&responder, &entries, 1, 20, WALL, &other_parts[1]),
        Ok(Answered::Held)
    ));
    assert!(
        matches!(
            cache.answer(&responder, &entries, 1, 30, WALL, &parts[0]),
            Ok(Answered::Fresh { .. })
        ),
        "part 0 of the first attempt did not complete the first attempt"
    );
}

/// The dedup ring, in one test: identical bytes are dropped, and the nonce is what makes a retry
/// survive it.
#[test]
fn a_nonce_makes_a_retry_survive_the_dedup_ring() {
    let mut mesh = VirtualMesh::new(3);
    let phone = mesh.add_node(NodeSpec::companion());
    let host = mesh.add_node(NodeSpec::companion());
    mesh.link(phone, host, 10.0);

    let (_, _phone_key, mut attempt) = setup();
    let path: Vec<u8> = Vec::new();

    let repeat = attempt.direct();
    mesh.send_direct(phone, &path, &repeat);
    let again = mesh.send_direct(phone, &path, &repeat);
    mesh.advance(2_000);
    let received = |mesh: &VirtualMesh| {
        mesh.deliveries().iter().filter(|d| d.to == host).count()
    };
    assert_eq!(received(&mesh), 1, "identical bytes were not deduplicated");
    assert!(
        again.is_empty(),
        "the identical resend reached an application anyway"
    );

    // The next rung is the same Noise content with a fresh nonce.
    let retry = attempt.direct();
    assert_eq!(
        retry[..MSG1_CONTENT_LEN],
        repeat[..MSG1_CONTENT_LEN],
        "the retry changed the Noise content, so the responder's cache cannot match it"
    );
    assert_ne!(retry, repeat, "the retry was byte-identical to the copy before it");
    mesh.send_direct(phone, &path, &retry);
    mesh.advance(2_000);
    assert_eq!(received(&mesh), 2, "the retried initiation was dropped as a duplicate");
}

/// The rung arithmetic, at the hop counts the field actually produced (0 and 1 at the desk, 5 and 14
/// at range, 28 the worst recorded).
#[test]
fn the_rung_covers_the_route_round_trip() {
    // Public preset: a full record is 602.6 ms on the air.
    let air = 602.6;
    for hops in [0u8, 1] {
        assert_eq!(
            bootstrap::rung_ms(hops, air),
            bootstrap::FIRST_RUNG_MIN_MS,
            "a neighbour's rung fell below the floor at {hops} hops"
        );
    }
    assert_eq!(bootstrap::answer_window_ms(), bootstrap::ANSWER_WINDOW_MS);
    let mut previous = 0;
    for hops in [5u8, 14, 28] {
        let rung = bootstrap::rung_ms(hops, air);
        assert!(
            rung > previous,
            "the rung did not grow with the route: {hops} hops gave {rung} ms"
        );
        // A rung has to cover the flight there and back plus one forward delay per hop each way.
        let round_trip = 2.0 * f64::from(hops) * air;
        assert!(
            rung as f64 >= round_trip,
            "a {hops} hop rung of {rung} ms is shorter than the {round_trip} ms flight"
        );
        previous = rung;
    }
    assert!(
        bootstrap::rung_ms(28, air) < bootstrap::ANSWER_WINDOW_MS,
        "one rung is already the whole answer window"
    );
}

/// `KIND_NEED_HANDSHAKE`: accepted when it is ours, refused when it is stale, forged or repeated too
/// soon.
#[test]
fn need_handshake_is_authenticated_and_rate_limited() {
    let host = Keypair::generate().expect("a host key");
    let host_pub = host.public;
    let record = bootstrap::need_handshake_encode(&PSK, &host_pub, WALL);
    assert_eq!(record.len(), NEED_HANDSHAKE_LEN);
    assert_eq!(record[0], KIND_NEED_HANDSHAKE);
    assert_eq!(
        bootstrap::need_handshake_verify(&PSK, &host_pub, WALL, TIMESTAMP_WINDOW_MS, &record),
        Ok(WALL)
    );

    // Stale.
    assert_eq!(
        bootstrap::need_handshake_verify(
            &PSK,
            &host_pub,
            WALL + TIMESTAMP_WINDOW_MS + 1,
            TIMESTAMP_WINDOW_MS,
            &record
        ),
        Err(NeedHandshakeError::Stale)
    );
    // Forged: another host's key, so another HMAC.
    let other = Keypair::generate().expect("another host key");
    assert_eq!(
        bootstrap::need_handshake_verify(&PSK, &other.public, WALL, TIMESTAMP_WINDOW_MS, &record),
        Err(NeedHandshakeError::BadTag)
    );
    // A wrong-length record is not one at all.
    assert_eq!(
        bootstrap::need_handshake_verify(&PSK, &host_pub, WALL, TIMESTAMP_WINDOW_MS, &record[..24]),
        Err(NeedHandshakeError::Malformed)
    );

    // One emission per five seconds, because the record is a transmission.
    let mut sender = NeedHandshakeSender::new();
    assert!(sender.emit(&PSK, &host_pub, 0, WALL).is_some());
    assert!(
        sender.emit(&PSK, &host_pub, 1_000, WALL).is_none(),
        "a second record went out inside the rate window"
    );
    assert!(sender.emit(&PSK, &host_pub, bootstrap::ANSWER_RATE_MS, WALL).is_some());
}

/// A fragment carrying the wrong kind or a nonsense part byte is refused rather than joined.
#[test]
fn malformed_fragments_are_refused() {
    let (responder, _phone, mut attempt) = setup();
    let mut cache = AnswerCache::plan_default();
    let entries = [pairing()];
    let parts = attempt.parts();
    let mut bad = parts[0].clone();
    bad[0] = KIND_NEED_HANDSHAKE;
    assert_eq!(
        cache.answer(&responder, &entries, 1, 0, WALL, &bad).err(),
        Some(Refusal::Malformed)
    );
    let mut bad_index = parts[0].clone();
    bad_index[3] = bootstrap::part_byte(9, 2);
    assert_eq!(
        cache.answer(&responder, &entries, 1, 0, WALL, &bad_index).err(),
        Some(Refusal::Malformed)
    );
    assert!(bootstrap::parse_part(&parts[0]).is_some());
    assert!(bootstrap::parse_part(&[KIND_MSG1_FRAG]).is_none());
}
