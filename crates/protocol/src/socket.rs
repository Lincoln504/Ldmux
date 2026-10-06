//! Newline-delimited JSON client for the daemon's Unix socket.
//!
//! One call == one connection == one request/response; the first LF-terminated
//! line is the answer. (Do not multiplex requests on one connection without
//! adding id-correlation first.)
//!
//! Port of `packages/protocol/src/socket-client.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`). The
//! Node `net` module is replaced by a `tokio` Unix socket; the on-the-wire
//! contract (one JSON request line, one JSON response line) is the same.

use serde::{Serialize, de::DeserializeOwned};

/// A socket call error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError(pub String);

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for CallError {}

/// The largest response line a client will buffer. A full snapshot with its
/// history is a few MiB of JSON; 64 MiB is far above that and still bounds a
/// peer that never sends a newline.
pub const MAX_RESPONSE_LINE: usize = 64 * 1024 * 1024;

/// Read one LF-terminated line (without the LF) from `rd`, buffering at most
/// `max` bytes.
///
/// `Ok(None)` is end-of-stream before a newline arrived. A line longer than
/// `max` is an `InvalidData` error: the limit is what keeps a peer that never
/// stops sending (or never sends the newline) from growing this buffer without
/// bound. The one helper for the socket client, the keeper client and the
/// keeper's server, which had three copies of this loop.
pub async fn read_line<R: tokio::io::AsyncRead + Unpin>(
    rd: &mut R,
    max: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = rd.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        let chunk = &tmp[..n];
        if let Some(i) = chunk.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&chunk[..i]);
            if buf.len() > max {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line too large",
                ));
            }
            return Ok(Some(buf));
        }
        buf.extend_from_slice(chunk);
        if buf.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too large",
            ));
        }
    }
}

/// The default timeout, mirroring the TS `timeoutMs = 8000` default.
pub const DEFAULT_TIMEOUT_MS: u64 = 8000;

#[derive(serde::Deserialize)]
struct RpcResponse<T> {
    #[allow(dead_code)]
    id: Option<u64>,
    ok: bool,
    result: Option<T>,
    error: Option<String>,
}

