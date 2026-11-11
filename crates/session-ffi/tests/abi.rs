//! Drives the C ABI the way C does: raw pointers, status codes, buffers the caller owns.
//! Nothing here touches the safe `ldmux-session` API.
#![allow(unsafe_code)]
// Every call below passes buffers sized as the header states; per-call comments would repeat it.
#![allow(clippy::undocumented_unsafe_blocks)]

use std::ptr::{null, null_mut};

use ldmux_session::handshake::{Initiator, Keypair};
use ldmux_session_ffi::*;

const NOW: u64 = 1_800_000_000_000;

struct Pair {
    phone: *mut LdmuxSession,
    host: *mut LdmuxSession,
}

fn keypair() -> ([u8; 32], [u8; 32]) {
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    // SAFETY: both buffers are 32 writable bytes.
    assert_eq!(
        unsafe { ldmux_keypair_generate(a.as_mut_ptr(), b.as_mut_ptr()) },
        0
    );
    (a, b)
}

/// A whole handshake through the C surface.
fn handshake(limit: u32) -> Pair {
    let (hp, hq) = keypair();
    let (pp, pq) = keypair();
    let psk = [7u8; 32];
    let mut init: *mut LdmuxInitiator = null_mut();
    let mut msg1 = [0u8; LDMUX_MSG1_LEN];
    // SAFETY: all pointers are valid for the sizes the header states.
    unsafe {
        assert_eq!(
            ldmux_initiator_start(
                pp.as_ptr(),
                pq.as_ptr(),
                hq.as_ptr(),
                psk.as_ptr(),
                NOW,
                &mut init,
                msg1.as_mut_ptr()
            ),
            LDMUX_OK
        );
    }
    assert!(!init.is_null());
    let (mut reply, mut remote, mut ts) = ([0u8; LDMUX_MSG2_LEN], [0u8; 32], 0u64);
    let mut host: *mut LdmuxSession = null_mut();
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            ldmux_responder_accept(
                hp.as_ptr(),
                hq.as_ptr(),
                psk.as_ptr(),
                null(),
                0,
                NOW,
                msg1.as_ptr(),
                msg1.len(),
                3,
                limit,
                reply.as_mut_ptr(),
                remote.as_mut_ptr(),
                &mut ts,
                &mut host
            ),
            LDMUX_OK
        );
    }
    assert_eq!(remote, pq, "the responder reports the phone key to pin");
    assert_eq!(ts, NOW);
    let mut phone: *mut LdmuxSession = null_mut();
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            ldmux_initiator_finish(
                &mut init,
                reply.as_ptr(),
                reply.len(),
                limit,
                NOW,
                &mut phone
            ),
            LDMUX_OK
        );
    }
    assert!(init.is_null(), "finish consumes the initiator");
    let mut peer = [0u8; 32];
    // SAFETY: 32 writable bytes.
    unsafe { assert_eq!(ldmux_session_peer_key(phone, peer.as_mut_ptr()), LDMUX_OK) };
    assert_eq!(peer, hq);
    Pair { phone, host }
}

/// Poll `from` and deliver every sealed record into `to`.
fn pump(from: *mut LdmuxSession, to: *mut LdmuxSession, now: u64) -> usize {
    // SAFETY: handles are live; buffers are RECORD_MAX bytes.
    unsafe {
        let n = ldmux_session_poll(from, now, 20, 8);
        assert!(n >= 0, "poll: {n}");
        let mut moved = 0;
        loop {
            let mut buf = [0u8; LDMUX_RECORD_MAX];
            let mut len = 0usize;
            let rc = ldmux_session_take_record(from, buf.as_mut_ptr(), buf.len(), &mut len);
            if rc == 0 {
                break;
            }
            assert_eq!(rc, 1);
            assert!(len <= LDMUX_RECORD_MAX);
            assert!(ldmux_session_receive(to, now, buf.as_ptr(), len) >= 0);
            moved += 1;
        }
        moved
    }
}

fn take(s: *mut LdmuxSession) -> Option<(u8, Vec<u8>)> {
    let (mut shell, mut buf, mut len) = (0u8, [0u8; 256], 0usize);
    // SAFETY: live handle, valid buffers.
    let rc = unsafe {
        ldmux_session_take_delivered(s, &mut shell, buf.as_mut_ptr(), buf.len(), &mut len)
    };
    (rc == 1).then(|| (shell, buf[..len].to_vec()))
}

