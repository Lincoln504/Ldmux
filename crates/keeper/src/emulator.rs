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

use ldmux_vt_sys::{Cell, RenderState, Terminal};
use nix::libc as libc;
use tokio::sync::oneshot;

use ldmux_vt_sys::snapshot;

/// One unit of work the request handlers hand the emulator thread.
pub(crate) enum EmuMsg {
    /// Render the visible screen (the answer to a `Snapshot`).
    Snapshot(oneshot::Sender<Outcome>),
    /// The pane's size changed. The handler has already done `TIOCSWINSZ`;
    /// the emulator must resize its grid.
    Resize { cols: u16, rows: u16 },
}

/// The result of a render: the grid's geometry + the rendered text (one SGR
/// line per row, `\n`-joined — `snapshot::render`'s output).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub cols: u16,
    pub rows: u16,
    pub text: String,
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
        EmuMsg::Snapshot(reply) => {
            let _ = rs.update(term);
            let rows: Vec<Vec<Cell>> = (0..rs.rows()).map(|y| rs.row(y)).collect();
            let refs: Vec<&[Cell]> = rows.iter().map(|r| r.as_slice()).collect();
            let text = snapshot::render(&refs, rs.cols() as usize);
            let _ = reply.send(Outcome {
                cols: rs.cols(),
                rows: rs.rows(),
                text,
            });
        }
        EmuMsg::Resize { cols, rows } => {
            term.resize(cols, rows);
        }
    }
}