/// A typed call against the daemon socket.
///
/// `params` is serialized as the request body; the response's `result` is
/// deserialized as `T`. On a `{"ok":false,"error":...}` the error string is
/// returned as a `CallError`.
pub async fn call<R: Serialize, T: DeserializeOwned>(
    socket_path: &str,
    method: &str,
    params: &R,
    timeout_ms: u64,
) -> Result<T, CallError> {
    use tokio::io::AsyncWriteExt;

    let result = async {
        let mut sock = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| CallError(format!("connect: {e}")))?;
        let req = serde_json::json!({
            "id": 1,
            "method": method.to_string(),
            "params": serde_json::to_value(params).unwrap(),
        });
        let line = format!("{}\n", serde_json::to_string(&req).unwrap());
        sock.write_all(line.as_bytes())
            .await
            .map_err(|e| CallError(format!("write: {e}")))?;
        let line = match read_line(&mut sock, MAX_RESPONSE_LINE).await {
            Ok(Some(l)) => String::from_utf8_lossy(&l).into_owned(),
            Ok(None) => return Err(CallError("ldmux socket closed".to_string())),
            Err(e) => return Err(CallError(format!("read: {e}"))),
        };
        let res: RpcResponse<T> =
            serde_json::from_str(&line).map_err(|e| CallError(format!("parse: {e}")))?;
        if res.ok {
            match res.result {
                Some(r) => Ok(r),
                None => Err(CallError("ok response with no result".to_string())),
            }
        } else {
            Err(CallError(
                res.error
                    .unwrap_or_else(|| "ldmux request failed".to_string()),
            ))
        }
    };

    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), result).await {
        Ok(r) => r,
        Err(_) => Err(CallError(format!("ldmux request timed out: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one connection: read one request line, answer per `served`.
    /// When `served.respond` is false the server accepts and stays silent
    /// (the timeout test).
    async fn serve(
        listener: tokio::net::UnixListener,
        served: std::sync::Arc<std::sync::Mutex<Option<Served>>>,
    ) {
        use tokio::io::AsyncBufReadExt;
        use tokio::io::AsyncWriteExt;
        let (sock, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = sock.into_split();
        let mut lines = tokio::io::BufReader::new(rd).lines();
        let _req: String = lines.next_line().await.unwrap().unwrap();
        let s = {
            let g = served.lock().unwrap();
            g.clone().unwrap()
        };
        if !s.respond {
            // Stay silent; the client will time out.
            let _ = tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            return;
        }
        let resp = if s.ok {
            serde_json::json!({ "id": 1, "ok": true, "result": s.result })
        } else {
            serde_json::json!({ "id": 1, "ok": false, "error": s.error })
        };
        wr.write_all(format!("{}\n", serde_json::to_string(&resp).unwrap()).as_bytes())
            .await
            .unwrap();
        let _ = wr.flush().await;
    }

    #[derive(Clone)]
    struct Served {
        respond: bool,
        ok: bool,
        result: serde_json::Value,
        error: String,
    }

    /// A socket path directly under the temp dir (whose parent always exists).
    fn tmp_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ldmux-sock-{}.sock", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn a_successful_call_returns_its_result() {
        let path = tmp_path();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let served = std::sync::Arc::new(std::sync::Mutex::new(Some(Served {
            respond: true,
            ok: true,
            result: serde_json::json!({ "pong": true, "version": 1, "keeperSocket": "ldmux" }),
            error: String::new(),
        })));
        let srv = tokio::spawn(async move { serve(listener, served).await });
        let params = serde_json::json!({});
        let res: serde_json::Value = call(&path.to_string_lossy(), "ping", &params, 2000)
            .await
            .unwrap();
        assert_eq!(res["pong"], true);
        assert_eq!(res["version"], 1);
        let _ = srv.await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn an_error_response_is_a_call_error() {
        let path = tmp_path();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let served = std::sync::Arc::new(std::sync::Mutex::new(Some(Served {
            respond: true,
            ok: false,
            result: serde_json::Value::Null,
            error: "boom".into(),
        })));
        let srv = tokio::spawn(async move { serve(listener, served).await });
        let params = serde_json::json!({ "id": "x" });
        let err = call::<serde_json::Value, serde_json::Value>(
            &path.to_string_lossy(),
            "kill",
            &params,
            2000,
        )
        .await
        .unwrap_err();
        assert_eq!(err, CallError("boom".to_string()));
        let _ = srv.await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_silent_server_times_out() {
        let path = tmp_path();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let served = std::sync::Arc::new(std::sync::Mutex::new(Some(Served {
            respond: false,
            ok: true,
            result: serde_json::Value::Null,
            error: String::new(),
        })));
        let srv = tokio::spawn(async move { serve(listener, served).await });
        let params = serde_json::json!({});
        let err = call::<serde_json::Value, serde_json::Value>(
            &path.to_string_lossy(),
            "ping",
            &params,
            200,
        )
        .await
        .unwrap_err();
        assert!(err.0.contains("timed out"), "got: {}", err.0);
        let _ = srv.await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn read_line_splits_caps_and_reports_eof() {
        let mut r: &[u8] = b"hello\nrest";
        assert_eq!(
            read_line(&mut r, 100).await.unwrap(),
            Some(b"hello".to_vec())
        );
        let mut r: &[u8] = b"no newline";
        assert_eq!(read_line(&mut r, 100).await.unwrap(), None);
        let big = vec![b'x'; 10_000];
        let mut r: &[u8] = &big;
        let e = read_line(&mut r, 1000).await.unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        let mut with_nl = big.clone();
        with_nl.push(b'\n');
        let mut r: &[u8] = &with_nl;
        assert!(
            read_line(&mut r, 1000).await.is_err(),
            "a long complete line is refused too"
        );
    }

    #[tokio::test]
    async fn a_runaway_response_is_refused() {
        let path = tmp_path();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let srv = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut sock, _) = listener.accept().await.unwrap();
            let chunk = vec![b'a'; 1024 * 1024];
            // Stop once the client hangs up; never send a newline.
            for _ in 0..(MAX_RESPONSE_LINE / chunk.len() + 8) {
                if sock.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        let err = call::<serde_json::Value, serde_json::Value>(
            &path.to_string_lossy(),
            "x",
            &serde_json::json!({}),
            20_000,
        )
        .await
        .unwrap_err();
        assert!(err.0.contains("too large"), "got: {}", err.0);
        let _ = srv.await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_refused_socket_is_an_error() {
        let dir = std::env::temp_dir().join(format!("ldmux-sock-{}", uuid::Uuid::new_v4()));
        let path = dir.join("absent.sock");
        let params = serde_json::json!({});
        let err = call::<serde_json::Value, serde_json::Value>(
            &path.to_string_lossy(),
            "ping",
            &params,
            500,
        )
        .await
        .unwrap_err();
        assert!(!err.0.is_empty(), "expected a connect error");
        let _ = std::fs::remove_dir(&dir);
    }
}
