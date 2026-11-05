//! Framing for the MeshCore companion protocol, as carried on a byte stream.
//!
//! Two directions, two start bytes: the host writes `0x3C` (`<`) and the radio
//! answers with `0x3E` (`>`), each followed by a little-endian `u16` payload
//! length that excludes the three header bytes. A payload is at most 176 bytes,
//! because the firmware's buffer is `MAX_FRAME_SIZE` and the radio reserves four
//! of those bytes for region transport codes.
//!
//! The decoder is the part that matters in practice. A serial port also carries
//! the firmware's console output: boot banners, `Saving prefs...`, debug lines.
//! A reader that assumes the first byte of a read is a start byte reads text as
//! a length, and one that trusts a corrupt length waits for bytes that never
//! come. So [`FrameReader`] scans for the start byte, counts what it skipped, and
//! treats an implausible length as corruption rather than as a promise.

/// Host to radio.
pub const SOF_HOST: u8 = 0x3C; // '<'
/// Radio to host.
pub const SOF_RADIO: u8 = 0x3E; // '>'

/// The firmware's frame ceiling (`MAX_FRAME_SIZE`).
pub const MAX_PAYLOAD: usize = 176;

/// A length above this is corruption, not a frame in progress. The ceiling is
/// generous relative to [`MAX_PAYLOAD`] because a future frame may grow; what
/// matters is that it is finite, so a bad length cannot stall the reader.
pub const MAX_SANE_LENGTH: usize = 512;

/// Wrap a payload for transmission to the radio.
///
/// Returns an error rather than truncating: a silently shortened frame is a
/// command the radio will reject or, worse, misread.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::TooLong(payload.len()));
    }
    let mut out = Vec::with_capacity(payload.len() + 3);
    out.push(SOF_HOST);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// A payload larger than the firmware's buffer.
    TooLong(usize),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLong(n) => write!(f, "payload of {n} bytes exceeds the 176-byte frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// An incremental, resynchronising frame decoder.
///
/// Feed it whatever the port returned; it yields whole payloads. It never
/// buffers more than one frame, so a radio that spams text cannot grow it.
#[derive(Debug, Default)]
pub struct FrameReader {
    state: State,
    length: usize,
    body: Vec<u8>,
    /// Bytes discarded while looking for a start byte. Exposed because a reader
    /// that is dropping data should be able to say so.
    pub skipped: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    LenLow,
    LenHigh,
    Body,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset to the idle state. Used when the link is torn down and rebuilt.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.length = 0;
        self.body.clear();
    }

    /// Consume bytes, appending every complete payload to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Vec<u8>>) {
        for &byte in bytes {
            match self.state {
                State::Idle => {
                    if byte == SOF_RADIO {
                        self.state = State::LenLow;
                    } else {
                        self.skipped += 1;
                    }
                }
                State::LenLow => {
                    self.length = byte as usize;
                    self.state = State::LenHigh;
                }
                State::LenHigh => {
                    self.length |= (byte as usize) << 8;
                    if self.length == 0 || self.length > MAX_SANE_LENGTH {
                        // Zero-length frames do not exist and an oversized length
                        // is corruption; both mean the start byte was noise.
                        self.state = State::Idle;
                        self.length = 0;
                    } else {
                        self.body.clear();
                        self.body.reserve(self.length);
                        self.state = State::Body;
                    }
                }
                State::Body => {
                    self.body.push(byte);
                    if self.body.len() == self.length {
                        out.push(std::mem::take(&mut self.body));
                        self.state = State::Idle;
                        self.length = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(reader: &mut FrameReader, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        reader.feed(bytes, &mut out);
        out
    }

    #[test]
    fn encode_puts_the_length_in_little_endian() {
        assert_eq!(encode(b"hi").unwrap(), vec![0x3C, 0x02, 0x00, b'h', b'i']);
    }

    #[test]
    fn encode_refuses_an_oversized_payload() {
        assert_eq!(encode(&[0u8; MAX_PAYLOAD + 1]), Err(FrameError::TooLong(MAX_PAYLOAD + 1)));
        assert!(encode(&[0u8; MAX_PAYLOAD]).is_ok());
    }

    #[test]
    fn reader_yields_one_frame() {
        let mut r = FrameReader::new();
        assert_eq!(frames(&mut r, b"\x3e\x03\x00abc"), vec![b"abc".to_vec()]);
    }

    #[test]
    fn reader_survives_byte_at_a_time() {
        let mut r = FrameReader::new();
        let mut all = Vec::new();
        for b in b"\x3e\x05\x00hello" {
            all.extend(frames(&mut r, &[*b]));
        }
        assert_eq!(all, vec![b"hello".to_vec()]);
    }

    #[test]
    fn reader_skips_console_text_before_a_frame() {
        let mut r = FrameReader::new();
        let got = frames(&mut r, b"Saving prefs...\r\n\x3e\x02\x00ok");
        assert_eq!(got, vec![b"ok".to_vec()]);
        assert!(r.skipped > 0, "the skipped text must be counted");
    }

    #[test]
    fn reader_resynchronises_after_an_implausible_length() {
        let mut r = FrameReader::new();
        let got = frames(&mut r, b"\x3e\xff\xffjunk\x3e\x02\x00ok");
        assert_eq!(got, vec![b"ok".to_vec()]);
    }

    #[test]
    fn reader_handles_two_frames_in_one_read() {
        let mut r = FrameReader::new();
        assert_eq!(
            frames(&mut r, b"\x3e\x01\x00a\x3e\x02\x00bc"),
            vec![b"a".to_vec(), b"bc".to_vec()]
        );
    }

    #[test]
    fn zero_length_resets_rather_than_completing() {
        let mut r = FrameReader::new();
        assert!(frames(&mut r, b"\x3e\x00\x00").is_empty());
        assert_eq!(frames(&mut r, b"\x3e\x01\x00z"), vec![b"z".to_vec()]);
    }

    #[test]
    fn a_partial_frame_is_held_not_emitted() {
        let mut r = FrameReader::new();
        assert!(frames(&mut r, b"\x3e\x04\x00ab").is_empty());
        assert_eq!(frames(&mut r, b"cd"), vec![b"abcd".to_vec()]);
    }
}
