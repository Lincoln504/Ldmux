//! The ldmux keeper: the process that owns the PTYs and the shells.
//!
//! It is the session layer (plan §3): it allocates a PTY per session,
//! spawns the shell into it, supervises it (the authoritative death signal is
//! `waitpid`, not the master EOF), **always reads the master** (so the kernel
//! pty buffer never fills from a slow reader), and serves the keeper protocol
//! over a Unix socket. It is **stateless beyond the kernel** — fds, pids, one
//! ghostty-vt per session (the per-session emulator, [`emulator`]) — and is
//! auto-spawned detached by the daemon (phase 5); here it is built to run in
//! process (a library) and to run as a binary.
//!
//! New (no TS source of record): the keeper does not exist in the TS tree.
//! The design of record is `docs/RUST-PORT-PLAN.md` §3.2/§3.3/§3.5.

mod emulator;

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ldmux_protocol::keeper::{self as km, Ack, KeeperMessage, ListResult, PeekResult, SnapshotResult, State};
use tokio::sync::oneshot;

/// One session: a PTY + a supervised shell + one ghostty-vt (the per-session
/// emulator, [`emulator`]).
///
/// The whole `Keeper` is held behind one `Mutex`, so a session is touched by
/// one critical section at a time; the emulator thread owns its
/// `!Send` `Terminal`/`RenderState` — the `Mutex` here never guards them.
pub struct Session {
    pub id: String,
    pub pid: i32,
    /// The master side of the PTY. The reader task holds its own `Arc` clone.
    pub master: Arc<OwnedFd>,
    pub cols: u16,
    pub rows: u16,
    pub dead: bool,
    pub exit_code: i32,
    /// The master's bytes, buffered as they arrive (the "always reads the
    /// master" stream). The emulator thread feeds the emulator from this
    /// stream; phase 5 streams it to attaching clients.
    pub out: Arc<Mutex<OutBuf>>,
    /// The emulator thread's message sender (dropped with the session — the
    /// thread exits when it sees the disconnect).
    em_tx: mpsc::Sender<emulator::EmuMsg>,
    /// When this pane last took input or produced output, in unix ms. Written
    /// by the reader task (output) and by `Write` (input), read by `List`.
    /// This is the wire's `lastActivityAt`.
    activity: Arc<AtomicU64>,
}

/// The most output a session buffers for a reader that is not reading. Beyond
/// it the buffer is emptied and `lost` is raised, so an unattended shell that
/// prints forever (`yes`, a build log) costs a bounded amount of memory instead
/// of growing for the life of the keeper. A viewer polls every few ms, so only a
/// session nobody is watching can reach it.
pub const OUT_CAP: usize = 4 * 1024 * 1024;

/// A session's buffered, not-yet-peeked output.
#[derive(Default)]
pub struct OutBuf {
    pub q: VecDeque<u8>,
    /// Bytes were discarded at `OUT_CAP` since the last peek.
    pub lost: bool,
}

impl OutBuf {
    /// Append bytes read from the master, enforcing `OUT_CAP`.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.q.len() + bytes.len() > OUT_CAP {
            self.q.clear();
            self.lost = true;
            if bytes.len() > OUT_CAP {
                return;
            }
        }
        self.q.extend(bytes.iter().copied());
    }

    /// Take everything buffered (and whether any was lost before it).
    pub fn take(&mut self) -> (Vec<u8>, bool) {
        let data: Vec<u8> = self.q.drain(..).collect();
        (data, std::mem::take(&mut self.lost))
    }
}

/// Now, in unix milliseconds (0 if the clock is before the epoch — a clock that
/// wrong is not worth a panic).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The terminal type the keeper's emulator (libghostty-vt) actually implements.
/// Defined in `protocol` so the daemon's `Spawn.env` states the same value.
pub use ldmux_protocol::keeper::{CHILD_COLORTERM, CHILD_TERM};

