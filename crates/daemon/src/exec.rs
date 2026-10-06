//! Running a short-lived external command from the CLI.
//!
//! Port of `packages/daemon/src/exec.ts` (the source of record).
//!
//! Same discipline as the terminal control plane: asynchronous (a CLI that
//! blocks its event loop cannot be tested against an in-process daemon),
//! both pipes drained concurrently (a full pipe would otherwise deadlock),
//! and bounded so a wedged binary cannot hang the command forever.

use crate::proc::{SpawnOptions, kill_group};
use nix::sys::signal::Signal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The outcome of one `run`.
pub struct ExecResult {
    /// The child's exit code, or -1 when spawn failed, it was signalled, or it
    /// was killed at the timeout.
    pub code: i64,
    pub out: String,
    pub err: String,
}

/// The options `run` takes.
#[derive(Debug, Clone, Default)]
pub struct ExecOpts {
    /// The bound on how long the command may run. `None` uses
    /// [`EXEC_TIMEOUT_MS`].
    pub timeout_ms: Option<u64>,
    /// The child's environment, exactly. `None` inherits the live process
    /// environment: a child must see the environment as it is *now*, so a
    /// PATH changed at runtime (a stubbed binary in tests, or a user's PATH
    /// fix) is honoured instead of a snapshot taken at process start — which
    /// once made tests run the real binary instead of the stub.
    pub env: Option<Vec<(String, String)>>,
}

/// The default bound: finite and sane.
pub const EXEC_TIMEOUT_MS: u64 = 10_000;
/// How long a command may ignore SIGTERM before it is killed outright.
pub const EXEC_KILL_GRACE_MS: u64 = 500;

/// Run `argv` to completion or the timeout, capturing both pipes.
///
/// Escalate, like the terminal control plane does: SIGTERM, then SIGKILL. A
/// SIGTERM alone is not a bound — a binary that traps or ignores it would hang
/// this call (and the daemon's work behind it) forever, which is exactly what
/// this function promises not to do.
pub async fn run(argv: Vec<String>, opts: ExecOpts) -> ExecResult {
    let timeout_ms = opts.timeout_ms.unwrap_or(EXEC_TIMEOUT_MS);
    let spawned = match crate::proc::spawn_captured(
        &argv,
        &SpawnOptions {
            env: opts.env,
            cwd: None,
        },
    ) {
        Ok(s) => s,
        // Missing binary, permission denied, …
        Err(e) => {
            return ExecResult {
                code: -1,
                out: String::new(),
                err: e.to_string(),
            };
        }
    };
    let mut spawned = spawned;
    let pid = spawned.pid;

    // Watchdog: SIGTERM at the deadline, SIGKILL after the grace. It runs in
    // its own task because the wait future must stay driven from here; the
    // flag is what tells a kill of ours apart from the child's own death.
    let timed_out_flag = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let flag = timed_out_flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            flag.store(true, Ordering::Relaxed);
            kill_group(pid, Signal::SIGTERM);
            tokio::time::sleep(std::time::Duration::from_millis(EXEC_KILL_GRACE_MS)).await;
            kill_group(pid, Signal::SIGKILL);
        })
    };
    let (code, out, err) = spawned.wait_drained().await;
    // The child is reaped: the watchdog must not outlive it, or it sleeps out the
    // whole timeout (a leaked task per call) and then signals a pid the kernel
    // may have given to something else.
    watchdog.abort();
    if timed_out_flag.load(Ordering::Relaxed) {
        return ExecResult {
            code: -1,
            out,
            err: format!("command timed out after {timeout_ms}ms"),
        };
    }
    ExecResult { code, out, err }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(c: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), c.to_string()]
    }

    #[tokio::test]
    async fn captures_stdout_stderr_and_a_zero_exit_code() {
        let res = run(sh("echo out; echo err >&2"), ExecOpts::default()).await;
        assert_eq!(res.code, 0);
        assert_eq!(res.out.trim(), "out");
        assert_eq!(res.err.trim(), "err");
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported_as_its_own_code_not_the_minus_one_sentinel() {
        let res = run(sh("exit 3"), ExecOpts::default()).await;
        assert_eq!(res.code, 3);
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_never_thrown() {
        let res = run(
            vec!["definitely-not-a-real-binary-ldmux".to_string()],
            ExecOpts::default(),
        )
        .await;
        assert_eq!(res.code, -1);
        assert!(!res.err.is_empty());
    }

    #[tokio::test]
    async fn both_pipes_are_drained_concurrently_so_a_full_pipe_cannot_deadlock() {
        // 1 MiB on each stream is far beyond the ~64 KiB kernel pipe buffer:
        // if either stream were read only after the other finished, this would
        // hang.
        let res = run(
            sh("head -c 1048576 /dev/zero | tr '\\0' a; head -c 1048576 /dev/zero | tr '\\0' b >&2"),
            ExecOpts::default(),
        )
        .await;
        assert_eq!(res.code, 0);
        assert_eq!(res.out.len(), 1048576);
        assert_eq!(res.err.len(), 1048576);
    }

    #[tokio::test]
    async fn a_wedged_command_is_killed_at_the_timeout_and_reports_it() {
        let started = std::time::Instant::now();
        let res = run(
            sh("sleep 30"),
            ExecOpts {
                timeout_ms: Some(250),
                env: None,
            },
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(res.code, -1);
        assert!(res.err.contains("timed out"), "got: {}", res.err);
        // The kill really happened rather than the command finishing on its
        // own.
        assert!(elapsed.as_millis() >= 250);
        assert!(elapsed.as_millis() < 5000);
    }

    #[tokio::test]
    async fn an_explicit_env_replaces_the_child_environment() {
        let mut env: Vec<(String, String)> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        env.push(("LDMUX_EXEC_TEST".to_string(), "injected".to_string()));
        let res = run(
            sh("printf %s \"$LDMUX_EXEC_TEST\""),
            ExecOpts {
                timeout_ms: None,
                env: Some(env),
            },
        )
        .await;
        assert_eq!(res.out, "injected");
    }

    #[test]
    fn the_default_timeout_is_a_finite_sane_bound() {
        const { assert!(EXEC_TIMEOUT_MS > 0) };
        const { assert!(EXEC_TIMEOUT_MS <= 60_000) };
    }

    /// The watchdog is cancelled when the command finishes: a fast command with
    /// a long timeout must return at once and leave nothing to signal later.
    #[tokio::test]
    async fn a_finished_command_does_not_leave_its_watchdog_armed() {
        let t0 = std::time::Instant::now();
        let res = run(
            sh("echo hi"),
            ExecOpts {
                timeout_ms: Some(60_000),
                env: None,
            },
        )
        .await;
        assert_eq!(res.code, 0);
        assert!(t0.elapsed().as_secs() < 5);
    }
}
