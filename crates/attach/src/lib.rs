//! The ldmux attach client: the desktop-window client.
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
//! checked by peer uid), so a window needs no tailnet and no token. It is the only
//! way in. The device identity is the daemon's desktop device, so `makeLive`'s
//! claim and this attach agree.

use futures_util::SinkExt;
use futures_util::stream::StreamExt;
use ldmux_protocol::frames::{decode_frames, t as ft};
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
    // A paint is exactly the rows below, so nothing the program left behind may
    // reinterpret them: a scroll region (`DECSTBM`) would make the clear apply to that
    // region only and put the first row somewhere else, insert mode would push every
    // character along, and a pending SGR would colour the cells the pane did not style.
    // Autowrap is off for the paint and restored after it, because the rows are cut to the
    // window by `clip_row` and must never wrap.
    out.extend_from_slice(b"\x1b[r\x1b[4l\x1b[0m\x1b[?7l");
    // The window is already empty when it opens, but a resync re-renders over
    // whatever is on screen, so the visible screen is cleared either way.
    out.extend_from_slice(b"\x1b[2J\x1b[H");
    let body = s.screen.strip_suffix('\n').unwrap_or(s.screen);
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(clip_row(line, s.cols).as_bytes());
    }
    // The daemon's cursor is the authority, clamped as the mobile client clamps
    // it so a malformed frame cannot place the cursor outside the grid.
    out.extend_from_slice(b"\x1b[?7h");
    let x = s.cursor_x.min(s.cols.saturating_sub(1)) + 1;
    let y = s.cursor_y.min(s.rows.saturating_sub(1)) + 1;
    out.extend_from_slice(format!("\x1b[{y};{x}H").as_bytes());
    out
}

/// How long a burst of window-change signals is allowed to settle before one resize is sent.
const RESIZE_SETTLE: std::time::Duration = std::time::Duration::from_millis(120);

/// One pane row, cut to what this window can show.
///
/// The pane's grid is shared, so it can be wider than the window showing it (another device
/// resized it, or this window is a read-only viewer that must not resize it). Writing such a
/// row verbatim makes the terminal hard-wrap it: eighty-two rows became a hundred and sixty-four
/// lines, the screen scrolled, and the cursor was then placed on the wrong row. Escape sequences
/// are copied whole and do not count toward the width, so a styled row is cut, not corrupted.
fn clip_row(line: &str, cols: u64) -> String {
    let cols = cols as usize;
    let mut out = String::with_capacity(line.len());
    let mut width = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            out.push(c);
            if let Some(kind) = chars.next() {
                out.push(kind);
                if kind == '[' {
                    // CSI: parameters and intermediates, ended by a final byte 0x40..=0x7e.
                    for d in chars.by_ref() {
                        out.push(d);
                        if ('\u{40}'..='\u{7e}').contains(&d) {
                            break;
                        }
                    }
                } else if kind == ']' {
                    // OSC: ended by BEL or ST.
                    while let Some(d) = chars.next() {
                        out.push(d);
                        if d == '\u{7}' {
                            break;
                        }
                        if d == '\u{1b}' {
                            if let Some('\\') = chars.peek().copied() {
                                out.push(chars.next().unwrap());
                            }
                            break;
                        }
                    }
                }
            }
            continue;
        }
        if width >= cols {
            break;
        }
        out.push(c);
        width += 1;
    }
    out
}

/// The device identity the desk window announces: the daemon's desktop device
/// (`makeLive` claims the shell to this id before the window opens).
const DESKTOP_DEVICE: &str = "desktop";

/// What this window can take on the same socket, said once in its first `attach`
/// (the daemon keeps it for the connection).
///
/// `inplaceResync`: when the picture goes stale (output dropped for a slow link, or
/// more than the daemon will hold arriving while this window attaches to a busy
/// shell) the daemon sends a fresh `snapshot`, which [`pump`] already paints like any
/// other. Without it the daemon answers with the legacy `error`/`resync`, which costs
/// a second attach and used to cost the connection (the daemon's byte accounting for
/// that notice underflowed and panicked).
const FEATURES: [&str; 1] = ["inplaceResync"];

/// The first `attach` of a connection: the session, this window's size, and what it
/// understands.
fn first_attach(session_id: &str, size: Option<(u64, u64)>) -> serde_json::Value {
    let mut m = serde_json::json!({
        "type": "attach", "sessionId": session_id, "features": FEATURES,
    });
    // Both fields are optional in the protocol. Sending an invented size would resize the
    // pane to something the window is not, which is worse than saying nothing.
    if let Some((cols, rows)) = size {
        m["cols"] = serde_json::json!(cols);
        m["rows"] = serde_json::json!(rows);
    }
    m
}

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

