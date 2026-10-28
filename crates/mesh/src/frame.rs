//! Framing and the incremental decoder.
//!
//! `MG | version | type | flags | length (LE, 2) | payload`.
//!
//! TCP delivers arbitrary chunk boundaries, so the decoder buffers and yields
//! whole frames. It never allocates from an unvalidated length field.

use std::fmt;

use crate::constants::{FrameType, HEADER_BYTES, MAGIC, MAX_PAYLOAD, VERSION};

/// Everything that can go wrong in this crate.
#[derive(Debug)]
pub enum MeshError {
    /// The peer is not speaking this protocol, or the stream desynchronised.
    BadMagic,
    /// A version this codec does not implement.
    BadVersion,
    /// A length field above [`MAX_PAYLOAD`].
    LengthTooLarge,
    /// Non-zero flags. Reserved in v1.
    BadFlags,
    /// A frame shorter than its declared fields.
    Truncated,
    /// A payload that cannot be what its type allows.
    BadPayload(&'static str),
    /// The peer closed the connection.
    Closed,
    /// A `HELLO`, `PAIR` or `BIND` was refused.
    Refused(&'static str, u8),
    /// The socket failed.
    Io(std::io::Error),
}

impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not this protocol (bad magic)"),
            Self::BadVersion => write!(f, "unsupported contract version"),
            Self::LengthTooLarge => write!(f, "payload length above the ceiling"),
            Self::BadFlags => write!(f, "reserved flags are set"),
            Self::Truncated => write!(f, "frame ended before its payload"),
            Self::BadPayload(what) => write!(f, "malformed payload: {what}"),
            Self::Closed => write!(f, "the gateway closed the connection"),
            Self::Refused(what, code) => write!(f, "{what} refused with code {code}"),
            Self::Io(e) => write!(f, "socket: {e}"),
        }
    }
}

impl std::error::Error for MeshError {}

impl From<std::io::Error> for MeshError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A framed L2 message: type and an opaque payload.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub kind: FrameType,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Construction is checked: flags are always 0 in v1 and the payload has a
    /// ceiling.
    pub fn new(kind: FrameType, payload: Vec<u8>) -> Result<Self, MeshError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(MeshError::LengthTooLarge);
        }
        Ok(Self { kind, payload })
    }

    /// The complete wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let length = self.payload.len();
        let mut out = Vec::with_capacity(HEADER_BYTES + length);
        out.push(MAGIC[0]);
        out.push(MAGIC[1]);
        out.push(VERSION);
        out.push(self.kind.code());
        out.push(0);
        // Little-endian, one byte-order convention for the whole ecosystem.
        out.push((length & 0xFF) as u8);
        out.push(((length >> 8) & 0xFF) as u8);
        out.extend_from_slice(&self.payload);
        out
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame({}, {} B)", self.kind.name(), self.payload.len())
    }
}

/// Incremental decoder over a byte stream.
///
/// A fatal framing error is sticky: after it, the stream cannot be trusted to
/// have a frame boundary again, so the caller must close the connection.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    failed: bool,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes and take the frames they complete.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>, MeshError> {
        if self.failed {
            return Ok(Vec::new());
        }
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < HEADER_BYTES {
                return Ok(out);
            }
            if self.buf[0..2] != MAGIC {
                return Err(self.fail(MeshError::BadMagic));
            }
            if self.buf[2] != VERSION {
                return Err(self.fail(MeshError::BadVersion));
            }
            if self.buf[4] != 0 {
                return Err(self.fail(MeshError::BadFlags));
            }
            let length = usize::from(self.buf[5]) | (usize::from(self.buf[6]) << 8);
            if length > MAX_PAYLOAD {
                return Err(self.fail(MeshError::LengthTooLarge));
            }
            let total = HEADER_BYTES + length;
            if self.buf.len() < total {
                return Ok(out);
            }
            let frame = Frame {
                kind: FrameType::from_code(self.buf[3]),
                payload: self.buf[HEADER_BYTES..total].to_vec(),
            };
            self.buf.drain(..total);
            out.push(frame);
        }
    }

    fn fail(&mut self, error: MeshError) -> MeshError {
        self.failed = true;
        self.buf.clear();
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_frame() -> Frame {
        Frame::new(FrameType::Hello, vec![1, 3]).expect("frame")
    }

    #[test]
    fn encodes_header_and_payload() {
        let bytes = hello_frame().encode();
        assert_eq!(
            bytes,
            vec![0x4D, 0x47, 0x01, 0x01, 0x00, 0x02, 0x00, 0x01, 0x03]
        );
    }

    #[test]
    fn decodes_a_frame_split_across_reads() {
        let mut d = Decoder::new();
        assert!(d.push(&[0x4D]).expect("ok").is_empty());
        assert!(d.push(&[0x47, 0x01, 0x01]).expect("ok").is_empty());
        let frames = d.push(&[0x00, 0x02, 0x00, 0x01, 0x03]).expect("ok");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], hello_frame());
    }

    #[test]
    fn decodes_back_to_back_frames_in_one_read() {
        let mut d = Decoder::new();
        let mut bytes = hello_frame().encode();
        bytes.extend_from_slice(&hello_frame().encode());
        let frames = d.push(&bytes).expect("ok");
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn refuses_bad_magic_and_stays_failed() {
        let mut d = Decoder::new();
        assert!(matches!(
            d.push(&[0, 1, 2, 3, 4, 5, 6]),
            Err(MeshError::BadMagic)
        ));
        assert!(d.push(&hello_frame().encode()).expect("silent").is_empty());
    }

    #[test]
    fn refuses_a_length_above_the_ceiling() {
        let mut d = Decoder::new();
        // 0x0201 = 513
        assert!(matches!(
            d.push(&[0x4D, 0x47, 0x01, 0x01, 0x00, 0x01, 0x02]),
            Err(MeshError::LengthTooLarge)
        ));
    }

    #[test]
    fn refuses_set_flags() {
        let mut d = Decoder::new();
        assert!(matches!(
            d.push(&[0x4D, 0x47, 0x01, 0x01, 0x01, 0x00, 0x00]),
            Err(MeshError::BadFlags)
        ));
    }

    #[test]
    fn an_unknown_type_is_carried_not_rejected() {
        let mut d = Decoder::new();
        let bytes = vec![0x4D, 0x47, 0x01, 0x7E, 0x00, 0x01, 0x00, 0xAA];
        let frames = d.push(&bytes).expect("ok");
        assert_eq!(frames[0].kind, FrameType::Unknown(0x7E));
    }
}