/// The environment a session's child starts with: the keeper's own (which is
/// the daemon's), then `TERM`/`COLORTERM` pinned to what the keeper's emulator
/// is, then the `Spawn.env` overlay (the `LDMUX_*` set, plan §2.6), which wins.
///
/// `TERM` is **not** inherited: under systemd the daemon has none (bash then
/// invents `dumb` and every program turns colour off), and when started from a
/// terminal it carries that terminal's name, which is not the emulator the
/// session runs in. The keeper must be right independent of its parent, since
/// it outlives the daemon.
pub fn child_env(
    mut env: Vec<(String, String)>,
    extra: Option<&[(String, String)]>,
) -> Vec<(String, String)> {
    let mut set = |k: &str, v: &str| {
        env.retain(|(ek, _)| ek != k);
        env.push((k.to_string(), v.to_string()));
    };
    set("TERM", CHILD_TERM);
    set("COLORTERM", CHILD_COLORTERM);
    for (k, v) in extra.unwrap_or(&[]) {
        set(k, v);
    }
    env
}

#[cfg(test)]
mod out_buf_tests {
    use super::*;

    #[test]
    fn bytes_are_taken_once_in_order() {
        let mut o = OutBuf::default();
        o.push(b"ab");
        o.push(b"cd");
        assert_eq!(o.take(), (b"abcd".to_vec(), false));
        assert_eq!(o.take(), (Vec::new(), false));
    }

    #[test]
    fn an_unread_buffer_is_bounded_and_reports_the_loss() {
        let mut o = OutBuf::default();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..4 {
            o.push(&chunk);
        }
        assert_eq!(o.q.len(), OUT_CAP, "exactly at the cap is kept");
        assert!(!o.lost);
        o.push(b"y");
        // Over the cap: what was unread is discarded, the new bytes are kept,
        // and the reader is told it missed something.
        assert_eq!(o.q.len(), 1);
        let (data, lost) = o.take();
        assert_eq!(data, b"y");
        assert!(lost);
        assert!(!o.take().1, "the loss is reported once");
    }

    #[test]
    fn one_oversized_push_cannot_grow_the_buffer() {
        let mut o = OutBuf::default();
        o.push(&vec![b'z'; OUT_CAP + 1]);
        assert!(o.q.is_empty());
        assert!(o.lost);
    }
}

#[cfg(test)]
mod child_env_tests {
    use super::*;
    fn get<'a>(e: &'a [(String, String)], k: &str) -> Option<&'a str> {
        e.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str())
    }
    #[test]
    fn term_defaults_when_the_parent_has_none() {
        let e = child_env(vec![("LANG".into(), "C".into())], None);
        assert_eq!(get(&e, "TERM"), Some("xterm-256color"));
        assert_eq!(get(&e, "COLORTERM"), Some("truecolor"));
        assert_eq!(get(&e, "LANG"), Some("C"));
    }
    #[test]
    fn an_inherited_term_is_replaced() {
        let e = child_env(vec![("TERM".into(), "dumb".into())], None);
        assert_eq!(get(&e, "TERM"), Some("xterm-256color"));
        assert_eq!(e.iter().filter(|(k, _)| k == "TERM").count(), 1);
    }
    #[test]
    fn spawn_env_wins() {
        let x = vec![("TERM".to_string(), "vt100".to_string())];
        let e = child_env(vec![], Some(&x));
        assert_eq!(get(&e, "TERM"), Some("vt100"));
    }
}


/// The keeper itself. `sessions` is the one shared map; every message handler
/// takes it briefly. `shell` is the argv the keeper spawns per session (the
/// user's shell; the daemon decides, the keeper runs it).
pub struct Keeper {
    sessions: Mutex<HashMap<String, Session>>,
    shell: Vec<String>,
}

/// The pane's foreground command name (`/proc/<pid>/comm`), e.g. `dash`,
/// `vim`. Empty if the process is gone. (Read only for a *live* pane — a
/// reaped pid may be recycled, so a dead pane reports empty, not a read of a
/// possibly-different process.)
fn proc_comm(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// The pane's working directory (`/proc/<pid>/cwd`, a symlink). Empty if the
/// process is gone. Same liveness gating as [`proc_comm`].
fn proc_cwd(pid: i32) -> String {
    std::fs::read_link(format!("/proc/{pid}/cwd")).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The pane's foreground command name, the way a terminal names it: the process
/// group the tty has made foreground, then that leader's `comm`.
///
/// Reading the shell's own `comm` said `bash` forever, so every row read `bash`
/// and "which of these is my editor" could not be answered. tmux answered with
/// `pane_current_command`; this is the same fact from the same source.
fn proc_foreground(pid: i32) -> String {
    match proc_tpgid(pid) {
        Some(pgrp) if pgrp > 0 && pgrp != pid => proc_comm(pgrp),
        _ => proc_comm(pid),
    }
}

/// `/proc/<pid>/stat` field 8: the foreground process group of the process's
/// controlling terminal. Parsed after the last ')' because `comm` can contain
/// spaces and parentheses.
fn proc_tpgid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_tpgid(&stat)
}

/// The stat parser on its own, so its one subtlety (the command name can hold
/// spaces and parentheses) is testable without a live process.
fn parse_tpgid(stat: &str) -> Option<i32> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(5)?.parse().ok()
}

