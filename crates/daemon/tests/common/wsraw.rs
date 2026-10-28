//! A hand-rolled WebSocket client for the tests that need to see the wire:
//! tungstenite hides the frame header and cannot do `permessage-deflate`, and
//! the compression contract is exactly "RSV1, an extension header, and a byte
//! count". It reads server frames (unmasked), inflates RSV1 frames with an
//! independent `flate2` inflater (context taken over), and counts every byte the
//! server wrote after the `101`.
#![allow(dead_code)]

use flate2::{Decompress, FlushDecompress};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Any byte stream the client can run over.
pub trait Duplex: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Duplex for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

pub struct RawWs {
    stream: Box<dyn Duplex>,
    buf: Vec<u8>,
    inflate: Option<Decompress>,
    /// The `Sec-WebSocket-Extensions` the server answered with, if any.
    pub extension: Option<String>,
    /// Bytes read from the server since the last [`RawWs::reset_counters`], frame
    /// headers included (the number a link is charged for, bar TCP/IP framing).
    pub wire_bytes: u64,
    /// WebSocket messages received since the last reset.
    pub messages: u64,
    /// Messages that arrived with RSV1 set.
    pub compressed_messages: u64,
}

impl RawWs {
    /// Connect and upgrade. `offer` is the `Sec-WebSocket-Extensions` value to send
    /// (`None` sends no header, like a client that does not do compression).
    pub async fn connect(port: u16, token: &str, offer: Option<&str>) -> RawWs {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.set_nodelay(true).unwrap();
        Self::upgrade(
            Box::new(stream),
            &format!("127.0.0.1:{port}"),
            Some(token),
            offer,
        )
        .await
    }

    /// Like [`RawWs::connect`] with a tiny receive buffer, so a client that stops
    /// reading backs the daemon's writes up after kilobytes, not megabytes.
    pub async fn connect_small_rcvbuf(port: u16, token: &str, offer: Option<&str>) -> RawWs {
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        let stream = sock
            .connect(format!("127.0.0.1:{port}").parse().unwrap())
            .await
            .unwrap();
        Self::upgrade(
            Box::new(stream),
            &format!("127.0.0.1:{port}"),
            Some(token),
            offer,
        )
        .await
    }

    /// Connect to the daemon's local attach socket: no token, the kernel vouches.
    pub async fn connect_unix(path: &std::path::Path) -> RawWs {
        let stream = tokio::net::UnixStream::connect(path).await.unwrap();
        Self::upgrade(Box::new(stream), "ldmux.local", None, None).await
    }