/// How long the receive side may be **completely silent** before the connection is
/// declared lost.
///
/// The keepalive above is a `select!` branch, and that is its weakness: a loop that
/// is stuck in another branch never reaches it, so nothing is pinged, no pong is
/// missed, and nothing reconnects. A window was found frozen exactly that way
/// (2026-10-25): pane 46x22 (a phone's grid) in a 158x40 window, no claim, no notice,
/// zero read syscalls in twenty seconds, and only a daemon restart revived it. The
/// socket was fine on paper; nothing ever came from it.
///
/// The pongs are what this measures: one arrives every `PING_INTERVAL_MS` while the
/// daemon is there, so a quiet spell this long means the receive path is dead
/// whatever the socket claims. Derived from the interval rather than written beside
/// it, so the two cannot disagree.
fn read_stall_limit(ping_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis((ping_ms.max(1) * 7).max(15_000))
}

/// The longest a single frame write may wait for a socket that will not take it.
///
/// The same stall, on the other side: a `send` awaiting a flush that never completes
/// holds the whole loop, so neither the next keystroke nor the keepalive runs. Ten
/// seconds is longer than any local write and far shorter than a person's patience.
const SEND_TIMEOUT_MS: u64 = 10_000;

/// How long a size re-assert stands before it may be sent again.
///
/// The latch that stopped a device from answering a resizing device for ever also
/// made a *refused* re-assert permanent: a window whose shell was held by the phone
/// sent its size, the daemon refused it, and the latch stayed set, so when the phone
/// left the window kept painting the phone's grid (2026-10-25). Retried at this
/// interval, an exchange with a device that keeps resizing is one message per two
/// seconds — bounded, and nothing like the per-snapshot storm the latch prevented.
const REASSERT_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a size re-assert that was already sent may be sent again.
fn may_reassert_again(
    sent_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    sent_at.is_some_and(|t| now.saturating_duration_since(t) >= REASSERT_RETRY)
}

/// The terminal size of fd 0 (the window's pty), or `None` when it will not say.
///
/// It used to answer `80x24` when the ioctl failed, and that invented size was sent as the
/// attach and as every later resize. Because the client re-measures on each snapshot, the
/// invention repeated itself and nothing could ever correct it: the pane was told to be
/// 24x80, agreed, and the window painted 24x80 in a 2552x1404 frame until the user resized
/// it by hand. `None` means "no opinion": the pane keeps whatever size it has, and the
/// first real measurement re-asserts the window's own.
fn tty_size() -> Option<(u64, u64)> {
    // SAFETY: `winsize` is a plain C struct of four `u16`s, for which the
    // all-zero bit pattern is a valid value.
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes exactly one `winsize` through the pointer, and
    // `ws` is a live, exclusively borrowed, initialised `winsize` on this stack
    // frame. fd 0 may be closed or not a tty; that is reported as -1, not UB.
    let r = unsafe { nix::libc::ioctl(0, nix::libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col as u64, ws.ws_row as u64))
    } else {
        None
    }
}

/// The terminal size helper, exposed for tests.
pub fn tty_size_for_test() -> Option<(u64, u64)> {
    tty_size()
}

