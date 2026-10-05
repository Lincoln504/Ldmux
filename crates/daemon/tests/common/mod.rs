//! Shared test plumbing: the keeper reaper.
//!
//! A live test that starts a real `ldmux-keeper` (directly, or by letting the
//! daemon auto-spawn one) must not leave it behind: the keeper is detached and
//! outlives its parent by design, so a panic before the test's own cleanup left
//! one holding PTYs and a socket for the life of the machine (41 were found
//! running on 2026-10-05, all from `maintenance.rs`).
//!
//! `Reaper` is created first in a test, so it drops last, on success *and* on
//! panic. It matches keepers by **exact** argv (`argv[0]` basename
//! `ldmux-keeper`, `argv[1]` == this test's own unique socket path), never by
//! a substring, so it cannot touch the deployed keeper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub struct Reaper {
    dir: PathBuf,
    sock: PathBuf,
}

impl Reaper {
    /// `dir` is the scratch dir (removed on drop); `sock` the keeper socket
    /// inside it that identifies this test's keepers.
    pub fn new(dir: &Path, sock: &Path) -> Self {
        Self { dir: dir.to_path_buf(), sock: sock.to_path_buf() }
    }
}

/// Pids of `ldmux-keeper` processes whose first argument is exactly `sock`.
pub fn keepers_on(sock: &Path) -> Vec<i32> {
    let want = sock.to_string_lossy().into_owned();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let mut args = raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned());
        let prog = args.next().unwrap_or_default();
        let is_keeper = Path::new(&prog).file_name().is_some_and(|n| n == "ldmux-keeper");
        if is_keeper && args.next().as_deref() == Some(want.as_str()) {
            out.push(pid);
        }
    }
    out
}

impl Drop for Reaper {
    fn drop(&mut self) {
        // Twice: a daemon that is still winding down may respawn one in between.
        for _ in 0..2 {
            for pid in keepers_on(&self.sock) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