    async fn upgrade(
        mut stream: Box<dyn Duplex>,
        host: &str,
        token: Option<&str>,
        offer: Option<&str>,
    ) -> RawWs {
        let mut req = format!(
            "GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
        );
        if let Some(t) = token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if let Some(o) = offer {
            req.push_str(&format!("Sec-WebSocket-Extensions: {o}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        // The response head, byte by byte so nothing after it is consumed.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            stream.read_exact(&mut b).await.unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 101"), "upgrade refused: {head}");
        let extension = head
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Extensions: "))
            .map(|v| v.trim().to_string());
        let inflate = extension.as_ref().map(|_| Decompress::new(false));
        RawWs {
            stream,
            buf: Vec::new(),
            inflate,
            extension,
            wire_bytes: 0,
            messages: 0,
            compressed_messages: 0,
        }
    }

    pub fn reset_counters(&mut self) {
        self.wire_bytes = 0;
        self.messages = 0;
        self.compressed_messages = 0;
    }

    /// Send one masked, uncompressed text frame.
    pub async fn send_json(&mut self, v: &serde_json::Value) {
        let payload = v.to_string().into_bytes();
        let mask = [0xa5u8, 0x5a, 0x3c, 0xc3];
        let mut f = vec![0x81u8];
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
        self.stream.write_all(&f).await.unwrap();
    }

    /// The next message, or `None` at end of stream / after `wait`.
    pub async fn next_within(&mut self, wait: std::time::Duration) -> Option<Msg> {
        tokio::time::timeout(wait, self.next()).await.ok().flatten()
    }

    pub async fn next(&mut self) -> Option<Msg> {
        loop {
            if let Some(m) = self.parse() {
                return Some(m);
            }
            let mut tmp = [0u8; 16384];
            let n = self.stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            self.wire_bytes += n as u64;
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }

    fn parse(&mut self) -> Option<Msg> {
        let b = &self.buf;
        if b.len() < 2 {
            return None;
        }
        let rsv1 = b[0] & 0x40 != 0;
        let opcode = b[0] & 0x0f;
        assert!(b[0] & 0x80 != 0, "the server never fragments");
        assert_eq!(b[1] & 0x80, 0, "server frames are unmasked");
        let (len, hdr) = match b[1] & 0x7f {
            126 => {
                if b.len() < 4 {
                    return None;
                }
                (u16::from_be_bytes([b[2], b[3]]) as usize, 4)
            }
            127 => {
                if b.len() < 10 {
                    return None;
                }
                (
                    u64::from_be_bytes(b[2..10].try_into().unwrap()) as usize,
                    10,
                )
            }
            n => (n as usize, 2),
        };
        if b.len() < hdr + len {
            return None;
        }
        let payload = b[hdr..hdr + len].to_vec();
        self.buf.drain(..hdr + len);
        match opcode {
            0x8 => return Some(Msg::Close),
            0x9 | 0xa => return self.parse(),
            _ => {}
        }
        self.messages += 1;
        let data = if rsv1 {
            self.compressed_messages += 1;
            let d = self
                .inflate
                .as_mut()
                .expect("RSV1 without a negotiated extension");
            let mut input = payload;
            input.extend_from_slice(&[0, 0, 0xff, 0xff]);
            let mut out: Vec<u8> = Vec::with_capacity(input.len() * 8 + 1024);
            let start = d.total_in();
            loop {
                let used = (d.total_in() - start) as usize;
                if out.len() == out.capacity() {
                    out.reserve(out.capacity());
                }
                d.decompress_vec(&input[used..], &mut out, FlushDecompress::Sync)
                    .expect("valid deflate from the server");
                let used = (d.total_in() - start) as usize;
                if used >= input.len() && out.len() < out.capacity() {
                    break;
                }
            }
            out
        } else {
            payload
        };
        Some(if opcode == 0x1 {
            Msg::Text(String::from_utf8(data).expect("utf-8 text"))
        } else {
            Msg::Binary(data)
        })
    }

    /// Read until a text message of `ty` arrives; every binary OUTPUT payload seen
    /// on the way is appended to `out`.
    pub async fn until_type(&mut self, ty: &str, out: &mut Vec<u8>) -> serde_json::Value {
        for _ in 0..2000 {
            match self.next_within(std::time::Duration::from_secs(10)).await {
                Some(Msg::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == ty {
                        return v;
                    }
                }
                Some(Msg::Binary(b)) => decode_output(&b, out),
                Some(Msg::Close) | None => panic!("connection ended waiting for {ty}"),
            }
        }
        panic!("no {ty}");
    }

    /// Read OUTPUT until `needle` shows up, then keep reading until the link has
    /// been quiet for `quiet` (so a workload's trailing frames are counted).
    ///
    /// A needle that never arrives is a failure after a generous bounded wait.
    /// **The wait is sliced and the deadline is explicit**, not "one call that times
    /// out": a message arriving before the deadline used to restart the whole call,
    /// so a client that was being sent something every few hundred milliseconds
    /// waited forever (measured 2026-10-25: a test hung for the whole 40-minute
    /// workspace run). No keepalive is sent here either — a ping is answered with a
    /// pong, a pong is a message, and a ping loop is the same hang.
    pub async fn until_output(
        &mut self,
        out: &mut Vec<u8>,
        needle: &str,
        quiet: std::time::Duration,
    ) {
        let mut seen = needle.is_empty() || String::from_utf8_lossy(out).contains(needle);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let wait = if seen {
                quiet
            } else {
                std::time::Duration::from_millis(500)
            };
            let before = out.len();
            match self.next_within(wait).await {
                Some(Msg::Binary(b)) => decode_output(&b, out),
                Some(Msg::Text(_)) => {}
                Some(Msg::Close) => panic!("closed"),
                None => {
                    if !seen {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "never saw {needle:?}; tail {:?}",
                            String::from_utf8_lossy(&out[out.len().saturating_sub(300)..])
                        );
                        continue;
                    }
                    return;
                }
            }
            // Only the new bytes (and a needle's worth before them) are searched:
            // rescanning megabytes per message made this client slower than the
            // daemon it measures.
            if !seen {
                let from = before.saturating_sub(needle.len());
                seen = String::from_utf8_lossy(&out[from..]).contains(needle);
            }
        }
    }
}

/// An OUTPUT frame (8-byte header, type 0x10) into its payload.
pub fn decode_output(b: &[u8], out: &mut Vec<u8>) {
    if b.len() < 8 || b[0] != 0x10 {
        return;
    }
    let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
    out.extend_from_slice(&b[8..(8 + len).min(b.len())]);
}
