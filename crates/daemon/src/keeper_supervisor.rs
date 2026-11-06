//! The daemon's keeper auto-spawn (the ssh-agent pattern, plan §3.2).
//!
//! The daemon auto-spawns a detached keeper (`setsid`, own session) on first
//! need, and re-spawns if it finds it dead. The keeper is found via socket +
//! pid: the **socket connect is the authoritative liveness test** (plan §3.2) —
//! a connect succeeds iff the keeper is listening. A non-blocking `flock` on
//! `<stateDir>/keeper.lock` serializes concurrent spawn attempts (the kernel
//! releases it on death, unlike a pidfile); the pid is read *inside* the lock.
//!
//! **Two ways to start a keeper (ADR-0021).** When `LDMUX_KEEPER_UNIT` names a
//! systemd user unit and `systemctl` is found, "ensure the keeper" is
//! `systemctl --user start <unit>` (idempotent) and then the same wait on the
//! socket: the keeper is a unit, not the daemon's child, so its shells are not
//! charged to the daemon. Without it (a machine without systemd, every test) the
//! daemon spawns the keeper detached as before. The socket connect stays the only
//! liveness test and the flock still serializes concurrent daemons in both modes;
//! the keeper itself refuses to bind a socket a live keeper serves, so a unit
//! start racing a detached spawn can never leave two.
//!
//! New (no TS source of record): the keeper does not exist in the TS tree. The
//! TS daemon's `ldmuxd.lock` is the *daemon's* single-instance lock (a `wx`
//! create + recorded pid); this is a *different* lock — the keeper's.

use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

/// The keeper auto-spawn (the ssh-agent pattern, plan §3.2).
pub struct KeeperSupervisor {
    state_dir: PathBuf,
    socket_path: PathBuf,
    lock_path: PathBuf,
    keeper_bin: PathBuf,
    /// The pid of the last keeper we spawned (`0` if we have not spawned one).
    /// Exposed so a caller (a test) can stop a detached keeper; the daemon
    /// itself never signals it (the keeper outlives the daemon).
    last_pid: AtomicU32,
    /// The systemd user unit that runs the keeper and the absolute `systemctl`,
    /// when the keeper is started through systemd (see the module docs).
    unit: Option<KeeperUnit>,
    /// Consecutive keeper calls that reached a listening keeper and got no answer.
    ///
    /// `is_alive` is a socket connect, and **a wedged keeper still accepts**: it binds, accepts,
    /// and never answers. Liveness therefore said "alive" for ever while every shell was
    /// unreachable, and only restarting the service brought the daemon back - the shell list just
    /// stopped updating with nothing to say. One silent call is a slow reply under load; several
    /// in a row from the periodic tick is a wedged process, and the only cure is a new one.
    mute_streak: AtomicU32,
}

/// How many consecutive unanswered keeper calls mean a wedged process rather than a slow one.
pub const MUTE_LIMIT: u32 = 3;

/// How to start the keeper through systemd.
#[derive(Debug, Clone)]
pub struct KeeperUnit {
    /// The unit name (`ldmux-keeper.service`).
    pub name: String,
    /// Absolute path of `systemctl`.
    pub systemctl: PathBuf,
}

impl KeeperUnit {
    /// From `LDMUX_KEEPER_UNIT` and `PATH` (plus `/usr/bin:/bin`); `None` when the
    /// variable is unset/empty or no executable `systemctl` is found.
    pub fn from_env() -> Option<Self> {
        Self::resolve(
            std::env::var("LDMUX_KEEPER_UNIT").ok(),
            std::env::var("PATH").unwrap_or_default(),
            |p| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            },
        )
    }

    /// [`from_env`](Self::from_env) with the inputs injected.
    pub fn resolve(
        unit: Option<String>,
        path: String,
        executable: impl Fn(&Path) -> bool,
    ) -> Option<Self> {
        let name = unit.filter(|u| !u.trim().is_empty())?.trim().to_string();
        let systemctl = path
            .split(':')
            .chain(["/usr/bin", "/bin"])
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("systemctl"))
            .find(|p| executable(p))?;
        Some(Self { name, systemctl })
    }
}

