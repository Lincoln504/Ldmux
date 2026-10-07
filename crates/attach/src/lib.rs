//! The ldmux attach client: the desktop-window client (plan §3.9; "plan" in
//! these comments is `docs/archive/RUST-PORT-PLAN.md`).
//!
//! In production it is `alacritty -e ldmuxd attach <id>`. Its stdio **is**
//! alacritty's pty (it opens no pty pair of its own — a nested one would double
//! the winsize), and it is a **protocol client of the daemon**: it claims the
//! shell (ADR-0010 handover), streams the pane's output to its terminal, and
//! forwards its input back. When another device takes the shell the daemon sends
//! `owned` and cuts the stream; the client prints the notice and waits for
//! `r` to take it back (the keystroke is the consent, plan §3.9 / D-1).
//!
//! Transport: the daemon's **local attach socket** (`<control>-attach.sock`, a
//! Unix socket speaking the same WebSocket protocol the phone uses, 0600 and
//! checked by peer uid), so a window needs no tailnet and no token. Only a daemon
//! that predates that socket is reached the old way, over the remote endpoint the
//! `remote_info` control-socket call names. The device identity is the daemon's
//! desktop device, so `makeLive`'s claim and this attach agree.

use futures_util::SinkExt;
use futures_util::stream::StreamExt;
use ldmux_protocol::frames::{decode_frames, t as ft};
use ldmux_protocol::socket;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

/// The screen state a snapshot restores into a terminal.
///
/// The wire carries this state, not the previous implementation's assumption
/// that the content was the only thing that mattered. A window is a fresh
/// terminal, so the client must restore a screen *and* the terminal modes
/// around it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot<'a> {
    pub screen: &'a str,
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub cols: u64,
    pub rows: u64,
    pub alternate_on: bool,
    pub mouse: bool,
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
}

impl<'a> Snapshot<'a> {
    /// Read the state out of a `snapshot` frame.
    pub fn from_json(v: &'a serde_json::Value) -> Self {
        let b = |k: &str| v[k].as_bool().unwrap_or(false);
        let n = |k: &str| v[k].as_u64().unwrap_or(0);
        Self {
            screen: v["screen"].as_str().unwrap_or(""),
            cursor_x: n("cursorX"),
            cursor_y: n("cursorY"),
            cols: n("cols"),
            rows: n("rows"),
            alternate_on: b("alternateOn"),
            mouse: b("mouse"),
            mouse_sgr: b("mouseSgr"),
            app_cursor: b("appCursor"),
            bracketed_paste: b("bracketedPaste"),
        }
    }
}

/// Render a daemon snapshot onto a terminal: the desktop client's whole job.
///
/// This is not a terminal renderer. The daemon owns the emulator and serialises
/// its grid, and every client is an emulator that consumes the bytes — the
/// mobile client feeds the same `screen` string to its own libghostty-vt, and
/// this one feeds it to alacritty. Three things go wrong if this is written
/// carelessly, and all three were:
///
///   * **Rows must be separated by `\r\n`, not `\n`.** A terminal's LF moves
///     down without returning the column, so the screen staircases diagonally.
///   * **The trailing newline must not be written.** The daemon terminates
///     *every* row including the last, so an N-row pane carries N newlines.
///     Replaying all of them moves the cursor one row past the bottom, the
///     terminal scrolls, every line shifts up by one, and the prompt a fresh
///     shell had just printed slides off the top and reappears at the bottom.
///   * **The modes must be re-established.** A window is a fresh terminal: the
///     alternate screen, mouse reporting, application-cursor and bracketed
///     paste are all off, whatever the pane's program has set. A window
///     is a fresh terminal, so the client has to establish this itself.
///
/// The mobile client documents the first two from measurement
/// (`snapshotReplay` in `TerminalController.kt`); this is the same contract.
pub fn render_snapshot(s: &Snapshot) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.screen.len() + 64);
    // Which screen, then the modes, then clear, then content, then the cursor.
    // `?1049` is the save/switch form, which is what a full-screen program uses,
    // and it is sent unconditionally (`l` included): on a fresh terminal leaving
    // the primary screen is a no-op, and being unconditional is what makes the
    // two screens impossible to get out of step.
    out.extend_from_slice(if s.alternate_on {
        b"\x1b[?1049h"
    } else {
        b"\x1b[?1049l"
    });
    out.extend_from_slice(if s.mouse {
        b"\x1b[?1000h"
    } else {
        b"\x1b[?1000l"
    });
    out.extend_from_slice(if s.mouse_sgr {
        b"\x1b[?1006h"
    } else {
        b"\x1b[?1006l"
    });
    out.extend_from_slice(if s.app_cursor {
        b"\x1b[?1h"
    } else {
        b"\x1b[?1l"
    });
    out.extend_from_slice(if s.bracketed_paste {
        b"\x1b[?2004h"
    } else {
        b"\x1b[?2004l"
    });
    // The window is already empty when it opens, but a resync re-renders over
    // whatever is on screen, so the visible screen is cleared either way.
    out.extend_from_slice(b"\x1b[2J\x1b[H");
    let body = s.screen.strip_suffix('\n').unwrap_or(s.screen);
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line.as_bytes());
    }
    // The daemon's cursor is the authority, clamped as the mobile client clamps
    // it so a malformed frame cannot place the cursor outside the grid.
    let x = s.cursor_x.min(s.cols.saturating_sub(1)) + 1;
    let y = s.cursor_y.min(s.rows.saturating_sub(1)) + 1;
    out.extend_from_slice(format!("\x1b[{y};{x}H").as_bytes());
    out
}

