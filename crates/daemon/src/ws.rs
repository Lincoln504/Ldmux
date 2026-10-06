//! The server side of the WebSocket protocol (RFC 6455) with `permessage-deflate`
//! (RFC 7692), over any byte stream.
//!
//! This exists because tungstenite cannot do either half of what compression
//! needs: it rejects a frame with RSV1 set (`NonZeroReservedBits`) and has no hook
//! to set RSV1 on an outgoing one, so negotiating the extension means owning the
//! frame layer. The layer is small (a server never fragments what it sends, and a
//! client message is bounded), it is one code path for the tailnet socket and the
//! local Unix socket, and `tungstenite` stays what it was before: the client the
//! desk attach window and the tests speak.
//!
//! ## Compression
//!
//! - Negotiated only when the client offers `permessage-deflate`; the answer is
//!   built by [`negotiate`] from the offer, never assumed.
//! - Per-connection context takeover is kept (that is where the ratio comes from:
//!   terminal output repeats itself across messages). `server_no_context_takeover`
//!   and `client_no_context_takeover` are honoured when offered.
//! - `server_max_window_bits` below 15 is not honoured (the pure-Rust backend's
//!   window is fixed at 32 KiB), so such an offer is declined, as the RFC requires
//!   of a server that cannot meet it; the next offer, or no compression, follows.
//!   `client_max_window_bits` needs nothing from us: a 32 KiB inflater reads any
//!   smaller window.
//! - One WebSocket message is one frame, compressed or not, so the Android client's
//!   one-frame-per-message assumption holds. A message under [`MIN_COMPRESS_BYTES`]
//!   is sent plain (RSV1 clear) without touching the compressor: the choice has to
//!   be made before compressing, because a message that was compressed and then
//!   discarded would leave the two ends' dictionaries out of step.
//! - A message over [`INLINE_COMPRESS_BYTES`] is compressed on the blocking pool,
//!   so a large snapshot cannot hold the runtime thread.
//!
//! The inflater is bounded to the connection's message cap, so a small compressed
//! message cannot expand into memory the cap was meant to protect.

use std::io;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Below this a message is sent plain: deflate cannot make a keystroke echo
/// smaller, only slower.
pub const MIN_COMPRESS_BYTES: usize = 96;
/// Above this the compression runs on the blocking pool.
pub const INLINE_COMPRESS_BYTES: usize = 16 * 1024;
/// The deflate level. Measured (ADR-0019): level 3 gets 7.1x on `ls -laR` against
/// 7.5x at level 6 and 5.6x at level 1, at 61 MB/s against 25 and 182.
pub const DEFLATE_LEVEL: u32 = 3;

/// The tail a sync flush leaves, which RFC 7692 removes from the wire.
const TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// What was agreed for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeflateParams {
    /// The server resets its compressor after every message.
    pub server_no_context_takeover: bool,
    /// The client resets its compressor after every message (so the server
    /// resets its inflater).
    pub client_no_context_takeover: bool,
}

