//! Putting each shell in its own transient systemd scope (ADR-0021).
//!
//! Under a systemd user manager the keeper is a unit (`ldmux-keeper.service`), so
//! every shell it forks, and everything a shell ever daemonizes, would be charged
//! to that one unit. Instead each shell is moved into its own `.scope` under
//! `ldmux-shells.slice`: memory is attributed per session, and `systemctl status`
//! for the keeper shows the keeper.
//!
//! **The mechanism is `StartTransientUnit` with `PIDs=[shell pid]`, called from the
//! keeper after the fork, through `busctl`.** Not `systemd-run --scope` in the
//! child: that exec's the command after it has made the scope, so "systemd-run
//! failed" and "the shell ran and exited" are the same exit status and a fallback
//! could start the shell twice. Here the shell is already running when the scope is
//! requested, so a failure of any kind (no bus, name collision, the shell already
//! gone) leaves it exactly where it was, in the keeper's own cgroup, and is logged.
//! A shell is never refused or delayed beyond the call's timeout for want of a scope.
//!
//! The scope is **not** stopped when the shell is killed: a job the user
//! daemonized keeps running, as it always has, now in its own scope. `CollectMode`
//! lets systemd drop the scope once its last process has gone.

use std::path::{Path, PathBuf};

/// Where and how to ask for a scope. Present only under a systemd user manager.
#[derive(Debug, Clone)]
pub struct ScopeEnv {
    /// Absolute path of `busctl`, resolved once at keeper start.
    pub busctl: PathBuf,
    /// The slice the scopes live in (`ldmux-shells.slice`).
    pub slice: String,
}

/// The default slice. `LDMUX_SCOPE_SLICE` overrides it (tests use a throwaway one).
pub const DEFAULT_SLICE: &str = "ldmux-shells.slice";

impl ScopeEnv {
    /// Detect from the process environment; `None` means "spawn shells directly".
    pub fn detect() -> Option<Self> {
        Self::detect_with(
            |k| std::env::var(k).ok(),
            |p| p.exists(),
            |p| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            },
        )
    }

    /// [`detect`](Self::detect) with the environment and the file system injected.
    pub fn detect_with(
        env: impl Fn(&str) -> Option<String>,
        exists: impl Fn(&Path) -> bool,
        executable: impl Fn(&Path) -> bool,
    ) -> Option<Self> {
        // The documented off switch.
        if env("LDMUX_NO_SCOPE").is_some_and(|v| !v.is_empty() && v != "0") {
            return None;
        }
        let runtime = env("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
        if !exists(&Path::new(&runtime).join("bus")) {
            return None;
        }
        let path = env("PATH").unwrap_or_default();
        let busctl = path
            .split(':')
            .chain(["/usr/bin", "/bin"])
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("busctl"))
            .find(|p| executable(p))?;
        let slice = env("LDMUX_SCOPE_SLICE")
            .filter(|s| s.ends_with(".slice"))
            .unwrap_or_else(|| DEFAULT_SLICE.to_string());
        Some(Self { busctl, slice })
    }
}

/// A unit name for one shell: `ldmux-shell-<id>-<pid>.scope`. The pid makes it
/// unique when a dead session's scope (kept alive by a daemonized job) still holds
/// the name of an earlier session with the same id.
pub fn scope_name(session_id: &str, pid: i32) -> String {
    let id: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    format!("ldmux-shell-{id}-{pid}.scope")
}

/// The `busctl` argv that asks the user manager for a scope holding `pid`.
pub fn busctl_args(env: &ScopeEnv, name: &str, pid: i32) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        s("--user"),
        s("--timeout=5"),
        s("--quiet"),
        s("call"),
        s("org.freedesktop.systemd1"),
        s("/org/freedesktop/systemd1"),
        s("org.freedesktop.systemd1.Manager"),
        s("StartTransientUnit"),
        s("ssa(sv)a(sa(sv))"),
        s(name),
        s("fail"),
        s("4"),
        s("PIDs"),
        s("au"),
        s("1"),
        pid.to_string(),
        s("Slice"),
        s("s"),
        env.slice.clone(),
        s("CollectMode"),
        s("s"),
        s("inactive-or-failed"),
        s("Description"),
        s("s"),
        format!("ldmux shell {name}"),
        s("0"),
    ]
}

/// Move `pid` into a new scope. Blocking (a short `busctl` run): call it from
/// `spawn_blocking`. Any error is for the log only; the shell is already running.
pub fn enter(env: &ScopeEnv, session_id: &str, pid: i32) -> Result<String, String> {
    let name = scope_name(session_id, pid);
    let out = std::process::Command::new(&env.busctl)
        .args(busctl_args(env, &name, pid))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("run {}: {e}", env.busctl.display()))?;
    if out.status.success() {
        // The call returns once the start job is queued, not when the process has
        // moved. Wait for the move (milliseconds), so that the caller releasing the
        // shell means every process it forks lands in the scope.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
            if cg.contains(&name) {
                return Ok(name);
            }
            if std::time::Instant::now() > deadline {
                return Err(format!(
                    "{name}: queued, but pid {pid} did not move (in {})",
                    cg.trim()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    } else {
        Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn detect_needs_a_runtime_dir_a_bus_and_busctl() {
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        let base: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/run/user/1"), ("PATH", "/x:rel:/y")];
        let got =
            ScopeEnv::detect_with(env_of(base), yes, |p| p == Path::new("/y/busctl")).unwrap();
        assert_eq!(got.busctl, Path::new("/y/busctl"));
        assert_eq!(got.slice, DEFAULT_SLICE);
        // No bus socket, no busctl, no runtime dir: direct exec.
        assert!(ScopeEnv::detect_with(env_of(base), no, yes).is_none());
        assert!(ScopeEnv::detect_with(env_of(base), yes, no).is_none());
        assert!(ScopeEnv::detect_with(env_of(&[("PATH", "/y")]), yes, yes).is_none());
        // The fallback dirs apply when PATH has none; a relative PATH entry never wins.
        let bare: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("PATH", "rel")];
        let got = ScopeEnv::detect_with(env_of(bare), yes, |p| p == Path::new("/usr/bin/busctl"));
        assert_eq!(got.unwrap().busctl, Path::new("/usr/bin/busctl"));
    }

    #[test]
    fn the_off_switch_wins() {
        let on: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("LDMUX_NO_SCOPE", "1")];
        assert!(ScopeEnv::detect_with(env_of(on), |_| true, |_| true).is_none());
        let zero: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("LDMUX_NO_SCOPE", "0")];
        assert!(ScopeEnv::detect_with(env_of(zero), |_| true, |_| true).is_some());
    }

    #[test]
    fn scope_names_are_valid_unit_names_and_unique_per_pid() {
        assert_eq!(scope_name("ab/c d", 7), "ldmux-shell-ab_c_d-7.scope");
        assert_ne!(scope_name("s", 1), scope_name("s", 2));
        assert!(scope_name(&"x".repeat(500), 1).len() < 140);
    }
}
