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

/// What went wrong in a keeper call.
///
/// The kind is not decoration: the daemon retries a `Connect` failure, because
/// the request never left this process and retrying cannot apply it twice, and
/// it must **not** retry a timeout or a read failure, because the keeper may
/// have run the request already (a duplicated `Write` types the bytes twice, a
/// duplicated `New` leaves two shells). A string that says "connect: " and a
/// caller that greps it would be the same rule with no type to hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperCallKind {
    /// The socket could not be connected (dead keeper, or a socket file with
    /// no one accepting on it yet).
    Connect,
    /// The request could not be written.
    Write,
    /// The response could not be read.
    Read,
    /// The response was not a [`crate::keeper::KeeperMessage`].
    Protocol,
    /// The keeper answered an `Error` message.
    Keeper,
    /// No answer inside the timeout. The keeper may have run the request.
    Timeout,
}

/// A keeper-call error: what kind, and the sentence explaining it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperCallError {
    pub kind: KeeperCallKind,
    pub message: String,
}

impl KeeperCallError {
    /// A new error of `kind`.
    pub fn new(kind: KeeperCallKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// True when the request never reached the keeper, so a retry cannot
    /// apply it twice.
    pub fn is_connect(&self) -> bool {
        self.kind == KeeperCallKind::Connect
    }
}

impl std::fmt::Display for KeeperCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
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
    use tokio::io::AsyncWriteExt;

    let result = async {
        let mut sock =
            tokio::net::UnixStream::connect(socket)
                .await
                .map_err(|e| KeeperCallError::new(KeeperCallKind::Connect, format!("connect: {e}")))?;
        let line = format!("{}\n", serde_json::to_string(req).unwrap());
        sock.write_all(line.as_bytes())
            .await
            .map_err(|e| KeeperCallError::new(KeeperCallKind::Write, format!("write: {e}")))?;
        let line = match crate::socket::read_line(&mut sock, crate::socket::MAX_RESPONSE_LINE).await {
            Ok(Some(l)) => String::from_utf8_lossy(&l).into_owned(),
            Ok(None) => return Err(KeeperCallError::new(KeeperCallKind::Read, "keeper closed the connection")),
            Err(e) => return Err(KeeperCallError::new(KeeperCallKind::Read, format!("read: {e}"))),
        };
        let res: KeeperMessage =
            serde_json::from_str(&line).map_err(|e| KeeperCallError::new(KeeperCallKind::Protocol, format!("parse: {e}")))?;
        // A keeper `Error` answer is a failure, not a result.
        match res {
            crate::keeper::KeeperMessage::Error(e) => Err(KeeperCallError::new(KeeperCallKind::Keeper, e.error)),
            other => Ok(other),
        }
    };

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), result).await {
        Ok(r) => r,
        Err(_) => Err(KeeperCallError::new(KeeperCallKind::Timeout, "keeper call timed out")),
    }
}