/// Choose an offer from the `Sec-WebSocket-Extensions` header values and build the
/// response value. `None` means no compression (nothing offered, or nothing the
/// server can honour); the handshake then carries no extension header at all.
pub fn negotiate(header_values: &[&str]) -> Option<(DeflateParams, String)> {
    for value in header_values {
        for offer in value.split(',') {
            let mut parts = offer.split(';').map(str::trim);
            if parts.next() != Some("permessage-deflate") {
                continue;
            }
            let mut params = DeflateParams::default();
            let (mut seen_sct, mut seen_cct, mut seen_cmb, mut seen_smb) =
                (false, false, false, false);
            let mut ok = true;
            for p in parts.filter(|p| !p.is_empty()) {
                let (name, val) = match p.split_once('=') {
                    Some((n, v)) => (n.trim(), Some(v.trim().trim_matches('"'))),
                    None => (p, None),
                };
                match (name, val) {
                    ("server_no_context_takeover", None) if !seen_sct => {
                        seen_sct = true;
                        params.server_no_context_takeover = true;
                    }
                    ("client_no_context_takeover", None) if !seen_cct => {
                        seen_cct = true;
                        params.client_no_context_takeover = true;
                    }
                    // A hint (with or without a value): see the module doc.
                    ("client_max_window_bits", v) if !seen_cmb && v.is_none_or(valid_bits) => {
                        seen_cmb = true;
                    }
                    // Only the full window can be honoured.
                    ("server_max_window_bits", Some("15")) if !seen_smb => seen_smb = true,
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let mut resp = String::from("permessage-deflate");
            if params.server_no_context_takeover {
                resp.push_str("; server_no_context_takeover");
            }
            if params.client_no_context_takeover {
                resp.push_str("; client_no_context_takeover");
            }
            return Some((params, resp));
        }
    }
    None
}

fn valid_bits(v: &str) -> bool {
    v.parse::<u8>().is_ok_and(|b| (8..=15).contains(&b))
}

/// A WebSocket-level failure, with the close code the peer is owed.
#[derive(Debug)]
pub struct WsError {
    pub code: u16,
    pub reason: &'static str,
    /// The transport error, when that is what it was (no close frame can follow).
    pub io: Option<io::Error>,
}

impl WsError {
    fn proto(reason: &'static str) -> Self {
        Self {
            code: 1002,
            reason,
            io: None,
        }
    }
    fn too_big() -> Self {
        Self {
            code: 1009,
            reason: "message too big",
            io: None,
        }
    }
}

impl From<io::Error> for WsError {
    fn from(e: io::Error) -> Self {
        Self {
            code: 1006,
            reason: "io",
            io: Some(e),
        }
    }
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.io {
            Some(e) => write!(f, "{e}"),
            None => write!(f, "{} ({})", self.reason, self.code),
        }
    }
}

/// One message (or control frame) from the peer.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// The inbound limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The largest frame payload, as it arrives.
    pub max_frame: usize,
    /// The largest message, after reassembly and inflation.
    pub max_message: usize,
}

/// The reading half.
pub struct WsReader<R> {
    r: R,
    buf: Vec<u8>,
    limits: Limits,
    /// An unfinished fragmented message: `(is_text, compressed, bytes)`.
    partial: Option<(bool, bool, Vec<u8>)>,
    inflate: Option<Inflater>,
}

struct Inflater {
    d: Decompress,
    reset_each: bool,
}

impl<R: AsyncRead + Unpin> WsReader<R> {
    /// `leftover` is whatever the handshake read past the request head.
    pub fn new(r: R, leftover: Vec<u8>, limits: Limits, deflate: Option<DeflateParams>) -> Self {
        Self {
            r,
            buf: leftover,
            limits,
            partial: None,
            inflate: deflate.map(|p| Inflater {
                d: Decompress::new(false),
                reset_each: p.client_no_context_takeover,
            }),
        }
    }

