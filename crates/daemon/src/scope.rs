//! Putting each desk window in its own transient systemd scope.
//!
//! The keeper does this for shells (`crates/keeper/src/scope.rs`, ADR-0021) for
//! one reason: a process that is a child of a unit is charged to that unit.
//! A window launched by the daemon is such a child, and the bill was visible —
//! `/proc/<pid>/cgroup` said `ldmux.service` for every window and every attach
//! client, `systemctl --user status ldmux` reported 252 MB and an 811 MB peak
//! against a daemon whose own RSS is 15 MB, and every restart wrote
//! `Found left-over process … Ignoring … This usually indicates unclean
//! termination` once per window, which reads as a fault and is the first thing an
//! operator sees. `systemctl --user stop ldmux` also left the windows behind.
//!
//! The mechanism is deliberately the keeper's: `StartTransientUnit` with
//! `PIDs=[window pid]` through `busctl`, called after the spawn, with the child
//! **blocked on a pipe before exec** until the move is visible. The pipe is what
//! makes the move complete: a terminal emulator forks the attach client as soon
//! as it starts, and a child forked before the move lands in the daemon's cgroup —
//! the very attribution this exists to fix. A failure of any kind (no user bus, a
//! name collision, a busctl that hangs) releases the pipe and leaves the window
//! exactly where it was: a window that cannot get a scope must still open.
//!
//! The scope is not stopped when the window closes; `CollectMode` lets systemd
//! drop it once its last process is gone.

use std::path::{Path, PathBuf};

/// The slice windows live in. Separate from `ldmux-shells.slice` because a window
/// is not a shell, and because `systemctl status ldmux-shells.slice` should keep
/// meaning "the sessions".
pub const WINDOW_SLICE: &str = "ldmux-windows.slice";

/// Where and how to ask for a scope. Present only under a systemd user manager.
#[derive(Debug, Clone)]
pub struct ScopeEnv {
    /// Absolute path of `busctl`, resolved once.
    pub busctl: PathBuf,
    /// The slice the scopes live in.
    pub slice: String,
}

impl ScopeEnv {
    /// Detect from the process environment; `None` means "spawn windows directly".
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
        // One off switch for both kinds of scope: a machine that does not want
        // systemd invented around it does not want it for windows either.
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
        let slice = env("LDMUX_WINDOW_SLICE")
            .filter(|s| s.ends_with(".slice"))
            .unwrap_or_else(|| WINDOW_SLICE.to_string());
        Some(Self { busctl, slice })
    }
}

/// A unit name for one window: `ldmux-window-<class>-<pid>.scope`.
///
/// The class is already restricted to `[A-Za-z0-9_-]` by `desktop::window_class`,
/// and is truncated so the unit name stays well inside systemd's 255-byte limit.
pub fn scope_name(class: &str, pid: u32) -> String {
    let class: String = class
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(100)
        .collect();
    format!("ldmux-window-{class}-{pid}.scope")
}

/// The `busctl` argv that asks the user manager for a scope holding `pid`.
fn busctl_args(env: &ScopeEnv, name: &str, pid: u32) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        s("--user"),
        // Shorter than the keeper's: this runs in front of a window opening, and
        // a user manager that cannot answer in two seconds is not going to.
        s("--timeout=2"),
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
        format!("ldmux desk window {name}"),
        s("0"),
    ]
}

/// Move `pid` into a new scope. Blocking (one short `busctl` run, then a poll):
/// call it from `spawn_blocking`. Any error is for the log only — the window is
/// already running, and it is released either way.
pub fn enter(env: &ScopeEnv, class: &str, pid: u32) -> Result<String, String> {
    let name = scope_name(class, pid);
    let out = std::process::Command::new(&env.busctl)
        .args(busctl_args(env, &name, pid))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("run {}: {e}", env.busctl.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // The call returns when the start job is queued, not when the process has
    // moved. Wait for the move before the caller releases the child: the whole
    // point is that nothing it forks can be charged to the daemon.
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
        assert_eq!(got.slice, WINDOW_SLICE);
        // No bus socket, no busctl, no runtime dir: no scope, windows spawn directly.
        assert!(ScopeEnv::detect_with(env_of(base), no, yes).is_none());
        assert!(ScopeEnv::detect_with(env_of(base), yes, no).is_none());
        assert!(ScopeEnv::detect_with(env_of(&[("PATH", "/y")]), yes, yes).is_none());
        // The fallback dirs apply when PATH has none; a relative entry never wins.
        let bare: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("PATH", "rel")];
        let got = ScopeEnv::detect_with(env_of(bare), yes, |p| p == Path::new("/usr/bin/busctl"));
        assert_eq!(got.unwrap().busctl, Path::new("/usr/bin/busctl"));
    }

    #[test]
    fn the_off_switch_and_the_slice_override_both_apply() {
        let on: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("LDMUX_NO_SCOPE", "1")];
        assert!(ScopeEnv::detect_with(env_of(on), |_| true, |_| true).is_none());
        let zero: &[(&str, &str)] = &[("XDG_RUNTIME_DIR", "/r"), ("LDMUX_NO_SCOPE", "0")];
        assert!(ScopeEnv::detect_with(env_of(zero), |_| true, |_| true).is_some());
        let slice: &[(&str, &str)] = &[
            ("XDG_RUNTIME_DIR", "/r"),
            ("LDMUX_WINDOW_SLICE", "test-windows.slice"),
        ];
        assert_eq!(
            ScopeEnv::detect_with(env_of(slice), |_| true, |_| true)
                .unwrap()
                .slice,
            "test-windows.slice"
        );
        // A value that is not a slice is ignored rather than passed to the bus.
        let bad: &[(&str, &str)] = &[
            ("XDG_RUNTIME_DIR", "/r"),
            ("LDMUX_WINDOW_SLICE", "ldmux-windows"),
        ];
        assert_eq!(
            ScopeEnv::detect_with(env_of(bad), |_| true, |_| true)
                .unwrap()
                .slice,
            WINDOW_SLICE
        );
    }

    #[test]
    fn scope_names_are_valid_unit_names_and_unique_per_pid() {
        assert_eq!(
            scope_name("ldmux-live-ab/c d", 7),
            "ldmux-window-ldmux-live-abcd-7.scope"
        );
        assert_ne!(scope_name("ldmux-live-s", 1), scope_name("ldmux-live-s", 2));
        assert!(scope_name(&"x".repeat(500), 1).len() < 140);
    }

    #[test]
    fn the_bus_call_asks_for_the_window_slice_and_the_pid() {
        let env = ScopeEnv {
            busctl: PathBuf::from("/usr/bin/busctl"),
            slice: WINDOW_SLICE.to_string(),
        };
        let args = busctl_args(&env, "ldmux-window-c-9.scope", 9);
        assert!(args.contains(&"StartTransientUnit".to_string()));
        assert!(args.contains(&WINDOW_SLICE.to_string()));
        assert!(args.contains(&"9".to_string()));
        assert!(args.contains(&"inactive-or-failed".to_string()));
    }
}