fn free(mut s: *mut LdmuxSession) {
    // SAFETY: a live handle freed through its own variable.
    unsafe { ldmux_session_free(&mut s) };
}

#[test]
fn a_whole_handshake_then_one_record_each_way_through_the_c_surface() {
    let p = handshake(172);
    // SAFETY: live handle, 5 readable bytes.
    unsafe {
        assert_eq!(
            ldmux_session_send(p.phone, 1, b"hello".as_ptr(), 5),
            LDMUX_OK
        )
    };
    assert_eq!(pump(p.phone, p.host, NOW + 1), 1);
    assert_eq!(take(p.host), Some((1, b"hello".to_vec())));
    // SAFETY: live handle.
    unsafe {
        assert_eq!(
            ldmux_session_send(p.host, 1, b"world".as_ptr(), 5),
            LDMUX_OK
        )
    };
    assert_eq!(pump(p.host, p.phone, NOW + 2), 1);
    assert_eq!(take(p.phone), Some((1, b"world".to_vec())));
    free(p.phone);
    free(p.host);
}

#[test]
fn a_record_on_the_wire_is_header_inner_and_tag_around_the_payload() {
    let p = handshake(172);
    // SAFETY: live handle.
    unsafe {
        assert_eq!(
            ldmux_session_send(p.phone, 0, [9u8; 10].as_ptr(), 10),
            LDMUX_OK
        )
    };
    // SAFETY: live handle.
    unsafe { assert_eq!(ldmux_session_poll(p.phone, NOW, 5, 5), 1) };
    let (mut buf, mut len) = ([0u8; LDMUX_RECORD_MAX], 0usize);
    // SAFETY: live handle, valid buffer.
    unsafe {
        assert_eq!(
            ldmux_session_take_record(p.phone, buf.as_mut_ptr(), buf.len(), &mut len),
            1
        )
    };
    // 6 clear + 2 inner + 5 ARQ header (seq, ack, mark) + 10 payload + 16 tag
    assert_eq!(len, 6 + 2 + 5 + 10 + 16);
    assert_eq!(buf[0], 0x13);
    free(p.phone);
    free(p.host);
}

#[test]
fn poll_never_exceeds_the_budget_the_caller_states() {
    let p = handshake(172);
    for _ in 0..6 {
        // SAFETY: live handle.
        unsafe {
            assert_eq!(
                ldmux_session_send(p.phone, 0, [1u8; 144].as_ptr(), 144),
                LDMUX_OK
            )
        };
    }
    // SAFETY: live handle.
    unsafe {
        assert_eq!(
            ldmux_session_poll(p.phone, NOW, 0, 8),
            0,
            "no budget, nothing sealed"
        );
        assert_eq!(
            ldmux_session_poll(p.phone, NOW, 2, 8),
            2,
            "budget 2 caps at 2"
        );
    }
    free(p.phone);
    free(p.host);
}

/// The most a single record can carry: 172 - 24 overhead - 5 ARQ header. 144 was the
/// number before the ARQ header grew its `mark` byte, and these tests still sent 144, which
/// the session correctly split into two records.
#[test]
fn a_full_143_byte_payload_fills_the_172_byte_record() {
    let p = handshake(172);
    // SAFETY: live handle.
    unsafe {
        assert_eq!(
            ldmux_session_send(p.phone, 0, [5u8; 143].as_ptr(), 143),
            LDMUX_OK
        )
    };
    // SAFETY: live handle.
    unsafe { assert_eq!(ldmux_session_poll(p.phone, NOW, 5, 5), 1) };
    let (mut buf, mut len) = ([0u8; LDMUX_RECORD_MAX], 0usize);
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            ldmux_session_take_record(p.phone, buf.as_mut_ptr(), buf.len(), &mut len),
            1
        )
    };
    assert_eq!(len, 172);
    free(p.phone);
    free(p.host);
}

