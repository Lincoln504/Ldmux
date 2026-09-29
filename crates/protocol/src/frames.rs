//! ldmux wire framing.
//!
//! Terminal data is binary; control/handshake messages are JSON text. Every
//! frame shares one canonical 8-byte header — there is exactly ONE frame format
//! (dooboxes shipped three incompatible ones; we do not repeat that).
//!
//! ```text
//!   offset  size  field
//!   0       1     type   (frame type byte)
//!   1       1     flags  (must be 0: no flags are defined in v1)
//!   2       2     reserved (must be 0, big-endian)
//!   4       4     length (u32 big-endian, payload byte count)
//!   8       N     payload
//! ```
//!
//! Only `OUTPUT` is emitted by the shipped code; the remaining members are the
//! reserved vocabulary for later revisions (see docs/REMOTE.md).
//!
//! Port of `packages/protocol/src/frames.ts` (the source of record, unchanged
//! during the port's A/B window). The header layout is byte-exact: the TS
//! `DataView` writes big-endian, and this module reads and writes the same.

/// Frame type values. A const set rather than an `enum`: the wire field is a
/// bare u8, and unknown values (from a newer peer) must decode, not fail.
pub mod t {
    pub const HELLO: u8 = 0x01;
    pub const CAPS: u8 = 0x02;
    pub const OUTPUT: u8 = 0x10;
    pub const INPUT: u8 = 0x11;
    pub const RESIZE: u8 = 0x12;
    pub const SNAPSHOT: u8 = 0x13;
    pub const FOLLOW: u8 = 0x14;
    pub const SESSION_STATE: u8 = 0x15;
    pub const SCROLLBACK: u8 = 0x16;
    pub const PING: u8 = 0x30;
    pub const PONG: u8 = 0x31;
    pub const CLOSE: u8 = 0x40;
    pub const ERROR: u8 = 0xf0;
}

/// The canonical frame header: type(1) flags(1) reserved(2) length(4).
pub const HEADER_SIZE: usize = 8;

/// Absolute payload ceiling (DoS guard).
pub const MAX_FRAME_PAYLOAD: u32 = 16 * 1024 * 1024;

/// One decoded frame. `type_` and `flags` are carried through unvalidated,
/// exactly as the TS `Frame` carries them: the decoder trusts the framing,
/// not the semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub type_: u8,
    pub flags: u8,
    pub payload: Vec<u8>,
}

/// The result of decoding a buffer: every complete frame, plus the trailing
/// partial (the bytes that do not yet form a whole frame).
#[derive(Debug)]
pub struct DecodeResult {
    pub frames: Vec<Frame>,
    pub rest: Vec<u8>,
}

/// A frame error: the TS `RangeError`/throw sites, modelled as a `Result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError(pub String);

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FrameError {}

/// Encode one frame. `payload` may be empty; `flags` must be 0 in v1.
pub fn encode_frame(type_: u8, payload: &[u8], flags: u8) -> Result<Vec<u8>, FrameError> {
    if (payload.len() as u32) > MAX_FRAME_PAYLOAD {
        return Err(FrameError(format!("frame payload too large: {}", payload.len())));
    }
    let mut buf = vec![0u8; HEADER_SIZE + payload.len()];
    buf[0] = type_;
    buf[1] = flags;
    // reserved u16 BE is already 0 (the vec is zero-initialised).
    buf[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    buf[HEADER_SIZE..].copy_from_slice(payload);
    Ok(buf)
}

/// Encode a JSON control frame: the payload is the UTF-8 encoding of
/// `serde_json::to_vec` of `value`.
pub fn encode_json_frame<T: serde::Serialize>(type_: u8, value: &T, flags: u8) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(value).map_err(|e| FrameError(e.to_string()))?;
    encode_frame(type_, &json, flags)
}

/// Decode as many complete frames as `buf` contains; return the trailing
/// partial. A corrupt or hostile header (length > MAX) is an error, not a
/// trust: the decoder must not allocate it.
pub fn decode_frames(buf: &[u8]) -> Result<DecodeResult, FrameError> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while buf.len() - offset >= HEADER_SIZE {
        let header = &buf[offset..offset + HEADER_SIZE];
        let type_ = header[0];
        let flags = header[1];
        let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
        if length > MAX_FRAME_PAYLOAD {
            return Err(FrameError(format!("frame payload too large: {length}")));
        }
        let need = HEADER_SIZE + length as usize;
        if buf.len() - offset < need {
            break; // partial tail
        }
        let start = offset + HEADER_SIZE;
        frames.push(Frame {
            type_,
            flags,
            payload: buf[start..start + length as usize].to_vec(),
        });
        offset = start + length as usize;
    }
    Ok(DecodeResult {
        frames,
        rest: buf[offset..].to_vec(),
    })
}

