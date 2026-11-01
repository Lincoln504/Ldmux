//! The per-session emulator (plan §3.4): one ghostty-vt `Terminal` per
//! session, fed by the always-reads-the-master stream, and the `RenderState`
//! that snapshots render from.
//!
//! **Thread model.** The plan fixes `Terminal`/`RenderState` at `!Send`, one
//! per session, owned by one task. The owner here is a **blocking thread**:
//! it *creates* both (they cannot be moved in — a `spawn_blocking` closure
//! must be `Send`, and the values are not), then runs the loop: read the
//! master (the §3.3 rule — the kernel pty buffer must never fill), feed the
//! bytes to the emulator **and** the session's `out` buffer (the attach's
//! stream), and service the [`EmuMsg`]s the request handlers hand over.
//!
//! Multiplexing is a `poll(2)` on the master fd **and an eventfd**: a queued
//! message writes to the eventfd, which breaks the poll immediately, so a
//! snapshot/resize is never queued behind a silent pane. Until 2026-10-05 the
//! poll carried a 1 ms timeout instead, and that is what an idle session cost —
//! measured on the deployed keeper, ~1% of a core per session, 1000 wakeups a
//! second each (6.1% with five sessions), for no work at all. With the eventfd
//! the poll waits forever while the pane is silent and a message is serviced
//! *sooner* than the 1 ms poll managed. (There is no tokio async-fd wrapper to
//! drive a `select!` with — this is the keeper-side equivalent.)
//!
//! Replies ride a `tokio::sync::oneshot` per request: the `Sender` is `Send`,
//! so the blocking thread can answer, and the handler `await`s the `Receiver`
//! (a `!Send` value that may not cross into the thread).
//!
//! The thread exits when its message sender drops (the `Session` is removed —
//! a kill) or the master fd errors. On master EOF (the shell died) it stops
//! reading and services messages only — a snapshot of a dead pane is still
//! valid (the grid keeps its last content).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Relock, now_ms};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, mpsc};

use ldmux_vt_sys::{
    Cell, MODE_APP_CURSOR, MODE_BRACKETED_PASTE, MODE_SGR_MOUSE, RenderState, ScrollViewport,
    Terminal,
};
use nix::libc;
use tokio::sync::oneshot;

use ldmux_vt_sys::{links, snapshot};

/// The grid history a session retains: the `history-limit 10000` analogue the
/// plan calls for (plan §3.7/§3.4), and the ceiling a `snapshot --history`
/// request is clamped to.
///
/// **It has to be set.** A `Terminal` created without it keeps only the C API's
/// own default, which is a small fraction of the plan's history (the byte budget
/// binds first — measured ~1 MB, ~995 rows of 12,000, see
/// [`MAX_SCROLLBACK_BYTES`]), so a session's scrollback was a fraction of what
/// the plan calls for: `ldmux capture --history` returned almost the live screen,
/// the `scrollback` message the phone pages history with had little to page, and
/// `historySize` stayed far below what tmux kept (measured 2026-10-05). tmux had a
/// history limit, so this was the port's to lose.
pub(crate) const MAX_SCROLLBACK_LINES: usize = 10_000;

/// The byte budget that goes with [`MAX_SCROLLBACK_LINES`].
///
/// The line cap alone retains nothing more than the C API's own default, because
/// its byte budget binds first (measured: ~1 MB, ~995 rows of 12,000 lines, and
/// 913 rows of 1,500 even with the line cap raised). The TypeScript's equivalent
/// history was "10,000 lines, ~40 MB/pane" (`docs/SPEC.md`, tmux `history-limit`),
/// so this is the same history at the same order of cost — and it only fills up
/// on a session that has actually printed that much.
pub(crate) const MAX_SCROLLBACK_BYTES: usize = 40 * 1024 * 1024;

/// The session's emulator, with its history cap applied.
///
/// One function so the cap cannot be forgotten by a second caller, and so it is
/// testable without a pty: an emulator that does not retain history is a
/// scrollback that does not exist.
fn new_terminal(cols: u16, rows: u16) -> Result<Terminal, ldmux_vt_sys::FfiError> {
    let term = Terminal::new(cols, rows)?;
    let (lines, bytes) = scrollback_limits();
    term.set_max_scrollback(lines);
    term.set_max_scrollback_bytes(bytes);
    Ok(term)
}