#[test]
fn a_tampered_record_is_refused_and_counted() {
    let p = handshake(172);
    // SAFETY: live handles.
    unsafe {
        assert_eq!(ldmux_session_send(p.phone, 0, b"x".as_ptr(), 1), LDMUX_OK);
        ldmux_session_poll(p.phone, NOW, 5, 5);
    }
    let (mut buf, mut len) = ([0u8; LDMUX_RECORD_MAX], 0usize);
    // SAFETY: as above.
    unsafe { ldmux_session_take_record(p.phone, buf.as_mut_ptr(), buf.len(), &mut len) };
    buf[len - 1] ^= 1;
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            ldmux_session_receive(p.host, NOW, buf.as_ptr(), len),
            LDMUX_ERR_BAD_TAG
        );
        assert_eq!(ldmux_session_dropped(p.host), 1);
    }
    assert_eq!(take(p.host), None);
    free(p.phone);
    free(p.host);
}

#[test]
fn a_too_small_output_buffer_reports_the_size_and_loses_nothing() {
    let p = handshake(172);
    // SAFETY: live handle.
    unsafe {
        assert_eq!(ldmux_session_send(p.phone, 0, b"abc".as_ptr(), 3), LDMUX_OK);
        ldmux_session_poll(p.phone, NOW, 5, 5);
    }
    let (mut tiny, mut len) = ([0u8; 4], 0usize);
    // SAFETY: valid 4-byte buffer.
    let rc = unsafe { ldmux_session_take_record(p.phone, tiny.as_mut_ptr(), tiny.len(), &mut len) };
    assert_eq!(rc, LDMUX_ERR_BUFFER);
    assert!(len > 4, "the needed size is reported");
    let mut big = [0u8; LDMUX_RECORD_MAX];
    // SAFETY: valid buffer.
    let rc = unsafe { ldmux_session_take_record(p.phone, big.as_mut_ptr(), big.len(), &mut len) };
    assert_eq!(rc, 1, "the record stayed queued");
    free(p.phone);
    free(p.host);
}

#[test]
fn wrong_psk_wrong_size_and_replay_are_distinct_errors() {
    let (hp, hq) = keypair();
    let (pp, pq) = keypair();
    let mut init: *mut LdmuxInitiator = null_mut();
    let mut msg1 = [0u8; LDMUX_MSG1_LEN];
    // SAFETY: valid sizes.
    unsafe {
        ldmux_initiator_start(
            pp.as_ptr(),
            pq.as_ptr(),
            hq.as_ptr(),
            [1u8; 32].as_ptr(),
            NOW,
            &mut init,
            msg1.as_mut_ptr(),
        );
        ldmux_initiator_free(&mut init);
    }
    assert!(init.is_null());
    let (mut reply, mut remote, mut ts) = ([0u8; 50], [0u8; 32], 0u64);
    let mut out: *mut LdmuxSession = null_mut();
    let accept = |psk: &[u8; 32], last: u64, m: &[u8], out: &mut *mut LdmuxSession| {
        let (mut reply, mut remote, mut ts) = ([0u8; 50], [0u8; 32], 0u64);
        // SAFETY: valid sizes.
        unsafe {
            ldmux_responder_accept(
                hp.as_ptr(),
                hq.as_ptr(),
                psk.as_ptr(),
                null(),
                last,
                NOW,
                m.as_ptr(),
                m.len(),
                1,
                172,
                reply.as_mut_ptr(),
                remote.as_mut_ptr(),
                &mut ts,
                out,
            )
        }
    };
    assert_eq!(accept(&[2u8; 32], 0, &msg1, &mut out), LDMUX_ERR_BAD_FILTER);
    assert_eq!(
        accept(&[1u8; 32], 0, &msg1[..100], &mut out),
        LDMUX_ERR_MALFORMED
    );
    // **A repeated msg1 is a retry, not a replay.** A handshake goes out as three identical copies
    // (three floods, fresh wrapper nonces, one record), so an equal timestamp has to be accepted or
    // copies two and three are discarded and the handshake dies whenever the first copy is the one
    // that was lost. The replay guard remains for a timestamp that sits *behind* the last accepted
    // one by more than `HANDSHAKE_REPLAY_TOLERANCE_MS`, which is unit-tested in the session crate.
    assert_eq!(accept(&[1u8; 32], NOW, &msg1, &mut out), LDMUX_OK);
    assert!(!out.is_null(), "a duplicate copy of the same attempt completes");
    let _ = (&mut reply, &mut remote, &mut ts);
}