    /// The next message. `Ok(None)` is a clean end of stream. Cancel-safe: the only
    /// await is a read into `self.buf`, and every parse step is synchronous.
    pub async fn next(&mut self) -> Result<Option<Incoming>, WsError> {
        loop {
            if let Some(m) = self.parse_one()? {
                return Ok(Some(m));
            }
            let n = self.r.read_buf(&mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
        }
    }

    /// Parse one complete frame out of the buffer, if one is there. Returns
    /// `Ok(None)` when more bytes are needed *or* the frame was a fragment that
    /// completed nothing yet (the caller reads more either way).
    fn parse_one(&mut self) -> Result<Option<Incoming>, WsError> {
        loop {
            let b = &self.buf;
            if b.len() < 2 {
                return Ok(None);
            }
            let fin = b[0] & 0x80 != 0;
            let rsv1 = b[0] & 0x40 != 0;
            if b[0] & 0x30 != 0 {
                return Err(WsError::proto("reserved bits set"));
            }
            let opcode = b[0] & 0x0f;
            if b[1] & 0x80 == 0 {
                return Err(WsError::proto("client frame not masked"));
            }
            let (len, hdr) = match b[1] & 0x7f {
                126 => {
                    if b.len() < 4 {
                        return Ok(None);
                    }
                    (u16::from_be_bytes([b[2], b[3]]) as u64, 4usize)
                }
                127 => {
                    if b.len() < 10 {
                        return Ok(None);
                    }
                    (u64::from_be_bytes(b[2..10].try_into().unwrap()), 10usize)
                }
                n => (n as u64, 2usize),
            };
            // Judged on the header, before the payload is buffered.
            if len > self.limits.max_frame as u64 {
                return Err(WsError::too_big());
            }
            let len = len as usize;
            if b.len() < hdr + 4 + len {
                return Ok(None);
            }
            let mask: [u8; 4] = b[hdr..hdr + 4].try_into().unwrap();
            let mut payload = b[hdr + 4..hdr + 4 + len].to_vec();
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i & 3];
            }
            self.buf.drain(..hdr + 4 + len);

            match opcode {
                // Control frames: whole, short, never compressed.
                0x8..=0xa => {
                    if !fin || len > 125 || rsv1 {
                        return Err(WsError::proto("bad control frame"));
                    }
                    return Ok(Some(match opcode {
                        0x8 => Incoming::Close,
                        0x9 => Incoming::Ping(payload),
                        _ => Incoming::Pong,
                    }));
                }
                0x1 | 0x2 => {
                    if self.partial.is_some() {
                        return Err(WsError::proto("new message inside a fragmented one"));
                    }
                    if rsv1 && self.inflate.is_none() {
                        return Err(WsError::proto("compressed frame without negotiation"));
                    }
                    if fin {
                        return self.finish(opcode == 0x1, rsv1, payload).map(Some);
                    }
                    self.partial = Some((opcode == 0x1, rsv1, payload));
                }
                0x0 => {
                    if rsv1 {
                        return Err(WsError::proto("rsv1 on a continuation"));
                    }
                    let Some((text, comp, mut acc)) = self.partial.take() else {
                        return Err(WsError::proto("continuation with nothing to continue"));
                    };
                    if acc.len() + payload.len() > self.limits.max_message {
                        return Err(WsError::too_big());
                    }
                    acc.extend_from_slice(&payload);
                    if fin {
                        return self.finish(text, comp, acc).map(Some);
                    }
                    self.partial = Some((text, comp, acc));
                }
                _ => return Err(WsError::proto("unknown opcode")),
            }
        }
    }

    fn finish(&mut self, text: bool, compressed: bool, data: Vec<u8>) -> Result<Incoming, WsError> {
        let data = if compressed {
            let inf = self.inflate.as_mut().expect("checked at the frame");
            let out = inflate_message(&mut inf.d, &data, self.limits.max_message)?;
            if inf.reset_each {
                inf.d.reset(false);
            }
            out
        } else {
            if data.len() > self.limits.max_message {
                return Err(WsError::too_big());
            }
            data
        };
        if text {
            String::from_utf8(data)
                .map(Incoming::Text)
                .map_err(|_| WsError {
                    code: 1007,
                    reason: "invalid utf-8",
                    io: None,
                })
        } else {
            Ok(Incoming::Binary(data))
        }
    }
}

/// Inflate one message (its tail restored), refusing to produce more than `max`.
pub fn inflate_message(d: &mut Decompress, data: &[u8], max: usize) -> Result<Vec<u8>, WsError> {
    let mut input = Vec::with_capacity(data.len() + 4);
    input.extend_from_slice(data);
    input.extend_from_slice(&TAIL);
    let mut out: Vec<u8> = Vec::with_capacity((data.len() * 4).clamp(256, max.max(256)));
    let start_in = d.total_in();
    loop {
        let consumed = (d.total_in() - start_in) as usize;
        if out.len() == out.capacity() {
            if out.len() >= max {
                return Err(WsError::too_big());
            }
            out.reserve(out.capacity().max(1024).min(max - out.len()));
        }
        let status = d
            .decompress_vec(&input[consumed..], &mut out, FlushDecompress::Sync)
            .map_err(|_| WsError {
                code: 1007,
                reason: "bad deflate data",
                io: None,
            })?;
        let consumed = (d.total_in() - start_in) as usize;
        if out.len() > max {
            return Err(WsError::too_big());
        }
        if matches!(status, Status::StreamEnd)
            || (consumed >= input.len() && out.len() < out.capacity())
        {
            return Ok(out);
        }
        if matches!(status, Status::BufError) && out.len() < out.capacity() {
            // No progress possible with space available: truncated input.
            return Err(WsError {
                code: 1007,
                reason: "truncated deflate data",
                io: None,
            });
        }
    }
}

/// The compressor state for one connection.
pub struct Deflater {
    c: Compress,
    reset_each: bool,
}

impl Deflater {
    pub fn new(p: DeflateParams) -> Self {
        Self::with_level(p, DEFLATE_LEVEL)
    }