/// The scrollback caps, from the environment when the user set one.
///
/// A constant is the wrong shape for this one: it stands in for tmux's
/// `history-limit`, which every multiplexer makes configurable, and a user who
/// deliberately runs a deeper (or shallower) history is configuring their shell
/// session, not the multiplexer that happens to host it. The keeper reads these
/// once per terminal, so a change in `~/.config/ldmux/env` reaches new sessions.
///
/// Bad or absent values fall back to the defaults, never to zero: a typo must not
/// silently disable scrollback.
pub(crate) fn scrollback_limits() -> (usize, usize) {
    let read = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    };
    (
        read("LDMUX_SCROLLBACK_LINES", MAX_SCROLLBACK_LINES),
        read("LDMUX_SCROLLBACK_BYTES", MAX_SCROLLBACK_BYTES),
    )
}

/// One unit of work the request handlers hand the emulator thread.
pub(crate) enum EmuMsg {
    /// Render the screen: the `rows` live lines, plus `history_lines` of
    /// history above them when requested (the `capture-pane -S -N -E -`
    /// contract). `0` = live screen only.
    Snapshot {
        history_lines: u64,
        drain_output: bool,
        reply: oneshot::Sender<Outcome>,
    },
    /// The pane's size changed. The handler has already done `TIOCSWINSZ`;
    /// the emulator must resize its grid.
    Resize { cols: u16, rows: u16 },
    /// Read the pane's state (the display-message fields), no render — the
    /// `State`/`List` liveness report. The `cols`/`rows`/`dead` and the
    /// `/proc` fields are added by the keeper (which owns the pid).
    StateQuery {
        reply: oneshot::Sender<StateOutcome>,
    },
}

/// The pane's state, read by a `StateQuery` (no render). The keeper adds
/// `cols`/`rows`/`dead` (from the `Session`) and the `/proc` fields
/// (`foreground`, `cwd`; empty for a dead pane).
#[derive(Debug, Clone)]
pub struct StateOutcome {
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub alternate_on: bool,
    pub mouse: bool,
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    /// The Kitty keyboard protocol flags the program has set, as a bitset (see
    /// [`ldmux_vt_sys::Terminal::kitty_keyboard_flags`]). The daemon encodes a
    /// modified Enter from this: legacy bytes cannot carry the modifier.
    pub kitty_keyboard: u8,
    pub history_size: u32,
}

/// The terminal's write-back: the answers to the queries a program asks it.
///
/// Registered on each session's emulator (`set_write_pty`), with the pty master
/// as the userdata. This is the one answerer: the daemon's sanitizer drops these
/// queries before any client sees them (a client's reply would arrive as typed
/// input), and no client could answer for the grid agreeably anyway.
///
/// The header forbids calling back into the terminal from a callback. Writing to
/// a pty master is a syscall, so this is that and nothing else.
///
/// SAFETY: called synchronously from `Terminal::write`; `userdata` is the pty
/// master fd packed in a pointer, and `data`/`len` describe a buffer valid for
/// the call.
unsafe extern "C" fn write_pty_cb(
    _terminal: ldmux_vt_sys::ffi::GhosttyTerminal,
    userdata: *mut std::os::raw::c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() || data.is_null() || len == 0 {
        return;
    }
    let fd = userdata as isize as libc::c_int;
    // **Never block here.** The master is blocking (opened by `openpty`, never set
    // `O_NONBLOCK`), and this callback runs on the thread that also drains the
    // master. A program that asks a query and does not read its input — a `cat`
    // fed binary, a child busy on its own work — fills the pty input queue, and a
    // blocking write would wedge that thread: the child would wait on our write,
    // we would wait on the child's read, and the session would stop draining
    // output. It is reachable only since the keeper started answering queries at
    // all, and it reads exactly like a hung shell.
    //
    // `poll` with a zero timeout answers "can this be written now" without
    // touching the fd, and a dropped reply costs nothing: it is the answer to a
    // question about a terminal, and the program will ask again (or has already
    // given up on this one).
    let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    // SAFETY: one `pollfd` of ours, nfds 1, zero timeout. `fd` is the master this
    // thread owns and `data`/`len` a live buffer.
    let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
    if ready <= 0 || pfd.revents & libc::POLLOUT == 0 {
        return;
    }
    // SAFETY: as above; a partial write is not retried, for the same reason — a
    // reply this small that does not fit means the pane is gone, and the next read
    // reports it.
    unsafe { libc::write(fd, data as *const libc::c_void, len) };
}

