//! Golden vectors.
//!
//! The hex strings were produced by running the Dart reference codec
//! (`packages/meshcore_gateway`, `tool/vectors.dart`), not typed from the
//! document. If this crate and the Dart package ever disagree, one of them
//! fails here first.

use ldmux_mesh::constants::{
    BindError, CAP_STATUS_PUSH, CloseReason, CloseReason::Shutdown, FrameType, HelloError,
    PairError, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI,
};
use ldmux_mesh::frame::{Decoder, Frame, MeshError};
use ldmux_mesh::messages;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn builders_match_the_reference_byte_for_byte() {
    let token: Vec<u8> = (0u8..16).collect();
    let peer: Vec<u8> = (0u8..32).collect();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("hello", messages::hello(3).encode()),
        (
            "hello_ok",
            messages::hello_ok(RadioState::Connected, 2).encode(),
        ),
        (
            "hello_err",
            messages::hello_err(HelloError::VersionMismatch).encode(),
        ),
        ("pair", messages::pair(&token).expect("pair").encode()),
        ("pair_ok", messages::pair_ok().encode()),
        ("pair_err", messages::pair_err(PairError::BadToken).encode()),
        ("bind", messages::bind(&peer).expect("bind").encode()),
        (
            "bind_ok",
            messages::bind_ok(1, 172).expect("bind_ok").encode(),
        ),
        ("bind_err", messages::bind_err(BindError::NoPath).encode()),
        (
            "record",
            messages::record(7, &[0xAA, 0xBB]).expect("record").encode(),
        ),
        ("recv", messages::recv(&[1, 2, 3]).expect("recv").encode()),
        (
            "send_result",
            messages::send_result(7, SendOutcome::SentDirect).encode(),
        ),
        (
            "status5",
            messages::status(
                RadioState::RadioLost,
                UNKNOWN_RSSI,
                UNKNOWN_BATTERY,
                0x1234,
                None,
            )
            .encode(),
        ),
        (
            "status6",
            messages::status(RadioState::NoContact, -71, 88, 0x1234, Some(57)).encode(),
        ),
        ("close", messages::close(Shutdown).encode()),
    ];

    let expected: Vec<(&str, &str)> = vec![
        ("hello", "4d4701010002000103"),
        ("hello_ok", "4d470102000300010200"),
        ("hello_err", "4d47010300010001"),
        ("pair", "4d470110001000000102030405060708090a0b0c0d0e0f"),
        ("pair_ok", "4d470111000000"),
        ("pair_err", "4d47011200010001"),
        (
            "bind",
            "4d470120002000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        ),
        ("bind_ok", "4d47012100020001ac"),
        ("bind_err", "4d47012200010002"),
        ("record", "4d47013000030007aabb"),
        ("recv", "4d470131000300010203"),
        ("send_result", "4d4701320002000701"),
        ("status5", "4d4701400005000280ff3412"),
        ("status6", "4d47014000060003b958341239"),
        ("close", "4d47016000010005"),
    ];

    assert_eq!(cases.len(), expected.len());
    for ((name, actual), (expected_name, expected_hex)) in cases.iter().zip(expected) {
        assert_eq!(name, &expected_name);
        assert_eq!(hex(actual), expected_hex, "vector {name}");
    }
}

#[test]
fn decodes_every_reference_vector_back_to_its_frame() {
    let hexes = [
        "4d4701010002000103",
        "4d470102000300010200",
        "4d470110001000000102030405060708090a0b0c0d0e0f",
        "4d47013000030007aabb",
        "4d47014000060003b958341239",
        "4d47016000010005",
    ];
    for h in hexes {
        let mut decoder = Decoder::new();
        let frames = decoder.push(&bytes(h)).expect("decode");
        assert_eq!(frames.len(), 1, "one frame from {h}");
        assert_eq!(hex(&frames[0].encode()), h, "round trip {h}");
    }
}

#[test]
fn the_sixth_status_byte_is_optional() {
    let five = Frame {
        kind: FrameType::Status,
        payload: bytes("0280ff3412"),
    };
    assert_eq!(
        messages::read_status(&five)
            .expect("status")
            .ops_left_this_hour,
        None
    );

    let six = Frame {
        kind: FrameType::Status,
        payload: bytes("03b958341239"),
    };
    let status = messages::read_status(&six).expect("status");
    assert_eq!(status.ops_left_this_hour, Some(57));
    assert_eq!(status.radio_state, RadioState::NoContact);
    assert_eq!(status.rssi, -71);
    assert_eq!(status.battery_percent, 88);
    assert_eq!(status.tx_airtime_tenths_this_hour, 0x1234);
}

#[test]
fn the_unknown_sentinels_survive_the_wire() {
    let frame = messages::status(
        RadioState::Connected,
        UNKNOWN_RSSI,
        UNKNOWN_BATTERY,
        0,
        None,
    );
    let status = messages::read_status(&frame).expect("status");
    assert_eq!(status.rssi, -128);
    assert_eq!(status.battery_percent, 0xFF);
}

#[test]
fn readers_reject_a_frame_of_the_wrong_type_instead_of_guessing() {
    let hello = messages::hello(0);
    assert!(matches!(
        messages::read_bind_ok(&hello),
        Err(MeshError::BadPayload(_))
    ));
    assert!(matches!(
        messages::read_send_result(&hello),
        Err(MeshError::BadPayload(_))
    ));
}

#[test]
fn builders_refuse_values_the_contract_does_not_allow() {
    assert!(messages::pair(&[0; 15]).is_err());
    assert!(messages::bind(&[0; 31]).is_err());
    assert!(messages::record(1, &[0; 173]).is_err());
    assert!(messages::bind_ok(0, 173).is_err());
    assert!(messages::bind_ok(0, 0).is_err());
}

#[test]
fn close_reasons_and_outcomes_round_trip() {
    for reason in [
        CloseReason::Protocol,
        CloseReason::Version,
        CloseReason::Busy,
        CloseReason::PreAuthTimeout,
        CloseReason::Shutdown,
    ] {
        let frame = messages::close(reason);
        assert_eq!(messages::read_close(&frame).expect("close"), reason);
    }
    for outcome in [
        SendOutcome::SentDirect,
        SendOutcome::SentFlood,
        SendOutcome::ErrNotFound,
        SendOutcome::ErrBusy,
        SendOutcome::ErrTooLong,
        SendOutcome::LocalTimeout,
    ] {
        let frame = messages::send_result(3, outcome);
        assert_eq!(
            messages::read_send_result(&frame).expect("result").outcome,
            outcome
        );
    }
}

#[test]
fn capability_bits_are_the_documented_ones() {
    assert_eq!(CAP_STATUS_PUSH, 1 << 1);
    assert_eq!(ldmux_mesh::CAP_RECV_CREDIT, 1 << 0);
}
