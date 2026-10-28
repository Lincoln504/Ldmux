//! Typed access to the frame set: one builder and one total reader per frame.
//!
//! Readers are total. A frame of the wrong type, or one too short for its
//! fields, yields `Err(MeshError::BadPayload)`. A caller that gets that for a
//! frame it expected must close the connection with
//! [`crate::CloseReason::Protocol`].

use crate::constants::{
    BindError, CloseReason, FrameType, HelloError, PAIRING_TOKEN_BYTES, PairError, RECORD_FLOOR,
    RECORD_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI, VERSION,
};
use crate::frame::{Frame, MeshError};

/// `HELLO_OK`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HelloOk {
    pub version: u8,
    pub capabilities: u8,
    pub radio_state: RadioState,
}

/// `BIND_OK`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BindOk {
    pub capabilities: u8,
    /// The largest record this peer's path allows. Size records to this.
    pub uplink_record_max: u8,
}

/// `SEND_RESULT`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SendResult {
    pub seq: u8,
    pub outcome: SendOutcome,
}

/// `STATUS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    pub radio_state: RadioState,
    /// Signed; [`UNKNOWN_RSSI`] means no reading.
    pub rssi: i8,
    /// [`UNKNOWN_BATTERY`] means not reported.
    pub battery_percent: u8,
    /// The gateway's own transmit airtime this hour, in 0.1 s units.
    pub tx_airtime_tenths_this_hour: u16,
    /// Records of the current uplink size that still fit this hour. `None`
    /// from a gateway that predates the sixth byte.
    pub ops_left_this_hour: Option<u8>,
}

/// One downlink record. Opaque: never parsed here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecvRecord(pub Vec<u8>);

/// `HELLO`: version + capability bits.
#[must_use]
pub fn hello(capabilities: u8) -> Frame {
    Frame {
        kind: FrameType::Hello,
        payload: vec![VERSION, capabilities],
    }
}

/// `HELLO_OK`.
#[must_use]
pub fn hello_ok(radio_state: RadioState, capabilities: u8) -> Frame {
    Frame {
        kind: FrameType::HelloOk,
        payload: vec![VERSION, capabilities, radio_state.code()],
    }
}

/// `HELLO_ERR`.
#[must_use]
pub fn hello_err(reason: HelloError) -> Frame {
    let code = match reason {
        HelloError::VersionMismatch => 1,
        HelloError::Busy => 2,
        HelloError::Unknown(code) => code,
    };
    Frame {
        kind: FrameType::HelloErr,
        payload: vec![code],
    }
}

/// `PAIR`: the pairing token, exactly [`PAIRING_TOKEN_BYTES`].
pub fn pair(token: &[u8]) -> Result<Frame, MeshError> {
    if token.len() != PAIRING_TOKEN_BYTES {
        return Err(MeshError::BadPayload("pairing token must be 16 bytes"));
    }
    Ok(Frame {
        kind: FrameType::Pair,
        payload: token.to_vec(),
    })
}

/// `PAIR_OK`.
#[must_use]
pub fn pair_ok() -> Frame {
    Frame {
        kind: FrameType::PairOk,
        payload: Vec::new(),
    }
}

/// `PAIR_ERR`.
#[must_use]
pub fn pair_err(reason: PairError) -> Frame {
    let code = match reason {
        PairError::BadToken => 1,
        PairError::AlreadyPaired => 2,
        PairError::Revoked => 3,
        PairError::Unknown(code) => code,
    };
    Frame {
        kind: FrameType::PairErr,
        payload: vec![code],
    }
}

/// `BIND`: the peer's 32-byte public key.
pub fn bind(peer_pubkey: &[u8]) -> Result<Frame, MeshError> {
    if peer_pubkey.len() != 32 {
        return Err(MeshError::BadPayload("peer public key must be 32 bytes"));
    }
    Ok(Frame {
        kind: FrameType::Bind,
        payload: peer_pubkey.to_vec(),
    })
}

/// `BIND_OK`.
pub fn bind_ok(capabilities: u8, uplink_record_max: u8) -> Result<Frame, MeshError> {
    if uplink_record_max == 0 || usize::from(uplink_record_max) > RECORD_MAX {
        return Err(MeshError::BadPayload("uplink record max out of range"));
    }
    Ok(Frame {
        kind: FrameType::BindOk,
        payload: vec![capabilities, uplink_record_max],
    })
}

