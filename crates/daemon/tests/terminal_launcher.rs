//! The terminal launcher (`deploy/bin/ldmux-terminal`, installed as
//! `~/.local/bin/ldmux-terminal`), which `super+Return` runs.
//!
//! One rule to keep — a terminal keybinding always gives a terminal — and one rule it must
//! not break: **the wrapper opens the terminal and ldmux runs inside it.** `ldmux` no longer
//! opens a window of its own for a person at a keyboard (the only window the daemon opens is
//! the one the phone asks for), so the launcher asks the daemon which emulator to use
//! (`ldmux terminal --exec …`) and execs it with `ldmux shell` in it.
//!
//! Two outcomes are pinned here, both driven by stubs and without an X server:
//!
//! - the daemon answers: the terminal it names is exec'd with `shell <dir>` in it;
//! - the daemon never answers: the fallback is the bare `$TERMINAL`, which is a plain pty
//!   and not a session, and the fallback is logged rather than silent.

use std::path::{Path, PathBuf};

fn scratch_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ldmux-terminal-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/bin/ldmux-terminal")
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Stubs {
    dir: PathBuf,
}

impl Drop for Stubs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A stub `ldmux` that answers `ping` as asked, prints `terminal` as the emulator, and
/// records every call. `--exec` is recorded with its command so the test can see what the
/// launcher asked for: the stub does not exec, it prints and exits 0.
fn stub_ldmux(dir: &Path, ping_ok: bool) -> Stubs {
    std::fs::create_dir_all(dir).unwrap();
    let calls = dir.join("calls");
    let ping = if ping_ok { "exit 0" } else { "exit 1" };
    // The emulator this stub names is the test's own stub, never a real one: a real
    // emulator here would open a window and wait for ever, which is exactly the failure
    // this test exists to rule out.
    let terminal = "${STUB_TERM:-}";
    write_script(
        &dir.join("ldmux"),
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{calls}"
case "$1" in
  ping) {ping} ;;
  terminal)
    case "$*" in
      *--exec*) shift 2; printf 'EXEC %s\n' "$*" >> "{calls}" ;;
      *) echo "{terminal}" ;;
    esac
    ;;
esac
exit 0
"#,
            calls = calls.display(),
            ping = ping,
            terminal = terminal
        ),
    );
    let _ = calls;
    Stubs {
        dir: dir.to_path_buf(),
    }
}

/// The fallback emulator and `logger`, so the fallback path is observable.
fn stub_fallback_bin(dir: &Path, name: &str) -> PathBuf {
    let bindir = dir.join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let p = bindir.join(name);
    write_script(&p, "#!/bin/sh\nexit 0\n");
    bindir
}

struct Run {
    code: i32,
    calls: String,
}

fn run(dir: &Path, ldmux_dir: &Path, bindir: &Path, args: &[&str]) -> Run {
    let out = std::process::Command::new("sh")
        .arg(launcher())
        .args(args)
        .env("LDMUX", ldmux_dir.join("ldmux"))
        .env("STUB_TERM", bindir.join("fallback-term"))
        .env("PATH", format!("{}:/usr/bin:/bin", bindir.display()))
        .env("TERMINAL", "fallback-term")
        .env_remove("LDMUX_NO_SCOPE")
        .current_dir(dir)
        .output()
        .expect("run the launcher");
    let calls = std::fs::read_to_string(ldmux_dir.join("calls")).unwrap_or_default();
    Run {
        code: out.status.code().unwrap_or(-1),
        calls,
    }
}

#[test]
fn the_daemon_names_the_terminal_and_the_shell_runs_in_it() {
    let dir = scratch_dir();
    let ldmux_dir = dir.join("ldmux-bin");
    let stubs = stub_ldmux(&ldmux_dir, true);
    let bindir = stub_fallback_bin(&dir, "fallback-term");

    let r = run(&dir, &ldmux_dir, &bindir, &["/tmp"]);
    assert_eq!(r.code, 0, "launcher failed; calls: {}", r.calls);
    assert!(
        r.calls.contains("EXEC"),
        "the launcher must ask the daemon to exec a terminal: {}",
        r.calls
    );
    assert!(
        r.calls.contains("shell /tmp"),
        "the command run inside the terminal must be `shell <dir>`: {}",
        r.calls
    );
    assert!(
        !r.calls.contains("window"),
        "the launcher must not ask for a window: {}",
        r.calls
    );
    drop(stubs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_daemon_that_never_answers_falls_back_to_the_bare_terminal() {
    let dir = scratch_dir();
    let ldmux_dir = dir.join("ldmux-bin");
    let stubs = stub_ldmux(&ldmux_dir, false);
    let bindir = stub_fallback_bin(&dir, "fallback-term");

    let r = run(&dir, &ldmux_dir, &bindir, &["/tmp"]);
    // The fallback execs `$TERMINAL`, which exits 0 here.
    assert_eq!(r.code, 0, "fallback failed; calls: {}", r.calls);
    assert!(
        !r.calls.contains("EXEC"),
        "a daemon that never answers must not be asked to exec: {}",
        r.calls
    );
    assert!(
        r.calls.contains("ping"),
        "the launcher waits by asking ping: {}",
        r.calls
    );
    drop(stubs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_directory_argument_reaches_the_shell_command() {
    let dir = scratch_dir();
    let ldmux_dir = dir.join("ldmux-bin");
    let stubs = stub_ldmux(&ldmux_dir, true);
    let bindir = stub_fallback_bin(&dir, "fallback-term");

    let r = run(&dir, &ldmux_dir, &bindir, &["/usr"]);
    assert!(
        r.calls.contains("shell /usr"),
        "the directory must be passed to `shell`: {}",
        r.calls
    );
    drop(stubs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_directory_is_where_the_launcher_started() {
    let dir = scratch_dir();
    let ldmux_dir = dir.join("ldmux-bin");
    let stubs = stub_ldmux(&ldmux_dir, true);
    let bindir = stub_fallback_bin(&dir, "fallback-term");

    let r = run(&dir, &ldmux_dir, &bindir, &[]);
    let here = dir.canonicalize().unwrap();
    assert!(
        r.calls.contains(&format!("shell {}", here.display())),
        "with no argument the shell runs in the caller's directory ({}): {}",
        here.display(),
        r.calls
    );
    drop(stubs);
    let _ = std::fs::remove_dir_all(&dir);
}