/// Decode a JSON control frame's payload into `T`.
pub fn decode_json<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, FrameError> {
    let text = String::from_utf8(frame.payload.clone()).map_err(|e| FrameError(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| FrameError(e.to_string()))
}

/// A frame's type byte as a name (for the CLI and tests).
pub fn type_name(type_: u8) -> &'static str {
    match type_ {
        t::HELLO => "hello",
        t::CAPS => "caps",
        t::OUTPUT => "output",
        t::INPUT => "input",
        t::RESIZE => "resize",
        t::SNAPSHOT => "snapshot",
        t::FOLLOW => "follow",
        t::SESSION_STATE => "session_state",
        t::SCROLLBACK => "scrollback",
        t::PING => "ping",
        t::PONG => "pong",
        t::CLOSE => "close",
        t::ERROR => "error",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame codec's byte contract, pinned from both sides: this test file
    /// and the TS `frames.test.ts` assert the same cases against the same
    /// vectors. (The TS suite is the source of record; this is its Rust twin.)

    #[test]
    fn round_trips_a_binary_frame() {
        let payload = [1u8, 2, 3, 255, 0, 128];
        let encoded = encode_frame(t::OUTPUT, &payload, 0).unwrap();
        assert_eq!(encoded.len(), HEADER_SIZE + payload.len());
        let r = decode_frames(&encoded).unwrap();
        assert_eq!(r.frames.len(), 1);
        assert_eq!(r.frames[0].type_, t::OUTPUT);
        assert_eq!(r.frames[0].payload, payload.to_vec());
        assert!(r.rest.is_empty());
    }

    #[test]
    fn decodes_multiple_frames_and_keeps_a_partial_tail() {
        let a = encode_frame(t::INPUT, b"hello", 0).unwrap();
        let b = encode_frame(t::PING, &[], 0).unwrap();
        let mut blob = Vec::new();
        blob.extend_from_slice(&a);
        blob.extend_from_slice(&b);
        blob.extend_from_slice(&[0x99, 0x99]);
        let r = decode_frames(&blob).unwrap();
        let types: Vec<u8> = r.frames.iter().map(|f| f.type_).collect();
        assert_eq!(types, vec![t::INPUT, t::PING]);
        assert_eq!(r.rest, vec![0x99, 0x99]);
    }

    #[test]
    fn handles_an_empty_payload() {
        let enc = encode_frame(t::CLOSE, &[], 0).unwrap();
        let r = decode_frames(&enc).unwrap();
        assert_eq!(r.frames[0].payload.len(), 0);
    }

    #[test]
    fn json_frames_round_trip() {
        let value = serde_json::json!({ "type": "hello", "protocolVersion": 1 });
        let enc = encode_json_frame(t::HELLO, &value, 0).unwrap();
        let r = decode_frames(&enc).unwrap();
        let out: serde_json::Value = decode_json(&r.frames[0]).unwrap();
        assert_eq!(out, value);
    }

    #[test]
    fn rejects_oversized_payloads() {
        let big = vec![0u8; (MAX_FRAME_PAYLOAD as usize) + 1];
        assert!(encode_frame(t::OUTPUT, &big, 0).is_err());
    }

    #[test]
    fn decode_rejects_an_oversized_length_field_instead_of_allocating() {
        // A hostile/corrupt header must not make the decoder trust the length.
        let mut header = [0u8; HEADER_SIZE];
        header[4..8].copy_from_slice(&((MAX_FRAME_PAYLOAD + 1) as u32).to_be_bytes());
        assert!(decode_frames(&header).is_err());
    }

    #[test]
    fn decodes_from_a_subslice_with_a_non_zero_offset() {
        // The decoder reads from &buf[offset..]; a view into the middle of a
        // larger buffer must not read the wrong bytes.
        let inner = encode_json_frame(t::OUTPUT, &serde_json::json!({ "hello": "world" }), 0)
            .unwrap();
        let mut padded = vec![0u8; inner.len() + 8];
        padded[0..3].copy_from_slice(&[0xaa, 0xbb, 0xcc]);
        padded[5..5 + inner.len()].copy_from_slice(&inner);
        let view = &padded[5..5 + inner.len()];
        let r = decode_frames(view).unwrap();
        let out: serde_json::Value = decode_json(&r.frames[0]).unwrap();
        assert_eq!(out, serde_json::json!({ "hello": "world" }));
    }

    #[test]
    fn a_partial_header_is_kept_as_rest() {
        // Fewer than HEADER_SIZE bytes is a partial, not an error: the next
        // chunk completes it.
        let r = decode_frames(&[1u8, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(r.frames.is_empty());
        assert_eq!(r.rest.len(), 7);
    }

    #[test]
    fn empty_buffer_is_empty_result() {
        let r = decode_frames(&[]).unwrap();
        assert!(r.frames.is_empty());
        assert!(r.rest.is_empty());
    }

    /// Property: any payload of length <= MAX encodes and decodes back to
    /// itself, with the header byte-exact.
    #[test]
    fn property_round_trip_and_header_layout() {
        for len in [0usize, 1, 2, 3, 1023, 1024, 1025, 65535, 65536, 65537] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let enc = encode_frame(t::OUTPUT, &payload, 0).unwrap();
            assert_eq!(enc.len(), HEADER_SIZE + len);
            // header layout, byte-exact
            assert_eq!(enc[0], t::OUTPUT);
            assert_eq!(enc[1], 0);
            assert_eq!(&enc[2..4], &[0, 0]); // reserved
            assert_eq!(&enc[4..8], &(len as u32).to_be_bytes());
            let r = decode_frames(&enc).unwrap();
            assert_eq!(r.frames[0].payload, payload);
            assert!(r.rest.is_empty());
        }
    }
}
