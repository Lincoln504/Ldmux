//! The daemon's single-instance lock (a pid lockfile, `ldmuxd.lock`).
//!
//! A file that contains the daemon's pid, created exclusively (O_CREAT | O_EXCL).
//! A concurrent daemon that finds the lock held by a *live* pid refuses to start;
//! one that finds it held by a *dead* pid steals it (the daemon crashed without
//! releasing).
//!
//! This is the §3.2 daemon-side piece. The keeper uses a `flock` (the kernel
//! releases it on death); the daemon's lock is a pid lockfile because the TS
//! daemon uses one and the parity suite exercises the steal path.

/// The daemon's single-instance lock (a pid lockfile, `ldmuxd.lock`).
pub struct DaemonLock {
    lock_path: std::path::PathBuf,
    owns: bool,
}

impl DaemonLock {
    /// Create a lock for `<state_dir>/ldmuxd.lock`.
    pub fn new(state_dir: &std::path::Path) -> Self {
        Self {
            lock_path: state_dir.join("ldmuxd.lock"),
            owns: false,
        }
    }

    /// Try to acquire the lock.
    ///
    /// Creates the file exclusively. If it exists, reads the recorded pid; if
    /// that pid is live, refuses with "ldmuxd already running"; if it is dead
    /// (or the file is unreadable), steals the lock.
    pub fn acquire(&mut self) -> Result<(), String> {
        if let Err(e) = Self::create_and_write(&self.lock_path) {
            if matches!(e.kind(), std::io::ErrorKind::AlreadyExists) {
                // Lock exists: steal only if the recorded pid is dead.
                let recorded = std::fs::read_to_string(&self.lock_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .unwrap_or(0);
                if recorded > 0 && is_process_alive(recorded) {
                    return Err("ldmuxd already running".to_string());
                }
                // Steal: remove the stale lock and retry.
                let _ = std::fs::remove_file(&self.lock_path);
                Self::create_and_write(&self.lock_path)
                    .map_err(|e| format!("create lock after steal: {e}"))?;
                self.owns = true;
                return Ok(());
            }
            return Err(format!("open lock: {e}"));
        }
        self.owns = true;
        Ok(())
    }

    /// Release the lock (remove the file).
    pub fn release(&mut self) {
        if !self.owns {
            return;
        }
        self.owns = false;
        let _ = std::fs::remove_file(&self.lock_path);
    }

    /// Does the daemon own the lock?
    pub fn owns(&self) -> bool {
        self.owns
    }

    /// The lock path (for tests).
    pub fn path(&self) -> &std::path::Path {
        &self.lock_path
    }

    /// Create the lock file exclusively and write our pid into it.
    fn create_and_write(path: &std::path::Path) -> Result<(), std::io::Error> {
        use std::io::Write;
        let mut fd = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        if let Err(e) = write!(fd, "{}", std::process::id()) {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(())
    }
}

/// Is the process alive? On Linux, `/proc/<pid>/` exists iff the process is
/// alive — the same liveness contract as the TS daemon's `process.kill(pid, 0)`
/// (a process that exists is signalable, one that doesn't is not). A dead pid
/// leaves no `/proc` entry, so the lock is steal-able.
fn is_process_alive(pid: i32) -> bool {
    std::path::Path::new("/proc").join(pid.to_string()).exists()
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_dir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ldmux-daemon-lock-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A second daemon that finds a *live* lock refuses to start.
    #[test]
    fn refuses_when_a_live_pid_holds_the_lock() {
        let dir = state_dir();
        let mut lock = DaemonLock::new(&dir);
        lock.acquire().unwrap();

        // A concurrent daemon sees a live pid (ours) and refuses.
        let mut other = DaemonLock::new(&dir);
        let err = other.acquire().unwrap_err();
        assert!(err.contains("already running"), "got: {err}");
        assert!(!other.owns());

        // A daemon that sees a *dead* pid steals the lock.
        let mut dead = DaemonLock::new(&dir);
        // Rewrite the lock with a dead pid (a crashed daemon that didn't release).
        std::fs::write(dir.join("ldmuxd.lock"), "999999999").unwrap();
        dead.acquire().unwrap();
        assert!(dead.owns());
        assert_eq!(std::fs::read_to_string(dir.join("ldmuxd.lock")).unwrap(), format!("{}", std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Acquiring, releasing, and re-acquiring is clean.
    #[test]
    fn acquire_release_reacquire_is_clean() {
        let dir = state_dir();
        let mut a = DaemonLock::new(&dir);
        a.acquire().unwrap();
        assert!(a.owns());
        a.release();
        assert!(!a.owns());
        assert!(!dir.join("ldmuxd.lock").exists());

        let mut b = DaemonLock::new(&dir);
        b.acquire().unwrap();
        assert!(b.owns());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