#[test]
fn a_pin_for_a_different_phone_is_refused() {
    let (hp, hq) = keypair();
    let (pp, pq) = keypair();
    let psk = [3u8; 32];
    let mut init: *mut LdmuxInitiator = null_mut();
    let mut msg1 = [0u8; LDMUX_MSG1_LEN];
    // SAFETY: valid sizes.
    unsafe {
        ldmux_initiator_start(
            pp.as_ptr(),
            pq.as_ptr(),
            hq.as_ptr(),
            psk.as_ptr(),
            NOW,
            &mut init,
            msg1.as_mut_ptr(),
        );
        ldmux_initiator_free(&mut init);
    }
    let other = [9u8; 32];
    let (mut reply, mut remote, mut ts) = ([0u8; 50], [0u8; 32], 0u64);
    let mut out: *mut LdmuxSession = null_mut();
    // SAFETY: valid sizes.
    let rc = unsafe {
        ldmux_responder_accept(
            hp.as_ptr(),
            hq.as_ptr(),
            psk.as_ptr(),
            other.as_ptr(),
            0,
            NOW,
            msg1.as_ptr(),
            msg1.len(),
            1,
            172,
            reply.as_mut_ptr(),
            remote.as_mut_ptr(),
            &mut ts,
            &mut out,
        )
    };
    assert_eq!(rc, LDMUX_ERR_PIN_MISMATCH);
}

#[test]
fn freeing_twice_is_a_no_op_and_a_nulled_handle_is_an_error_not_ub() {
    let p = handshake(172);
    let mut s = p.phone;
    // SAFETY: `s` is live for the first call; afterwards the slot is null by contract.
    unsafe {
        ldmux_session_free(&mut s);
        assert!(s.is_null());
        ldmux_session_free(&mut s);
        ldmux_session_free(&mut s);
        assert_eq!(ldmux_session_send(s, 0, b"x".as_ptr(), 1), LDMUX_ERR_NULL);
        assert_eq!(ldmux_session_poll(s, NOW, 1, 1), LDMUX_ERR_NULL);
        assert_eq!(ldmux_session_status(s, NOW), LDMUX_ERR_NULL);
        ldmux_session_free(null_mut());
        ldmux_initiator_free(null_mut());
    }
    free(p.host);
}

#[test]
fn null_arguments_are_errors_everywhere() {
    // SAFETY: the whole point is passing null.
    unsafe {
        assert_eq!(
            ldmux_keypair_generate(null_mut(), null_mut()),
            LDMUX_ERR_NULL
        );
        let mut out: *mut LdmuxInitiator = null_mut();
        assert_eq!(
            ldmux_initiator_start(null(), null(), null(), null(), 0, &mut out, null_mut()),
            LDMUX_ERR_NULL
        );
        let mut s: *mut LdmuxSession = null_mut();
        let mut hs: *mut LdmuxInitiator = null_mut();
        assert_eq!(
            ldmux_initiator_finish(&mut hs, null(), 0, 172, 0, &mut s),
            LDMUX_ERR_NULL
        );
        assert_eq!(
            ldmux_initiator_finish(null_mut(), null(), 0, 172, 0, &mut s),
            LDMUX_ERR_NULL
        );
    }
}

#[test]
fn a_bad_message_2_still_consumes_the_initiator() {
    let (_, hq) = keypair();
    let (pp, pq) = keypair();
    let mut init: *mut LdmuxInitiator = null_mut();
    let mut msg1 = [0u8; LDMUX_MSG1_LEN];
    // SAFETY: valid sizes.
    unsafe {
        ldmux_initiator_start(
            pp.as_ptr(),
            pq.as_ptr(),
            hq.as_ptr(),
            [1u8; 32].as_ptr(),
            NOW,
            &mut init,
            msg1.as_mut_ptr(),
        );
    }
    let mut s: *mut LdmuxSession = null_mut();
    // SAFETY: valid sizes.
    let rc = unsafe { ldmux_initiator_finish(&mut init, [0u8; 50].as_ptr(), 50, 172, NOW, &mut s) };
    assert!(rc < 0);
    assert!(init.is_null());
    assert!(s.is_null());
}

