//! The ldmux attach client: the desktop-window client (plan §3.9).
//!
//! In production it is `alacritty -e ldmuxd attach <id>`. Its stdio **is**
//! alacritty's pty (it opens no pty pair of its own — a nested one would double
//! the winsize), and it is a **protocol client of the daemon**: it claims the
//! shell (ADR-0010 handover), streams the pane's output to its terminal, and
//! forwards its input back. When another device takes the shell the daemon sends
//! `owned` and cuts the stream; the client prints the notice and waits for
//! Ctrl+R to take it back (the keystroke is the consent, plan §3.9 / D-1).
//!
//! Transport: the daemon's remote WebSocket (the same endpoint the phone uses),
//! addressed via the `remote_info` control-socket call. The device identity is
//! the daemon's desktop device, so `makeLive`'s claim and this attach agree.

use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use ldmux_protocol::frames::{decode_frames, t as ft};
use ldmux_protocol::socket;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

/// The device identity the desk window announces: the daemon's desktop device
/// (`makeLive` claims the shell to this id before the window opens).
const DESKTOP_DEVICE: &str = "desktop";

/// The terminal size of fd 0 (the window's pty), or 80x24.
fn tty_size() -> (u64, u64) {
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { nix::libc::ioctl(0, nix::libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col as u64, ws.ws_row as u64)
    } else {
        (80, 24)
    }
}

/// The terminal size helper, exposed for tests.
pub fn tty_size_for_test() -> (u64, u64) {
    tty_size()
}

/// Put the local terminal in raw mode for the life of the window, restoring the
/// previous termios on drop. Without it, line discipline would hold a keystroke
/// until Enter and the shell would echo it a second time.
struct RawMode {
    original: Option<nix::sys::termios::Termios>,
}

impl RawMode {
    fn enable() -> Self {
        use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};
        let original = tcgetattr(unsafe { std::os::fd::BorrowedFd::borrow_raw(0) }).ok();
        if let Some(ref t) = original {
            let mut raw = t.clone();
            raw.local_flags.remove(
                LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG | LocalFlags::IEXTEN,
            );
            raw.input_flags.remove(
                nix::sys::termios::InputFlags::IXON | nix::sys::termios::InputFlags::ICRNL,
            );
            let _ = tcsetattr(
                unsafe { std::os::fd::BorrowedFd::borrow_raw(0) },
                SetArg::TCSANOW,
                &raw,
            );
        }
        Self { original }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        use nix::sys::termios::{tcsetattr, SetArg};
        if let Some(ref t) = self.original {
            let _ = tcsetattr(
                unsafe { std::os::fd::BorrowedFd::borrow_raw(0) },
                SetArg::TCSANOW,
                t,
            );
        }
    }
}

/// Attach this process's terminal to `session_id` and run until the window
/// closes or the shell ends. `readonly` attaches with no identity, so it holds
/// nothing (the daemon's read-only mirror path).
pub async fn run(session_id: &str, readonly: bool) -> Result<(), String> {
    let sock = ldmux_protocol::paths::resolve_socket_path();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_with(&sock, session_id, readonly, stdin, stdout).await
}