impl Keeper {
    /// A keeper that spawns `shell` (argv) per session.
    pub fn new(shell: Vec<String>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            shell,
        }
    }

    /// Process one keeper message, returning the answer (or an [`km::Error`]).
    ///
    /// Asynchronous: a `Snapshot` hands the render to the session's emulator
    /// thread and awaits its reply. The daemon (phase 5) calls it from its
    /// event loop. The keeper→daemon *pushes* (`ChildExited`) are not request
    /// methods in this skeleton — the daemon learns a death by
    /// `State`/`List`/`Read` reporting `dead: true` (the push channel is
    /// wired in phase 5/6).
    pub async fn handle(&self, msg: &KeeperMessage) -> Result<KeeperMessage, String> {
        self.reap(); // refresh liveness before answering anything
        match msg {
            km::KeeperMessage::Spawn(s) => match self.spawn_session(s).await {
                Ok(sess) => {
                    self.sessions
                        .lock()
                        .unwrap()
                        .insert(s.session_id.clone(), sess);
                    self.state_of(&s.session_id).await
                }
                Err(e) => Err(e.to_string()),
            },
            km::KeeperMessage::Kill(k) => {
                let pid = {
                    let g = self.sessions.lock().unwrap();
                    match g.get(&k.session_id) {
                        Some(s) => s.pid,
                        None => return Err(format!("no such session: {}", k.session_id)),
                    }
                };
                self.group_kill(pid);
                self.reap();
                self.sessions.lock().unwrap().remove(&k.session_id);
                // The session is removed (the emulator thread exits), so the
                // state fields are defaults (a dead pane's state is not read
                // from a gone emulator); `dead` is the load-bearing field.
                Ok(KeeperMessage::State(State {
                    session_id: k.session_id.clone(),
                    pid,
                    cols: 0,
                    rows: 0,
                    dead: true,
                    cursor_x: 0,
                    cursor_y: 0,
                    alternate_on: false,
                    mouse: false,
                    mouse_sgr: false,
                    app_cursor: false,
                    bracketed_paste: false,
                    history_size: 0,
                    foreground: String::new(),
                    cwd: String::new(),
                }))
            }
            km::KeeperMessage::Write(w) => {
                use nix::libc as libc;
                let m = {
                    let g = self.sessions.lock().unwrap();
                    match g.get(&w.session_id) {
                        Some(s) if !s.dead => {
                            // Input is activity too: a client typing into a
                            // silent shell is the case the clock exists to catch.
                            // Stamped under the lock we already hold, never a
                            // second acquisition.
                            s.activity.store(now_ms(), Ordering::Relaxed);
                            s.master.clone()
                        }
                        _ => return Err(format!("no live session: {}", w.session_id)),
                    }
                };
                let fd = m.as_raw_fd();
                let bytes = w.data.as_bytes();
                let mut off = 0usize;
                while off < bytes.len() {
                    let n = unsafe {
                        libc::write(fd, bytes[off..].as_ptr() as *const _, bytes.len() - off)
                    };
                    if n <= 0 {
                        return Err(format!("write to {} failed", w.session_id));
                    }
                    off += n as usize;
                }
                Ok(KeeperMessage::Ack(Ack {}))
            }
            km::KeeperMessage::Resize(r) => {
                use nix::libc as libc;
                // Clamp at the PTY. This is the last place a size can be
                // corrected, and the frame's selection rows are one byte, so a
                // pane above 255 rows truncates them silently (protocol
                // `MAX_PANE_ROWS`).
                let (cols, rows) = ldmux_protocol::clamp_pane_size(r.cols, r.rows);
                let m = {
                    let g = self.sessions.lock().unwrap();
                    match g.get(&r.session_id) {
                        Some(s) if !s.dead => (s.master.clone(), s.em_tx.clone()),
                        _ => return Err(format!("no live session: {}", r.session_id)),
                    }
                };
                let (m, m_tx) = m;
                let ws = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let fd = m.as_raw_fd();
                if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } < 0 {
                    return Err(format!("TIOCSWINSZ failed on session {}", r.session_id));
                }
                // Tell the emulator to resize its grid (FIFO: it applies this
                // before answering a later `Snapshot`).
                let _ = m_tx.send(emulator::EmuMsg::Resize {
                    cols: cols as u16,
                    rows: rows as u16,
                });
                {
                    let mut g = self.sessions.lock().unwrap();
                    if let Some(s) = g.get_mut(&r.session_id) {
                        s.cols = cols as u16;
                        s.rows = rows as u16;
                    }
                }
                self.state_of(&r.session_id).await
            }
            km::KeeperMessage::Read(rd) => self.state_of(&rd.session_id).await,
            km::KeeperMessage::Peek(p) => {
                let g = self.sessions.lock().unwrap();
                match g.get(&p.session_id) {
                    Some(s) => {
                        // Drain the buffer (the attach's stream pull). The reader
                        // task holds only the `out` lock, never the sessions
                        // lock, so this cannot deadlock with it.
                        let (data, lost) = s.out.lock().unwrap().take();
                        Ok(KeeperMessage::PeekResult(PeekResult {
                            session_id: p.session_id.clone(),
                            data,
                            lost,
                        }))
                    }
                    None => Err(format!("no such session: {}", p.session_id)),
                }
            }
            // The full snapshot: hand the render to the session's emulator
            // thread and await its reply (the cells are the source of truth;
            // the format is ours — plan §3.11(3)).
            km::KeeperMessage::Snapshot(s) => {
                let (tx, pid, dead) = {
                    let g = self.sessions.lock().unwrap();
                    // A snapshot is valid for a dead pane too: the emulator
                    // thread keeps running after the master EOF (it services
                    // messages only) and the grid keeps its last content. The
                    // `/proc` fields are reported empty for a dead pane — a
                    // reaped pid may be recycled, so we don't read it.
                    match g.get(&s.session_id) {
                        Some(sess) => (sess.em_tx.clone(), sess.pid, sess.dead),
                        _ => return Err(format!("no such session: {}", s.session_id)),
                    }
                };
                let (rtx, rrx) = oneshot::channel();
                tx.send(emulator::EmuMsg::Snapshot {
                    history_lines: s.history_lines.unwrap_or(0),
                    drain_output: s.drain_output,
                    reply: rtx,
                })
                .map_err(|_| "the emulator is gone")?;
                let o = match rrx.await {
                    Ok(o) => o,
                    Err(_) => return Err("the emulator is gone".into()),
                };
                let (foreground, cwd) = if !dead {
                    (proc_foreground(pid), proc_cwd(pid))
                } else {
                    (String::new(), String::new())
                };
                Ok(KeeperMessage::SnapshotResult(SnapshotResult {
                    session_id: s.session_id.clone(),
                    cols: o.cols as u64,
                    rows: o.rows as u64,
                    history_lines: o.history_lines,
                    text: o.text,
                    cursor_x: o.cursor_x as u64,
                    cursor_y: o.cursor_y as u64,
                    alternate_on: o.alternate_on,
                    mouse: o.mouse,
                    mouse_sgr: o.mouse_sgr,
                    app_cursor: o.app_cursor,
                    bracketed_paste: o.bracketed_paste,
                    history_size: o.history_size as u64,
                    dead,
                    foreground,
                    cwd,
                    tail: o.tail,
                }))
            }
            km::KeeperMessage::List(_l) => {
                // One `State` per session, read from each emulator (async).
                // A session removed between listing and reading is skipped.
                let ids: Vec<String> = {
                    let g = self.sessions.lock().unwrap();
                    g.values().map(|s| s.id.clone()).collect()
                };
                let mut sessions = Vec::new();
                let mut activity = std::collections::BTreeMap::new();
                for id in ids {
                    if let Ok(KeeperMessage::State(st)) = self.state_of(&id).await {
                        sessions.push(st);
                    }
                    // Read after the state, so a session that died between the
                    // two is simply absent rather than a zero.
                    let g = self.sessions.lock().unwrap();
                    if let Some(sess) = g.get(&id) {
                        activity.insert(id.clone(), sess.activity.load(Ordering::Relaxed));
                    }
                }
                Ok(KeeperMessage::ListResult(ListResult { sessions, activity }))
            }
            km::KeeperMessage::Close(_c) => Ok(KeeperMessage::Ack(Ack {})),
            // The keeper→daemon pushes (`ChildInfo`, `ChildExited`) and the
            // request-only shapes are not request methods in this skeleton.
            other => Err(format!("not a request: {other:?}")),
        }
    }

    /// The `State` answer for a session (the attach/liveness shape). Reads the
    /// pane's state from the emulator (no render) so the daemon gets the
    /// display-message fields without a `Snapshot`; the keeper adds
    /// `cols`/`rows`/`dead` (from the `Session`) and the `/proc` fields
    /// (`foreground`, `cwd` — empty for a dead pane, a reaped pid may be
    /// recycled).
    async fn state_of(&self, id: &str) -> Result<KeeperMessage, String> {
        // The em_tx + the Session fields, without holding the lock across the
        // emulator round-trip.
        let (em_tx, s_id, cols, rows, dead, pid) = {
            let g = self.sessions.lock().unwrap();
            match g.get(id) {
                Some(s) => (s.em_tx.clone(), s.id.clone(), s.cols, s.rows, s.dead, s.pid),
                None => return Err(format!("no such session: {id}")),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if em_tx
            .send(emulator::EmuMsg::StateQuery { reply: reply_tx })
            .is_err()
        {
            // The emulator thread is gone (the session was removed).
            return Err(format!("no such session: {id}"));
        }
        let st = match reply_rx.await {
            Ok(st) => st,
            Err(_) => return Err(format!("no such session: {id}")),
        };
        let (foreground, cwd) = if !dead {
            (proc_foreground(pid), proc_cwd(pid))
        } else {
            (String::new(), String::new())
        };
        Ok(KeeperMessage::State(State {
            session_id: s_id,
            pid,
            cols: cols as u64,
            rows: rows as u64,
            dead,
            cursor_x: st.cursor_x as u64,
            cursor_y: st.cursor_y as u64,
            alternate_on: st.alternate_on,
            mouse: st.mouse,
            mouse_sgr: st.mouse_sgr,
            app_cursor: st.app_cursor,
            bracketed_paste: st.bracketed_paste,
            history_size: st.history_size as u64,
            foreground,
            cwd,
        }))
    }

    /// Allocate a PTY, spawn the shell into it (own session, slave as 0/1/2),
    /// and start the per-session emulator (which always reads the master —
    /// plan §3.3 — feeds the ghostty-vt, and services the snapshot/resize
    /// messages).
    async fn spawn_session(&self, s: &km::Spawn) -> std::io::Result<Session> {
        use nix::libc as libc;
        let winsize = libc::winsize {
            ws_row: s.rows as u16,
            ws_col: s.cols as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = nix::pty::openpty(Some(&winsize), None)
            .map_err(|e| std::io::Error::other(format!("openpty: {e}")))?;
        let master = opened.master;
        let slave = opened.slave;

        // The child becomes its own session (`setsid`) so `group_kill` takes
        // the whole process group down, and the slave is its 0/1/2 (the
        // controlling terminal). We fork+exec ourselves — NOT
        // `tokio::process::Command` — so the keeper is the SOLE reaper: tokio's
        // global `ChildReaper` would reap the child before our raw `waitpid` in
        // `reap()` could, so a natural shell exit was never reported dead
        // (only a `Kill` set it). See the 2026-09-28 finding in
        // docs/RUST-PORT-LOG.md.
        let slave_in = slave.try_clone()?;
        let slave_out = slave.try_clone()?;
        let slave_err = slave.try_clone()?;
        let slave_in_raw = slave_in.as_raw_fd();
        let slave_out_raw = slave_out.as_raw_fd();
        let slave_err_raw = slave_err.as_raw_fd();
        let master_raw = master.as_raw_fd();
        // argv as CStrings + the NULL-terminated pointer array, built BEFORE
        // the fork (allocation is not async-signal-safe; the child must not
        // build it).
        // The argv CStrings (Send) are built here; the non-`Send` pointer array
        // and the fork itself live in `fork_shell` (a sync fn), so nothing
        // non-`Send` is held across an await below. `fork_shell`'s child
        // `_exit`s; only the parent returns a pid.
        // The program and argv: the `Spawn.command` when given (the `pi` kind),
        // else the keeper's login shell. `argv[0]` is the program name (the
        // `execvpe` convention), so the whole vector is passed.
        let argv_s: Vec<String> = match &s.command {
            Some(cmd) if !cmd.is_empty() => cmd.clone(),
            _ => self.shell.clone(),
        };
        let prog_s = argv_s
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty argv"))?;
        let prog = std::ffi::CString::new(prog_s.as_str())
            .map_err(|e| std::io::Error::other(format!("CString: {e}")))?;
        let argv: Vec<std::ffi::CString> = argv_s
            .iter()
            .map(|a| std::ffi::CString::new(a.as_str()).map_err(|e| std::io::Error::other(format!("CString: {e}"))))
            .collect::<Result<_, _>>()?;
        let env = child_env(std::env::vars().collect(), s.env.as_deref());
        let envp_cstrings: Vec<std::ffi::CString> = env
            .iter()
            .map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")))
            .collect::<Result<_, _>>()
            .map_err(|e| std::io::Error::other(format!("CString: {e}")))?;
        let cwd_cstr: Option<std::ffi::CString> = match &s.cwd {
            Some(c) => Some(std::ffi::CString::new(c.as_str()).map_err(|e| std::io::Error::other(format!("CString: {e}")))?),
            None => None,
        };
        let pid = fork_shell(
            slave_in_raw,
            slave_out_raw,
            slave_err_raw,
            master_raw,
            &prog,
            &argv,
            cwd_cstr.as_ref(),
            &envp_cstrings,
        )?;
        // The child holds its own 0/1/2 dups; close the parent's copies.
        drop(slave_in);
        drop(slave_out);
        drop(slave_err);
        drop(slave);

        let master = Arc::new(master);
        let out = Arc::new(Mutex::new(OutBuf::default()));
        // The session's activity clock, shared with its emulator thread (which
        // stamps it on every byte the master produces).
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let (em_tx, em_rx) = mpsc::channel();
        let (boot_tx, boot_rx) = oneshot::channel();
        // The emulator thread owns this session's ghostty-vt (the `!Send`
        // values are created inside the thread, not moved in). It always
        // reads the master (plan §3.3 — the kernel pty buffer must never
        // fill from a slow reader) and feeds the emulator and the buffer.
        emulator::spawn(
            s.cols as u16,
            s.rows as u16,
            master.clone(),
            out.clone(),
            activity.clone(),
            em_rx,
            boot_tx,
        );
        // The boot reply: the `Terminal`/`RenderState` exist before the
        // session is visible.
        let booted = boot_rx.await.ok().unwrap_or_default();
        if !booted {
            self.group_kill(pid);
            return Err(std::io::Error::other("the emulator did not boot"));
        }
        Ok(Session {
            id: s.session_id.clone(),
            pid,
            master,
            cols: s.cols as u16,
            rows: s.rows as u16,
            dead: false,
            exit_code: 0,
            out,
            em_tx,
            activity,
        })
    }

    /// Group-kill the session's process group (SIGTERM, grace, SIGKILL).
    ///
    /// The group alone is not enough: an interactive shell with job control
    /// puts every foreground job in its *own* process group, so `kill(-pid)`
    /// reaps the shell and orphans the job it was running (a `pi` agent left
    /// with no controlling terminal, invisible to ldmux). The session is the
    /// unit that must die, so both the group and every process whose session is
    /// this one are signalled.
    fn group_kill(&self, pid: i32) {
        use nix::sys::signal::Signal;
        let g = nix::unistd::Pid::from_raw(-pid);
        let _ = nix::sys::signal::kill(g, Signal::SIGTERM);
        signal_session(pid, Signal::SIGTERM);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = nix::sys::signal::kill(g, Signal::SIGKILL);
        signal_session(pid, Signal::SIGKILL);
    }

    /// Reap any exited children (the authoritative death signal), marking the
    /// matching sessions dead. Non-blocking: `waitpid(-1, WNOHANG)`.
    fn reap(&self) {
        use nix::errno::Errno;
        use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
        // Only OUR children, by pid. `waitpid(-1)` reaps *any* child of the
        // process, so a second keeper in the same process (every keeper test
        // runs several) consumed another's exit status, and the keeper that
        // owned the shell never learned it had died. One keeper per process is
        // the deployed shape, but a reaper that cannot steal is also the one
        // that cannot be surprised by a child spawned elsewhere (a `/proc`
        // reader, a future helper).
        let live: Vec<i32> = {
            let g = self.sessions.lock().unwrap();
            g.values().filter(|s| !s.dead).map(|s| s.pid).collect()
        };
        for pid in live {
            match waitpid(nix::unistd::Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {}
                Ok(WaitStatus::Exited(p, code)) => self.mark_dead(p.as_raw(), code),
                Ok(WaitStatus::Signaled(p, _sig, _core)) => self.mark_dead(p.as_raw(), -1),
                Ok(_) => {}
                // Not our child any more (already reaped): it is gone.
                Err(Errno::ECHILD) => self.mark_dead(pid, -1),
                Err(_) => {}
            }
        }
    }

    fn mark_dead(&self, pid: i32, code: i32) {
        let mut g = self.sessions.lock().unwrap();
        let mut newly_dead = None;
        for s in g.values_mut() {
            if s.pid == pid && !s.dead {
                s.dead = true;
                s.exit_code = code;
                newly_dead = Some(s.pid);
            }
        }
        drop(g);
        // The shell is gone and the session is over. A job it left in its own
        // process group (pi, a server) would otherwise survive with no tty and
        // cannot be reached or seen again, so tear the whole session down. This
        // is the natural-exit path, which used to do nothing at all.
        if let Some(sid) = newly_dead {
            use nix::sys::signal::Signal;
            signal_session(sid, Signal::SIGTERM);
            std::thread::sleep(std::time::Duration::from_millis(50));
            signal_session(sid, Signal::SIGKILL);
        }
    }

    /// How many master bytes are currently buffered for a session (test/
    /// diagnostic: proves the keeper is reading the master).
    pub fn buffered_len(&self, id: &str) -> Option<usize> {
        let g = self.sessions.lock().unwrap();
        g.get(id).map(|s| s.out.lock().unwrap().q.len())
    }
}

/// Signal every process in the session `sid`.
///
/// `/proc/<pid>/stat` is `pid (comm) state ppid pgrp session ...`, and `comm`
/// may itself contain spaces and `)`, so the fields after the *last* `)` are
/// split rather than the whole line. Reading `session` here is the only way to
/// reach a job that job control moved into its own process group.
fn signal_session(sid: i32, sig: nix::sys::signal::Signal) {
    use nix::sys::signal::kill;
    let me = std::process::id() as i32;
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if pid == sid || pid == me {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let _state = fields.next();
        let _ppid = fields.next();
        let _pgrp = fields.next();
        let session = fields.next().and_then(|s| s.parse::<i32>().ok());
        if session == Some(sid) {
            let _ = kill(nix::unistd::Pid::from_raw(pid), sig);
        }
    }
}

/// Fork+exec the shell so the keeper is the SOLE reaper (see the call-site
/// comment in `spawn_session`): the child `setsid`s, makes the slave its
/// 0/1/2, and execs the shell; only the parent returns (the child `_exit`s).
/// The non-`Send` pointer array is built here and dropped on return, so it is
/// never held across an await in the async caller.
fn fork_shell(
    slave_in_raw: std::os::fd::RawFd,
    slave_out_raw: std::os::fd::RawFd,
    slave_err_raw: std::os::fd::RawFd,
    master_raw: std::os::fd::RawFd,
    prog: &std::ffi::CString,
    argv: &[std::ffi::CString],
    cwd: Option<&std::ffi::CString>,
    envp: &[std::ffi::CString],
) -> std::io::Result<i32> {
    use nix::libc as libc;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null()); // execve's argv is NULL-terminated
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|c| c.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null()); // execve's envp is NULL-terminated
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Child) => {
            // Only async-signal-safe calls here, then exec.
            let _ = unsafe { libc::setsid() };
            // Make the slave this session's controlling terminal. Without it the
            // shell has tty descriptors but no controlling tty, so job control
            // and the terminal's own signals (Ctrl+C to the foreground group)
            // depend on the shell claiming it. Ignored on failure: the shell still
            // runs, it just behaves like a non-interactive one.
            let _ = unsafe { libc::ioctl(slave_in_raw, libc::TIOCSCTTY, 0) };
            let _ = unsafe { libc::dup2(slave_in_raw, 0) };
            let _ = unsafe { libc::dup2(slave_out_raw, 1) };
            let _ = unsafe { libc::dup2(slave_err_raw, 2) };
            let _ = unsafe { libc::close(master_raw) };
            let _ = unsafe { libc::close(slave_in_raw) };
            let _ = unsafe { libc::close(slave_out_raw) };
            let _ = unsafe { libc::close(slave_err_raw) };
            if let Some(c) = cwd {
                let _ = unsafe { libc::chdir(c.as_ptr()) };
            }
            // `execvpe` resolves `prog` on PATH and uses the given environment.
            let _ = unsafe { libc::execvpe(prog.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr()) };
            unsafe { libc::_exit(127) } // execvpe failed
        }
        Ok(nix::unistd::ForkResult::Parent { child }) => Ok(child.as_raw()),
        Err(e) => Err(std::io::Error::other(format!("fork: {e}"))),
    }
}