#[test]
fn a_record_limit_with_no_room_is_refused_not_clamped() {
    let (hp, hq) = keypair();
    let (pp, pq) = keypair();
    let psk = [4u8; 32];
    let mut init: *mut LdmuxInitiator = null_mut();
    let mut msg1 = [0u8; LDMUX_MSG1_LEN];
    // SAFETY: valid sizes.
    unsafe {
        ldmux_initiator_start(
            pp.as_ptr(),
            pq.as_ptr(),
            hq.as_ptr(),
            psk.as_ptr(),
            NOW,
            &mut init,
            msg1.as_mut_ptr(),
        );
        ldmux_initiator_free(&mut init);
    }
    let (mut reply, mut remote, mut ts) = ([0u8; 50], [0u8; 32], 0u64);
    let mut out: *mut LdmuxSession = null_mut();
    // SAFETY: valid sizes.
    let rc = unsafe {
        ldmux_responder_accept(
            hp.as_ptr(),
            hq.as_ptr(),
            psk.as_ptr(),
            null(),
            0,
            NOW,
            msg1.as_ptr(),
            msg1.len(),
            1,
            24,
            reply.as_mut_ptr(),
            remote.as_mut_ptr(),
            &mut ts,
            &mut out,
        )
    };
    assert_eq!(rc, LDMUX_ERR_RECORD_LIMIT);
}

#[test]
fn the_abi_is_wire_compatible_with_the_safe_api() {
    // A message 1 made by the safe API is accepted through the ABI: one implementation.
    let (hp, hq) = keypair();
    let local = Keypair::generate().expect("keypair");
    let psk = [8u8; 32];
    let (_hs, m1) = Initiator::start(&local, &hq, &psk, NOW).expect("start");
    let (mut reply, mut remote, mut ts) = ([0u8; 50], [0u8; 32], 0u64);
    let mut out: *mut LdmuxSession = null_mut();
    // SAFETY: valid sizes.
    let rc = unsafe {
        ldmux_responder_accept(
            hp.as_ptr(),
            hq.as_ptr(),
            psk.as_ptr(),
            null(),
            0,
            NOW,
            m1.as_ptr(),
            m1.len(),
            1,
            172,
            reply.as_mut_ptr(),
            remote.as_mut_ptr(),
            &mut ts,
            &mut out,
        )
    };
    assert_eq!(rc, LDMUX_OK);
    free(out);
}

/// The refused-record path over the C ABI: a record is popped with the ARQ sequence that names its
/// slot, and a refusal for that number gives the attempt back so the retry follows the radio's
/// transmit-table pace instead of a full retry interval.
#[test]
fn a_refused_record_is_named_by_its_sequence_and_retried_at_the_radio_pace() {
    let p = handshake(172);
    // SAFETY: the handles are live from `handshake` and the buffers match the header's sizes.
    unsafe {
        assert_eq!(ldmux_session_send(p.phone, 4, b"refused".as_ptr(), 7), LDMUX_OK);
        assert!(ldmux_session_poll(p.phone, NOW, 20, 8) >= 1);
        let mut buf = [0u8; LDMUX_RECORD_MAX];
        let mut len = 0usize;
        let mut seq = 0u16;
        assert_eq!(
            ldmux_session_take_record_seq(p.phone, buf.as_mut_ptr(), buf.len(), &mut len, &mut seq),
            1
        );
        // The refusal gives the attempt back: the slot stays in flight.
        assert_eq!(ldmux_session_refuse_seq(p.phone, NOW + 10, seq), 1);
        // Nothing is sent before the table could have drained.
        assert_eq!(
            ldmux_session_poll(
                p.phone,
                NOW + 10 + ldmux_session::arq::REFUSED_RETRY_MS - 1,
                20,
                8
            ),
            0
        );
        // Then the retry, from the same slot.
        assert!(ldmux_session_poll(p.phone, NOW + 10 + ldmux_session::arq::REFUSED_RETRY_MS, 20, 8) >= 1);
        let mut seq2 = 0u16;
        assert_eq!(
            ldmux_session_take_record_seq(p.phone, buf.as_mut_ptr(), buf.len(), &mut len, &mut seq2),
            1
        );
        assert_eq!(seq2, seq, "the retransmission comes from the same slot");
        // A number that names no live slot changes nothing and says so.
        assert_eq!(ldmux_session_refuse_seq(p.phone, NOW + 1000, 999), 0);
        // The plain accessor still works and ignores the number.
        assert_eq!(ldmux_session_poll(p.phone, NOW + 2000, 20, 8), 0);
    }
}