/// The device identity the desk window announces: the daemon's desktop device
/// (`makeLive` claims the shell to this id before the window opens).
const DESKTOP_DEVICE: &str = "desktop";

/// How often this client says something to the daemon.
///
/// One number, because it is a property of the **protocol**, not of a client: the
/// daemon reaps a connection silent for `WS_IDLE_MS` (20 s). The mobile client
/// states the same rule (`CLIENT_PING_INTERVAL_MS`) and documents what happens
/// when it is stated in two places and they disagree — one socket pinged, the
/// other did not, and the silent one was reaped every minute and read as the
/// daemon being flaky. This window was the third place and stated nothing at all.
const PING_INTERVAL_MS: u64 = 4_000;

/// Unanswered pings before the link is declared dead (the mobile client's
/// `CLIENT_MISSED_PONGS`): the OS only reports a broken link when it notices,
/// and on a sleeping radio or a NAT that forgot the flow it does not notice.
const MAX_MISSED_PONGS: u32 = 3;

/// The terminal size of fd 0 (the window's pty), or 80x24.
fn tty_size() -> (u64, u64) {
    // SAFETY: `winsize` is a plain C struct of four `u16`s, for which the
    // all-zero bit pattern is a valid value.
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes exactly one `winsize` through the pointer, and
    // `ws` is a live, exclusively borrowed, initialised `winsize` on this stack
    // frame. fd 0 may be closed or not a tty; that is reported as -1, not UB.
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

/// Whether a snapshot's grid means this window should re-assert its own size.
///
/// The pane is shared, so another device can have resized it. A window whose own
/// tty never changed gets no SIGWINCH, so seeing the disagreement in a snapshot is
/// the only notice it will ever get. Re-asserting once per mismatch (rather than on
/// every snapshot) bounds it against a device that keeps resizing. A read-only
/// mirror holds nothing and must never resize.
fn should_reassert(
    cols: u64,
    rows: u64,
    snap_cols: u64,
    snap_rows: u64,
    readonly: bool,
    asserted: Option<(u64, u64)>,
) -> bool {
    !readonly && (cols, rows) != (snap_cols, snap_rows) && asserted != Some((cols, rows))
}

/// What a window leaves the terminal in when it exits, undone. Unconditional and
/// idempotent: a snapshot establishes alternate screen, mouse reporting,
/// application cursor and bracketed paste, and the pane's program can change them
/// later through the output stream, so "what the snapshot enabled" is not all
/// there is to undo. Every one of these is a no-op on a terminal that does not
/// have the mode on.
pub const TERMINAL_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[0m";

/// The termios to restore. Static, because a panic hook and a signal path have no
/// `self` to ask.
static SAVED: std::sync::Mutex<Option<nix::sys::termios::Termios>> = std::sync::Mutex::new(None);

/// Put the terminal back: the mode resets, then the termios the window found.
/// Safe to call any number of times, from any path (drop, signal, panic).
pub fn restore_terminal() {
    use nix::sys::termios::{SetArg, tcsetattr};
    let saved = SAVED.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(t) = saved {
        // SAFETY: the pointer/length pair is a `'static` byte slice, readable for
        // its whole length; `write` does not retain it. fd 1 may be closed, which
        // is an ignored error return, not UB.
        unsafe { nix::libc::write(1, TERMINAL_RESET.as_ptr().cast(), TERMINAL_RESET.len()) };
        let _ = tcsetattr(
            // SAFETY: fd 0 is the process's stdin, open for the life of the
            // process; the `BorrowedFd` is used only for this call and never
            // closed. A closed fd would only make `tcsetattr` return EBADF.
            unsafe { std::os::fd::BorrowedFd::borrow_raw(0) },
            SetArg::TCSANOW,
            &t,
        );
    }
}

/// The local terminal in raw mode for the life of the window, put back on drop,
/// on SIGTERM/SIGHUP/SIGINT (see [run]) and on panic. Without raw mode, line
/// discipline would hold a keystroke until Enter and the shell would echo it a
/// second time.
struct Terminal;

impl Terminal {
    /// Refuses when stdin or stdout is not a terminal: there is nothing to put
    /// in raw mode and nothing a snapshot could be painted on.
    fn enter() -> Result<Self, String> {
        use nix::sys::termios::{SetArg, SpecialCharacterIndices, cfmakeraw, tcgetattr, tcsetattr};
        // SAFETY: `isatty` takes only an integer and tolerates any value,
        // returning 0 with EBADF for a closed descriptor.
        let is_tty = |fd| unsafe { nix::libc::isatty(fd) } == 1;
        if !is_tty(0) || !is_tty(1) {
            return Err(
                "stdin/stdout is not a terminal; ldmux attach must run in one (use `ldmux shell` \
                 in a terminal, or `ldmux window` to open a desk window)"
                    .to_string(),
            );
        }
        // SAFETY: fd 0 is the process's stdin and was just checked to be a tty,
        // so it is open; it stays open for the life of the process and the
        // `BorrowedFd` is never closed or stored beyond this function.
        let fd0 = unsafe { std::os::fd::BorrowedFd::borrow_raw(0) };
        let original =
            tcgetattr(fd0).map_err(|e| format!("cannot read the terminal settings: {e}"))?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        // cfmakeraw leaves the read thresholds alone: a read must return on one
        // byte, with no timer.
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = Some(original);
        install_panic_hook();
        tcsetattr(fd0, SetArg::TCSANOW, &raw)
            .map_err(|e| format!("cannot set the terminal to raw mode: {e}"))?;
        Ok(Terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// A panic puts the terminal back *before* its message is printed, so the message
/// is readable and the shell that gets the terminal next is not left raw.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

/// Resolves with the name of the first of SIGTERM, SIGHUP, SIGINT to arrive. With
/// ISIG off, ^C is a byte for the shell, so these only ever come from outside: a
/// `kill`, a closed terminal, a session ending.
async fn termination() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = hup.recv() => "SIGHUP",
        _ = int.recv() => "SIGINT",
    }
}

/// Attach this process's terminal to `session_id` and run until the window
/// closes or the shell ends. `readonly` attaches with no identity, so it holds
/// nothing (the daemon's read-only mirror path).
///
/// The terminal is put in raw mode here, not in the loop, and is restored on every
/// way out: return, a termination signal, a panic.
pub async fn run(session_id: &str, readonly: bool) -> Result<(), String> {
    let sock = ldmux_protocol::paths::resolve_socket_path();
    let _terminal = Terminal::enter()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    tokio::select! {
        r = run_with(&sock, session_id, readonly, stdin, stdout) => r,
        name = termination() => Err(format!("terminated by {name}")),
    }
}

/// The attach loop, with the daemon socket and the terminal streams injected
/// (the production `run` passes the resolved socket and the process's stdio).
pub async fn run_with<R, W>(
    daemon_socket: &str,
    session_id: &str,
    readonly: bool,
    input: R,
    output: W,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    run_with_ping(
        daemon_socket,
        session_id,
        readonly,
        input,
        output,
        PING_INTERVAL_MS,
    )
    .await
}

/// One connection's lifetime, from the caller's point of view.
enum Outcome {
    /// The shell is gone, or the window closed: stop, do not reconnect.
    Ended,
    /// The link dropped but the shell may still be there: reconnect.
    Lost,
}

/// The daemon's remote endpoint, as the control socket reports it.
struct RemoteInfo {
    hostname: String,
    port: u64,
    path: String,
    token: String,
}

async fn remote_info(daemon_socket: &str) -> Result<RemoteInfo, String> {
    let info: serde_json::Value =
        socket::call(daemon_socket, "remote_info", &serde_json::json!({}), 5000)
            .await
            .map_err(|e| e.0)?;
    let hostname = info["hostname"].as_str().unwrap_or("").to_string();
    let port = info["port"].as_u64().unwrap_or(0);
    let path = info["path"].as_str().unwrap_or("/ws").to_string();
    let token = info["token"].as_str().unwrap_or("").to_string();
    if !info["listening"].as_bool().unwrap_or(false) {
        return Err(
            "the daemon's remote is not listening (is Tailscale up? LDMUX_REMOTE_HOST must be an address this machine has; `ldmux remote` shows what the daemon bound)"
                .to_string(),
        );
    }
    if hostname.is_empty() || port == 0 {
        return Err("the daemon has no remote address".to_string());
    }
    Ok(RemoteInfo {
        hostname,
        port,
        path,
        token,
    })
}

/// 250 ms doubling to 5 s. A daemon restart takes a second or two; a window must
/// not hammer a socket that is not there, and must reconnect promptly when it is.
fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let ms = 250u64.saturating_mul(1u64 << attempt.min(5));
    std::time::Duration::from_millis(ms.min(5000))
}

async fn note<W: AsyncWriteExt + Unpin>(output: &mut W, text: &str) {
    let _ = output
        .write_all(format!("\r\n  {text}\r\n").as_bytes())
        .await;
    let _ = output.flush().await;
}

/// `run_with`, with the keep-alive interval injectable so a test can prove the
/// contract in milliseconds instead of twenty seconds.
///
/// The window lives across a daemon restart. The socket is expected to blink
/// (a restart, a `systemctl stop`, a deploy) and the window reconnects with
/// backoff and re-attaches for a fresh snapshot. Exiting on the first close made
/// every deploy close every desk window, which the daemon then read as "the user
/// closed the window" and ended the shell.
pub async fn run_with_ping<R, W>(
    daemon_socket: &str,
    session_id: &str,
    readonly: bool,
    mut input: R,
    mut output: W,
    ping_ms: u64,
) -> Result<(), String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // Raw mode is the caller's (see `run`): it spans every reconnect, and a
    // test's duplex streams have no terminal to set up.
    let mut attempt: u32 = 0;
    loop {
        match attach_once(
            daemon_socket,
            session_id,
            readonly,
            &mut input,
            &mut output,
            ping_ms,
        )
        .await
        {
            // The shell ended, or the window closed: this process is done.
            Ok(Outcome::Ended) => return Ok(()),
            Ok(Outcome::Lost) => {
                note(&mut output, "ldmux: link lost, reconnecting").await;
                attempt = 0;
            }
            Err(e) => {
                note(&mut output, &format!("ldmux: {e}; retrying")).await;
            }
        }
        attempt += 1;
        tokio::time::sleep(reconnect_delay(attempt)).await;
    }
}

/// One WebSocket connection: connect, attach, pump until the link or the shell
/// ends. Keeping raw mode and the retry policy out of here is what makes a
/// reconnect a fresh call instead of a second copy of the loop.
async fn attach_once<R, W>(
    daemon_socket: &str,
    session_id: &str,
    readonly: bool,
    input: &mut R,
    output: &mut W,
    ping_ms: u64,
) -> Result<Outcome, String>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // The way in is the daemon's local attach socket (`<control>-attach.sock`):
    // the same WebSocket protocol over a Unix socket, so a window needs neither
    // the tailnet nor the token. Only a daemon that predates the socket (nothing
    // there, or nothing listening) is reached the old way, over the remote
    // endpoint `remote_info` names.
    let local = ldmux_protocol::paths::attach_socket_path_from(daemon_socket);
    match tokio::net::UnixStream::connect(&local).await {
        Ok(stream) => {
            let (ws, _) = tokio_tungstenite::client_async("ws://ldmux.local/ws", stream)
                .await
                .map_err(|e| format!("attach socket {local}: {e}"))?;
            pump(ws, session_id, readonly, input, output, ping_ms).await
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            let info = remote_info(daemon_socket).await?;
            let uri = format!("ws://{}:{}{}", info.hostname, info.port, info.path);
            let mut req = {
                use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                uri.clone()
                    .into_client_request()
                    .map_err(|e| e.to_string())?
            };
            req.headers_mut().insert(
                "Authorization",
                format!("Bearer {}", info.token)
                    .parse()
                    .map_err(|_| "bad token header".to_string())?,
            );
            let (ws, _) = tokio_tungstenite::connect_async(req)
                .await
                .map_err(|e| format!("connect {uri}: {e}"))?;
            pump(ws, session_id, readonly, input, output, ping_ms).await
        }
        Err(e) => Err(format!("attach socket {local}: {e}")),
    }
}

