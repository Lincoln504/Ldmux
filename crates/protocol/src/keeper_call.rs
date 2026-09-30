//! The daemon→keeper client: one tagged [`keeper::KeeperMessage`] per line,
//! one request line → one response line, one call == one connection.
//!
//! This is "the same client protocol as the WS connection, minus auth" (plan
//! §3.5): control messages are tagged JSON text (the WS decodes them with
//! `JSON.parse`, not the 8-byte frame codec — frames are reserved for the
//! binary OUTPUT stream), and the keeper connection simply drops the auth
//! handshake. Line discipline (one JSON per LF) is the framing, exactly as the
//! daemon's own control socket uses it ([`crate::socket`]).
//!
//! New (no TS source of record): the keeper does not exist in the TS tree.

use crate::keeper::KeeperMessage;

/// A keeper-call error (connect/read/parse/timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperCallError(pub String);

impl std::fmt::Display for KeeperCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for KeeperCallError {}

/// The default timeout, mirroring the daemon control socket's 8000 ms.
pub const DEFAULT_TIMEOUT_MS: u64 = 8000;

/// Send one [`KeeperMessage`] to the keeper and read its one response message.
///
/// One connection per call: connect, write the request line, read the response
/// line, close. (Do not multiplex on one connection without id-correlation.)
pub async fn keeper_call(
    socket: &str,
    req: &KeeperMessage,
    timeout_ms: u64,
) -> Result<KeeperMessage, KeeperCallError> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let result = async {
        let mut sock =
            tokio::net::UnixStream::connect(socket)
                .await
                .map_err(|e| KeeperCallError(format!("connect: {e}")))?;
        let line = format!("{}\n", serde_json::to_string(req).unwrap());
        sock.write_all(line.as_bytes())
            .await
            .map_err(|e| KeeperCallError(format!("write: {e}")))?;
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock
                .read(&mut tmp)
                .await
                .map_err(|e| KeeperCallError(format!("read: {e}")))?;
            if n == 0 {
                return Err(KeeperCallError("keeper closed the connection".into()));
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let nl = buf.iter().position(|&b| b == b'\n').unwrap();
        let line = String::from_utf8_lossy(&buf[..nl]).to_string();
        let res: KeeperMessage =
            serde_json::from_str(&line).map_err(|e| KeeperCallError(format!("parse: {e}")))?;
        // A keeper `Error` answer is a failure, not a result.
        match res {
            crate::keeper::KeeperMessage::Error(e) => Err(KeeperCallError(e.error)),
            other => Ok(other),
        }
    };

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), result).await {
        Ok(r) => r,
        Err(_) => Err(KeeperCallError("keeper call timed out".into())),
    }
}
