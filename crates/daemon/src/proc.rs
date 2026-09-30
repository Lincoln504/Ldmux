//! Running an external process, async, with both pipes drained concurrently.
//!
//! Port of `packages/daemon/src/proc.ts` (the source of record).
//!
//! Two properties matter and both are easy to get wrong:
//!
//! - **Asynchronous.** The daemon has one event loop shared by every reader;
//!   a synchronous spawn blocks all of them for the lifetime of the process.
//! - **Both pipes drained at once.** Reading stdout to completion before
//!   touching stderr deadlocks as soon as the child fills the one we are not
//!   reading.
//!
//! The `env` is passed explicitly rather than left to the default: a child
//! must see the environment as it is *now*, so a PATH changed at runtime (a
//! stubbed binary in tests, or a user's PATH fix) is honoured instead of a
//! snapshot taken at process start. (In Rust the inherited environment is
//! read at spawn time, which *is* "now"; `Some(env)` replaces it exactly.)
//!
//! The group-kill is the load-bearing part, and it is spelled out in
//! AGENTS.md: a wedged child may have started helpers of its own, and
//! signalling only the process we know about leaves those helpers holding the
//! pipes open — so the caller's read never ends. That is not hypothetical: it
//! turned a 300 ms timeout into a 300-second hang.

use nix::sys::signal::Signal;
use nix::unistd::Pid;
use std::process::Stdio;

/// A spawned child, in its own session/process group.
pub struct Spawned {
    pub pid: u32,
    child: tokio::process::Child,
}

/// Signal the child's whole process group, falling back to the child alone.
///
/// A negative pid means "the group": the child was started in its own
/// session, so its pgid is its own pid and every descendant is reached by
/// one call. Two caveats are inherent to the technique and accepted: the
/// child now leads its own session, so it outlives a SIGKILL of *this*
/// process (terminal layers are meant to), and a pid that has already been
/// reaped could in principle be recycled before the signal lands. Callers
/// only kill children they still hold.
pub fn kill_group(pid: u32, signal: Signal) {
    let raw = pid as i32;
    match nix::sys::signal::kill(Pid::from_raw(-raw), signal) {
        Ok(()) => {}
        // The group is already gone, or pid is unset: fall through.
        Err(_) => {
            let _ = nix::sys::signal::kill(Pid::from_raw(raw), signal);
        }
    }
}

impl Spawned {
    /// Terminate the child **and its descendants** with the given signal.
    pub fn kill(&self, signal: Signal) {
        kill_group(self.pid, signal);
    }

    /// The wait future for the child's exit status.
    pub fn wait<'a>(
        &'a mut self,
    ) -> impl std::future::Future<
        Output = std::io::Result<std::process::ExitStatus>,
    > + 'a {
        self.child.wait()
    }

    /// Wait for the child to exit **while** draining both pipes concurrently,
    /// returning (exit code, stdout, stderr).
    ///
    /// The ordering is the point: the child cannot exit until its pipes are
    /// drained, so waiting for exit and *then* reading the pipes deadlocks the
    /// moment a stream exceeds the kernel buffer. The pipes are read in their
    /// own tasks, so a full pipe never blocks the exit wait.
    ///
    /// A child killed mid-write ends the stream with an error we do not care
    /// about: resolve with what arrived rather than failing.
    pub async fn wait_drained(&mut self) -> (i64, String, String) {
        use tokio::io::AsyncReadExt;
        let out_pipe = self.child.stdout.take();
        let err_pipe = self.child.stderr.take();
        let out_f = async {
            let mut v = Vec::new();
            if let Some(mut r) = out_pipe {
                let _ = r.read_to_end(&mut v).await;
            }
            String::from_utf8_lossy(&v).into_owned()
        };
        let err_f = async {
            let mut v = Vec::new();
            if let Some(mut r) = err_pipe {
                let _ = r.read_to_end(&mut v).await;
            }
            String::from_utf8_lossy(&v).into_owned()
        };
        let (status, out, err) = tokio::join!(self.child.wait(), out_f, err_f);
        let code = match status {
            Ok(s) => s.code().unwrap_or(-1) as i64,
            Err(_) => -1,
        };
        (code, out, err)
    }

    /// Both pipes, drained as text, concurrently.
    ///
    /// A child killed mid-write ends the stream with an error we do not care
    /// about: resolve with what arrived rather than failing.
    pub async fn drain(&mut self) -> (String, String) {
        use tokio::io::AsyncReadExt;
        let out_r = self.child.stdout.take();
        let err_r = self.child.stderr.take();
        let (out, err) = tokio::join!(
            async {
                let mut v = Vec::new();
                if let Some(mut r) = out_r {
                    let _ = r.read_to_end(&mut v).await;
                }
                String::from_utf8_lossy(&v).into_owned()
            },
            async {
                let mut v = Vec::new();
                if let Some(mut r) = err_r {
                    let _ = r.read_to_end(&mut v).await;
                }
                String::from_utf8_lossy(&v).into_owned()
            },
        );
        (out, err)
    }
}

/// The options `spawn_captured` takes: the child's environment and directory.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// The child's environment, exactly. `None` inherits the live process
    /// environment (the child sees the environment as it is *now*).
    pub env: Option<Vec<(String, String)>>,
    /// The child's working directory.
    pub cwd: Option<String>,
}

/// Spawn, asynchronously, with stdin ignored, both pipes piped, and the child
/// in its own session (its own process group), so `kill_group` can take the
/// whole tree down.
///
/// The TS source reports spawn failures (ENOENT) *asynchronously* on the
/// child's error event; Rust surfaces them at spawn time, which is strictly
/// earlier — the observable behaviour (a spawn failure is reported, not
/// thrown) is the same.
pub fn spawn_captured(
    argv: &[String],
    opts: &SpawnOptions,
) -> std::io::Result<Spawned> {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    match &opts.env {
        Some(env) => {
            cmd.env_clear();
            for (k, v) in env {
                cmd.env(k, v);
            }
        }
        None => {}
    }
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    // Its own session + process group, so kill_group can take the whole tree
    // down.
    unsafe {
        cmd.pre_exec(|| match nix::unistd::setsid() {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        });
    }
    let child = cmd.spawn()?;
    let pid = child
        .id()
        .expect("tokio reports the pid for every successful spawn");
    Ok(Spawned { pid, child })
}
