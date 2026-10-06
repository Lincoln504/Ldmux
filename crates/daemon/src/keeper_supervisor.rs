//! The daemon's keeper auto-spawn (the ssh-agent pattern, plan §3.2).
//!
//! The daemon auto-spawns a detached keeper (`setsid`, own session) on first
//! need, and re-spawns if it finds it dead. The keeper is found via socket +
//! pid: the **socket connect is the authoritative liveness test** (plan §3.2) —
//! a connect succeeds iff the keeper is listening. A non-blocking `flock` on
//! `<stateDir>/keeper.lock` serializes concurrent spawn attempts (the kernel
//! releases it on death, unlike a pidfile); the pid is read *inside* the lock.
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
        }
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
            self.spawn_detached().await?;
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
    use nix::sys::signal;
    use nix::unistd::Pid;

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
}
