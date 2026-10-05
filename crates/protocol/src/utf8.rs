//! Chunked UTF-8 decoding.
//!
//! Bytes arrive in arbitrary chunks (a pty read, a keeper peek, a stdin read),
//! and a multi-byte character can straddle two of them. Decoding each chunk on
//! its own turns both halves into U+FFFD. `Utf8Carry` holds back the unfinished
//! tail of a chunk and prepends it to the next, so only bytes that are
//! genuinely invalid are replaced.

/// How many bytes at the end of `buf` are the *start* of a multi-byte UTF-8
/// character whose remaining bytes have not arrived yet (0 when the buffer ends
/// on a character boundary or in bytes that can never complete).
pub fn incomplete_tail(buf: &[u8]) -> usize {
    // A UTF-8 sequence is at most 4 bytes, so the lead byte is within the last 3.
    for back in 1..=buf.len().min(3) {
        let b = buf[buf.len() - back];
        if b & 0xC0 == 0x80 {
            continue; // a continuation byte: keep looking for its lead
        }
        let need = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0, // ASCII or an invalid lead: nothing to wait for
        };
        return if back < need { back } else { 0 };
    }
    0
}

/// The held-back tail between chunks.
#[derive(Debug, Default)]
pub struct Utf8Carry(Vec<u8>);

impl Utf8Carry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode `data` after whatever was carried; returns the complete text (empty
    /// when the whole chunk is an unfinished character).
    pub fn decode(&mut self, data: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.0);
        buf.extend_from_slice(data);
        let split = buf.len() - incomplete_tail(&buf);
        self.0 = buf[split..].to_vec();
        buf.truncate(split);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Forget the held-back bytes (a resync point).
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_detection() {
        assert_eq!(incomplete_tail(b"abc"), 0);
        assert_eq!(incomplete_tail("\u{e9}".as_bytes()), 0);
        assert_eq!(incomplete_tail(&"\u{e9}".as_bytes()[..1]), 1);
        assert_eq!(incomplete_tail(&"\u{1f600}".as_bytes()[..3]), 3);
        assert_eq!(incomplete_tail(&[0xff]), 0);
        assert_eq!(incomplete_tail(&[0x80, 0x80, 0x80, 0x80]), 0);
    }

    #[test]
    fn a_character_split_anywhere_survives() {
        let text = "a\u{e9}\u{20ac}\u{1f600}z";
        let bytes = text.as_bytes();
        for split in 1..bytes.len() {
            let mut c = Utf8Carry::new();
            let got = c.decode(&bytes[..split]) + &c.decode(&bytes[split..]);
            assert_eq!(got, text, "split at {split}");
        }
    }

    #[test]
    fn invalid_bytes_are_replaced_not_held_forever() {
        let mut c = Utf8Carry::new();
        assert_eq!(c.decode(&[b'a', 0xff, b'b']), "a\u{fffd}b");
        assert_eq!(c.decode(b"c"), "c");
    }
}