/// Drop the *reply* form of the Kitty keyboard query from the output stream.
///
/// The write-back writes a reply into the pty master, i.e. to the program's input.
/// A program that echoes what it reads (`cat`, a pager, a shell with echo on) turns
/// it back into output, the emulator parses it, and — because the parameterised
/// form is also a query-shaped sequence — answers it again. That loop never stops:
/// measured 2026-10-25 at about sixty replies a second through a pane running `cat`,
/// growing the pane and the wire at once.
///
/// The **question** is the bare `CSI ? u`; the **answer** is `CSI ? <params> u`, and
/// a reply is never a question — so the parameterised form is dropped before the
/// emulator sees it, and the loop cannot start. Programs push the protocol with
/// `CSI > <params> u`, which is untouched.
fn without_kitty_replies(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const PREFIX: &[u8] = b"\x1b[?";
    fn is_param(b: u8) -> bool {
        b.is_ascii_digit() || b == b';'
    }
    if !bytes.windows(PREFIX.len()).any(|w| w == PREFIX) {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.len() > PREFIX.len() + 1
            && rest.starts_with(PREFIX)
            && rest[PREFIX.len()].is_ascii_digit()
        {
            // Walk the parameters; a `u` right after them ends the sequence.
            let mut j = PREFIX.len();
            while j < rest.len() && is_param(rest[j]) {
                j += 1;
            }
            if j < rest.len() && rest[j] == b'u' {
                i += j + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

/// The result of a render: the grid's geometry + the rendered text (one SGR
/// line per row, `\n`-joined — `snapshot::render`'s output). When
/// `history_lines > 0`, `text` is that many history lines above the `rows`
/// live lines (the count is what was actually captured, clamped to the
/// scrollback).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub cols: u16,
    pub rows: u16,
    /// The number of history lines actually captured (0 = live only).
    pub history_lines: u64,
    /// Every line the pane holds, history and live: `total_rows - rows` is the absolute line
    /// number of the row at the top of the screen.
    pub total_rows: u64,
    pub text: String,
    // The pane's state, read from the `Terminal` the thread owns (plan §3.4
    // — the `display-message` fields, direct from ghostty-vt). The `/proc`
    // fields (`foreground`, `cwd`) and `dead` are added by the keeper, which
    // owns the pid.
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub alternate_on: bool,
    pub mouse: bool,
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub history_size: u32,
    /// The buffered, not-yet-peeked output taken at the instant of the render
    /// (only when asked): exactly the bytes already reflected in `text`.
    pub tail: Vec<u8>,
}

/// The wake side of the emulator's poll (an `eventfd`), or `None` if the kernel
/// would not give us one — in which case the thread falls back to the 1 ms poll
/// this replaced. `EFD_CLOEXEC` so no shell inherits it, `EFD_NONBLOCK` so the
/// wake can never block the sender.
pub(crate) fn new_waker() -> Option<Arc<OwnedFd>> {
    use nix::libc;
    // SAFETY: `eventfd` takes only integers; it returns a new descriptor or -1.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is >= 0 (checked above), was just created by `eventfd` and
    // is referenced nowhere else, so this `OwnedFd` is its sole owner and closes
    // it exactly once.
    Some(Arc::new(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) }))
}

/// Break the emulator's `poll` so the message just queued is serviced now.
/// Failure is ignored: a full eventfd counter still reads as readable, and a
/// closed one means the thread is gone.
pub(crate) fn wake(waker: &Option<Arc<OwnedFd>>) {
    use nix::libc;
    if let Some(fd) = waker {
        let one: u64 = 1;
        // SAFETY: the descriptor is kept open by the `Arc<OwnedFd>` borrowed for
        // this call, and the buffer is the 8 initialised bytes of the local `one`
        // (an eventfd write is exactly a `u64`); `write` does not retain it.
        unsafe {
            libc::write(fd.as_raw_fd(), &one as *const u64 as *const libc::c_void, 8);
        }
    }
}

/// Read the eventfd's counter back to zero.
fn drain_waker(fd: std::os::fd::RawFd) {
    use nix::libc;
    let mut buf = [0u8; 8];
    // SAFETY: `buf` is 8 writable bytes, the size an eventfd read requires and
    // writes. `fd` is the waker's descriptor, kept open by the `Arc<OwnedFd>`
    // the emulator thread holds for as long as this function can be called.
    unsafe {
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 8);
    }
}