/// Attach and pump on an established WebSocket until the link or the shell ends.
async fn pump<S, R, W>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    session_id: &str,
    readonly: bool,
    input: &mut R,
    output: &mut W,
    ping_ms: u64,
) -> Result<Outcome, String>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let (mut cols, mut rows) = tty_size();
    let (mut sink, mut stream) = ws.split();

    // Keep-alive. The daemon reaps a connection that has said nothing for
    // `WS_IDLE_MS` (20 s) — that is what makes a phone that vanished without a
    // FIN stop holding shells it is not looking at — so every socket has to
    // speak on a schedule. The mobile client states this once for both of its
    // sockets and documents the failure of stating it twice; this window is the
    // second place, and it said nothing, so the daemon closed it and the window
    // exited about 20 s after it opened.
    let mut ping = tokio::time::interval(std::time::Duration::from_millis(ping_ms.max(1)));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the first tick is immediate; the first ping is not
    let mut missed_pongs: u32 = 0;

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

    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .map_err(|e| e.to_string())?;
    let mut inbuf = [0u8; 4096];
    // A multi-byte character can straddle two reads (a large paste): decoding
    // each read alone turned both halves into U+FFFD in the user's shell.
    let mut in_utf8 = ldmux_protocol::utf8::Utf8Carry::new();
    let mut detached = false;
    // The size this window last re-asserted after a snapshot disagreed with it.
    // `None` means we are in step. Bounded so a device that keeps resizing the
    // shared pane cannot make this an endless exchange.
    let mut asserted: Option<(u64, u64)> = None;

    loop {
        tokio::select! {
            msg = stream.next() => {
                match msg {
                    None | Some(Err(_)) => return Ok(Outcome::Lost),
                    Some(Ok(Message::Text(t))) => {
                        let v: serde_json::Value = match serde_json::from_str(t.as_ref()) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        match v["type"].as_str() {
                            Some("snapshot") => {
                                detached = false;
                                let snap = Snapshot::from_json(&v);
                                // **The pane is shared, and a window keeps its own
                                // size.** Another device that attached and resized
                                // (then left) leaves the pty at its grid, and this
                                // window — whose tty never changed, so it got no
                                // SIGWINCH — would paint every row at the wrong
                                // width for the rest of the session. Getting a
                                // `snapshot` (rather than `owned`) means this window
                                // holds the shell, so its own size is the one that
                                // should win: re-assert it, once per mismatch.
                                let mismatch = (cols, rows) != (snap.cols, snap.rows);
                                if should_reassert(cols, rows, snap.cols, snap.rows, readonly, asserted)
                                {
                                    asserted = Some((cols, rows));
                                    let _ = send(
                                        &mut sink,
                                        &serde_json::json!({
                                            "type": "resize", "cols": cols, "rows": rows
                                        }),
                                    )
                                    .await;
                                } else if !mismatch {
                                    asserted = None;
                                }
                                let out = render_snapshot(&snap);
                                let _ = output.write_all(&out).await;
                                let _ = output.flush().await;
                            }
                            Some("owned") => {
                                detached = true;
                                let _ = output
                                    .write_all(b"\r\n  This shell is open on another device. Press r to take it back.\r\n")
                                    .await;
                                let _ = output.flush().await;
                            }
                            Some("sessions") => {
                                let entry = v["sessions"].as_array().and_then(|a| {
                                    a.iter().find(|s| s["id"].as_str() == Some(session_id))
                                });
                                let running = entry
                                    .map(|s| s["status"].as_str() == Some("running"))
                                    .unwrap_or(false);
                                if !running {
                                    // Gone, or no longer running: this shell is over
                                    // (or the daemon does not know it). Ask, and let
                                    // the verdict end the window — `session_dead` and
                                    // `unknown_session` print "This shell has ended."
                                    // and exit.
                                    //
                                    // A displaced window used to sit on its notice
                                    // for a shell that no longer existed: the notice
                                    // is only ever replaced by an answer, and once
                                    // the phone took the shell this window had
                                    // stopped asking. Asking is also what makes a
                                    // list this window misreads harmless — one round
                                    // trip, and the daemon's own state decides.
                                    let _ = send(
                                        &mut sink,
                                        &serde_json::json!({
                                            "type": "attach", "sessionId": session_id,
                                            "cols": cols, "rows": rows
                                        }),
                                    )
                                    .await;
                                } else if detached {
                                    // The shell may have been let go while this
                                    // window waited. A device that puts a shell down
                                    // is not using it, so the window shows it again —
                                    // the TypeScript shim re-attached on exactly
                                    // this, by watching for the daemon to clear its
                                    // "held elsewhere" note. Only a shell *nobody*
                                    // holds comes back on its own: one another device
                                    // holds keeps waiting for `r`, which is the
                                    // consent.
                                    let free = entry
                                        .and_then(|s| s.get("owner"))
                                        .map(|o| o.is_null())
                                        .unwrap_or(true);
                                    if free {
                                        detached = false;
                                        let _ = send(
                                            &mut sink,
                                            &serde_json::json!({
                                                "type": "attach", "sessionId": session_id,
                                                "cols": cols, "rows": rows
                                            }),
                                        )
                                        .await;
                                    }
                                }
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
                                    let _ = output.flush().await;
                                    return Ok(Outcome::Ended);
                                }
                            }
                            Some("pong") => {
                                missed_pongs = 0;
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
                    // The window closed (EOF on our pty). Reconnecting would be
                    // pointless: there is no terminal left to paint.
                    return Ok(Outcome::Ended);
                }
                let bytes = &inbuf[..n];
                if detached {
                    // One key is the consent: `r`, and nothing else. Every other
                    // byte is swallowed, so a paste or a stream of dictation
                    // cannot steal the shell back.
                    if is_reclaim(bytes) {
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
                let text = in_utf8.decode(bytes);
                if text.is_empty() {
                    continue; // only the start of a character so far
                }
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
            // Speak on a schedule or the daemon reaps the socket and this window
            // exits about 20 s after it opens.
            _ = ping.tick() => {
                if missed_pongs >= MAX_MISSED_PONGS {
                    return Ok(Outcome::Lost);
                }
                missed_pongs += 1;
                if send(&mut sink, &serde_json::json!({ "type": "ping" })).await.is_err() {
                    return Ok(Outcome::Lost);
                }
            }
        }
    }
}

/// Whether these bytes are the user asking for the shell back.
///
/// `r` is the key, and the only key. One definition, so the notice and the check
/// cannot drift apart.
///
/// **A bare keypress, and nothing else:** the read must be exactly one byte and
/// that byte an `r` or `R`. Every emulator sends a bare `r` as the byte 0x72 —
/// that is what a tty *is*, not a convention — and a single keystroke arrives as
/// a single read.
///
/// Anything longer is text, not a keypress. A paste arrives as one read
/// (`\x1b[200~remove the file\x1b[201~`), and so does a word typed quickly
/// enough. Counting "a read that contains an `r`" as the consent meant typing
/// **`echo BACK_MARK`** into a detached window took the shell back — measured,
/// by a test that was trying to prove the opposite.
fn is_reclaim(bytes: &[u8]) -> bool {
    matches!(bytes, [b'r'] | [b'R'])
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
#[cfg(test)]
mod render_tests {
    use super::{Snapshot, render_snapshot, should_reassert};

    fn snap(screen: &str) -> Snapshot<'_> {
        Snapshot {
            screen,
            cursor_x: 0,
            cursor_y: 0,
            cols: 80,
            rows: 24,
            ..Default::default()
        }
    }

    /// Another device can resize the shared pane under a window whose own tty did
    /// not change. The window must take its size back once, and not spin doing it.
    #[test]
    fn a_snapshot_mismatch_re_asserts_the_size_once() {
        // In step: nothing to do.
        assert!(!should_reassert(158, 82, 158, 82, false, None));
        // The pane was resized by another device: take it back.
        assert!(should_reassert(158, 82, 48, 17, false, None));
        // Already re-asserted this size and the answer has not caught up: wait,
        // do not send one resize per snapshot.
        assert!(!should_reassert(158, 82, 48, 17, false, Some((158, 82))));
        // A read-only mirror holds nothing and must never resize.
        assert!(!should_reassert(158, 82, 48, 17, true, None));
        // Once in step again, the next mismatch is a new one to answer.
        assert!(should_reassert(158, 82, 48, 17, false, Some((100, 40))));
    }

    /// A bare LF moves down without returning the column, so the screen
    /// staircases diagonally. Both clients replay a capture, so both need CRLF.
    #[test]
    fn rows_are_separated_by_carriage_return_and_line_feed() {
        let out = String::from_utf8(render_snapshot(&snap("aa\nbb\n"))).unwrap();
        assert!(out.contains("aa\r\nbb"), "rows must not staircase: {out:?}");
        assert_eq!(
            out.matches('\n').count(),
            1,
            "one separator for two rows: {out:?}"
        );
    }

    /// The daemon terminates *every* row, including the last, so an N-row pane
    /// carries N newlines and the replay must write N-1 separators. Replaying
    /// the last one moves the cursor a row past the bottom and scrolls, which is
    /// what slid a fresh shell's prompt off the top of the window.
    ///
    /// The invariant is exact: a screen with its terminator and the same screen
    /// without it must produce identical bytes.
    #[test]
    fn the_trailing_newline_is_not_written() {
        fn body(out: &[u8]) -> String {
            let s = String::from_utf8(out.to_vec()).unwrap();
            let after = s.split("\u{1b}[2J\u{1b}[H").nth(1).unwrap();
            after.rsplit_once('\u{1b}').unwrap().0.to_string()
        }
        let with = render_snapshot(&snap("aa\nbb\n"));
        let without = render_snapshot(&snap("aa\nbb"));
        assert_eq!(
            body(&with),
            body(&without),
            "the terminator must make no difference"
        );
        assert_eq!(body(&with), "aa\r\nbb", "two rows, one separator");
        assert_eq!(body(&render_snapshot(&snap("aa\n"))), "aa", "one row, none");
    }

    /// The daemon's cursor is the authority: it decides where the prompt shows.
    #[test]
    fn the_cursor_is_placed_where_the_daemon_says() {
        let out = String::from_utf8(render_snapshot(&Snapshot {
            screen: "[ x$ ]\n\n",
            cursor_x: 6,
            cursor_y: 0,
            ..snap("")
        }))
        .unwrap();
        assert!(out.ends_with("\u{1b}[1;7H"), "{out:?}");
        let out = String::from_utf8(render_snapshot(&Snapshot {
            screen: "[ x$ ]\n\n",
            cursor_x: 2,
            cursor_y: 3,
            ..snap("")
        }))
        .unwrap();
        assert!(out.ends_with("\u{1b}[4;3H"), "{out:?}");
    }

    /// The cursor is clamped to the grid, as the mobile client clamps it: a
    /// malformed frame must not place it outside the pane.
    #[test]
    fn the_cursor_is_clamped_to_the_grid() {
        let out = String::from_utf8(render_snapshot(&Snapshot {
            cursor_x: 999,
            cursor_y: 999,
            cols: 80,
            rows: 24,
            ..snap("x")
        }))
        .unwrap();
        assert!(out.ends_with("\u{1b}[24;80H"), "{out:?}");
    }

    /// A window is a fresh terminal, so everything the pane's program set is off
    /// unless the snapshot says otherwise. Without this a
    /// full-screen program loses its mouse mode.
    #[test]
    fn the_screen_and_the_modes_are_established() {
        let out = String::from_utf8(render_snapshot(&Snapshot {
            alternate_on: true,
            mouse: true,
            mouse_sgr: true,
            app_cursor: true,
            bracketed_paste: true,
            ..snap("x")
        }))
        .unwrap();
        assert!(
            out.starts_with("\u{1b}[?1049h"),
            "alternate screen: {out:?}"
        );
        for seq in [
            "\u{1b}[?1000h",
            "\u{1b}[?1006h",
            "\u{1b}[?1h",
            "\u{1b}[?2004h",
        ] {
            assert!(out.contains(seq), "missing {seq:?} in {out:?}");
        }
    }

    /// And they are sent unconditionally, in the `l` form too: on a fresh
    /// terminal that is a no-op, and being unconditional is what keeps the two
    /// screens from getting out of step.
    #[test]
    fn modes_are_sent_even_when_off() {
        let out = String::from_utf8(render_snapshot(&snap("x"))).unwrap();
        for seq in [
            "\u{1b}[?1049l",
            "\u{1b}[?1000l",
            "\u{1b}[?1006l",
            "\u{1b}[?1l",
            "\u{1b}[?2004l",
        ] {
            assert!(out.contains(seq), "missing {seq:?} in {out:?}");
        }
    }

    /// The visible screen is cleared, so a resync does not lay the new screen
    /// over the old one.
    /// The reclaim is one named key, and nothing else takes a shell back.
    #[test]
    fn only_the_named_key_reclaims_a_shell() {
        use super::is_reclaim;
        assert!(is_reclaim(b"r"));
        assert!(is_reclaim(b"R"));
        assert!(!is_reclaim(b"\x12"), "there is no second key");
        // The reason the consent is a named key at all: a paste, an escape
        // sequence, or dictation must not take a shell nobody asked for.
        assert!(!is_reclaim(b""));
        assert!(!is_reclaim(b"hello"));
        assert!(!is_reclaim(b"\x1b[200~pasted\x1b[201~"));
        assert!(!is_reclaim(b"\r"));
        // Text is not a keypress, however short, and whether or not it holds an
        // `r`: this is what a word typed into a detached window looks like, and
        // what a paste looks like.
        assert!(!is_reclaim(b"echo BACK_MARK"));
        assert!(!is_reclaim(b"rs"));
        assert!(!is_reclaim(b"\x1b[200~remove the file\x1b[201~"));
        assert!(!is_reclaim(b"\x1br"), "Alt-R is not the key");
        assert!(!is_reclaim(b"\x1b[1;3C"));
    }

    /// The reconnect backoff grows and then holds. A window must retry quickly
    /// after a one-second restart and must not hammer a daemon that is down.
    #[test]
    fn reconnect_delay_grows_to_a_cap() {
        use super::reconnect_delay;
        assert_eq!(reconnect_delay(1).as_millis(), 500);
        assert_eq!(reconnect_delay(2).as_millis(), 1000);
        assert_eq!(reconnect_delay(3).as_millis(), 2000);
        assert_eq!(reconnect_delay(9).as_millis(), 5000);
        let mut prev = 0u128;
        for a in 1..10 {
            let ms = reconnect_delay(a).as_millis();
            assert!(ms >= prev, "backoff must not shrink at attempt {a}");
            prev = ms;
        }
    }

    #[test]
    fn the_screen_is_cleared_and_homed_before_the_content() {
        let out = String::from_utf8(render_snapshot(&snap("x"))).unwrap();
        let clear = out.find("\u{1b}[2J\u{1b}[H").expect("clear");
        let content = out.find('x').expect("content");
        assert!(clear < content, "clear before content: {out:?}");
    }
}
