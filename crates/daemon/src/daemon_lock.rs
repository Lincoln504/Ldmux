//! The daemon's single-instance lock (`ldmuxd.lock`).
//!
//! An exclusive, non-blocking `flock` on the file, with the daemon's pid written
//! into it for humans. A concurrent daemon that cannot take the flock refuses to
//! start ("ldmuxd already running").
//!
//! The lock is the kernel's, not the file's: it dies with the process, so a
//! crashed daemon leaves nothing to steal, and two daemons starting at once
//! cannot both win. (This was a pid file created `O_EXCL`: the stale-lock steal
//! was remove-then-recreate, which two daemons could both do, and its liveness
//! test read `/proc`, which is Linux-only.) It is the same mechanism the keeper
//! supervisor uses for `keeper.lock`.
//!
//! The file is **never unlinked**. Unlinking a flocked file races with an
//! acquirer that already opened the old inode: it would lock a file nobody else
//! can see while a second daemon creates a new one. Releasing is closing the
//! descriptor, which is also all a kill does.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;

/// The daemon's single-instance lock.
pub struct DaemonLock {
    lock_path: std::path::PathBuf,
    /// The open, flocked file; `Some` exactly while this process owns the lock.
    file: Option<File>,
}

impl DaemonLock {
    /// Create a lock for `<state_dir>/ldmuxd.lock`.
    pub fn new(state_dir: &std::path::Path) -> Self {
        Self {
            lock_path: state_dir.join("ldmuxd.lock"),
            file: None,
        }
    }

    /// Try to acquire the lock.
    ///
    /// Refuses with "ldmuxd already running" when another process holds it.
    /// Taking it is atomic, so there is no steal path to race on.
    pub fn acquire(&mut self) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        if self.file.is_some() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock_path)
            .map_err(|e| format!("open lock: {e}"))?;
        // SAFETY: `flock` on a descriptor this function owns for the call.
        let rc =
            unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            return if err.raw_os_error() == Some(nix::libc::EWOULDBLOCK) {
                Err("ldmuxd already running".to_string())
            } else {
                Err(format!("lock {}: {err}", self.lock_path.display()))
            };
        }
        // Ours now: record the pid (informational) over whatever a dead owner left.
        file.set_len(0).map_err(|e| format!("write lock: {e}"))?;
        write!(file, "{}", std::process::id()).map_err(|e| format!("write lock: {e}"))?;
        self.file = Some(file);
        Ok(())
    }

    /// Release the lock (close the descriptor). The file stays, see the module docs.
    pub fn release(&mut self) {
        self.file = None;
    }

    /// Does the daemon own the lock?
    pub fn owns(&self) -> bool {
        self.file.is_some()
    }

    /// The lock path (for tests).
    pub fn path(&self) -> &std::path::Path {
        &self.lock_path
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

    /// A second daemon that finds the lock held refuses to start.
    #[test]
    fn refuses_when_another_daemon_holds_the_lock() {
        let dir = state_dir();
        let mut lock = DaemonLock::new(&dir);
        lock.acquire().unwrap();

        let mut other = DaemonLock::new(&dir);
        let err = other.acquire().unwrap_err();
        assert!(err.contains("already running"), "got: {err}");
        assert!(!other.owns());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lock file a crashed daemon left behind (a dead pid in it, nobody
    /// holding the flock) is simply taken: there is no liveness probe to get wrong.
    #[test]
    fn a_lock_file_nobody_holds_is_taken_whatever_pid_it_names() {
        let dir = state_dir();
        std::fs::write(dir.join("ldmuxd.lock"), "999999999").unwrap();
        let mut lock = DaemonLock::new(&dir);
        lock.acquire().unwrap();
        assert!(lock.owns());
        assert_eq!(
            std::fs::read_to_string(dir.join("ldmuxd.lock")).unwrap(),
            std::process::id().to_string()
        );
        // ...and a live pid in the file does not stand in for the flock either.
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

        let mut b = DaemonLock::new(&dir);
        b.acquire().unwrap();
        assert!(b.owns());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Releasing must not take away a lock somebody else now holds. The old
    /// release removed the file by path, so a daemon whose lock had been taken
    /// (or that never owned it) deleted the new owner's lock and let a third
    /// daemon start beside it.
    #[test]
    fn releasing_never_takes_a_lock_someone_else_holds() {
        let dir = state_dir();
        let mut first = DaemonLock::new(&dir);
        first.acquire().unwrap();
        // A second handle that never owned it releases: nothing happens.
        let mut stranger = DaemonLock::new(&dir);
        stranger.release();
        let mut third = DaemonLock::new(&dir);
        assert!(third.acquire().is_err(), "the owner still holds it");
        // Once the owner lets go, the next one gets it.
        first.release();
        third.acquire().unwrap();
        // And dropping an old, released handle does not disturb the new owner.
        drop(first);
        let mut fourth = DaemonLock::new(&dir);
        assert!(fourth.acquire().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two daemons racing for the lock cannot both win.
    #[test]
    fn concurrent_acquirers_have_exactly_one_winner() {
        let dir = state_dir();
        let wins = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let held: std::sync::Arc<std::sync::Mutex<Vec<DaemonLock>>> = Default::default();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (dir, wins, barrier, held) =
                    (dir.clone(), wins.clone(), barrier.clone(), held.clone());
                std::thread::spawn(move || {
                    let mut l = DaemonLock::new(&dir);
                    barrier.wait();
                    if l.acquire().is_ok() {
                        wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        held.lock().unwrap().push(l);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