/// What the emulator thread is wired to: the pty master it always reads, the
/// wake-up that breaks its poll, the session's output buffer, and its activity
/// clock. Named so the spawn call reads as wiring rather than as seven values in
/// a row.
pub(crate) struct Wiring {
    pub master: Arc<OwnedFd>,
    pub waker: Option<Arc<OwnedFd>>,
    pub out: Arc<Mutex<crate::OutBuf>>,
    pub activity: Arc<AtomicU64>,
}

/// Start the session's emulator thread.
///
/// `boot` gets `true` once the `Terminal`/`RenderState` are created. On a
/// creation failure the thread sends nothing (it drops the sender) and exits;
/// the caller must `await` the boot reply **before** the session is visible,
/// and clean up (group-kill the child) on a missing reply.
pub(crate) fn spawn(
    cols: u16,
    rows: u16,
    wiring: Wiring,
    rx: mpsc::Receiver<EmuMsg>,
    boot: oneshot::Sender<bool>,
) {
    let Wiring {
        master,
        waker,
        out,
        activity,
    } = wiring;
    // A dedicated OS thread: the loop blocks in `poll` for the life of the
    // session, which would pin a slot of tokio's bounded blocking pool (shared
    // with the pane writes) for as long as the pane lives. If the thread cannot
    // be created the closure is dropped with `boot`, and the caller sees a
    // missing boot reply.
    let _ = std::thread::Builder::new()
        .name("ldmux-emulator".into())
        .spawn(move || {
            // Created here: the values are `!Send`, so they cannot be moved into
            // this closure from the caller.
            let mut term = match new_terminal(cols, rows) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("ldmux-keeper: emulator: {e}");
                    let _ = boot.send(false);
                    return;
                }
            };
            let mut rs = match RenderState::new() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("ldmux-keeper: emulator: {e}");
                    let _ = boot.send(false);
                    return;
                }
            };
            let _ = boot.send(true);

            let fd = master.as_raw_fd();
            let wake_fd = waker.as_ref().map(|w| w.as_raw_fd()).unwrap_or(-1);
            let mut buf = [0u8; 4096];
            // The emulator answers the queries a program asks (DA1, DSR/CPR,
            // DECRQM, the Kitty keyboard query, mode reports) by writing back to
            // the pty. `fd` is the master this thread owns, packed as the
            // userdata; the terminal never outlives this thread.
            // SAFETY: see `write_pty_cb`; the fd is valid for as long as the
            // emulator is used and the callback does nothing but write to it.
            unsafe {
                term.set_write_pty(write_pty_cb, fd as isize as *mut std::os::raw::c_void);
            }
            let mut reading = true;
            loop {
                // 1. A waiting message is serviced before any I/O.
                match rx.try_recv() {
                    Ok(msg) => {
                        service(msg, &mut term, &mut rs, &out);
                        continue;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => break,
                }
                // 2. Otherwise wait for the master and for the wake-up. With an
                //    eventfd the wait is unbounded — an idle session costs nothing
                //    and a queued message breaks it at once. Without one (the
                //    fallback), 1 ms, so 1. is re-checked and a message is never
                //    queued behind a silent pane.
                let mut pfds = [
                    libc::pollfd {
                        fd: wake_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // A dead pane is not polled: its master returns `POLLHUP` for ever.
                let nfds: libc::nfds_t = if reading { 2 } else { 1 };
                let timeout = if wake_fd >= 0 { -1 } else { 1 };
                // SAFETY: `pfds` is an initialised array of two `pollfd`s and
                // `nfds` is at most its length (1 or 2); it outlives the call.
                // `fd` is the master this thread owns, `wake_fd` the waker held
                // by this thread (a negative `wake_fd` is ignored by poll).
                let n = unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout) };
                if n < 0 {
                    // A signal landed in the poll: look again. Anything else (the
                    // fd closed out from under us) means the session is gone; the
                    // sender's drop ends the loop.
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if n == 0 {
                    continue; // the fallback timeout
                }
                if wake_fd >= 0 && pfds[0].revents & libc::POLLIN != 0 {
                    // A message is waiting: drain the counter and service it at
                    // the top of the loop, before any master I/O.
                    drain_waker(wake_fd);
                    continue;
                }
                if !reading || pfds[1].revents == 0 {
                    continue;
                }
                // SAFETY: `buf` is a live, exclusively borrowed 4096-byte array and
                // `buf.len()` is the length passed; `fd` is the master this thread
                // owns (see the poll above).
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) {
                        // Not the end of the stream: try again.
                        continue;
                    }
                    if e.raw_os_error() != Some(libc::EIO) {
                        // Unexpected, and retrying a persistent error would spin.
                        eprintln!("ldmux-keeper: emulator: read: {e}");
                    }
                }
                if n <= 0 {
                    // EOF, or EIO (what a pty master reads once the slave side is
                    // gone): the shell's side is closed. Stop reading; keep
                    // servicing messages (a snapshot of a dead pane is still valid
                    // — the grid keeps its last content).
                    reading = false;
                    continue;
                }
                let bytes = &buf[..n as usize];
                // A reply the terminal wrote can come back as output when a program
                // echoes its input (see `without_kitty_replies`): dropping it here is
                // what stops a reply from being answered for ever.
                let bytes = without_kitty_replies(bytes);
                // The terminal answers its own queries from inside this call.
                term.write(&bytes);
                out.relock().push(&bytes);
                // The pane said something: this is the activity clock
                // (`lastActivityAt`). Stamped here — the one place every byte
                // from the master passes — and not where the buffer is drained,
                // because a `Peek` takes the bytes away and says nothing about
                // whether they were new.
                activity.store(now_ms(), Ordering::Relaxed);
            }
        });
}

