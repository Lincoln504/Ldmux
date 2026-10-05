//! Newline-delimited JSON client for the daemon's Unix socket.
//!
//! One call == one connection == one request/response; the first LF-terminated
//! line is the answer. (Do not multiplex requests on one connection without
//! adding id-correlation first.)
//!
//! Port of `packages/protocol/src/socket-client.ts` (the source of record). The
//! Node `net` module is replaced by a `tokio` Unix socket; the on-the-wire
//! contract (one JSON request line, one JSON response line) is the same.

use serde::{de::DeserializeOwned, Serialize};

/// A socket call error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError(pub String);

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for CallError {}

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
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let result = async {
        let mut sock =
            tokio::net::UnixStream::connect(socket_path)
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
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock
                .read(&mut tmp)
                .await
                .map_err(|e| CallError(format!("read: {e}")))?;
            if n == 0 {
                return Err(CallError("ldmux socket closed".to_string()));
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let nl = buf.iter().position(|&b| b == b'\n').unwrap();
        let line = String::from_utf8_lossy(&buf[..nl]).to_string();
        let res: RpcResponse<T> =
            serde_json::from_str(&line).map_err(|e| CallError(format!("parse: {e}")))?;
        if res.ok {
            match res.result {
                Some(r) => Ok(r),
                None => Err(CallError("ok response with no result".to_string())),
            }
        } else {
            Err(CallError(res.error.unwrap_or_else(|| "ldmux request failed".to_string())))
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
        let res: serde_json::Value =
            call(&path.to_string_lossy(), "ping", &params, 2000)
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
        let err = call::<serde_json::Value, serde_json::Value>(&path.to_string_lossy(), "kill", &params, 2000)
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
        let err = call::<serde_json::Value, serde_json::Value>(&path.to_string_lossy(), "ping", &params, 200)
            .await
            .unwrap_err();
        assert!(err.0.contains("timed out"), "got: {}", err.0);
        let _ = srv.await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_refused_socket_is_an_error() {
        let dir = std::env::temp_dir().join(format!("ldmux-sock-{}", uuid::Uuid::new_v4()));
        let path = dir.join("absent.sock");
        let params = serde_json::json!({});
        let err = call::<serde_json::Value, serde_json::Value>(&path.to_string_lossy(), "ping", &params, 500)
            .await
            .unwrap_err();
        assert!(!err.0.is_empty(), "expected a connect error");
        let _ = std::fs::remove_dir(&dir);
    }
}