impl KeeperSupervisor {
    /// Create a supervisor for the keeper at `state_dir/keeper.sock`, spawned
    /// from `keeper_bin`.
    pub fn new(state_dir: PathBuf, keeper_bin: PathBuf) -> Self {
        let socket_path = state_dir.join("keeper.sock");
        let lock_path = state_dir.join("keeper.lock");
        Self {
            state_dir,
            socket_path,
            lock_path,
            keeper_bin,
            last_pid: AtomicU32::new(0),
            unit: KeeperUnit::from_env(),
            mute_streak: AtomicU32::new(0),
        }
    }

    /// Start the keeper through `unit` (`None`: spawn it detached). [`new`](Self::new)
    /// reads `LDMUX_KEEPER_UNIT`; this is for callers that already know.
    #[must_use]
    pub fn with_unit(mut self, unit: Option<KeeperUnit>) -> Self {
        self.unit = unit;
        self
    }

    /// The pid of the last keeper we spawned (`0` if we have not spawned one).
    pub fn last_pid(&self) -> u32 {
        self.last_pid.load(Ordering::SeqCst)
    }

    /// The daemon's state dir (the keeper's socket and lock live under it).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The keeper's socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The keeper's binary path.
    pub fn keeper_bin(&self) -> &Path {
        &self.keeper_bin
    }