/// `BIND_ERR`.
#[must_use]
pub fn bind_err(reason: BindError) -> Frame {
    let code = match reason {
        BindError::NotAContact => 1,
        BindError::NoPath => 2,
        BindError::Unknown(code) => code,
    };
    Frame {
        kind: FrameType::BindErr,
        payload: vec![code],
    }
}

/// `RECORD`: one uplink record, at most [`RECORD_MAX`].
pub fn record(seq: u8, opaque_record: &[u8]) -> Result<Frame, MeshError> {
    if opaque_record.len() > RECORD_MAX {
        return Err(MeshError::BadPayload("record above the ceiling"));
    }
    let mut payload = Vec::with_capacity(1 + opaque_record.len());
    payload.push(seq);
    payload.extend_from_slice(opaque_record);
    Ok(Frame {
        kind: FrameType::Record,
        payload,
    })
}

/// `RECV`: one downlink record.
pub fn recv(opaque_record: &[u8]) -> Result<Frame, MeshError> {
    if opaque_record.len() > RECORD_MAX {
        return Err(MeshError::BadPayload("record above the ceiling"));
    }
    Ok(Frame {
        kind: FrameType::Recv,
        payload: opaque_record.to_vec(),
    })
}

/// `SEND_RESULT`: what the radio did with the record carrying `seq`.
#[must_use]
pub fn send_result(seq: u8, outcome: SendOutcome) -> Frame {
    Frame {
        kind: FrameType::SendResult,
        payload: vec![seq, outcome.code()],
    }
}

/// `STATUS`.
pub fn status(
    radio_state: RadioState,
    rssi: i8,
    battery_percent: u8,
    tx_airtime_tenths_this_hour: u16,
    ops_left_this_hour: Option<u8>,
) -> Frame {
    let mut payload = vec![
        radio_state.code(),
        rssi as u8,
        battery_percent,
        (tx_airtime_tenths_this_hour & 0xFF) as u8,
        ((tx_airtime_tenths_this_hour >> 8) & 0xFF) as u8,
    ];
    if let Some(ops) = ops_left_this_hour {
        payload.push(ops);
    }
    Frame {
        kind: FrameType::Status,
        payload,
    }
}