/// Serve the keeper protocol on a Unix socket: one tagged
/// [`KeeperMessage`] per line, one request line → one response line per
/// connection (one call == one connection).
pub async fn serve(self_: &Arc<Keeper>, socket: &std::path::Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = tokio::net::UnixListener::bind(socket)?;
    // The keeper owns every PTY: its socket is owner-only whatever the umask, and
    // not merely by the directory it happens to sit in.
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    loop {
        let (sock, _addr) = listener.accept().await?;
        let me = self_.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(&me, sock).await {
                eprintln!("ldmux-keeper: connection: {e}");
            }
        });
    }
}

/// The largest request line the keeper will buffer (a Spawn with its env, or a Write).
const MAX_REQUEST_LINE: usize = 4 * 1024 * 1024;
/// How long a connection may take to deliver its one request line.
const REQUEST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn serve_conn(me: &Arc<Keeper>, sock: tokio::net::UnixStream) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut rd, mut wr) = sock.into_split();
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    // A request is one line. Bounded in size and in time, so a client that never
    // sends the newline (or never stops sending) cannot hold memory and a task for
    // the life of the keeper.
    let read_line = async {
        loop {
            let n = rd.read(&mut tmp).await?;
            if n == 0 {
                return Ok::<bool, std::io::Error>(false);
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                return Ok(true);
            }
            if buf.len() > MAX_REQUEST_LINE {
                return Err(std::io::Error::other("request line too large"));
            }
        }
    };
    match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_line).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return Ok(()),
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(std::io::Error::other("timed out waiting for the request line")),
    }
    let nl = buf.iter().position(|&b| b == b'\n').unwrap();
    let line = String::from_utf8_lossy(&buf[..nl]).to_string();
    let req: KeeperMessage = match serde_json::from_str(&line) {
        Ok(m) => m,
        Err(e) => {
            let err = serde_json::json!({ "error": e.to_string() });
            let _ = wr
                .write_all(format!("{}\n", serde_json::to_string(&err).unwrap()).as_bytes())
                .await;
            return Ok(());
        }
    };
    let resp = match me.handle(&req).await {
        Ok(m) => m,
        Err(e) => KeeperMessage::Error(km::Error {
            error: e,
            session_id: None,
        }),
    };
    let resp_line = format!("{}\n", serde_json::to_string(&resp).unwrap());
    wr.write_all(resp_line.as_bytes()).await?;
    wr.flush().await?;
    Ok(())
}

#[cfg(test)]
mod proc_tests {
    use super::parse_tpgid;

    /// The command name is field 2 and can hold spaces and parentheses, so the
    /// fields are counted from the *last* `)`. Getting this wrong reads a
    /// neighbouring number as the foreground group and names the row after a pid.
    #[test]
    fn the_foreground_group_comes_from_the_field_after_the_command_name() {
        // pid 42, comm "weird )name", state S, ppid 1, pgrp 42, session 42,
        // tty_nr 34816, tpgid 99.
        assert_eq!(parse_tpgid("42 (weird )name) S 1 42 42 34816 99 0 0"), Some(99));
        // A plain one, and one with parens inside the name.
        assert_eq!(parse_tpgid("7 (bash) S 1 7 7 34816 7 0 0"), Some(7));
        assert_eq!(parse_tpgid("7 ((a)(b)) S 1 7 7 1 2"), Some(2));
        // Garbage is not a panic and not a wrong number.
        assert_eq!(parse_tpgid("no parens here"), None);
        assert_eq!(parse_tpgid(""), None);
    }
}