/// The attach loop, with the daemon socket and the terminal streams injected
/// (the production `run` passes the resolved socket and the process's stdio).
pub async fn run_with<R, W>(
    daemon_socket: &str,
    session_id: &str,
    readonly: bool,
    mut input: R,
    mut output: W,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // Where is the daemon's remote? The control socket is the source of truth.
    let info: serde_json::Value = socket::call(daemon_socket, "remote_info", &serde_json::json!({}), 5000)
        .await
        .map_err(|e| e.0)?;
    let hostname = info["hostname"].as_str().unwrap_or("").to_string();
    let port = info["port"].as_u64().unwrap_or(0);
    let path = info["path"].as_str().unwrap_or("/ws").to_string();
    let token = info["token"].as_str().unwrap_or("").to_string();
    if !info["listening"].as_bool().unwrap_or(false) {
        return Err("the daemon's remote is not listening".to_string());
    }
    if hostname.is_empty() || port == 0 {
        return Err("the daemon has no remote address".to_string());
    }

    let uri = format!("ws://{hostname}:{port}{path}");
    let mut req = {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        uri.clone().into_client_request().map_err(|e| e.to_string())?
    };
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| "bad token header".to_string())?,
    );
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("connect {uri}: {e}"))?;

    let _raw = RawMode::enable();
    let (mut cols, mut rows) = tty_size();
    let (mut sink, mut stream) = ws.split();

    // Identify (desktop) and attach. A read-only mirror sends no `hello`, so it
    // holds no claim and cannot type.
    if !readonly {
        send(
            &mut sink,
            &serde_json::json!({
                "type": "hello",
                "device": { "id": DESKTOP_DEVICE, "name": "desktop", "kind": "desktop" }
            }),
        )
        .await?;
    }
    send(
        &mut sink,
        &serde_json::json!({
            "type": "attach", "sessionId": session_id, "cols": cols, "rows": rows
        }),
    )
    .await?;

    let mut sigwinch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .map_err(|e| e.to_string())?;
    let mut inbuf = [0u8; 4096];
    let mut detached = false;

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Text(t))) => {
                        let v: serde_json::Value = match serde_json::from_str(&t.to_string()) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        match v["type"].as_str() {
                            Some("snapshot") => {
                                detached = false;
                                let screen = v["screen"].as_str().unwrap_or("");
                                let mut out = Vec::with_capacity(screen.len() + 8);
                                out.extend_from_slice(b"\x1b[2J\x1b[H");
                                out.extend_from_slice(screen.as_bytes());
                                let _ = output.write_all(&out).await;
                                let _ = output.flush().await;
                            }
                            Some("owned") => {
                                detached = true;
                                let _ = output
                                    .write_all(b"\r\n  This shell is open on another device. Press Ctrl+R to take it back.\r\n")
                                    .await;
                                let _ = output.flush().await;
                            }
                            Some("error") => {
                                let code = v["code"].as_str().unwrap_or("");
                                if code == "resync" {
                                    // Re-attach for a fresh snapshot.
                                    let _ = send(
                                        &mut sink,
                                        &serde_json::json!({
                                            "type": "attach", "sessionId": session_id,
                                            "cols": cols, "rows": rows
                                        }),
                                    )
                                    .await;
                                } else if code == "unknown_session" || code == "session_dead" {
                                    let _ = output
                                        .write_all(b"\r\n  This shell has ended.\r\n")
                                        .await;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(r) = decode_frames(&b) {
                            for f in r.frames {
                                if f.type_ == ft::OUTPUT {
                                    let _ = output.write_all(&f.payload).await;
                                }
                            }
                            let _ = output.flush().await;
                        }
                    }
                    _ => {}
                }
            }
            n = input.read(&mut inbuf) => {
                let n = n.map_err(|e| e.to_string())?;
                if n == 0 {
                    break; // the window closed (EOF on our pty)
                }
                let bytes = &inbuf[..n];
                if detached {
                    // Only Ctrl+R is consent; any other byte is swallowed (so a
                    // stream of dictation cannot steal the shell back).
                    if bytes.contains(&0x12) {
                        detached = false;
                        let _ = send(
                            &mut sink,
                            &serde_json::json!({
                                "type": "attach", "sessionId": session_id,
                                "cols": cols, "rows": rows, "takeover": true
                            }),
                        )
                        .await;
                    }
                    continue;
                }
                if readonly {
                    continue;
                }
                let text = String::from_utf8_lossy(bytes).to_string();
                let _ = send(
                    &mut sink,
                    &serde_json::json!({ "type": "input", "data": text }),
                )
                .await;
            }
            _ = sigwinch.recv() => {
                let (c, r) = tty_size();
                if (c, r) != (cols, rows) {
                    cols = c;
                    rows = r;
                    let _ = send(
                        &mut sink,
                        &serde_json::json!({ "type": "resize", "cols": cols, "rows": rows }),
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

async fn send<S>(sink: &mut S, value: &serde_json::Value) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    sink.send(Message::Text(value.to_string().into()))
        .await
        .map_err(|e| e.to_string())
}