/// `CLOSE`.
#[must_use]
pub fn close(reason: CloseReason) -> Frame {
    Frame {
        kind: FrameType::Close,
        payload: vec![reason.code()],
    }
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// Reads `HELLO`.
pub fn read_hello(f: &Frame) -> Result<(u8, u8), MeshError> {
    if f.kind != FrameType::Hello || f.payload.len() < 2 {
        return Err(MeshError::BadPayload("expected HELLO"));
    }
    Ok((f.payload[0], f.payload[1]))
}

/// Reads `HELLO_OK`.
pub fn read_hello_ok(f: &Frame) -> Result<HelloOk, MeshError> {
    if f.kind != FrameType::HelloOk || f.payload.len() < 3 {
        return Err(MeshError::BadPayload("expected HELLO_OK"));
    }
    Ok(HelloOk {
        version: f.payload[0],
        capabilities: f.payload[1],
        radio_state: RadioState::from_code(f.payload[2]),
    })
}

/// Reads `HELLO_ERR`.
pub fn read_hello_err(f: &Frame) -> Result<HelloError, MeshError> {
    let code = *f
        .payload
        .first()
        .filter(|_| f.kind == FrameType::HelloErr)
        .ok_or(MeshError::BadPayload("expected HELLO_ERR"))?;
    Ok(HelloError::from_code(code))
}

/// Reads `PAIR`.
pub fn read_pair(f: &Frame) -> Result<&[u8], MeshError> {
    if f.kind != FrameType::Pair || f.payload.len() != PAIRING_TOKEN_BYTES {
        return Err(MeshError::BadPayload("expected PAIR"));
    }
    Ok(&f.payload)
}

/// Reads `PAIR_ERR`.
pub fn read_pair_err(f: &Frame) -> Result<PairError, MeshError> {
    let code = *f
        .payload
        .first()
        .filter(|_| f.kind == FrameType::PairErr)
        .ok_or(MeshError::BadPayload("expected PAIR_ERR"))?;
    Ok(PairError::from_code(code))
}

/// Reads `BIND`.
pub fn read_bind(f: &Frame) -> Result<&[u8], MeshError> {
    if f.kind != FrameType::Bind || f.payload.len() != 32 {
        return Err(MeshError::BadPayload("expected BIND"));
    }
    Ok(&f.payload)
}

/// Reads `BIND_OK`.
pub fn read_bind_ok(f: &Frame) -> Result<BindOk, MeshError> {
    if f.kind != FrameType::BindOk || f.payload.len() < 2 {
        return Err(MeshError::BadPayload("expected BIND_OK"));
    }
    Ok(BindOk {
        capabilities: f.payload[0],
        uplink_record_max: f.payload[1],
    })
}

/// Reads `BIND_ERR`.
pub fn read_bind_err(f: &Frame) -> Result<BindError, MeshError> {
    let code = *f
        .payload
        .first()
        .filter(|_| f.kind == FrameType::BindErr)
        .ok_or(MeshError::BadPayload("expected BIND_ERR"))?;
    Ok(BindError::from_code(code))
}

/// Reads `RECORD`: the sequence number and the opaque record.
pub fn read_record(f: &Frame) -> Result<(u8, &[u8]), MeshError> {
    if f.kind != FrameType::Record || f.payload.len() < 2 {
        return Err(MeshError::BadPayload("expected RECORD"));
    }
    Ok((f.payload[0], &f.payload[1..]))
}

/// Reads `RECV`.
pub fn read_recv(f: &Frame) -> Result<RecvRecord, MeshError> {
    if f.kind != FrameType::Recv {
        return Err(MeshError::BadPayload("expected RECV"));
    }
    Ok(RecvRecord(f.payload.clone()))
}

/// Reads `SEND_RESULT`.
pub fn read_send_result(f: &Frame) -> Result<SendResult, MeshError> {
    if f.kind != FrameType::SendResult || f.payload.len() < 2 {
        return Err(MeshError::BadPayload("expected SEND_RESULT"));
    }
    Ok(SendResult {
        seq: f.payload[0],
        outcome: SendOutcome::from_code(f.payload[1]),
    })
}

/// Reads `STATUS`.
pub fn read_status(f: &Frame) -> Result<Status, MeshError> {
    if f.kind != FrameType::Status || f.payload.len() < 5 {
        return Err(MeshError::BadPayload("expected STATUS"));
    }
    Ok(Status {
        radio_state: RadioState::from_code(f.payload[0]),
        rssi: f.payload[1] as i8,
        battery_percent: f.payload[2],
        tx_airtime_tenths_this_hour: u16::from(f.payload[3]) | (u16::from(f.payload[4]) << 8),
        ops_left_this_hour: f.payload.get(5).copied(),
    })
}

/// Reads `CLOSE`.
pub fn read_close(f: &Frame) -> Result<CloseReason, MeshError> {
    let code = *f
        .payload
        .first()
        .filter(|_| f.kind == FrameType::Close)
        .ok_or(MeshError::BadPayload("expected CLOSE"))?;
    Ok(CloseReason::from_code(code))
}

/// Compares a presented pairing token with the stored one in constant time.
#[must_use]
pub fn pairing_token_matches(presented: &[u8], stored: &[u8]) -> bool {
    if stored.len() != PAIRING_TOKEN_BYTES {
        return false;
    }
    let mut diff = presented.len() ^ stored.len();
    for (i, byte) in stored.iter().enumerate() {
        let a = presented.get(i).copied().unwrap_or(0);
        diff |= usize::from(a ^ byte);
    }
    diff == 0
}

/// Both sentinels, in one place, so a reader can test them without importing
/// constants: [`UNKNOWN_RSSI`] and [`UNKNOWN_BATTERY`].
#[must_use]
pub fn unknown() -> (i8, u8) {
    (UNKNOWN_RSSI, UNKNOWN_BATTERY)
}

/// The conservative record floor, re-exported for callers that need a default
/// before `BIND_OK` arrives.
#[must_use]
pub fn record_floor() -> usize {
    RECORD_FLOOR
}