/// Whether a snapshot's grid means this window should re-assert its own size.
///
/// The pane is shared, so another device can have resized it. A window whose own
/// tty never changed gets no SIGWINCH, so seeing the disagreement in a snapshot is
/// the only notice it will ever get. Re-asserting once per mismatch (rather than on
/// every snapshot) bounds it against a device that keeps resizing. A read-only
/// read-only viewer holds nothing and must never resize.
fn should_reassert(
    cols: u64,
    rows: u64,
    snap_cols: u64,
    snap_rows: u64,
    readonly: bool,
    asserted: Option<(u64, u64)>,
) -> bool {
    // Clamped first: the daemon clamps, and a window outside the range would otherwise see a
    // mismatch on every snapshot and re-assert on the 2 s retry for ever (a tty taller than
    // 255 rows, or narrower than 20 columns, is reachable).
    let (cols, rows) = ldmux_protocol::clamp_pane_size(cols, rows);
    if readonly || (cols, rows) == (snap_cols, snap_rows) {
        return false;
    }
    match asserted {
        Some((c, r)) => (c, r) != (cols, rows),
        None => true,
    }
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
                 in a terminal, or `ldmux new` to create a shell without attaching)"
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
/// nothing (the read-only viewer path).
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
    // **One transport.** The daemon's local attach socket (`<control>-attach.sock`): the
    // same WebSocket protocol over a Unix socket, 0600 and peer-uid checked, so a desk
    // window needs neither the tailnet nor the token. There is no second way in.
    let local = ldmux_protocol::paths::attach_socket_path_from(daemon_socket);
    let stream = tokio::net::UnixStream::connect(&local)
        .await
        .map_err(|e| format!("attach socket {local}: {e}"))?;
    let (ws, _) = tokio_tungstenite::client_async("ws://ldmux.local/ws", stream)
        .await
        .map_err(|e| format!("attach socket {local}: {e}"))?;
    pump(ws, session_id, readonly, input, output, ping_ms).await
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
    // The daemon clamps to the same range, so the window's own idea of its grid has to be
    // the clamped one: comparing a raw 300-row tty against a 255-row pane is a disagreement
    // that no resize can ever settle, and the re-assert below would retry it for ever.
    // **The signal handler comes first.** A window that opens under a window manager is placed
    // (and therefore resized) while this process is still starting, and a SIGWINCH delivered
    // before the handler exists is simply ignored — the default disposition for that signal is
    // to do nothing. Registering it before the size is read means a placement resize is queued
    // for the loop below rather than lost.
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            .map_err(|e| e.to_string())?;
    // Unknown until the window says, or until the pane tells us in its first snapshot.
    // `0, 0` is the sentinel for "no opinion", not a size.
    let (mut cols, mut rows) = {
        // One read: calling `tty_size` twice can straddle a resize between the two fields.
        match tty_size() {
            Some((c, r)) => ldmux_protocol::clamp_pane_size(c, r),
            None => (0, 0),
        }
    };
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

    // Identify (desktop) and attach. A read-only viewer sends no `hello`, so it
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
    let announced = if cols == 0 || rows == 0 { None } else { Some((cols, rows)) };
    send(&mut sink, &first_attach(session_id, announced)).await?;

    let mut inbuf = [0u8; 4096];
    // A multi-byte character can straddle two reads (a large paste): decoding
    // each read alone turned both halves into U+FFFD in the user's shell.
    let mut in_utf8 = ldmux_protocol::utf8::Utf8Carry::new();
    let mut detached = false;
    // The size this window last re-asserted after a snapshot disagreed with it.
    // `None` means we are in step. Bounded so a device that keeps resizing the
    // shared pane cannot make this an endless exchange.
    let mut asserted: Option<(u64, u64)> = None;
    let mut asserted_at: Option<std::time::Instant> = None;

    let stall = read_stall_limit(ping_ms);
    loop {
        tokio::select! {
            // **Bounded silence, not just a closed socket.** A receive path that
            // stops delivering never looks broken to the OS (see
            // `read_stall_limit`), so the window has to give up on it and reconnect.
            msg = tokio::time::timeout(stall, stream.next()) => {
                match msg {
                    Err(_) => return Ok(Outcome::Lost),
                    Ok(None) | Ok(Some(Err(_))) => return Ok(Outcome::Lost),
                    Ok(Some(Ok(Message::Text(t)))) => {
                        let v: serde_json::Value = match serde_json::from_str(t.as_ref()) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        match v["type"].as_str() {
                            Some("snapshot") => {
                                detached = false;
                                let snap = Snapshot::from_json(&v);
                                // **Measure the window now rather than trusting the size cached
                                // at startup.** That size is a reading taken before the window
                                // manager had necessarily finished placing the window, and it is
                                // also the fallback `80x24` when `TIOCGWINSZ` fails on a pty whose
                                // size has not been set yet. Either way the pane was resized to
                                // the wrong grid by the attach that carried it, so the snapshot
                                // *agrees* with the stale value and no later mismatch reveals it.
                                // The reported symptom was exactly that: a window opened at the
                                // wrong size that only a manual resize cured, because that resize
                                // is the first SIGWINCH the loop ever sees. One ioctl per snapshot
                                // is cheaper than a repaint and cannot disagree with the window.
                                match tty_size() {
                                    Some((c, r)) => {
                                        let (wc, wr) = ldmux_protocol::clamp_pane_size(c, r);
                                        if (wc, wr) != (cols, rows) {
                                            cols = wc;
                                            rows = wr;
                                        }
                                    }
                                    // The window has no opinion, so adopt the pane's own
                                    // grid: that is a real size, and comparing against it
                                    // later cannot manufacture a resize.
                                    None if cols == 0 || rows == 0 => {
                                        cols = snap.cols;
                                        rows = snap.rows;
                                    }
                                    None => {}
                                }
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
                                // A refusenik latch: a re-assert the daemon refused
                                // (the shell was somebody else's) must be retried once
                                // the wait is over, or the window stays at their grid.
                                if !mismatch || may_reassert_again(asserted_at, std::time::Instant::now()) {
                                    asserted = None;
                                }
                                if should_reassert(cols, rows, snap.cols, snap.rows, readonly, asserted)
                                {
                                    asserted = Some((cols, rows));
                                    asserted_at = Some(std::time::Instant::now());
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
                                } else if code == "input_failed" {
                                    // The pane did not take the bytes: the shell died under the
                                    // keystroke, or its pty is gone. The window accepted them and
                                    // nothing else says so, so a key that went nowhere had no
                                    // trace at all.
                                    let _ = output
                                        .write_all(b"\r\n  This shell did not take that input.\r\n")
                                        .await;
                                    let _ = output.flush().await;
                                } else if code == "not_holder" {
                                    let _ = output
                                        .write_all(
                                            b"\r\n  Only the device holding this shell can close it.\r\n",
                                        )
                                        .await;
                                    let _ = output.flush().await;
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
                    Ok(Some(Ok(Message::Binary(b)))) => {
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
                // **A drag emits tens of these.** Every one used to become a PTY resize, an
                // emulator reflow and a full screen snapshot back, so dragging a window edge
                // was a stream of repaints of widths nobody ever saw. Waiting out the gesture
                // costs the resize one beat and sends the size the drag ended on.
                tokio::time::sleep(RESIZE_SETTLE).await;
                // One read, for the same reason the startup read is one: two calls can
                // straddle a resize and produce a width from one window and a height from the
                // next, which is a grid neither window ever had.
                let measured = tty_size().map(|(c, r)| ldmux_protocol::clamp_pane_size(c, r));
                if let Some((c, r)) = measured
                    && (c, r) != (cols, rows)
                {
                    cols = c;
                    rows = r;
                    // A read-only viewer holds no claim and must not resize the shared
                    // pane: only the device that owns the shell decides its geometry.
                    if !readonly {
                        let _ = send(
                            &mut sink,
                            &serde_json::json!({ "type": "resize", "cols": cols, "rows": rows }),
                        )
                        .await;
                    }
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
    // Bounded: a write the socket will not take must end the connection, not hold
    // the loop (see `SEND_TIMEOUT_MS`).
    match tokio::time::timeout(
        std::time::Duration::from_millis(SEND_TIMEOUT_MS),
        sink.send(Message::Text(value.to_string().into())),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("send timed out".to_string()),
    }
}
#[cfg(test)]
mod stall_tests {
    use super::{may_reassert_again, read_stall_limit};
    use std::time::{Duration, Instant};

    /// A window whose receive path went silent must give up on it.
    ///
    /// The keepalive is a `select!` branch, so a loop stuck elsewhere never reaches
    /// it: measured on a frozen window 2026-10-25 with zero read syscalls in twenty
    /// seconds and no reconnection, because nothing was missed — nothing was sent.
    #[test]
    fn a_stalled_receive_path_is_given_up_on() {
        assert!(read_stall_limit(4_000) >= Duration::from_secs(15));
        assert_eq!(read_stall_limit(4_000), Duration::from_secs(28));
        // Never shorter than a person's patience, whatever a caller passes.
        assert_eq!(read_stall_limit(1), Duration::from_secs(15));
        // And it scales with the ping interval rather than being written beside it.
        assert_eq!(read_stall_limit(60_000), Duration::from_secs(420));
    }

    /// A refused size re-assert is retried; a fresh one is not sent twice at once.
    #[test]
    fn a_refused_size_reassert_is_retried_after_the_wait() {
        let now = Instant::now();
        // Never sent: nothing to retry.
        assert!(!may_reassert_again(None, now));
        // Just sent: do not send again (the storm the latch prevented).
        assert!(!may_reassert_again(Some(now), now));
        // Sent a moment ago, still inside the wait.
        assert!(!may_reassert_again(
            Some(now - Duration::from_millis(500)),
            now
        ));
        // The wait is over: the pane is still the other device's, so ask again.
        assert!(may_reassert_again(Some(now - Duration::from_secs(3)), now));
    }
}

#[cfg(test)]
mod render_tests {
    use super::{Snapshot, clip_row, first_attach, render_snapshot, should_reassert};

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
    /// The window says what it can take, in the first attach (the daemon reads the
    /// features off an `attach` as well as a `hello`, so a read-only viewer that
    /// sends no `hello` is covered too).
    #[test]
    fn the_first_attach_advertises_in_place_resync() {
        let m = first_attach("abc", Some((158, 82)));
        assert_eq!(m["type"], "attach");
        assert_eq!(m["sessionId"], "abc");
        assert_eq!(m["cols"], 158);
        assert_eq!(m["rows"], 82);
        assert_eq!(m["features"], serde_json::json!(["inplaceResync"]));
    }

    #[test]
    fn a_snapshot_mismatch_re_asserts_the_size_once() {
        // In step: nothing to do.
        assert!(!should_reassert(158, 82, 158, 82, false, None));
        // The pane was resized by another device: take it back.
        assert!(should_reassert(158, 82, 48, 17, false, None));
        // Already re-asserted this size and the answer has not caught up: wait,
        // do not send one resize per snapshot.
        assert!(!should_reassert(158, 82, 48, 17, false, Some((158, 82))));
        // A read-only viewer holds nothing and must never resize.
        assert!(!should_reassert(158, 82, 48, 17, true, None));
        // Once in step again, the next mismatch is a new one to answer.
        assert!(should_reassert(158, 82, 48, 17, false, Some((100, 40))));
    }

    /// A window outside the daemon's range must not re-assert for ever: the pane can never
    /// report a grid this window will agree with, so the clamp is what makes it settle.
    #[test]
    fn an_out_of_range_window_does_not_loop_on_its_own_size() {
        // 3000 columns and 300 rows are clamped by the daemon to 2000 x 255.
        assert!(!should_reassert(3000, 300, 2000, 255, false, None));
        // A window inside the range is still taken back.
        assert!(should_reassert(300, 100, 48, 17, false, None));
        // And a wide window at a small font is inside the range, so it is taken back too:
        // this is the size that used to be clamped to 500 and stop filling the window.
        assert!(should_reassert(600, 60, 480, 60, false, None));
    }

    #[test]
    fn a_wide_pane_row_is_cut_to_the_window_not_wrapped() {
        assert_eq!(clip_row("abcdefghij", 4), "abcd");
        assert_eq!(clip_row("ab", 4), "ab", "short rows are untouched");
        // Escape sequences are copied whole and do not count toward the width.
        assert_eq!(clip_row("\u{1b}[31mabcdef\u{1b}[0m", 3), "\u{1b}[31mabc");
        // An OSC 52 clipboard write is not cut in the middle.
        assert_eq!(clip_row("\u{1b}]52;c;AAAA\u{7}xy", 1), "\u{1b}]52;c;AAAA\u{7}x");
    }

    /// A snapshot whose grid is wider than this window must not reflow the window.
    #[test]
    fn a_snapshot_wider_than_the_window_is_clipped_per_row() {
        let mut s = snap("0123456789\nab\n");
        s.cols = 3;
        s.rows = 2;
        let out = String::from_utf8(render_snapshot(&s)).unwrap();
        assert!(out.contains("012\r\nab"), "{out:?}");
        assert!(!out.contains("345"), "the row was not clipped: {out:?}");
    }

    /// A paint must not inherit the program's scroll region, insert mode or wrap setting.
    #[test]
    fn a_snapshot_neutralises_what_the_program_left_behind() {
        let out = String::from_utf8(render_snapshot(&snap("aa\n"))).unwrap();
        let reset = out.find("\u{1b}[r\u{1b}[4l\u{1b}[0m\u{1b}[?7l").expect("modes are reset before the clear");
        let clear = out.find("\u{1b}[2J").expect("the screen is cleared");
        let wrap_on = out.find("\u{1b}[?7h").expect("wrap comes back");
        assert!(reset < clear, "reset before the clear: {out:?}");
        assert!(clear < wrap_on, "wrap returns after the last row: {out:?}");
        assert!(out[clear..wrap_on].contains("aa"), "the rows are inside the wrap-off span");
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
            // The rows are what sits between the clear and the wrap setting coming
            // back, which is also where the cursor escape begins.
            let after = s.split("\u{1b}[2J\u{1b}[H").nth(1).unwrap();
            after.split("\u{1b}[?7h").next().unwrap().to_string()
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
