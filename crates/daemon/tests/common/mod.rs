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
        Self {
            dir: dir.to_path_buf(),
            sock: sock.to_path_buf(),
        }
    }
}

/// Pids of `ldmux-keeper` processes whose first argument is exactly `sock`.
pub fn keepers_on(sock: &Path) -> Vec<i32> {
    keepers_matching(&sock.to_string_lossy())
}

/// Pids of `ldmux-keeper` processes whose socket path is `prefix` or sits under it.
///
/// The exact-argv match above misses two real cases: a keeper the daemon spawns in the last
/// milliseconds of a test (after the reaper's last pass), and one started with its socket named
/// differently inside the same scratch directory. A leaked keeper holds PTYs and a socket for
/// the life of the machine, which is exactly what this file exists to prevent, so the sweep is
/// by directory as well as by the exact path.
fn keepers_matching(prefix: &str) -> Vec<i32> {
    let want = prefix.to_owned();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let mut args = raw
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned());
        let prog = args.next().unwrap_or_default();
        let is_keeper = Path::new(&prog)
            .file_name()
            .is_some_and(|n| n == "ldmux-keeper");
        let sock = args.next().unwrap_or_default();
        let same = sock == want || sock.starts_with(&format!("{want}/"));
        if is_keeper && same {
            out.push(pid);
        }
    }
    out
}

impl Drop for Reaper {
    fn drop(&mut self) {
        // Several passes with a settle: a daemon that is still winding down may respawn one
        // between passes, and the last spawn can land after the first sweep.
        for _ in 0..6 {
            for pid in keepers_matching(&self.dir.to_string_lossy()) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
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

pub mod rig;
pub mod wsraw;

/// A scratch directory that removes itself, ready for a test to write into.
///
/// Every test that keeps state in a scratch directory leaks one directory per run unless the
/// directory removes itself, and several helpers had grown here without that: by 2026-11-07 the
/// temp dir on the machine this was measured on held 184 `ldmux-auth-*`, 146
/// `ldmux-keeper-supervisor-*`, 231 `ldmux-risk-*`, 73 `ldmux-qr-*` and 6187 `ldmux-daemon-core-*`
/// directories, and 2775 `ldmux-keeper-test-*` sockets. A `Drop` guard is the only version that
/// also cleans up when the test panics, which is when a leak matters most.
pub struct Scratch(PathBuf);

impl Scratch {
    /// Create a directory named `<prefix>-<unique>` under the temp dir.
    pub fn dir(prefix: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Self(p)
    }

    /// The path, for the call that needs an owned `PathBuf`.
    pub fn to_path_buf(&self) -> PathBuf {
        self.0.clone()
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}


impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
