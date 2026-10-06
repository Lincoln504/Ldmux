//! The daemon's control socket (the `ldmuxd.sock` Unix socket).
//!
//! A newline-delimited JSON API: each line is an RPC request (an `id`, a
//! `method`, and `params`), and each response is a JSON line. The CLI, the
//! mobile/web clients, and the desktop attach client are all clients of this
//! same API (plan §3.5: the daemon↔keeper connection is the same client
//! protocol, minus auth).
//!
//! This is the §3.2 daemon-side piece: the socket is a full-control channel,
//! owner-only (0600). The umask closes the window between the bind (which
//! creates the file with the process umask, typically 0755) and the chmod, so
//! another local user cannot connect to a control channel in that instant.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::net::UnixStream;

/// The largest request line the control socket will buffer.
const MAX_REQUEST_LINE: usize = 4 * 1024 * 1024;
/// How long a connection may take to deliver its one request line.
const REQUEST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The daemon's control socket (the `ldmuxd.sock` Unix socket).
pub struct ControlSocketServer {
    socket_path: std::path::PathBuf,
}

/// A handler that dispatches an RPC request to a response.
///
/// The daemon's `handle` method is the production handler; the tests use a
/// simple stub. This indirection lets the serve loop call the daemon without
/// holding a `&mut` reference (each connection is a spawned task).
///
/// The handler is async: the daemon's `handle` method is async, and the
/// `tokio::sync::Mutex` guard is `Send` (the daemon is `Send`), so the guard
/// can be held across an `.await`.
pub type RpcHandler = Box<
    dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>> + Send + Sync,
>;

impl ControlSocketServer {
    /// Create a server for `<state_dir>/ldmuxd.sock`.
    pub fn new(state_dir: &std::path::Path) -> Self {
        Self {
            socket_path: state_dir.join("ldmuxd.sock"),
        }
    }

    /// A server for the socket at exactly `path` (what `LDMUX_SOCK` names), not
    /// necessarily under the state dir.
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self { socket_path: path.into() }
    }

    /// The socket path (for tests).
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Bind the socket (0600) and return the listener.
    ///
    /// Removes any stale socket, binds with the process umask set to 0o077 (so
    /// the file is created 0600, not 0755), and returns the listener. The umask
    /// is restored before returning.
    pub fn bind(&self) -> Result<UnixListener, String> {
        let _ = std::fs::remove_file(&self.socket_path);
        // SAFETY: `umask` is a pure process-global setter; we restore it before
        // returning, and the only code between the set and the restore is the
        // (synchronous) `bind`, so no other thread can observe a wrong umask.
        let prev = unsafe { nix::libc::umask(0o077) };
        let listener = tokio::net::UnixListener::bind(&self.socket_path)
            .map_err(|e| format!("bind control socket: {e}"))?;
        unsafe { nix::libc::umask(prev) };
        std::fs::set_permissions(&self.socket_path, Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod control socket: {e}"))?;
        Ok(listener)
    }

    /// Serve one connection: read a JSON line, dispatch, write the response.
    ///
    /// Reads bytes until a newline (or EOF); an empty line is a no-op (a client
    /// that opened and closed without sending). The response is a JSON line
    /// (the `id` echoed back, an `ok`/`result`, or an `error`).
    pub async fn serve_one_with(
        stream: UnixStream,
        handler: &RpcHandler,
    ) -> Result<(), String> {
        let mut stream = stream;
        let mut buf: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        // Bounded in size and time: a client that never sends the newline must not
        // hold memory and a task for the life of the daemon.
        let read_line = async {
            loop {
                match stream.read(&mut byte).await {
                    Ok(0) => return Ok(()), // EOF
                    Ok(_) => {
                        if byte[0] == b'\n' {
                            return Ok(());
                        }
                        if buf.len() >= MAX_REQUEST_LINE {
                            return Err("request line too large".to_string());
                        }
                        buf.push(byte[0]);
                    }
                    Err(e) => return Err(format!("read request: {e}")),
                }
            }
        };
        match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_line).await {
            Ok(r) => r?,
            Err(_) => return Err("timed out waiting for the request line".to_string()),
        }
        if buf.is_empty() {
            return Ok(());
        }
        let req: Value =
            serde_json::from_slice(&buf).map_err(|e| format!("parse request: {e}"))?;
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        let result = handler(method, &req).await;
        let response = match result {
            Ok(r) => Value::from_iter([
                ("ok", Value::Bool(true)),
                ("id", req.get("id").cloned().unwrap_or(Value::Null)),
                ("result", r),
            ]),
            Err(e) => Value::from_iter([
                ("ok", Value::Bool(false)),
                ("id", req.get("id").cloned().unwrap_or(Value::Null)),
                ("error", Value::String(e)),
            ]),
        };
        let mut out = response.to_string();
        out.push('\n');
        stream
            .write_all(out.as_bytes())
            .await
            .map_err(|e| format!("write response: {e}"))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("flush response: {e}"))?;
        Ok(())
    }
}