/// Service one message (called on the emulator thread).
fn service(msg: EmuMsg, term: &mut Terminal, rs: &mut RenderState, out: &Mutex<crate::OutBuf>) {
    match msg {
        EmuMsg::Snapshot {
            history_lines,
            drain_output,
            reply,
        } => {
            // Clamp to the available scrollback (a capture cannot exceed what
            // the grid retained).
            let n = (history_lines as usize).min(term.scrollback_rows());
            // 1. The live screen, at the bottom (the `rows` live lines).
            term.scroll_viewport(ScrollViewport::Bottom);
            let _ = rs.update(term);
            let rows = rs.rows();
            let live: Vec<Vec<Cell>> = (0..rows).map(|y| rs.row(y)).collect();
            // The OSC 8 target of each linked cell, row for row alongside `live`/`hist`
            // (an empty entry for the common row with no link).
            let live_links: Vec<Vec<Option<String>>> = (0..rows)
                .map(|y| links::row_links(term, rs.cols(), y))
                .collect();
            // 2. The last `n` history lines, **a screenful at a time**. The
            //    render state holds one screen, so reading row indices past it
            //    returned blanks: a 60-line history capture answered with the 24
            //    lines above the live screen and 36 empty ones (measured
            //    2026-10-05). The TypeScript's `capture-pane -S -N -E -` returned
            //    the lines themselves, so this walks the window instead of
            //    indexing into a screen it does not have.
            let (hist, hist_links): (Vec<Vec<Cell>>, Vec<Vec<Option<String>>>) = if n > 0 {
                let rows = rs.rows() as usize;
                // Absolute top, then down to the first wanted history row: that
                // row is `scrollback_rows - n` above the live screen.
                term.scroll_viewport(ScrollViewport::Top);
                let _ = rs.update(term);
                let skip = term.scrollback_rows().saturating_sub(n);
                if skip > 0 {
                    term.scroll_viewport(ScrollViewport::Delta(skip as i32));
                    let _ = rs.update(term);
                }
                let mut h: Vec<Vec<Cell>> = Vec::with_capacity(n);
                let mut hl: Vec<Vec<Option<String>>> = Vec::with_capacity(n);
                while h.len() < n {
                    let take = (n - h.len()).min(rows);
                    for y in 0..take {
                        h.push(rs.row(y as u16));
                        hl.push(links::row_links(term, rs.cols(), y as u16));
                    }
                    if h.len() >= n {
                        break;
                    }
                    // The next screenful down (a zero-row screen cannot happen:
                    // the emulator is created with the pane's rows).
                    term.scroll_viewport(ScrollViewport::Delta(rows.max(1) as i32));
                    let _ = rs.update(term);
                }
                term.scroll_viewport(ScrollViewport::Bottom);
                let _ = rs.update(term);
                (h, hl)
            } else {
                (Vec::new(), Vec::new())
            };
            // 3. Render: the history lines first, then the live screen (the
            //    order the client replays into its own scrollback).
            let all: Vec<&[Cell]> = hist
                .iter()
                .chain(live.iter())
                .map(|r| r.as_slice())
                .collect();
            let all_links: Vec<Vec<Option<String>>> =
                hist_links.into_iter().chain(live_links).collect();
            let text = snapshot::render_with_links(&all, &all_links, rs.cols() as usize);
            // Taken here, on the thread that feeds both the emulator and `out`,
            // so the tail is exactly the bytes the render already contains.
            let tail = if drain_output {
                out.relock().take().0
            } else {
                Vec::new()
            };
            // The state reflects the live screen (we're back at `Bottom`).
            let _ = reply.send(Outcome {
                cols: rs.cols(),
                rows: rs.rows(),
                history_lines: n as u64,
                // Live rows plus retained history: the keeper's own line numbering, which is what
                // lets a client name a line instead of counting the rows it happens to hold.
                total_rows: term.total_rows() as u64,
                text,
                cursor_x: term.cursor_x(),
                cursor_y: term.cursor_y(),
                alternate_on: term.alternate_on(),
                mouse: term.mouse_tracking(),
                mouse_sgr: term.mode(MODE_SGR_MOUSE),
                app_cursor: term.mode(MODE_APP_CURSOR),
                bracketed_paste: term.mode(MODE_BRACKETED_PASTE),
                history_size: term.scrollback_rows() as u32,
                tail,
            });
        }
        EmuMsg::StateQuery { reply } => {
            let _ = reply.send(StateOutcome {
                cursor_x: term.cursor_x(),
                cursor_y: term.cursor_y(),
                alternate_on: term.alternate_on(),
                mouse: term.mouse_tracking(),
                mouse_sgr: term.mode(MODE_SGR_MOUSE),
                app_cursor: term.mode(MODE_APP_CURSOR),
                bracketed_paste: term.mode(MODE_BRACKETED_PASTE),
                kitty_keyboard: term.kitty_keyboard_flags(),
                history_size: term.scrollback_rows() as u32,
            });
        }
        EmuMsg::Resize { cols, rows } => {
            term.resize(cols, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constructor the session uses retains history, and the cap it sets is
    /// what makes that true: a `Terminal` made without it keeps only what the
    /// plan calls the C API's own default, which is a viewport's worth — the
    /// state every session was in until this was set.
    #[test]
    fn the_sessions_emulator_retains_history() {
        // Past the C API's own default (a thousand rows, measured), so the cap is
        // what the assertion is against.
        const LINES: usize = 1500;
        let mut t = new_terminal(80, 10).expect("terminal");
        for i in 0..LINES {
            t.write(format!("L{i:04}\r\n").as_bytes());
        }
        let retained = t.scrollback_rows();
        assert!(
            retained >= LINES - 10,
            "the session's emulator must retain history: {retained} rows of {LINES}"
        );

        // And the default, which is why the cap is not optional. If libghostty
        // raises its own default past the cap, this is what says so.
        let mut bare = Terminal::new(80, 10).expect("terminal");
        for i in 0..LINES {
            bare.write(format!("L{i:04}\r\n").as_bytes());
        }
        assert!(
            bare.scrollback_rows() < retained,
            "the cap is what buys the difference: {} against {retained}",
            bare.scrollback_rows()
        );
    }
}