    pub fn with_level(p: DeflateParams, level: u32) -> Self {
        Self {
            c: Compress::new(Compression::new(level), false),
            reset_each: p.server_no_context_takeover,
        }
    }

    /// Compress one message into its wire form (sync-flushed, tail removed).
    pub fn compress(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::with_capacity(data.len() / 2 + 64);
        let start_in = self.c.total_in();
        loop {
            let consumed = (self.c.total_in() - start_in) as usize;
            if out.len() == out.capacity() {
                out.reserve(out.capacity().max(256));
            }
            self.c
                .compress_vec(&data[consumed..], &mut out, FlushCompress::Sync)
                .map_err(|e| io::Error::other(format!("deflate: {e}")))?;
            let consumed = (self.c.total_in() - start_in) as usize;
            // Everything consumed and room left over: the flush is complete.
            if consumed >= data.len() && out.len() < out.capacity() {
                break;
            }
        }
        if out.ends_with(&TAIL) {
            out.truncate(out.len() - TAIL.len());
        }
        if self.reset_each {
            self.c.reset();
        }
        Ok(out)
    }
}

/// The writing half. A server never fragments, never masks.
pub struct WsWriter<W> {
    w: W,
    deflate: Option<Deflater>,
    /// Bytes put on the wire, frame headers included (the measurement hook).
    wire_bytes: u64,
}

impl<W: AsyncWrite + Unpin> WsWriter<W> {
    pub fn new(w: W, deflate: Option<DeflateParams>) -> Self {
        Self {
            w,
            deflate: deflate.map(Deflater::new),
            wire_bytes: 0,
        }
    }

    pub fn wire_bytes(&self) -> u64 {
        self.wire_bytes
    }

    pub async fn send_text(&mut self, s: &str) -> io::Result<()> {
        self.send_data(0x1, s.as_bytes()).await
    }

    pub async fn send_binary(&mut self, b: &[u8]) -> io::Result<()> {
        self.send_data(0x2, b).await
    }

    pub async fn send_pong(&mut self, payload: &[u8]) -> io::Result<()> {
        self.write_frame(0xa, false, &payload[..payload.len().min(125)])
            .await
    }

    pub async fn send_close(&mut self, code: u16) -> io::Result<()> {
        self.write_frame(0x8, false, &code.to_be_bytes()).await?;
        self.w.flush().await
    }

    async fn send_data(&mut self, opcode: u8, data: &[u8]) -> io::Result<()> {
        if data.len() >= MIN_COMPRESS_BYTES && self.deflate.is_some() {
            let compressed = if data.len() > INLINE_COMPRESS_BYTES {
                // The compressor goes to the blocking pool and comes back with the
                // result; the connection is not serving anything else meanwhile
                // (it is one task), so nothing can reach it in between.
                let mut d = self.deflate.take().expect("checked");
                let owned = data.to_vec();
                let (d, out) = tokio::task::spawn_blocking(move || {
                    let out = d.compress(&owned);
                    (d, out)
                })
                .await
                .map_err(|e| io::Error::other(format!("deflate task: {e}")))?;
                self.deflate = Some(d);
                out?
            } else {
                self.deflate.as_mut().expect("checked").compress(data)?
            };
            return self.write_frame(opcode, true, &compressed).await;
        }
        self.write_frame(opcode, false, data).await
    }

