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
//! The design of record is `docs/archive/RUST-PORT-PLAN.md` §3.2/§3.3/§3.5
//! ("plan" in the comments of this crate means that file); the current state is
//! `docs/SPEC.md` and `docs/STATE.md`.

mod emulator;
pub mod scope;

/// Lock a mutex, taking the guard back if a panicking holder poisoned it.
///
/// The keeper owns every PTY, so it is the one process whose state must outlive a
/// bug: with `lock().unwrap()` a single panic while the `sessions` table (or a
/// session's output buffer, which the emulator thread fills) was locked poisoned it,
/// and every later request — and the emulator thread's next write — panicked too,
/// until the keeper was useless and every shell with it. What these guard is plain
/// data (a map, a byte queue) that is valid across a panic; the daemon's own `relock`
/// makes the same call for the same reason.
pub(crate) trait Relock<T> {
    fn relock(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> Relock<T> for Mutex<T> {
    fn relock(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Run one pass of a detached background loop, catching a panic so it costs the
/// pass and not the loop.
///
/// The keeper's loops are detached tasks: a panic in one is a journal line and
/// then silence, and the keeper carries on serving requests while never again
/// doing the thing the loop was for. The reaper is the one that matters most —
/// with it dead, a shell that exits while nobody happens to call the keeper stays
/// a zombie and its session stays `running` for the life of the process. The
/// `sessions` table is poison-tolerant ([`Relock`]), so the next pass still gets a
/// usable lock.
pub(crate) fn survive(what: &'static str, pass: impl FnOnce()) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(pass)) {
        Ok(()) => true,
        Err(e) => {
            let why = e
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_string());
            eprintln!("ldmux-keeper: {what} pass ended abnormally ({why}); the loop continues");
            false
        }
    }
}

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ldmux_protocol::keeper::{
    self as km, Ack, KeeperMessage, ListResult, PeekResult, SnapshotResult, State,
};
use tokio::sync::oneshot;

/// One session: a PTY + a supervised shell + one ghostty-vt (the per-session
/// emulator, [`emulator`]).
///
/// The whole `Keeper` is held behind one `Mutex`, so a session is touched by
/// one critical section at a time; the emulator thread owns its
/// `!Send` `Terminal`/`RenderState` — the `Mutex` here never guards them.
/// The handle on a session's emulator thread.
///
/// Sending is two steps on purpose: the message goes on the queue, then the
/// eventfd is written so the thread's `poll` returns. Without the second step the
/// thread would have to poll with a timeout to notice the message at all, which
/// is what an idle keeper used to do 1000 times a second per session.
#[derive(Clone)]
struct Emu {
    tx: mpsc::Sender<emulator::EmuMsg>,
    waker: Option<Arc<OwnedFd>>,
}

impl Emu {
    fn send(&self, msg: emulator::EmuMsg) -> Result<(), mpsc::SendError<emulator::EmuMsg>> {
        let sent = self.tx.send(msg);
        if sent.is_ok() {
            emulator::wake(&self.waker);
        }
        sent
    }
}

/// The last handle going away is a message too.
///
/// The emulator thread leaves its loop when its channel disconnects — but it
/// waits in `poll` for ever while the pane is silent, so a disconnect that
/// happens then is never noticed and the thread outlives its session. The 1 ms
/// poll this replaced hid that: it re-checked the channel a thousand times a
/// second. Waking on the last drop is exact instead of lucky. (It also wakes on
/// the clones the request handlers hold, which costs one `poll` return each and
/// is harmless: the thread re-checks, finds nothing, and waits again.)
impl Drop for Emu {
    fn drop(&mut self) {
        emulator::wake(&self.waker);
    }
}

pub struct Session {
    pub id: String,
    pub pid: i32,
    /// The master side of the PTY. The reader task holds its own `Arc` clone.
    pub master: Arc<OwnedFd>,
    pub cols: u16,
    pub rows: u16,
    pub dead: bool,
    pub exit_code: i32,
    /// How the shell ended (see [`Exit`]); `None` while alive or when unknown.
    pub exit: Option<Exit>,
    /// When it was found dead (monotonic), for the dead-session TTL.
    died_at: Option<std::time::Instant>,
    /// The master's bytes, buffered as they arrive (the "always reads the
    /// master" stream). The emulator thread feeds the emulator from this
    /// stream; phase 5 streams it to attaching clients.
    pub out: Arc<Mutex<OutBuf>>,
    /// The emulator thread's message channel (dropped with the session — the
    /// thread exits when it sees the disconnect), plus the wake-up that breaks
    /// its `poll` so a queued message is serviced at once rather than after a
    /// timeout. See [`Emu`].
    em: Emu,
    /// When this pane last took input or produced output, in unix ms. Written
    /// by the reader task (output) and by `Write` (input), read by `List`.
    /// This is the wire's `lastActivityAt`.
    activity: Arc<AtomicU64>,
    /// Serialises writes to this pane's master. A `Write` that timed out is not
    /// cancelled (it finishes when the pane reads), so without this the next
    /// `Write` could start while the first is still mid-way through its bytes
    /// and the two would interleave on the pty.
    write_lock: Arc<Mutex<()>>,
}

/// How a shell ended: it exited with a status, or a signal killed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

/// How long a dead session (its shell gone) stays in the keeper with its last
/// screen readable, if nobody asks for it to be released. The daemon releases it
/// sooner (it keeps it a short while for a reconnecting client); this is the
/// backstop for a keeper whose daemon is away, so a dead pane cannot pin an
/// emulator for the life of the process.
pub const DEAD_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// How long a write to a pane's input may wait before the caller is told it is
/// still blocked. Below the daemon's keeper-call timeout (8 s) on purpose: the
/// answer "the pane is not reading" is more useful than a bare timeout, and it
/// arrives first.
const WRITE_TIMEOUT_MS: u64 = 5_000;

/// Write every byte to `fd`, retrying short writes and `EINTR`.
///
/// Runs on the blocking pool (see the `Write` handler): the call can wait for
/// the pane's reader, so it must never occupy the runtime. A non-blocking
/// descriptor's `EAGAIN` waits for writability and retries rather than failing
/// a write that would have succeeded a moment later.
fn write_all(fd: std::os::fd::RawFd, bytes: &[u8]) -> std::io::Result<()> {
    use nix::libc;
    let mut off = 0usize;
    while off < bytes.len() {
        // SAFETY: `off < bytes.len()` (loop condition), so `bytes[off..]` is a
        // valid non-empty slice and the length passed is exactly its length;
        // `write` only reads it. The caller keeps `fd` open for the call.
        let n = unsafe { libc::write(fd, bytes[off..].as_ptr() as *const _, bytes.len() - off) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            match e.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => {
                    let mut p = libc::pollfd {
                        fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: `p` is one initialised `pollfd` on this stack frame
                    // and the count passed is 1.
                    unsafe { libc::poll(&mut p, 1, 1000) };
                    continue;
                }
                _ => return Err(e),
            }
        }
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        off += n as usize;
    }
    Ok(())
}

/// [`write_all`] under the pane's write lock: one writer at a time, so a write
/// that outlived its caller's timeout finishes before the next one begins.
fn locked_write(lock: &Mutex<()>, fd: std::os::fd::RawFd, bytes: &[u8]) -> std::io::Result<()> {
    // A poisoned lock guards nothing (`()`): carry on.
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    write_all(fd, bytes)
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

    /// Take at most `max` bytes (and whether any was lost before them), and
    /// whether bytes remain. Linear in what it returns: the buffer is a deque, so
    /// a 4 MiB backlog is drained in chunks without ever being copied whole.
    pub fn take_up_to(&mut self, max: usize) -> (Vec<u8>, bool, bool) {
        let n = self.q.len().min(max);
        let data: Vec<u8> = self.q.drain(..n).collect();
        (data, std::mem::take(&mut self.lost), !self.q.is_empty())
    }

    /// Take everything buffered (and whether any was lost before it).
    pub fn take(&mut self) -> (Vec<u8>, bool) {
        let data: Vec<u8> = self.q.drain(..).collect();
        (data, std::mem::take(&mut self.lost))
    }
}

/// Mark a descriptor close-on-exec.
///
/// The PTY descriptors are the one place this matters: `openpty(3)` does not set
/// `FD_CLOEXEC`, so every shell the keeper forked inherited **every other
/// session's master** — measured 2026-10-05: a shell spawned after four sessions
/// held four `/dev/ptmx` descriptors that were not its own, one per session before
/// it, so the descriptors a keeper holds for N sessions were N(N-1)/2 in the
/// children. Two consequences. A shell could read a sibling session's output off
/// its master. And a keeper that died did not reliably take its shells with it:
/// the kernel's `SIGHUP` fires when the last descriptor for a master closes, so
/// the death travelled backwards along a chain — the last-spawned shell was the
/// real holder of every earlier master, and one shell that ignores `SIGHUP`
/// (permitted, and a thing programs do) left every session spawned before it
/// alive with its master held open by a stranger. That is a plausible origin for
/// the orphan shells found on this box. `dup2` clears the flag on the new
/// descriptor, so the child's 0/1/2 survive exec (POSIX).
fn set_cloexec(fd: std::os::fd::RawFd) {
    use nix::libc;
    // SAFETY: `fcntl(F_SETFD)` takes integers only; a bad `fd` yields EBADF
    // (ignored), never undefined behaviour.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

/// Close every descriptor above the standard three except `keep`, in a forked
/// child.
///
/// `close_range` is one syscall (Linux 5.9+), invoked raw: the libc wrapper is a
/// link-time symbol (glibc 2.34+), and a binary that names it does not even load
/// on an older glibc. On `ENOSYS` (an older kernel) or any failure the loop is
/// the fallback; 4096 is far above any descriptor this process holds. Belt and
/// braces with [`set_cloexec`]: the flag is what is correct, and this is what
/// makes correctness not depend on every future descriptor remembering to set
/// it. `keep` is the exec-failure pipe, which closes itself on a successful exec.
fn close_above_std(keep: std::os::fd::RawFd) {
    use nix::libc;
    let range = |lo: libc::c_uint, hi: libc::c_uint| -> bool {
        if lo > hi {
            return true;
        }
        // SAFETY: `close_range(lo, hi, 0)` takes three integers and no
        // pointers; it is async-signal-safe (the caller is a forked child) and
        // only closes descriptors, which the caller wants closed.
        unsafe { libc::syscall(libc::SYS_close_range, lo, hi, 0 as libc::c_uint) == 0 }
    };
    let keep = keep as libc::c_uint;
    if range(3, keep.saturating_sub(1).max(2)) && range(keep + 1, libc::c_uint::MAX) {
        return;
    }
    for fd in 3..4096 {
        if fd != keep as i32 {
            // SAFETY: `close` takes an integer; closing an unopened number is
            // EBADF, ignored. Only called in the forked child, where every
            // descriptor above 2 (but `keep`) is meant to be shut.
            unsafe {
                libc::close(fd);
            }
        }
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

    /// A panic while a lock is held must not make every later use of it panic: the
    /// keeper owns every PTY, and a poisoned `sessions` table or output buffer used
    /// to end all of them.
    #[test]
    fn a_poisoned_lock_is_recovered_not_fatal() {
        let m = Arc::new(Mutex::new(OutBuf::default()));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("a holder that fails");
        })
        .join();
        assert!(m.is_poisoned());
        m.relock().push(b"still works");
        assert_eq!(m.relock().take().0, b"still works".to_vec());
    }

    /// A panic in one pass must not end the loop: the keeper's reaper is a
    /// detached task, and a dead reaper leaves every later exit unreaped and every
    /// dead session looking alive.
    #[test]
    fn a_panicking_pass_does_not_end_the_loop() {
        let mut seen = Vec::new();
        for i in 0..2 {
            let ok = survive("test", || {
                seen.push(i);
                if i == 0 {
                    panic!("a pass that fails");
                }
            });
            assert_eq!(ok, i != 0, "pass {i}");
        }
        assert_eq!(seen, vec![0, 1], "the second pass still ran");
    }

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
    /// When set, each shell is moved into its own systemd scope after it starts
    /// (ADR-0021). `None` (the default of [`Keeper::new`]) leaves shells where
    /// the keeper forked them.
    scope: Option<scope::ScopeEnv>,
}

/// The pane's foreground command name (`/proc/<pid>/comm`), e.g. `dash`,
/// `vim`. Empty if the process is gone. (Read only for a *live* pane — a
/// reaped pid may be recycled, so a dead pane reports empty, not a read of a
/// possibly-different process.)
fn proc_comm(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The pane's working directory (`/proc/<pid>/cwd`, a symlink). Empty if the
/// process is gone. Same liveness gating as [`proc_comm`].
fn proc_cwd(pid: i32) -> String {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
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
            scope: None,
        }
    }

    /// Put each shell in its own transient scope (see [`scope`]); `None` keeps
    /// shells in the keeper's cgroup. The binary passes [`scope::ScopeEnv::detect`].
    #[must_use]
    pub fn with_scope(mut self, scope: Option<scope::ScopeEnv>) -> Self {
        self.scope = scope;
        self
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
            km::KeeperMessage::Spawn(s) => {
                // An id that is already here is refused, not replaced: replacing
                // drops the old session's handle and leaves its shell running
                // with nobody able to reach it. (A dead session still holds its
                // id until it is killed.)
                let exists = |me: &Self| me.sessions.relock().contains_key(&s.session_id);
                let dup = || format!("session already exists: {}", s.session_id);
                if exists(self) {
                    return Err(dup());
                }
                match self.spawn_session(s).await {
                    Ok(sess) => {
                        // Checked again at insert: a second Spawn for the same id
                        // may have completed while this one was booting.
                        let lost_race = {
                            let mut g = self.sessions.relock();
                            if g.contains_key(&s.session_id) {
                                Some(sess)
                            } else {
                                g.insert(s.session_id.clone(), sess);
                                None
                            }
                        };
                        if let Some(sess) = lost_race {
                            terminate(sess.pid).await;
                            return Err(dup());
                        }
                        self.state_of(&s.session_id).await
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            km::KeeperMessage::Kill(k) => {
                let (pid, dead) = {
                    let g = self.sessions.relock();
                    match g.get(&k.session_id) {
                        Some(s) => (s.pid, s.dead),
                        None => return Err(format!("no such session: {}", k.session_id)),
                    }
                };
                // A session already reaped is not signalled: its pid is free
                // for the kernel to hand to an unrelated process, and
                // `mark_dead` already tore down whatever it left behind. A live
                // one is signalled and then waited for, so no zombie remains.
                if !dead {
                    terminate(pid).await;
                }
                self.sessions.relock().remove(&k.session_id);
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
                    exit_code: None,
                    exit_signal: None,
                }))
            }
            km::KeeperMessage::Write(w) => {
                let (m, wl) = {
                    let g = self.sessions.relock();
                    match g.get(&w.session_id) {
                        Some(s) if !s.dead => {
                            // Input is activity too: a client typing into a
                            // silent shell is the case the clock exists to catch.
                            // Stamped under the lock we already hold, never a
                            // second acquisition.
                            s.activity.store(now_ms(), Ordering::Relaxed);
                            (s.master.clone(), s.write_lock.clone())
                        }
                        _ => return Err(format!("no live session: {}", w.session_id)),
                    }
                };
                // The write is potentially blocking, and it runs on the
                // blocking pool, never on the runtime. A pty's input buffer is
                // small (4 KiB in canonical mode) and a foreground program that
                // is not reading — `sleep 300`, a stopped job — fills it, at
                // which point `write(2)` waits for the reader, not for the
                // kernel. Done inline that waits for the *keeper*: the runtime
                // is single-threaded, so one paste into one such pane stopped
                // every session's List, Snapshot and Peek for as long as the
                // program kept not reading. Measured 2026-10-05 with the test
                // below: a 1 MiB paste into a `sleep 60`, with a `List` behind
                // it, hung the List.
                //
                // The wait is bounded so the caller gets an answer that says
                // what happened, well inside the daemon's 8 s keeper-call
                // timeout. The blocked task is not cancelled — it finishes the
                // write when the pane reads — so the bytes are late, not lost,
                // and the error says so.
                let bytes = w.data.as_bytes().to_vec();
                let sid = w.session_id.clone();
                let done = tokio::time::timeout(
                    std::time::Duration::from_millis(WRITE_TIMEOUT_MS),
                    tokio::task::spawn_blocking(move || locked_write(&wl, m.as_raw_fd(), &bytes)),
                )
                .await;
                match done {
                    Ok(Ok(Ok(()))) => Ok(KeeperMessage::Ack(Ack {})),
                    Ok(Ok(Err(e))) => Err(format!("write to {sid} failed: {e}")),
                    Ok(Err(e)) => Err(format!("write to {sid}: the task failed: {e}")),
                    Err(_) => Err(format!(
                        "write to {sid} is still blocked after {WRITE_TIMEOUT_MS} ms: the pane is not reading. The bytes may land when it does."
                    )),
                }
            }
            km::KeeperMessage::Resize(r) => {
                use nix::libc;
                // Clamp at the PTY. This is the last place a size can be
                // corrected, and the frame's selection rows are one byte, so a
                // pane above 255 rows truncates them silently (protocol
                // `MAX_PANE_ROWS`).
                let (cols, rows) = ldmux_protocol::clamp_pane_size(r.cols, r.rows);
                let m = {
                    let g = self.sessions.relock();
                    match g.get(&r.session_id) {
                        Some(s) if !s.dead => (s.master.clone(), s.em.clone()),
                        _ => return Err(format!("no live session: {}", r.session_id)),
                    }
                };
                let (m, em) = m;
                let ws = libc::winsize {
                    ws_row: rows as u16,
                    ws_col: cols as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let fd = m.as_raw_fd();
                // SAFETY: TIOCSWINSZ reads one `winsize` through the pointer; `ws`
                // is a live, initialised local. `fd` is the master `m` owns for
                // this scope.
                if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } < 0 {
                    return Err(format!("TIOCSWINSZ failed on session {}", r.session_id));
                }
                // Tell the emulator to resize its grid (FIFO: it applies this
                // before answering a later `Snapshot`).
                let _ = em.send(emulator::EmuMsg::Resize {
                    cols: cols as u16,
                    rows: rows as u16,
                });
                {
                    let mut g = self.sessions.relock();
                    if let Some(s) = g.get_mut(&r.session_id) {
                        s.cols = cols as u16;
                        s.rows = rows as u16;
                    }
                }
                self.state_of(&r.session_id).await
            }
            km::KeeperMessage::Read(rd) => self.state_of(&rd.session_id).await,
            km::KeeperMessage::Peek(p) => {
                let g = self.sessions.relock();
                match g.get(&p.session_id) {
                    Some(s) => {
                        // The attach's stream pull. The reader task holds only the
                        // `out` lock, never the sessions lock, so this cannot
                        // deadlock with it.
                        match p.max_bytes {
                            // The original contract, kept for a daemon that
                            // predates the field: everything, as a byte array.
                            None => {
                                let (data, lost) = s.out.relock().take();
                                Ok(KeeperMessage::PeekResult(PeekResult {
                                    session_id: p.session_id.clone(),
                                    data,
                                    data64: None,
                                    more: false,
                                    lost,
                                }))
                            }
                            Some(max) => {
                                let max = max.clamp(1, km::PEEK_MAX_BYTES) as usize;
                                let (data, lost, more) = s.out.relock().take_up_to(max);
                                Ok(KeeperMessage::PeekResult(PeekResult::compact(
                                    p.session_id.clone(),
                                    &data,
                                    more,
                                    lost,
                                )))
                            }
                        }
                    }
                    None => Err(format!("no such session: {}", p.session_id)),
                }
            }
            // The full snapshot: hand the render to the session's emulator
            // thread and await its reply (the cells are the source of truth;
            // the format is ours — plan §3.11(3)).
            km::KeeperMessage::Snapshot(s) => {
                let (em, pid, dead) = {
                    let g = self.sessions.relock();
                    // A snapshot is valid for a dead pane too: the emulator
                    // thread keeps running after the master EOF (it services
                    // messages only) and the grid keeps its last content. The
                    // `/proc` fields are reported empty for a dead pane — a
                    // reaped pid may be recycled, so we don't read it.
                    match g.get(&s.session_id) {
                        Some(sess) => (sess.em.clone(), sess.pid, sess.dead),
                        _ => return Err(format!("no such session: {}", s.session_id)),
                    }
                };
                let (rtx, rrx) = oneshot::channel();
                em.send(emulator::EmuMsg::Snapshot {
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
                    let g = self.sessions.relock();
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
                    let g = self.sessions.relock();
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
        // The emulator handle + the Session fields, without holding the lock
        // across the emulator round-trip.
        let (em, s_id, cols, rows, dead, pid, exit) = {
            let g = self.sessions.relock();
            match g.get(id) {
                Some(s) => (
                    s.em.clone(),
                    s.id.clone(),
                    s.cols,
                    s.rows,
                    s.dead,
                    s.pid,
                    s.exit,
                ),
                None => return Err(format!("no such session: {id}")),
            }
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if em
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
            exit_code: match exit {
                Some(Exit::Code(c)) => Some(c as i64),
                _ => None,
            },
            exit_signal: match exit {
                Some(Exit::Signal(sg)) => Some(sg),
                _ => None,
            },
        }))
    }

    /// Allocate a PTY, spawn the shell into it (own session, slave as 0/1/2),
    /// and start the per-session emulator (which always reads the master —
    /// plan §3.3 — feeds the ghostty-vt, and services the snapshot/resize
    /// messages).
    async fn spawn_session(&self, s: &km::Spawn) -> std::io::Result<Session> {
        use nix::libc;
        // The same clamp `Resize` applies: the grid the emulator holds, the PTY's
        // size and the size the client is told must all be the one number.
        let (cols, rows) = ldmux_protocol::clamp_pane_size(s.cols, s.rows);
        let (cols, rows) = (cols as u16, rows as u16);
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = nix::pty::openpty(Some(&winsize), None)
            .map_err(|e| std::io::Error::other(format!("openpty: {e}")))?;
        let master = opened.master;
        let slave = opened.slave;
        // The child must not inherit these (see `set_cloexec`).
        set_cloexec(master.as_raw_fd());
        set_cloexec(slave.as_raw_fd());

        // The child becomes its own session (`setsid`) so `group_kill` takes
        // the whole process group down, and the slave is its 0/1/2 (the
        // controlling terminal). We fork+exec ourselves — NOT
        // `tokio::process::Command` — so the keeper is the SOLE reaper: tokio's
        // global `ChildReaper` would reap the child before our raw `waitpid` in
        // `reap()` could, so a natural shell exit was never reported dead
        // (only a `Kill` set it). See the 2026-09-28 finding in
        // docs/archive/RUST-PORT-LOG.md.
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
            .map(|a| {
                std::ffi::CString::new(a.as_str())
                    .map_err(|e| std::io::Error::other(format!("CString: {e}")))
            })
            .collect::<Result<_, _>>()?;
        let env = child_env(std::env::vars().collect(), s.env.as_deref());
        let envp_cstrings: Vec<std::ffi::CString> = env
            .iter()
            .map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")))
            .collect::<Result<_, _>>()
            .map_err(|e| std::io::Error::other(format!("CString: {e}")))?;
        let cwd_cstr: Option<std::ffi::CString> = match &s.cwd {
            Some(c) => Some(
                std::ffi::CString::new(c.as_str())
                    .map_err(|e| std::io::Error::other(format!("CString: {e}")))?,
            ),
            None => None,
        };
        // The exec-failure pipe: close-on-exec on both ends, so a successful
        // exec closes the child's end and the parent reads EOF; a failure (chdir,
        // exec) writes what went wrong first.
        let mut pipe_fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe_fds` is a writable `[c_int; 2]`, the array `pipe2` fills.
        if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `pipe2` succeeded, so both descriptors are open and
        // exclusively ours; each is wrapped exactly once, so each is closed once.
        let (err_rd, err_wr) = unsafe {
            (
                <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(pipe_fds[0]),
                <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(pipe_fds[1]),
            )
        };
        // The go pipe: with a scope configured the child blocks on it until the
        // keeper has moved it into its scope, so nothing it forks can be left in
        // the keeper's cgroup. A byte from the keeper (or EOF) releases it.
        let go = if self.scope.is_some() {
            let mut g = [0 as libc::c_int; 2];
            // SAFETY: `g` is a writable `[c_int; 2]`, the array `pipe2` fills.
            if unsafe { libc::pipe2(g.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `pipe2` succeeded; both descriptors are open, ours alone,
            // and each is wrapped once.
            Some(unsafe {
                (
                    <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(g[0]),
                    <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(g[1]),
                )
            })
        } else {
            None
        };
        let go_raw = go.as_ref().map(|(r, w)| (r.as_raw_fd(), w.as_raw_fd()));
        let pid = fork_shell(
            go_raw,
            slave_in_raw,
            slave_out_raw,
            slave_err_raw,
            master_raw,
            err_wr.as_raw_fd(),
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
        drop(err_wr);
        // From here every error path must reap `pid`.
        //
        // Move the child (still blocked on the go pipe) into its own scope, then
        // release it. A failure is logged and ignored: the shell then starts in the
        // keeper's cgroup, exactly as it does without scopes. A cancelled future drops
        // the write end, which releases the child by EOF.
        if let Some((go_r, go_w)) = go {
            drop(go_r);
            if let Some(env) = self.scope.clone() {
                let sid = s.session_id.clone();
                match tokio::task::spawn_blocking(move || scope::enter(&env, &sid, pid)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => eprintln!("ldmux-keeper: no scope for pid {pid}: {e}"),
                    Err(e) => eprintln!("ldmux-keeper: scope task failed: {e}"),
                }
            }
            // Release the child with a BYTE, not by closing: a concurrent spawn's
            // child holds a copy of this write end until it execs, so EOF alone
            // could wait on a sibling that is itself waiting on us. (EOF still
            // frees a child whose keeper died.) A failed write is the same as EOF.
            let _ = nix::unistd::write(&go_w, b"g");
            drop(go_w);
        }
        let report = tokio::task::spawn_blocking(move || read_exec_report(&err_rd))
            .await
            .unwrap_or(ExecReport::Failed(0, 0));
        match report {
            ExecReport::Exec => {}
            ExecReport::Failed(stage, errno) => {
                wait_child(pid).await;
                let what = match stage {
                    STAGE_DUP2 => "dup2 of the terminal onto stdio",
                    STAGE_CHDIR => "chdir",
                    STAGE_EXEC => "exec",
                    _ => "starting the child",
                };
                let target = match stage {
                    STAGE_CHDIR => s.cwd.clone().unwrap_or_default(),
                    STAGE_EXEC => prog_s.clone(),
                    _ => String::new(),
                };
                return Err(std::io::Error::other(format!(
                    "{what} {target}: {}",
                    std::io::Error::from_raw_os_error(errno)
                )));
            }
        }

        let master = Arc::new(master);
        let out = Arc::new(Mutex::new(OutBuf::default()));
        // The session's activity clock, shared with its emulator thread (which
        // stamps it on every byte the master produces).
        let activity = Arc::new(AtomicU64::new(now_ms()));
        let (em_tx, em_rx) = mpsc::channel();
        let waker = emulator::new_waker();
        let (boot_tx, boot_rx) = oneshot::channel();
        // The emulator thread owns this session's ghostty-vt (the `!Send`
        // values are created inside the thread, not moved in). It always
        // reads the master (plan §3.3 — the kernel pty buffer must never
        // fill from a slow reader) and feeds the emulator and the buffer.
        emulator::spawn(
            cols,
            rows,
            emulator::Wiring {
                master: master.clone(),
                waker: waker.clone(),
                out: out.clone(),
                activity: activity.clone(),
            },
            em_rx,
            boot_tx,
        );
        // The boot reply: the `Terminal`/`RenderState` exist before the
        // session is visible.
        let booted = boot_rx.await.ok().unwrap_or_default();
        if !booted {
            terminate(pid).await;
            return Err(std::io::Error::other("the emulator did not boot"));
        }
        Ok(Session {
            id: s.session_id.clone(),
            pid,
            master,
            cols,
            rows,
            dead: false,
            exit_code: 0,
            exit: None,
            died_at: None,
            out,
            em: Emu { tx: em_tx, waker },
            activity,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Reap any exited children (the authoritative death signal), marking the
    /// matching sessions dead. Non-blocking: `waitpid(-1, WNOHANG)`.
    fn reap(&self) {
        use nix::errno::Errno;
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        // Only OUR children, by pid. `waitpid(-1)` reaps *any* child of the
        // process, so a second keeper in the same process (every keeper test
        // runs several) consumed another's exit status, and the keeper that
        // owned the shell never learned it had died. One keeper per process is
        // the deployed shape, but a reaper that cannot steal is also the one
        // that cannot be surprised by a child spawned elsewhere (a `/proc`
        // reader, a future helper).
        let live: Vec<i32> = {
            let g = self.sessions.relock();
            g.values().filter(|s| !s.dead).map(|s| s.pid).collect()
        };
        for pid in live {
            match waitpid(nix::unistd::Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {}
                Ok(WaitStatus::Exited(p, code)) => {
                    self.mark_dead(p.as_raw(), Some(Exit::Code(code)))
                }
                Ok(WaitStatus::Signaled(p, sig, _core)) => {
                    self.mark_dead(p.as_raw(), Some(Exit::Signal(sig as i32)))
                }
                Ok(_) => {}
                // Not our child any more (already reaped): it is gone, and how it
                // went is unknown.
                Err(Errno::ECHILD) => self.mark_dead(pid, None),
                Err(_) => {}
            }
        }
        // The backstop for a dead pane nobody released (see `DEAD_SESSION_TTL`).
        let now = std::time::Instant::now();
        self.sessions.relock().retain(|_, s| match s.died_at {
            Some(t) => now.duration_since(t) < DEAD_SESSION_TTL,
            None => true,
        });
    }

    fn mark_dead(&self, pid: i32, exit: Option<Exit>) {
        let mut g = self.sessions.relock();
        let mut newly_dead = None;
        for s in g.values_mut() {
            if s.pid == pid && !s.dead {
                s.dead = true;
                s.exit = exit;
                s.exit_code = match exit {
                    Some(Exit::Code(c)) => c,
                    _ => -1,
                };
                s.died_at = Some(std::time::Instant::now());
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
            // The grace period is a timer, not a sleep: this runs on the
            // runtime, and a blocking sleep here stalled every session.
            tokio::spawn(async move {
                tokio::time::sleep(KILL_GRACE).await;
                signal_session(sid, Signal::SIGKILL);
            });
        }
    }

    /// How many master bytes are currently buffered for a session (test/
    /// diagnostic: proves the keeper is reading the master).
    pub fn buffered_len(&self, id: &str) -> Option<usize> {
        let g = self.sessions.relock();
        g.get(id).map(|s| s.out.relock().q.len())
    }
}

/// How long a terminated session gets between SIGTERM and SIGKILL.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(50);

/// Take a live session's process tree down and reap the shell.
///
/// SIGTERM to the process group and to every process whose session is this one,
/// a grace period, SIGKILL, then `waitpid`. The group alone is not enough: an
/// interactive shell with job control puts every foreground job in its *own*
/// process group, so `kill(-pid)` reaps the shell and orphans the job it was
/// running (a `pi` agent left with no controlling terminal, invisible to
/// ldmux). The session is the unit that must die. Only for a child that has not
/// been reaped: until `waitpid` collects it the pid cannot be recycled, which is
/// what makes signalling it safe.
async fn terminate(pid: i32) {
    use nix::sys::signal::Signal;
    let g = nix::unistd::Pid::from_raw(-pid);
    let _ = nix::sys::signal::kill(g, Signal::SIGTERM);
    signal_session(pid, Signal::SIGTERM);
    tokio::time::sleep(KILL_GRACE).await;
    let _ = nix::sys::signal::kill(g, Signal::SIGKILL);
    signal_session(pid, Signal::SIGKILL);
    wait_child(pid).await;
}

/// Collect the exit status of `pid` so no zombie remains.
///
/// Polls without blocking; a child stuck in uninterruptible sleep (it cannot
/// die even to SIGKILL until its I/O returns) is handed to a blocking waiter
/// that reaps it whenever it does.
async fn wait_child(pid: i32) {
    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    let p = nix::unistd::Pid::from_raw(pid);
    for _ in 0..40 {
        match waitpid(p, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await
            }
            Err(Errno::EINTR) => {}
            // Reaped (by us, or by the periodic reaper) or not ours: done.
            _ => return,
        }
    }
    tokio::task::spawn_blocking(move || {
        let _ = waitpid(p, None);
    });
}

/// What the child's exec-failure pipe said.
enum ExecReport {
    /// EOF with nothing written: the pipe closed on exec.
    Exec,
    /// `(stage, errno)` of the step that failed.
    Failed(u8, i32),
}

const STAGE_DUP2: u8 = 1;
const STAGE_CHDIR: u8 = 2;
const STAGE_EXEC: u8 = 3;

/// Read the child's report from the error pipe (blocks until the child execs or
/// fails — both are immediate; run it on the blocking pool).
fn read_exec_report(rd: &OwnedFd) -> ExecReport {
    use nix::libc;
    let mut buf = [0u8; 5];
    let mut got = 0usize;
    while got < buf.len() {
        // SAFETY: `buf[got..]` is a valid writable slice (`got < buf.len()` by
        // the loop condition) and the length passed is exactly its length; `rd`
        // is an open descriptor borrowed for the call.
        let n = unsafe {
            libc::read(
                rd.as_raw_fd(),
                buf[got..].as_mut_ptr() as *mut _,
                buf.len() - got,
            )
        };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if n == 0 {
            break;
        }
        got += n as usize;
    }
    match got {
        0 => ExecReport::Exec,
        5 => ExecReport::Failed(buf[0], i32::from_ne_bytes([buf[1], buf[2], buf[3], buf[4]])),
        _ => ExecReport::Failed(0, 0),
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
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
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
// The fds are the point: four distinct descriptors used in a fixed `dup2`
// order. Bundling them would name them, not make them any harder to swap.
#[allow(clippy::too_many_arguments)]
fn fork_shell(
    go: Option<(std::os::fd::RawFd, std::os::fd::RawFd)>,
    slave_in_raw: std::os::fd::RawFd,
    slave_out_raw: std::os::fd::RawFd,
    slave_err_raw: std::os::fd::RawFd,
    master_raw: std::os::fd::RawFd,
    err_fd: std::os::fd::RawFd,
    prog: &std::ffi::CString,
    argv: &[std::ffi::CString],
    cwd: Option<&std::ffi::CString>,
    envp: &[std::ffi::CString],
) -> std::io::Result<i32> {
    use nix::libc;
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null()); // execve's argv is NULL-terminated
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|c| c.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null()); // execve's envp is NULL-terminated
    // SAFETY: the keeper is multi-threaded, so the child may only call
    // async-signal-safe functions until exec. Everything it needs (the C
    // strings, `argv_ptrs`, `envp_ptrs`) was built above and is only read; the
    // child never allocates, takes a lock or returns into Rust code, it ends in
    // `execvpe` or `child_fail` (`_exit`). (`execvpe` is not on POSIX's
    // async-signal-safe list, but glibc's implementation does not allocate.)
    match unsafe { nix::unistd::fork() } {
        Ok(nix::unistd::ForkResult::Child) => {
            // Only async-signal-safe calls here, then exec.
            if let Some((go_r, go_w)) = go {
                // Wait to be put in our scope (see `spawn_session`): drop our copy of
                // the write end, so the keeper dying releases us too, then block for
                // the byte or the EOF. SAFETY: both descriptors are open in this
                // child; `close` and `read` are async-signal-safe and `buf` is a
                // valid 1-byte buffer.
                unsafe {
                    libc::close(go_w);
                    let mut buf = [0u8; 1];
                    loop {
                        let n = libc::read(go_r, buf.as_mut_ptr() as *mut _, 1);
                        if n >= 0 || *libc::__errno_location() != libc::EINTR {
                            break;
                        }
                    }
                    libc::close(go_r);
                }
            }
            // SAFETY: `setsid` takes no arguments and is async-signal-safe.
            let _ = unsafe { libc::setsid() };
            // Make the slave this session's controlling terminal. Without it the
            // shell has tty descriptors but no controlling tty, so job control
            // and the terminal's own signals (Ctrl+C to the foreground group)
            // depend on the shell claiming it. Ignored on failure: the shell still
            // runs, it just behaves like a non-interactive one.
            // SAFETY: integer-only arguments (TIOCSCTTY with arg 0 takes no
            // pointer); `slave_in_raw` is the open slave the parent passed in.
            let _ = unsafe { libc::ioctl(slave_in_raw, libc::TIOCSCTTY, 0) };
            // SAFETY: `dup2` takes integers only; the three slave descriptors are
            // open in this (forked, single-threaded) child.
            if unsafe {
                libc::dup2(slave_in_raw, 0) < 0
                    || libc::dup2(slave_out_raw, 1) < 0
                    || libc::dup2(slave_err_raw, 2) < 0
            } {
                // SAFETY: `err_fd` is the open error pipe; `child_fail` is
                // async-signal-safe and never returns.
                unsafe { child_fail(err_fd, STAGE_DUP2) }
            }
            // SAFETY: `close` takes integers only; these four descriptors are the
            // child's copies of the pty ends, no longer needed after `dup2`
            // (none of them is `err_fd`, which the parent keeps distinct).
            unsafe {
                let _ = libc::close(master_raw);
                let _ = libc::close(slave_in_raw);
                let _ = libc::close(slave_out_raw);
                let _ = libc::close(slave_err_raw);
            }
            // Everything else the keeper holds goes with them: the pty fds of
            // every other session, any eventfd, the socket. A shell is a shell,
            // not a view of its siblings. (The error pipe stays; it is
            // close-on-exec.)
            close_above_std(err_fd);
            // Rust's runtime sets SIGPIPE to ignored, and an ignored disposition
            // survives exec: every shell and every program under it would see
            // `write` fail with EPIPE instead of dying quietly in a pipeline
            // (`yes | head` loops on errors; the shell's `set -o pipefail`
            // reports nonsense). Default it, and clear the signal mask the
            // thread forking us carries.
            // SAFETY: `signal`, `sigemptyset` and `sigprocmask` are
            // async-signal-safe; `set` is zero-initialised (valid for the
            // plain-integer `sigset_t`) and then initialised by `sigemptyset`,
            // and the old-set pointer is null, which `sigprocmask` allows.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            }
            if let Some(c) = cwd {
                // A directory that cannot be entered fails the spawn: a shell in
                // the wrong place is worse than an error the caller can show.
                // SAFETY: `c` is a NUL-terminated `CString` that outlives the
                // call; `chdir` only reads it.
                if unsafe { libc::chdir(c.as_ptr()) } != 0 {
                    // SAFETY: as for the dup2 failure above.
                    unsafe { child_fail(err_fd, STAGE_CHDIR) }
                }
            }
            // `execvpe` resolves `prog` on PATH and uses the given environment.
            // SAFETY: `prog` is NUL-terminated; `argv_ptrs` and `envp_ptrs` are
            // NULL-terminated arrays of pointers into `CString`s that outlive
            // the call (they are borrowed by this function), which `execvpe` only
            // reads. If it returns it failed, and `child_fail` (never returns)
            // reports it.
            unsafe {
                libc::execvpe(prog.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr());
                child_fail(err_fd, STAGE_EXEC)
            }
        }
        Ok(nix::unistd::ForkResult::Parent { child }) => Ok(child.as_raw()),
        Err(e) => Err(std::io::Error::other(format!("fork: {e}"))),
    }
}

/// Tell the parent which step failed and with what errno, then `_exit`.
/// Async-signal-safe: raw `write` of a fixed buffer, no allocation.
///
/// # Safety
/// Only for use in a forked child before exec: `err_fd` must be an open
/// descriptor (the error pipe's write end), and the caller must not rely on
/// anything after the call, which never returns.
unsafe fn child_fail(err_fd: std::os::fd::RawFd, stage: u8) -> ! {
    use nix::libc;
    // SAFETY: `__errno_location` returns the calling thread's valid errno slot;
    // `buf` is 5 initialised bytes and that length is passed; `write` and
    // `_exit` are async-signal-safe. `err_fd` is open per this function's contract.
    unsafe {
        let errno = *libc::__errno_location();
        let e = errno.to_ne_bytes();
        let buf = [stage, e[0], e[1], e[2], e[3]];
        libc::write(err_fd, buf.as_ptr() as *const _, buf.len());
        libc::_exit(127)
    }
}

/// Serve the keeper protocol on a Unix socket: one tagged
/// [`KeeperMessage`] per line, one request line → one response line per
/// connection (one call == one connection).
pub async fn serve(self_: &Arc<Keeper>, socket: &std::path::Path) -> std::io::Result<()> {
    let listener = bind_private(socket)?;
    // Reaping is not left to requests alone: a shell that exits while nobody
    // asks stays a zombie, and its session stays "alive", until the next call.
    // The task holds a `Weak`, so it ends with the keeper.
    {
        let weak = Arc::downgrade(self_);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(REAP_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                match weak.upgrade() {
                    Some(k) => {
                        // A panic ends the pass, not the reaper: the task is
                        // detached, and a dead reaper would leave every later exit
                        // unreaped and every dead session looking alive.
                        let _ = survive("reap", || k.reap());
                    }
                    None => return,
                }
            }
        });
    }
    loop {
        match listener.accept().await {
            Ok((sock, _addr)) => {
                let me = self_.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_conn(&me, sock).await {
                        eprintln!("ldmux-keeper: connection: {e}");
                    }
                });
            }
            // A failed accept is not the end of the keeper: it owns every PTY, and
            // exiting here would kill every session. `EMFILE`/`ENFILE` clear when a
            // descriptor frees; `ECONNABORTED` is one dropped client. Back off so a
            // persistent error is not a busy loop.
            Err(e) => {
                eprintln!("ldmux-keeper: accept: {e}");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
            }
        }
    }
}

/// How often the keeper looks for exited shells on its own.
const REAP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// The pause after a failed `accept`.
const ACCEPT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// Bind the keeper socket, owner-only from the first instant.
///
/// A socket that something is still answering on is **not** replaced: a second
/// keeper unlinking the first's socket would leave the first running with every
/// PTY and no way to reach it. Only a stale socket (nothing accepting) is
/// unlinked. The umask is tightened around `bind` so the file never exists with
/// wider permissions, as the daemon's control socket does; the `chmod` after is
/// belt and braces.
fn bind_private(socket: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    ldmux_protocol::paths::check_socket_path(&socket.to_string_lossy())
        .map_err(|m| Error::new(ErrorKind::InvalidInput, m))?;
    match std::fs::symlink_metadata(socket) {
        Ok(md) if md.file_type().is_dir() => {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                format!("{} is a directory", socket.display()),
            ));
        }
        Ok(md) => {
            if md.file_type().is_socket() {
                match std::os::unix::net::UnixStream::connect(socket) {
                    Ok(_) => {
                        return Err(Error::new(
                            ErrorKind::AddrInUse,
                            format!("a keeper is already serving on {}", socket.display()),
                        ));
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            ErrorKind::ConnectionRefused | ErrorKind::NotFound
                        ) => {}
                    Err(e) => {
                        return Err(Error::new(
                            e.kind(),
                            format!("cannot probe {}: {e}", socket.display()),
                        ));
                    }
                }
            }
            // Stale (a dead keeper's socket, or a leftover file): clear it.
            match std::fs::remove_file(socket) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // SAFETY: `umask` only swaps a process-global integer and cannot fail. The
    // previous value is restored below straight after the synchronous `bind`
    // (a concurrent file creation in between sees a stricter mask, never a looser).
    let old = unsafe { nix::libc::umask(0o177) };
    let bound = tokio::net::UnixListener::bind(socket);
    // SAFETY: restores the value returned above; same reasoning as the first call.
    unsafe { nix::libc::umask(old) };
    let listener = bound?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The largest request line the keeper will buffer (a Spawn with its env, or a Write).
const MAX_REQUEST_LINE: usize = 4 * 1024 * 1024;
/// How long a connection may take to deliver its one request line.
const REQUEST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn serve_conn(me: &Arc<Keeper>, sock: tokio::net::UnixStream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let (mut rd, mut wr) = sock.into_split();
    // A request is one line. Bounded in size and in time, so a client that never
    // sends the newline (or never stops sending) cannot hold memory and a task for
    // the life of the keeper.
    let line = match tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        ldmux_protocol::socket::read_line(&mut rd, MAX_REQUEST_LINE),
    )
    .await
    {
        Ok(Ok(Some(l))) => String::from_utf8_lossy(&l).into_owned(),
        Ok(Ok(None)) => return Ok(()),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(std::io::Error::other(
                "timed out waiting for the request line",
            ));
        }
    };
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

/// The shell argv for sessions that name no command: `$SHELL` when it is set,
/// non-empty and executable; else the passwd entry's shell; else `/bin/sh`.
///
/// **Login-shell policy: not a login shell.** The argv is the bare program (no
/// `-l`, `argv[0]` not prefixed with `-`). The keeper is spawned by the daemon,
/// which already runs in the user's session with their environment (and the
/// `LDMUX_*` overlay); a login shell would re-run the profile files on every
/// pane and stack PATH entries onto an environment that was already built. A
/// user who wants one configures their rc file, or the daemon passes a `command`.
pub fn default_shell() -> Vec<String> {
    let passwd = nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|u| u.shell.to_string_lossy().into_owned());
    vec![pick_shell(
        std::env::var("SHELL").ok(),
        passwd,
        is_executable,
    )]
}

/// The choice in [`default_shell`], with the environment and the executable
/// check injected so it is testable.
pub fn pick_shell(
    env_shell: Option<String>,
    passwd_shell: Option<String>,
    ok: impl Fn(&str) -> bool,
) -> String {
    [env_shell, passwd_shell]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty() && ok(s))
        .unwrap_or_else(|| "/bin/sh".to_string())
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    // `metadata` follows symlinks (a distro `/bin/sh` is one).
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod shell_tests {
    use super::pick_shell;

    #[test]
    fn env_shell_wins_when_usable_else_passwd_else_sh() {
        let ok = |p: &str| p != "/nope";
        assert_eq!(
            pick_shell(Some("/bin/zsh".into()), Some("/bin/bash".into()), ok),
            "/bin/zsh"
        );
        assert_eq!(
            pick_shell(Some("".into()), Some("/bin/bash".into()), ok),
            "/bin/bash"
        );
        assert_eq!(
            pick_shell(Some("/nope".into()), Some("/bin/bash".into()), ok),
            "/bin/bash"
        );
        assert_eq!(pick_shell(None, Some("/nope".into()), ok), "/bin/sh");
        assert_eq!(pick_shell(None, None, ok), "/bin/sh");
    }

    #[test]
    fn executability_is_checked_on_disk() {
        assert!(super::is_executable("/bin/sh"));
        assert!(!super::is_executable("/etc/passwd"));
        assert!(!super::is_executable("/definitely/not/here"));
    }
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
        assert_eq!(
            parse_tpgid("42 (weird )name) S 1 42 42 34816 99 0 0"),
            Some(99)
        );
        // A plain one, and one with parens inside the name.
        assert_eq!(parse_tpgid("7 (bash) S 1 7 7 34816 7 0 0"), Some(7));
        assert_eq!(parse_tpgid("7 ((a)(b)) S 1 7 7 1 2"), Some(2));
        // Garbage is not a panic and not a wrong number.
        assert_eq!(parse_tpgid("no parens here"), None);
        assert_eq!(parse_tpgid(""), None);
    }
}

#[cfg(test)]
mod write_tests {
    use super::{locked_write, write_all};
    use nix::libc;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::sync::{Arc, Mutex};

    fn pipe(flags: libc::c_int) -> (std::fs::File, std::fs::File) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a writable `[c_int; 2]`, the array `pipe2` fills.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | flags) };
        assert_eq!(rc, 0);
        // SAFETY: `pipe2` succeeded, so both descriptors are open and
        // exclusively ours; each is wrapped in one `File`, which closes it once.
        unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                std::fs::File::from_raw_fd(fds[1]),
            )
        }
    }

    /// A non-blocking descriptor answers `EAGAIN` when full; that is "wait", not
    /// a failed write.
    #[test]
    fn write_all_waits_out_a_full_nonblocking_descriptor() {
        use std::os::fd::AsRawFd;
        let (mut rd, wr) = pipe(libc::O_NONBLOCK);
        // 1 MiB is far over the pipe's 64 KiB.
        let payload = vec![7u8; 1024 * 1024];
        let n = payload.len();
        let reader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            // The read end is non-blocking too: poll it.
            let mut got = 0usize;
            let mut buf = vec![0u8; 8192];
            while got < n {
                match rd.read(&mut buf) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            got
        });
        write_all(wr.as_raw_fd(), &payload).expect("the write must complete, not fail with EAGAIN");
        assert_eq!(reader.join().unwrap(), n);
    }

    /// A write that is still waiting on a slow reader finishes before the next one
    /// starts: the two never interleave.
    #[test]
    fn writes_to_one_pane_do_not_interleave() {
        use std::os::fd::AsRawFd;
        let (mut rd, wr) = pipe(0);
        let wr = Arc::new(wr);
        let lock = Arc::new(Mutex::new(()));
        let (a_len, b_len) = (300 * 1024usize, 16usize);
        let a = {
            let (wr, lock) = (wr.clone(), lock.clone());
            std::thread::spawn(move || {
                locked_write(&lock, wr.as_raw_fd(), &vec![b'a'; a_len]).unwrap()
            })
        };
        // A has filled the pipe and is blocked; B arrives behind it.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let b = {
            let (wr, lock) = (wr.clone(), lock.clone());
            std::thread::spawn(move || {
                locked_write(&lock, wr.as_raw_fd(), &vec![b'b'; b_len]).unwrap()
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut all = vec![0u8; a_len + b_len];
        rd.read_exact(&mut all).unwrap();
        a.join().unwrap();
        b.join().unwrap();
        let first_b = all.iter().position(|&c| c == b'b').unwrap();
        assert_eq!(first_b, a_len, "B's bytes must all follow A's");
        assert!(all[first_b..].iter().all(|&c| c == b'b'));
    }
}
