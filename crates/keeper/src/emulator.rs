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
//! Multiplexing is a `poll(2)` on the master fd with a 1 ms timeout: a
//! snapshot/resize is never queued behind a silent pane, and the read side is
//! checked at most a millisecond after any master data arrives, so the kernel
//! buffer still never fills. (There is no tokio async-fd wrapper to drive a
//! `select!` with — the 1 ms poll is the keeper-side equivalent.)
//!
//! Replies ride a `tokio::sync::oneshot` per request: the `Sender` is `Send`,
//! so the blocking thread can answer, and the handler `await`s the `Receiver`
//! (a `!Send` value that may not cross into the thread).
//!
//! The thread exits when its message sender drops (the `Session` is removed —
//! a kill) or the master fd errors. On master EOF (the shell died) it stops
//! reading and services messages only — a snapshot of a dead pane is still
//! valid (the grid keeps its last content).

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{mpsc, Arc, Mutex};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use ldmux_vt_sys::{
    Cell, RenderState, ScrollViewport, Terminal, MODE_APP_CURSOR, MODE_BRACKETED_PASTE,
    MODE_SGR_MOUSE,
};
use nix::libc as libc;
use tokio::sync::oneshot;

use ldmux_vt_sys::snapshot;

/// One unit of work the request handlers hand the emulator thread.
pub(crate) enum EmuMsg {
    /// Render the screen: the `rows` live lines, plus `history_lines` of
    /// history above them when requested (the `capture-pane -S -N -E -`
    /// contract). `0` = live screen only.
    Snapshot { history_lines: u64, reply: oneshot::Sender<Outcome> },
    /// The pane's size changed. The handler has already done `TIOCSWINSZ`;
    /// the emulator must resize its grid.
    Resize { cols: u16, rows: u16 },
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
    master: Arc<OwnedFd>,
    out: Arc<Mutex<VecDeque<u8>>>,
    rx: mpsc::Receiver<EmuMsg>,
    boot: oneshot::Sender<bool>,
) {
    tokio::task::spawn_blocking(move || {
        // Created here: the values are `!Send`, so they cannot be moved into
        // this closure from the caller.
        let mut term = match Terminal::new(cols, rows) {
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
        let mut pfd: libc::pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut buf = [0u8; 4096];
        let mut reading = true;
        loop {
            // 1. A waiting message is serviced before any I/O.
            match rx.try_recv() {
                Ok(msg) => {
                    service(msg, &mut term, &mut rs);
                    continue;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break,
            }
            // 2. Otherwise, wait for the master (1 ms, so 1. is re-checked —
            //    a message is never queued behind a silent pane).
            if !reading {
                // The child is gone: nothing left but messages, until the
                // sender drops (session killed).
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            pfd.revents = 0;
            let n = unsafe { libc::poll(&mut pfd, 1, 1) };
            if n < 0 {
                // fd error (e.g. the fd closed out from under us) — the
                // session is gone; the sender's drop ends the loop.
                break;
            }
            if n == 0 {
                continue; // timeout
            }
            let n = unsafe {
                libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len())
            };
            if n <= 0 {
                // EOF/error: the shell's side of the pty is gone. Stop
                // reading; keep servicing messages (a snapshot of a dead
                // pane is still valid — the grid keeps its last content).
                reading = false;
                continue;
            }
            let bytes = &buf[..n as usize];
            term.write(bytes);
            {
                let mut o = out.lock().unwrap();
                o.extend(bytes.iter().copied());
            }
        }
    });
}

/// Service one message (called on the emulator thread).
fn service(msg: EmuMsg, term: &mut Terminal, rs: &mut RenderState) {
    match msg {
        EmuMsg::Snapshot { history_lines, reply } => {
            // Clamp to the available scrollback (a capture cannot exceed what
            // the grid retained).
            let n = (history_lines as usize).min(term.scrollback_rows());
            // 1. The live screen, at the bottom (the `rows` live lines).
            term.scroll_viewport(ScrollViewport::Bottom);
            let _ = rs.update(term);
            let rows = rs.rows();
            let live: Vec<Vec<Cell>> = (0..rows).map(|y| rs.row(y)).collect();
            // 2. The last `n` history lines: scroll up `n`, read the top `n`
            //    rows (the run immediately above the live screen), restore.
            let hist: Vec<Vec<Cell>> = if n > 0 {
                term.scroll_viewport(ScrollViewport::Delta(-(n as i32)));
                let _ = rs.update(term);
                let h: Vec<Vec<Cell>> = (0..n).map(|y| rs.row(y as u16)).collect();
                term.scroll_viewport(ScrollViewport::Bottom);
                let _ = rs.update(term);
                h
            } else {
                Vec::new()
            };
            // 3. Render: the history lines first, then the live screen (the
            //    order the client replays into its own scrollback).
            let all: Vec<&[Cell]> = hist.iter().chain(live.iter()).map(|r| r.as_slice()).collect();
            let text = snapshot::render(&all, rs.cols() as usize);
            // The state reflects the live screen (we're back at `Bottom`).
            let _ = reply.send(Outcome {
                cols: rs.cols(),
                rows: rs.rows(),
                history_lines: n as u64,
                text,
                cursor_x: term.cursor_x(),
                cursor_y: term.cursor_y(),
                alternate_on: term.alternate_on(),
                mouse: term.mouse_tracking(),
                mouse_sgr: term.mode(MODE_SGR_MOUSE),
                app_cursor: term.mode(MODE_APP_CURSOR),
                bracketed_paste: term.mode(MODE_BRACKETED_PASTE),
                history_size: term.scrollback_rows() as u32,
            });
        }
        EmuMsg::Resize { cols, rows } => {
            term.resize(cols, rows);
        }
    }
}