/// Connections served at once. One task each: a client that connects and says
/// nothing holds only its own slot (for up to [`REQUEST_READ_TIMEOUT`]), instead
/// of the whole accept loop, which is what made the CLI wait out a stranger's
/// silence.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// Accept connections until the listener fails, serving each on its own task.
///
/// Past [`MAX_CONCURRENT_CONNECTIONS`] a new connection is closed at once rather
/// than queued behind the others.
pub async fn serve_loop(
    listener: &UnixListener,
    handler: std::sync::Arc<RpcHandler>,
) -> Result<(), String> {
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| format!("accept control connection: {e}"))?;
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            eprintln!("ldmuxd: control socket: {MAX_CONCURRENT_CONNECTIONS} connections open; dropping one");
            continue;
        };
        let handler = handler.clone();
        tokio::spawn(async move {
            if let Err(e) = ControlSocketServer::serve_one_with(stream, &handler).await {
                eprintln!("ldmuxd: control socket: {e}");
            }
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub handler: `ping` answers `pong`, anything else is unknown.
    fn stub() -> RpcHandler {
        Box::new(|method, _req| {
            let result = match method {
                "ping" => Ok(Value::String("pong".to_string())),
                other => Err(format!("unknown method: {other}")),
            };
            Box::pin(async move { result })
        })
    }

    fn state_dir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ldmux-control-socket-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A client that opens the socket, sends a `ping`, and gets `pong`.
    #[tokio::test]
    async fn a_ping_is_served_as_a_json_line() {
        let dir = state_dir();
        let server = ControlSocketServer::new(&dir);
        let listener = server.bind().unwrap();
        let socket_path = server.socket_path().to_path_buf();

        // Spawn a task to serve the connection.
        let accept_task = tokio::spawn(async move {
        let (stream, _addr) = listener.accept().await.unwrap();
            ControlSocketServer::serve_one_with(stream, &stub()).await
        });

        // A client connects, sends a `ping`, and reads the response.
        let client = tokio::net::UnixStream::connect(&socket_path)
            .await
            .unwrap();
        let (mut rd, mut wr) = tokio::io::split(client);

        // Send a `ping`.
        let req = serde_json::json!({"id": 1, "method": "ping"});
        let mut out = req.to_string();
        out.push('\n');
        wr.write_all(out.as_bytes()).await.unwrap();
        wr.flush().await.unwrap();

        // Read the response.
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = rd.read(&mut byte).await.unwrap();
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        let resp: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"], "pong");
        assert_eq!(resp["id"], 1);

        drop(wr);
        drop(rd);
        drop(accept_task);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown method is an `error` response.
    #[tokio::test]
    async fn an_unknown_method_is_an_error_response() {
        let dir = state_dir();
        let server = ControlSocketServer::new(&dir);
        let listener = server.bind().unwrap();
        let socket_path = server.socket_path().to_path_buf();

        let accept_task = tokio::spawn(async move {
        let (stream, _addr) = listener.accept().await.unwrap();
            ControlSocketServer::serve_one_with(stream, &stub()).await
        });

        let client = tokio::net::UnixStream::connect(&socket_path)
            .await
            .unwrap();
        let (mut rd, mut wr) = tokio::io::split(client);

        let req = serde_json::json!({"id": 2, "method": "nope"});
        let mut out = req.to_string();
        out.push('\n');
        wr.write_all(out.as_bytes()).await.unwrap();
        wr.flush().await.unwrap();

        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = rd.read(&mut byte).await.unwrap();
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        let resp: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(resp["ok"], false);
        assert!(resp["error"].as_str().unwrap().contains("unknown method"));

        drop(wr);
        drop(rd);
        drop(accept_task);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unterminated request over the cap is refused, not buffered forever.
    #[tokio::test]
    async fn an_unterminated_oversized_request_is_refused() {
        let dir = state_dir();
        let server = ControlSocketServer::new(&dir);
        let listener = server.bind().unwrap();
        let socket_path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            ControlSocketServer::serve_one_with(stream, &stub()).await
        });
        let mut client = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let chunk = vec![b'a'; 64 * 1024];
        // Past the cap the server stops reading and drops the connection, so a late
        // write may fail: either outcome is the refusal.
        for _ in 0..80 {
            if client.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let r = task.await.unwrap();
        assert_eq!(r, Err("request line too large".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client that connects and says nothing must not hold up the next one:
    /// connections are served on their own tasks, not one at a time inside the
    /// accept loop (where the silent one cost everyone its 30 s read timeout).
    #[tokio::test]
    async fn a_silent_client_does_not_block_the_next() {
        let dir = state_dir();
        let server = ControlSocketServer::new(&dir);
        let listener = server.bind().unwrap();
        let socket_path = server.socket_path().to_path_buf();
        let task = tokio::spawn(async move {
            let handler: std::sync::Arc<RpcHandler> = std::sync::Arc::new(stub());
            serve_loop(&listener, handler).await
        });
        // The silent one connects first and holds its connection open.
        let _silent = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let mut client = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        client.write_all(b"{\"id\":7,\"method\":\"ping\"}\n").await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut byte = [0u8; 1];
            loop {
                let n = client.read(&mut byte).await.unwrap();
                if n == 0 || byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
        })
        .await;
        assert!(read.is_ok(), "the second client was not answered behind a silent one");
        let resp: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(resp["result"], "pong");
        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