    async fn write_frame(&mut self, opcode: u8, rsv1: bool, payload: &[u8]) -> io::Result<()> {
        let mut head = Vec::with_capacity(10);
        head.push(0x80 | if rsv1 { 0x40 } else { 0 } | opcode);
        match payload.len() {
            n if n < 126 => head.push(n as u8),
            n if n <= u16::MAX as usize => {
                head.push(126);
                head.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                head.push(127);
                head.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        self.wire_bytes += (head.len() + payload.len()) as u64;
        // Header and payload in one write where they are small, so a keystroke is
        // one segment, not two.
        if payload.len() <= 4096 {
            head.extend_from_slice(payload);
            self.w.write_all(&head).await?;
        } else {
            self.w.write_all(&head).await?;
            self.w.write_all(payload).await?;
        }
        self.w.flush().await
    }
}

/// A handshake request, parsed.
pub struct Upgrade {
    /// `Sec-WebSocket-Key`.
    pub key: String,
    pub deflate: Option<(DeflateParams, String)>,
}

/// The `101` response for `up`.
pub fn upgrade_response(up: &Upgrade) -> String {
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(up.key.as_bytes());
    let mut r = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n"
    );
    if let Some((_, ext)) = &up.deflate {
        r.push_str("Sec-WebSocket-Extensions: ");
        r.push_str(ext);
        r.push_str("\r\n");
    }
    r.push_str("\r\n");
    r
}

/// Read the HTTP request head from `s`, consuming exactly it. Returns the head
/// text and whatever followed it in the same read (normally nothing: the client
/// waits for the `101`). `None` on EOF, a read error, an over-long head or the
/// deadline.
pub async fn read_head<S: AsyncRead + Unpin>(
    s: &mut S,
    deadline: std::time::Duration,
) -> Option<(String, Vec<u8>)> {
    const MAX_HEAD: usize = 16 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let work = async {
        loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let rest = buf.split_off(end + 4);
                return Some((String::from_utf8_lossy(&buf).into_owned(), rest));
            }
            if buf.len() >= MAX_HEAD {
                return None;
            }
            match s.read_buf(&mut buf).await {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    };
    tokio::time::timeout(deadline, work).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_frame(opcode: u8, rsv1: bool, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut f = vec![if fin { 0x80 } else { 0 } | if rsv1 { 0x40 } else { 0 } | opcode];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 65535 => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        f
    }

    const LIM: Limits = Limits {
        max_frame: 64 * 1024,
        max_message: 64 * 1024,
    };

    #[test]
    fn plain_offer_is_accepted_with_a_bare_extension() {
        let (p, resp) = negotiate(&["permessage-deflate"]).unwrap();
        assert_eq!(p, DeflateParams::default());
        assert_eq!(resp, "permessage-deflate");
    }

    #[test]
    fn browser_and_apple_offers_are_accepted() {
        let (_, resp) = negotiate(&["permessage-deflate; client_max_window_bits"]).unwrap();
        assert_eq!(resp, "permessage-deflate");
        let (_, resp) = negotiate(&["permessage-deflate; client_max_window_bits=12"]).unwrap();
        assert_eq!(resp, "permessage-deflate");
    }

    #[test]
    fn no_context_takeover_is_echoed() {
        let (p, resp) = negotiate(&[
            "permessage-deflate; server_no_context_takeover; client_no_context_takeover",
        ])
        .unwrap();
        assert!(p.server_no_context_takeover && p.client_no_context_takeover);
        assert_eq!(
            resp,
            "permessage-deflate; server_no_context_takeover; client_no_context_takeover"
        );
    }

    #[test]
    fn a_window_the_backend_cannot_honour_is_declined_and_the_next_offer_taken() {
        assert!(negotiate(&["permessage-deflate; server_max_window_bits=10"]).is_none());
        let (_, resp) =
            negotiate(&["permessage-deflate; server_max_window_bits=9, permessage-deflate"])
                .unwrap();
        assert_eq!(resp, "permessage-deflate");
        let (_, resp) = negotiate(&["permessage-deflate; server_max_window_bits=15"]).unwrap();
        assert_eq!(resp, "permessage-deflate");
    }

    #[test]
    fn junk_and_other_extensions_decline() {
        assert!(negotiate(&[]).is_none());
        assert!(negotiate(&["x-webkit-deflate-frame"]).is_none());
        assert!(negotiate(&["permessage-deflate; bogus"]).is_none());
        assert!(negotiate(&["permessage-deflate; client_max_window_bits=99"]).is_none());
        assert!(
            negotiate(&[
                "permessage-deflate; client_no_context_takeover; client_no_context_takeover"
            ])
            .is_none()
        );
    }

    #[tokio::test]
    async fn a_masked_text_frame_is_read() {
        let bytes = client_frame(0x1, false, true, br#"{"type":"ping"}"#);
        let mut r = WsReader::new(&bytes[..], Vec::new(), LIM, None);
        assert_eq!(
            r.next().await.unwrap(),
            Some(Incoming::Text(r#"{"type":"ping"}"#.into()))
        );
        assert_eq!(r.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn fragments_are_reassembled_and_pings_pass_between_them() {
        let mut bytes = client_frame(0x1, false, false, b"hel");
        bytes.extend(client_frame(0x9, false, true, b"p"));
        bytes.extend(client_frame(0x0, false, true, b"lo"));
        let mut r = WsReader::new(&bytes[..], Vec::new(), LIM, None);
        assert_eq!(r.next().await.unwrap(), Some(Incoming::Ping(b"p".to_vec())));
        assert_eq!(
            r.next().await.unwrap(),
            Some(Incoming::Text("hello".into()))
        );
    }

    #[tokio::test]
    async fn protocol_violations_are_errors() {
        // Unmasked.
        let mut r = WsReader::new(&[0x81u8, 0x01, b'x'][..], Vec::new(), LIM, None);
        assert_eq!(r.next().await.unwrap_err().code, 1002);
        // RSV1 without negotiation.
        let b = client_frame(0x1, true, true, b"x");
        let mut r = WsReader::new(&b[..], Vec::new(), LIM, None);
        assert_eq!(r.next().await.unwrap_err().code, 1002);
        // Over the frame cap, judged on the header alone.
        let big = vec![0u8; 70_000];
        let b = client_frame(0x2, false, true, &big);
        let mut r = WsReader::new(&b[..14.min(b.len())], Vec::new(), LIM, None);
        assert_eq!(r.next().await.unwrap_err().code, 1009);
        // Bad UTF-8.
        let b = client_frame(0x1, false, true, &[0xff, 0xfe]);
        let mut r = WsReader::new(&b[..], Vec::new(), LIM, None);
        assert_eq!(r.next().await.unwrap_err().code, 1007);
    }

    /// Client-side compressor, the mirror of what a real client does.
    fn client_compress(c: &mut Compress, data: &[u8]) -> Vec<u8> {
        let mut d = Deflater {
            c: Compress::new(Compression::new(6), false),
            reset_each: false,
        };
        std::mem::swap(&mut d.c, c);
        let out = d.compress(data).unwrap();
        std::mem::swap(&mut d.c, c);
        out
    }

    #[tokio::test]
    async fn server_output_round_trips_with_context_takeover_and_shrinks() {
        let params = DeflateParams::default();
        let mut sink: Vec<u8> = Vec::new();
        let mut w = WsWriter::new(&mut sink, Some(params));
        let line = "drwxr-xr-x  2 user user 4096 Oct  5 12:00 some-directory-name\r\n".repeat(20);
        for _ in 0..5 {
            w.send_binary(line.as_bytes()).await.unwrap();
        }
        let small = b"x";
        w.send_binary(small).await.unwrap();
        let wire = w.wire_bytes() as usize;
        assert!(
            wire < line.len() * 5 / 10,
            "5 repeats of 1.3 KB must cost well under half: {wire}"
        );
        drop(w);

        // Decode with an independent inflater, tail restored, state carried.
        let mut d = Decompress::new(false);
        let mut at = 0usize;
        let mut plain_seen = false;
        for i in 0..6 {
            let b0 = sink[at];
            let rsv1 = b0 & 0x40 != 0;
            let (len, hdr) = match sink[at + 1] & 0x7f {
                126 => (u16::from_be_bytes([sink[at + 2], sink[at + 3]]) as usize, 4),
                n => (n as usize, 2),
            };
            let payload = &sink[at + hdr..at + hdr + len];
            at += hdr + len;
            if i < 5 {
                assert!(rsv1);
                let out = inflate_message(&mut d, payload, 1 << 20).unwrap();
                assert_eq!(out, line.as_bytes());
            } else {
                assert!(!rsv1, "a tiny message is sent plain");
                assert_eq!(payload, small);
                plain_seen = true;
            }
        }
        assert!(plain_seen && at == sink.len());
    }

    /// The threshold is where a cold stream (the first message after a reconnect)
    /// breaks even on terminal text: measured ~90 B, pinned at 96 (ADR-0019).
    #[tokio::test]
    async fn the_threshold_is_96_bytes() {
        assert_eq!(MIN_COMPRESS_BYTES, 96);
        let mut sink: Vec<u8> = Vec::new();
        let mut w = WsWriter::new(&mut sink, Some(DeflateParams::default()));
        w.send_binary(&[b'a'; 95]).await.unwrap();
        w.send_binary(&[b'a'; 96]).await.unwrap();
        drop(w);
        assert_eq!(sink[0] & 0x40, 0, "95 B is plain");
        let second = 2 + 95;
        assert_ne!(sink[second] & 0x40, 0, "96 B is compressed");
    }

    #[tokio::test]
    async fn no_context_takeover_messages_decode_independently() {
        let params = DeflateParams {
            server_no_context_takeover: true,
            client_no_context_takeover: false,
        };
        let mut sink: Vec<u8> = Vec::new();
        let mut w = WsWriter::new(&mut sink, Some(params));
        let msg = "hello hello hello hello hello hello hello hello hello hello hello hello hello hello hello hello hello hello";
        w.send_text(msg).await.unwrap();
        w.send_text(msg).await.unwrap();
        drop(w);
        // Each decodes with a fresh inflater.
        let mut at = 0;
        for _ in 0..2 {
            let len = (sink[at + 1] & 0x7f) as usize;
            let payload = &sink[at + 2..at + 2 + len];
            at += 2 + len;
            let mut d = Decompress::new(false);
            assert_eq!(
                inflate_message(&mut d, payload, 4096).unwrap(),
                msg.as_bytes()
            );
        }
    }

    #[tokio::test]
    async fn a_large_message_compresses_on_the_blocking_pool_and_keeps_state() {
        let params = DeflateParams::default();
        let mut sink: Vec<u8> = Vec::new();
        let mut w = WsWriter::new(&mut sink, Some(params));
        let big = "0123456789 abcdefghij\r\n".repeat(4000); // ~92 KB
        assert!(big.len() > INLINE_COMPRESS_BYTES);
        w.send_text(&big).await.unwrap();
        w.send_text(&format!(
            "small but over the threshold: {}",
            ".".repeat(100)
        ))
        .await
        .unwrap();
        drop(w);
        let mut d = Decompress::new(false);
        assert_eq!(sink[1] & 0x7f, 126, "92 KB of repeats deflates under 64 KB");
        let len = u16::from_be_bytes([sink[2], sink[3]]) as usize;
        let out = inflate_message(&mut d, &sink[4..4 + len], 1 << 20).unwrap();
        assert_eq!(out, big.as_bytes());
        let at = 4 + len;
        let l2 = (sink[at + 1] & 0x7f) as usize;
        let out2 = inflate_message(&mut d, &sink[at + 2..at + 2 + l2], 1 << 20).unwrap();
        assert!(out2.starts_with(b"small but"));
    }

    #[tokio::test]
    async fn a_compressed_client_message_is_inflated_and_bounded() {
        let mut c = Compress::new(Compression::new(6), false);
        let msg = br#"{"type":"input","data":"echo hello world hello world hello world"}"#;
        let comp = client_compress(&mut c, msg);
        let mut bytes = client_frame(0x1, true, true, &comp);
        // A second message reusing the dictionary (context takeover).
        let comp2 = client_compress(&mut c, msg);
        assert!(comp2.len() < comp.len());
        bytes.extend(client_frame(0x1, true, true, &comp2));
        let mut r = WsReader::new(&bytes[..], Vec::new(), LIM, Some(DeflateParams::default()));
        for _ in 0..2 {
            match r.next().await.unwrap() {
                Some(Incoming::Text(t)) => assert_eq!(t.as_bytes(), msg),
                other => panic!("{other:?}"),
            }
        }
        // A bomb: 1 MiB of zeros deflates to ~1 KB, and must be refused at the cap.
        let mut c = Compress::new(Compression::new(9), false);
        let bomb = client_compress(&mut c, &vec![0u8; 1 << 20]);
        assert!(bomb.len() < 8 * 1024);
        let b = client_frame(0x2, true, true, &bomb);
        let mut r = WsReader::new(&b[..], Vec::new(), LIM, Some(DeflateParams::default()));
        assert_eq!(r.next().await.unwrap_err().code, 1009);
    }

    #[tokio::test]
    async fn the_handshake_response_carries_the_accept_key_and_the_extension() {
        let up = Upgrade {
            key: "dGhlIHNhbXBsZSBub25jZQ==".into(),
            deflate: negotiate(&["permessage-deflate; client_max_window_bits"]),
        };
        let r = upgrade_response(&up);
        assert!(
            r.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "RFC 6455 §1.3 example"
        );
        assert!(r.contains("Sec-WebSocket-Extensions: permessage-deflate\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
        let up = Upgrade {
            key: "dGhlIHNhbXBsZSBub25jZQ==".into(),
            deflate: None,
        };
        assert!(!upgrade_response(&up).contains("Extensions"));
    }

    #[tokio::test]
    async fn read_head_consumes_exactly_the_head() {
        let data = b"GET /ws HTTP/1.1\r\nHost: x\r\n\r\n\x81\x80abcd";
        let mut s = &data[..];
        let (head, rest) = read_head(&mut s, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert!(head.starts_with("GET /ws"));
        assert_eq!(rest, b"\x81\x80abcd");
    }

    /// The CPU cost of compression, for ADR-0019. `cargo test -p ldmux-daemon --lib
    /// --release compression_cost -- --ignored --nocapture`. Not a gate (the number
    /// is the machine's); the assertion is only that it ran and round-tripped.
    #[test]
    #[ignore]
    fn compression_cost_report() {
        let ls = std::process::Command::new("sh")
            .args(["-c", "ls -laR /usr/lib 2>/dev/null | head -n 8000"])
            .output()
            .unwrap()
            .stdout;
        let ls: Vec<u8> = String::from_utf8_lossy(&ls)
            .replace('\n', "\r\n")
            .into_bytes();
        let yes: Vec<u8> = "y\r\n".repeat(16 * 1024 * 1024 / 3).into_bytes();
        for (name, data) in [("ls -laR", &ls), ("yes", &yes)] {
            for chunk in [4 * 1024usize, 64 * 1024] {
                for level in [1u32, 3, 6] {
                    let mut c = Deflater::with_level(DeflateParams::default(), level);
                    let mut d = Decompress::new(false);
                    let t = std::time::Instant::now();
                    let mut out_bytes = 0usize;
                    let mut worst = std::time::Duration::ZERO;
                    let mut wire = Vec::new();
                    for piece in data.chunks(chunk) {
                        let t1 = std::time::Instant::now();
                        let z = c.compress(piece).unwrap();
                        worst = worst.max(t1.elapsed());
                        out_bytes += z.len();
                        wire.push(z);
                    }
                    let dt = t.elapsed();
                    for (z, piece) in wire.iter().zip(data.chunks(chunk)) {
                        assert_eq!(inflate_message(&mut d, z, 1 << 22).unwrap(), piece);
                    }
                    println!(
                        "{name:8} chunk {:>3} KiB level {level}: {:>5.1}x  {:>6.0} MB/s  worst chunk {:?}",
                        chunk / 1024,
                        data.len() as f64 / out_bytes as f64,
                        data.len() as f64 / dt.as_secs_f64() / 1e6,
                        worst
                    );
                }
            }
        }
    }

    /// Where compression starts paying, for the threshold in ADR-0019: the mean
    /// wire size of consecutive `ls -la` fragments of each size, plain versus
    /// compressed, in a warm stream (context taken over) and a cold one (the first
    /// message after a reconnect).
    #[test]
    #[ignore]
    fn threshold_report() {
        let ls = std::process::Command::new("sh")
            .args([
                "-c",
                "ls -la --color=always /usr/lib /etc /usr/bin 2>/dev/null | head -n 3000",
            ])
            .output()
            .unwrap()
            .stdout;
        let ls: Vec<u8> = String::from_utf8_lossy(&ls)
            .replace('\n', "\r\n")
            .into_bytes();
        for size in [16usize, 32, 48, 64, 96, 128, 192, 256, 512, 1024] {
            let pieces: Vec<&[u8]> = ls.chunks(size).take(400).collect();
            let mut warm = Deflater::new(DeflateParams::default());
            let (mut plain, mut w) = (0usize, 0usize);
            for p in &pieces {
                plain += p.len() + 2;
                w += warm.compress(p).unwrap().len() + 2;
            }
            let mut cold = 0usize;
            for p in pieces.iter().take(50) {
                cold += Deflater::new(DeflateParams::default())
                    .compress(p)
                    .unwrap()
                    .len()
                    + 2;
            }
            let cold_plain: usize = pieces.iter().take(50).map(|p| p.len() + 2).sum();
            println!(
                "{size:5} B messages: warm {:.2} of plain, cold (first after reconnect) {:.2} of plain",
                w as f64 / plain as f64,
                cold as f64 / cold_plain as f64
            );
        }
    }
}