    /// The keeper's lock path.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Is the keeper alive? The socket connect is the authoritative liveness
    /// test (plan §3.2): a connect succeeds iff the keeper is listening.
    ///
    /// Connecting (and closing) is safe: the keeper's serve loop handles a clean
    /// EOF on a connection that sends no message (it reads one line, gets EOF,
    /// and drops the connection).
    ///
    /// The keeper's `bind` creates the socket file a moment before its
    /// accept-loop is scheduled to answer the handshake, so a connect can lose
    /// a race against a freshly bound keeper. We retry briefly: a connect that
    /// keeps failing is a dead keeper (no process will ever accept), so the
    /// retries collapse into the 1 s budget; a live keeper answers within a
    /// poll or two.
    pub async fn is_alive(&self) -> bool {
        // **No socket file means no keeper, so do not spend the retry budget.**
        // The retries exist to lose a race against a keeper that has bound but not
        // yet scheduled its accept loop — a keeper that has not written its file at
        // all cannot be that, and the twenty attempts cost half a second of every
        // caller that asks about a dead one. That is a fifth of a second per call
        // on a machine where the answer is already certain.
        if !self.socket_path.exists() {
            return false;
        }
        for _ in 0..20 {
            if tokio::net::UnixStream::connect(&self.socket_path)
                .await
                .is_ok()
            {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        false
    }

    /// Count a call that timed out, and say whether the keeper has to be replaced.
    pub fn note_mute(&self) -> bool {
        self.mute_streak.fetch_add(1, Ordering::SeqCst) + 1 >= MUTE_LIMIT
    }

    /// A keeper answered: whatever it was doing, it is doing its job.
    pub fn note_answer(&self) {
        self.mute_streak.store(0, Ordering::SeqCst);
    }

    /// Replace a keeper that accepts connections and never answers.
    ///
    /// This is destructive by design and is only ever reached from the periodic tick: the
    /// keeper's children die with it, so every shell ends. That is the lesser harm - a wedged
    /// keeper has already made every shell unreachable, and leaving it in place means the daemon
    /// reports a list that never changes and nothing ever recovers. A dead keeper needs none of
    /// this: `keeper_call` respawns it already.
    ///
    /// **Under systemd there is no pid to signal.** The keeper is a unit, not this process's
    /// child, so `last_pid` is 0 and the old body skipped the kill, called `note_answer`, and
    /// returned `Ok`: the mute detector said "keeper stopped answering; replacing it" and
    /// replaced nothing, for ever (RELEASE-AUDIT 8.5). In unit mode the cure is the unit's own
    /// `restart`; a keeper we did spawn ourselves (the detached fallback) is killed by pid.
    ///
    /// Success is **an answer**, not a listening socket: a wedged keeper still accepts
    /// connections, so a connect would certify the process being replaced.
    pub async fn replace_wedged(&self) -> Result<(), String> {
        if let Some(u) = &self.unit {
            match self.restart_unit(u).await {
                Ok(()) => {
                    if self.wait_until_answering().await {
                        self.note_answer();
                        return Ok(());
                    }
                    eprintln!(
                        "ldmuxd: {} was restarted but its keeper still does not answer",
                        u.name
                    );
                }
                Err(e) => eprintln!("ldmuxd: {e}; replacing the keeper directly"),
            }
            // A failed restart usually means the user manager is unreachable, so the unit is not
            // what is holding the socket either. Stop it if that is possible at all, then fall
            // through: the pid below is a detached keeper we spawned in an earlier fallback.
            let _ = self.stop_unit(u).await;
        }
        let pid = self.last_pid();
        if pid != 0 {
            let p = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGTERM);
            for _ in 0..20 {
                if !self.is_alive().await {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            // A process that never answered may never read its default signal either.
            let _ = nix::sys::signal::kill(p, nix::sys::signal::Signal::SIGKILL);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        self.ensure().await?;
        if !self.wait_until_answering().await {
            return Err("replaced the keeper but the new one does not answer".into());
        }
        self.note_answer();
        Ok(())
    }

    /// Did the keeper answer a real call, not merely accept a connection?
    ///
    /// One `List` with a short budget: it is the cheapest request the keeper serves, it changes
    /// nothing, and a keeper that answers it is doing its job. A connect is not evidence - that
    /// is the whole defect this method exists to work around.
    pub async fn keeper_answers(&self) -> bool {
        let sock = self.socket_path.to_string_lossy().into_owned();
        ldmux_protocol::keeper_call::keeper_call(
            &sock,
            &ldmux_protocol::keeper::KeeperMessage::List(ldmux_protocol::keeper::List {}),
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .is_ok()
    }

    /// Poll `keeper_answers` until it answers or the budget runs out. `false` means the keeper
    /// on the socket is still wedged.
    async fn wait_until_answering(&self) -> bool {
        // Each attempt already carries the keeper call's own timeout; this only bounds how long
        // we keep trying, and it is a real wait only when the answer is "no".
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if self.keeper_answers().await {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Ensure the keeper is running (auto-spawn if absent).
    pub async fn ensure(&self) -> Result<(), String> {
        // Fast path: if the keeper is alive, return.
        if self.is_alive().await {
            return Ok(());
        }
        // Slow path: acquire the flock, re-check, spawn if dead.
        let _lock = self.acquire_lock().await?;
        // Re-check (another daemon may have spawned it while we waited).
        if !self.is_alive().await {
            match &self.unit {
                Some(u) => {
                    if let Err(e) = self.start_unit(u).await {
                        // The unit may be missing or the user manager unreachable.
                        // The keeper must still come up: fall back to a detached
                        // spawn, unless the unit's keeper answered after all.
                        eprintln!("ldmuxd: {e}; spawning the keeper directly");
                        if !self.is_alive().await {
                            self.spawn_detached().await?;
                        }
                    }
                }
                None => self.spawn_detached().await?,
            }
        }
        drop(_lock);
        // Wait for the keeper to answer (it binds the socket on start).
        self.wait_for_keeper().await?;
        Ok(())
    }

    /// Acquire the flock (non-blocking exclusive), retrying while another
    /// daemon holds it (a short critical section). Returns an `OwnedFd` held
    /// for the critical section; the kernel releases the flock on drop.
    async fn acquire_lock(&self) -> Result<std::os::fd::OwnedFd, String> {
        let fd = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(|e| format!("open keeper.lock: {e}"))?;
        let raw = fd.as_raw_fd();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // SAFETY: `raw` is the descriptor of `fd`, which is open and owned by
            // this function until it is returned (as an `OwnedFd`) after the lock
            // succeeds; `flock` takes no pointers.
            match unsafe { nix::libc::flock(raw, nix::libc::LOCK_EX | nix::libc::LOCK_NB) } {
                0 => return Ok(std::os::fd::OwnedFd::from(fd)),
                _ => {
                    if std::time::Instant::now() > deadline {
                        return Err("could not acquire keeper.lock within 5 s".into());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
    }

    /// `systemctl --user start <unit>`: idempotent, and returns once the service
    /// process is forked (Type=simple), after which the caller waits on the socket.
    async fn start_unit(&self, u: &KeeperUnit) -> Result<(), String> {
        self.run_systemctl(u, "start").await
    }

    /// `systemctl --user restart <unit>`: the only way to replace a keeper this daemon did not
    /// spawn. `restart` also starts a stopped unit, so it covers "the unit is not running".
    async fn restart_unit(&self, u: &KeeperUnit) -> Result<(), String> {
        self.run_systemctl(u, "restart").await
    }

    /// `systemctl --user stop <unit>`, best effort: used before a direct replacement so the
    /// unit does not race us for the socket.
    async fn stop_unit(&self, u: &KeeperUnit) -> Result<(), String> {
        self.run_systemctl(u, "stop").await
    }

    /// One `systemctl --user <verb> <unit>`, bounded.
    async fn run_systemctl(&self, u: &KeeperUnit, verb: &str) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&u.systemctl);
        cmd.args(["--user", verb, &u.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let run = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output());
        match run.await {
            Err(_) => Err(format!("systemctl --user {verb} {} timed out", u.name)),
            Ok(Err(e)) => Err(format!("run {}: {e}", u.systemctl.display())),
            Ok(Ok(o)) if o.status.success() => Ok(()),
            Ok(Ok(o)) => Err(format!(
                "systemctl --user {verb} {} failed ({}): {}",
                u.name,
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
        }
    }

    /// Spawn the keeper detached (`setsid`, own session), with the socket path
    /// as its argument. The keeper outlives the daemon: the `Child` handle is
    /// dropped (not waited for), so the daemon does not block on its exit.
    async fn spawn_detached(&self) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&self.keeper_bin);
        cmd.arg(&self.socket_path);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        // Its own session (setsid), so it outlives the daemon.
        // SAFETY: the `pre_exec` closure runs in the forked child before exec,
        // where only async-signal-safe calls are allowed. It captures nothing
        // and calls `setsid(2)` (async-signal-safe, no allocation); the error
        // path turns an errno into `io::Error::Os` without allocating.
        unsafe {
            cmd.pre_exec(|| match nix::unistd::setsid() {
                Ok(_) => Ok(()),
                Err(e) => Err(e.into()),
            });
        }
        let child = cmd.spawn().map_err(|e| format!("spawn keeper: {e}"))?;
        self.last_pid
            .store(child.id().unwrap_or(0), Ordering::SeqCst);
        // Detach: do not wait — the keeper runs to process exit.
        drop(child);
        Ok(())
    }

    /// Wait until the keeper **answers** on its socket, not merely until the
    /// socket file exists.
    ///
    /// The file is the wrong test twice over: a keeper that was killed leaves
    /// its socket file behind, so "exists" is true before the fresh keeper has
    /// bound anything, and the first caller then connects into a file nobody
    /// is listening on. `is_alive` is the connect that answers the real
    /// question, and it already retries briefly for a keeper that has bound
    /// but not yet reached its accept loop.
    async fn wait_for_keeper(&self) -> Result<(), String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if self.is_alive().await {
                return Ok(());
            }
            if std::time::Instant::now() > deadline {
                return Err("keeper did not answer on its socket within 5 s".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::keeper::{KeeperMessage, List};
    use nix::sys::signal;
    use nix::unistd::Pid;

    /// Three mutes in a row is a wedged keeper; one is a slow reply.
    #[test]
    fn a_mute_streak_only_reaches_the_limit_on_the_third_silence() {
        let dir = std::env::temp_dir().join(format!("ldmux-mute-{}", std::process::id()));
        let sup = KeeperSupervisor::new(dir, PathBuf::from("/nonexistent"));
        assert!(!sup.note_mute(), "one slow call is not a wedge");
        assert!(!sup.note_mute());
        assert!(sup.note_mute(), "the third is");
        // Any answer clears it: a keeper that came back is not replaced.
        sup.note_answer();
        assert!(!sup.note_mute(), "the streak starts again");
    }

    /// The `ldmux-keeper` binary, in the target dir next to this test binary.
    fn keeper_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let deps = exe.parent().unwrap(); // …/deps
        let target = deps.parent().unwrap(); // …/ (e.g. target/debug)
        let bin = target.join("ldmux-keeper");
        assert!(bin.exists(), "keeper binary not found at {bin:?}");
        bin
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ldmux-keeper-supervisor-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Stop a detached keeper by pid (it is a single process, not a group).
    async fn kill_keeper(sup: &KeeperSupervisor) {
        let pid = sup.last_pid();
        if pid != 0 {
            let _ = signal::kill(Pid::from_raw(pid as i32), signal::Signal::SIGTERM);
        }
        for _ in 0..500 {
            if !sup.is_alive().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A keeper that accepts and never answers is replaced, because nothing else notices it.
    ///
    /// `SIGSTOP` is the honest reproduction: the process is alive, its socket is bound and the
    /// kernel queues connections to it, so `is_alive` says yes for ever, and every request waits
    /// out the full timeout. That is what "the keeper wedged" means, and before this nothing in
    /// the daemon could tell it from a healthy keeper.
    #[tokio::test]
    async fn a_stopped_keeper_is_replaced_and_the_new_one_answers() {
        let state_dir = scratch_dir();
        let sup = KeeperSupervisor::new(state_dir, keeper_bin());
        sup.ensure().await.unwrap();
        let pid = sup.last_pid();
        assert_ne!(pid, 0);
        signal::kill(Pid::from_raw(pid as i32), signal::Signal::SIGSTOP).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(sup.is_alive().await, "a stopped keeper still accepts connections");

        sup.replace_wedged().await.unwrap();

        assert!(sup.is_alive().await, "a new keeper is listening");
        assert_ne!(sup.last_pid(), pid, "and it is not the stopped one");
        let sock = sup.socket_path().to_str().unwrap().to_string();
        let answered = ldmux_protocol::keeper_call::keeper_call(
            &sock,
            &KeeperMessage::List(List {}),
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await;
        assert!(
            matches!(answered, Ok(KeeperMessage::ListResult(_))),
            "the new keeper answers: {answered:?}"
        );
        kill_keeper(&sup).await;
    }

    /// The ssh-agent pattern (plan §3.2): `ensure` auto-spawns a dead keeper,
    /// and re-spawns one we stopped. The socket connect is the liveness probe.
    #[tokio::test]
    async fn auto_spawns_and_re_spawns_the_keeper() {
        let state_dir = scratch_dir();
        let sup = KeeperSupervisor::new(state_dir.clone(), keeper_bin());

        // Initially dead.
        assert!(!sup.is_alive().await);

        // `ensure` auto-spawns the keeper.
        sup.ensure().await.unwrap();
        assert!(sup.is_alive().await);
        assert!(sup.last_pid() != 0, "a keeper should have been spawned");

        // Stop it.
        kill_keeper(&sup).await;
        assert!(!sup.is_alive().await, "the keeper should be dead");

        // `ensure` re-spawns it (the ssh-agent pattern).
        sup.ensure().await.unwrap();
        assert!(sup.is_alive().await);
        assert!(sup.last_pid() != 0, "a keeper should have been re-spawned");

        // Cleanup: stop the keeper, drop the scratch dir.
        kill_keeper(&sup).await;
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    /// An idempotent `ensure` against a live keeper is a no-op (no re-spawn).
    #[tokio::test]
    async fn ensure_is_idempotent_on_a_live_keeper() {
        let state_dir = scratch_dir();
        let sup = KeeperSupervisor::new(state_dir.clone(), keeper_bin());

        sup.ensure().await.unwrap();
        assert!(sup.is_alive().await);
        let first = sup.last_pid();

        // A second `ensure` against a live keeper is a no-op (no re-spawn).
        sup.ensure().await.unwrap();
        assert!(sup.last_pid() == first, "a live keeper is not re-spawned");

        // Cleanup.
        kill_keeper(&sup).await;
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    /// Write an executable stub `systemctl` that records its arguments in `mark`
    /// (proof it ran) and then runs `body`.
    fn stub_systemctl(dir: &Path, mark: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("systemctl");
        std::fs::write(
            &p,
            format!("#!/bin/sh\necho \"$@\" >> {}\n{body}\n", mark.display()),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn stop_pidfile(pidfile: &Path) {
        if let Some(pid) = std::fs::read_to_string(pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            let _ = signal::kill(Pid::from_raw(pid), signal::Signal::SIGTERM);
        }
    }

    /// Unit mode: `ensure` runs `systemctl --user start <unit>` and then waits on the
    /// socket; the daemon does not spawn the keeper itself.
    #[tokio::test]
    async fn unit_mode_starts_the_keeper_through_systemctl() {
        let state_dir = scratch_dir();
        let (mark, pidfile) = (state_dir.join("mark"), state_dir.join("pid"));
        let body = format!(
            "{} {} </dev/null >/dev/null 2>&1 &\necho $! > {}",
            keeper_bin().display(),
            state_dir.join("keeper.sock").display(),
            pidfile.display()
        );
        let systemctl = stub_systemctl(&state_dir, &mark, &body);
        let sup =
            KeeperSupervisor::new(state_dir.clone(), keeper_bin()).with_unit(Some(KeeperUnit {
                name: "ldmux-keeper.service".into(),
                systemctl,
            }));
        sup.ensure().await.unwrap();
        assert!(sup.is_alive().await);
        assert_eq!(
            std::fs::read_to_string(&mark).unwrap().trim(),
            "--user start ldmux-keeper.service",
            "the stub systemctl must have run"
        );
        assert_eq!(sup.last_pid(), 0, "the daemon did not spawn it");

        // A live keeper: no second `start`.
        sup.ensure().await.unwrap();
        assert_eq!(std::fs::read_to_string(&mark).unwrap().lines().count(), 1);

        stop_pidfile(&pidfile);
        for _ in 0..500 {
            if !sup.is_alive().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    /// A failing `systemctl` (no unit, no user manager) must not leave the daemon
    /// without a keeper: it falls back to the detached spawn.
    #[tokio::test]
    async fn unit_mode_falls_back_to_a_detached_spawn_when_systemctl_fails() {
        let state_dir = scratch_dir();
        let mark = state_dir.join("mark");
        let systemctl = stub_systemctl(&state_dir, &mark, "echo no such unit >&2; exit 5");
        let sup =
            KeeperSupervisor::new(state_dir.clone(), keeper_bin()).with_unit(Some(KeeperUnit {
                name: "ldmux-keeper.service".into(),
                systemctl,
            }));
        sup.ensure().await.unwrap();
        assert!(sup.is_alive().await);
        assert!(mark.exists(), "the stub systemctl must have run");
        assert!(sup.last_pid() != 0, "the fallback spawned the keeper");
        kill_keeper(&sup).await;
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    /// A `systemctl` stub whose body can dispatch on `$2` (the verb), spawn a real keeper, and
    /// record every invocation in `mark`.
    fn stub_unit(dir: &Path, body: &str) -> PathBuf {
        stub_systemctl(dir, &dir.join("mark"), body)
    }

    /// Unit mode: a wedged keeper is replaced by restarting the unit. `last_pid` is 0 here
    /// (the keeper is not our child), and that used to mean "skip the kill, report success, and
    /// replace nothing" for ever.
    #[tokio::test]
    async fn unit_mode_replaces_a_wedged_keeper_by_restarting_the_unit() {
        let state_dir = scratch_dir();
        let pidfile = state_dir.join("pid");
        let body = format!(
            "case \"$2\" in\n  restart) kill -9 \"$(cat {pid})\" 2>/dev/null ;;\nesac\n{} {} </dev/null >/dev/null 2>&1 &\necho $! > {pid}",
            keeper_bin().display(),
            state_dir.join("keeper.sock").display(),
            pid = pidfile.display()
        );
        let systemctl = stub_unit(&state_dir, &body);
        let sup =
            KeeperSupervisor::new(state_dir.clone(), keeper_bin()).with_unit(Some(KeeperUnit {
                name: "ldmux-keeper.service".into(),
                systemctl,
            }));
        sup.ensure().await.unwrap();
        assert_eq!(sup.last_pid(), 0, "unit mode: the daemon did not spawn it");
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        signal::kill(Pid::from_raw(pid), signal::Signal::SIGSTOP).unwrap();
        assert!(sup.is_alive().await, "a stopped keeper still accepts connections");

        sup.replace_wedged().await.unwrap();

        let calls = std::fs::read_to_string(state_dir.join("mark")).unwrap();
        assert!(
            calls.contains("--user restart ldmux-keeper.service"),
            "the unit must be restarted: {calls:?}"
        );
        assert!(
            sup.keeper_answers().await,
            "the replacement answers a real call, not just a connect"
        );
        assert_eq!(sup.last_pid(), 0, "and it is the unit's, not a detached spawn");
        stop_pidfile(&pidfile);
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    /// A unit whose `restart` fails (no user manager) must not leave the daemon without a
    /// keeper: the fallback is the same start-or-spawn path `ensure` owns.
    #[tokio::test]
    async fn a_failed_unit_restart_still_leaves_a_keeper_that_answers() {
        let state_dir = scratch_dir();
        let pidfile = state_dir.join("pid");
        let body = format!(
            "case \"$2\" in\n  restart) echo restart refused >&2; exit 5 ;;\n  stop) echo stop refused >&2; exit 5 ;;\nesac\n{} {} </dev/null >/dev/null 2>&1 &\necho $! > {pid}",
            keeper_bin().display(),
            state_dir.join("keeper.sock").display(),
            pid = pidfile.display()
        );
        let systemctl = stub_unit(&state_dir, &body);
        let sup =
            KeeperSupervisor::new(state_dir.clone(), keeper_bin()).with_unit(Some(KeeperUnit {
                name: "ldmux-keeper.service".into(),
                systemctl,
            }));
        sup.ensure().await.unwrap();
        // Dead, not wedged: killed outright, so its socket file is left behind and nothing
        // listens on it.
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        signal::kill(Pid::from_raw(pid), signal::Signal::SIGKILL).unwrap();
        for _ in 0..500 {
            if !sup.is_alive().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!sup.is_alive().await);

        sup.replace_wedged().await.unwrap();

        let calls = std::fs::read_to_string(state_dir.join("mark")).unwrap();
        assert!(
            calls.contains("--user restart ldmux-keeper.service"),
            "{calls:?}"
        );
        assert!(sup.keeper_answers().await, "a new keeper answers");
        stop_pidfile(&pidfile);
        let _ = std::fs::remove_dir_all(&state_dir);
    }

    #[test]
    fn the_unit_comes_from_the_env_with_an_absolute_systemctl() {
        let exe = |p: &Path| p == Path::new("/usr/bin/systemctl");
        let u = KeeperUnit::resolve(Some("k.service".into()), "rel:/nope".into(), exe).unwrap();
        assert_eq!(u.name, "k.service");
        assert_eq!(u.systemctl, Path::new("/usr/bin/systemctl"));
        assert!(KeeperUnit::resolve(None, "/usr/bin".into(), exe).is_none());
        assert!(KeeperUnit::resolve(Some(" ".into()), "/usr/bin".into(), exe).is_none());
        assert!(KeeperUnit::resolve(Some("k".into()), "/usr/bin".into(), |_| false).is_none());
    }
}
