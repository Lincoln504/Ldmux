//! The desk-window launcher (`deploy/bin/ldmux-terminal`, installed as
//! `~/.local/bin/ldmux-terminal`), which `super+Return` runs.
//!
//! It has one rule to keep — a terminal keybinding always gives a terminal —
//! and one rule it must not break: the fallback terminal is a plain pty, not a
//! session, so it is invisible to every ldmux client. The launcher therefore
//! waits for an *absent* daemon before it falls back, and the tests here pin
//! both halves: the wait, and that a reachable daemon's window failure is not
//! waited on.
//!
//! The script is driven with `LDMUX` pointing at a stub and `PATH` holding
//! stubs for `alacritty` (the fallback) and `logger`, so both outcomes are
//! observable without an X server and without touching the journal.

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

/// The stub `ldmux`, appending every call to `calls`. `window_after` is how
/// many failed `window` calls come first (a daemon that is not up yet);
/// `ping_ok` is whether `ping` answers at all.
fn write_ldmux_stub(dir: &Path, window_after: u32, ping_ok: bool) {
    let ping = if ping_ok { "exit 0" } else { "exit 1" };
    write_script(
        &dir.join("ldmux"),
        &format!(
            r#"#!/bin/sh
echo "$1" >> "{calls}"
case "$1" in
  ping)
    {ping}
    ;;
  window)
    n=$(cat "{count}" 2>/dev/null || echo 0)
    if [ "$n" -ge "{after}" ]; then exit 0; fi
    echo $((n + 1)) > "{count}"
    echo "stub: connect: Connection refused (os error 111)" >&2
    exit 1
    ;;
esac
exit 0
"#,
            calls = dir.join("calls").display(),
            count = dir.join("count").display(),
            after = window_after,
            ping = ping,
        ),
    );
}

/// The fallback terminal (never a real one) and a `logger` that records its
/// arguments instead of writing to the journal.
fn write_fallback_stubs(dir: &Path) {
    write_script(
        &dir.join("alacritty"),
        &format!(
            "#!/bin/sh\necho ran > {}\n",
            dir.join("alacritty-ran").display()
        ),
    );
    write_script(
        &dir.join("logger"),
        &format!("#!/bin/sh\necho \"$@\" >> {}\n", dir.join("log").display()),
    );
}

struct Run {
    calls: String,
    fell_back: bool,
    log_line: String,
}

fn run(dir: &Path) -> Run {
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(launcher())
        .env("LDMUX", dir.join("ldmux"))
        .env("PATH", &path)
        // Hermetic: the developer's own terminal choice must not pick the fallback.
        .env_remove("LDMUX_TERMINAL")
        .env_remove("LDMUX_TERMINAL_CMD")
        .env_remove("TERMINAL")
        .current_dir(dir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "launcher exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    Run {
        calls: std::fs::read_to_string(dir.join("calls")).unwrap_or_default(),
        fell_back: dir.join("alacritty-ran").exists(),
        log_line: std::fs::read_to_string(dir.join("log")).unwrap_or_default(),
    }
}

/// A daemon that is coming up is waited for: the launcher keeps trying and
/// opens the window once it can. Nothing falls back.
///
/// Before the wait was added, the very first failure dropped straight to
/// `alacritty`, so the keybinding produced a session-less terminal that no
/// client could see.
#[test]
fn the_launcher_waits_for_a_daemon_that_is_coming_up() {
    let dir = scratch_dir();
    write_fallback_stubs(&dir);
    // Absent for the whole wait (`ping` always fails), and the window succeeds
    // on the third try — the daemon arriving under the launcher.
    write_ldmux_stub(&dir, 2, false);
    let r = run(&dir);
    assert!(
        !r.fell_back,
        "the launcher must wait for the daemon, not fall back; calls: {}",
        r.calls
    );
    assert_eq!(
        r.calls.lines().filter(|l| *l == "window").count(),
        3,
        "it must keep asking; calls: {}",
        r.calls
    );
    assert!(r.log_line.is_empty(), "nothing to log when it worked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A daemon that never answers falls back to a plain terminal (the rule the
/// keybinding keeps) and says so in the log, with the daemon's own error.
#[test]
fn the_launcher_falls_back_when_the_daemon_never_answers() {
    let dir = scratch_dir();
    write_fallback_stubs(&dir);
    write_ldmux_stub(&dir, 99, false);
    let r = run(&dir);
    assert!(r.fell_back, "a missing daemon must still give a terminal");
    assert!(
        r.log_line.contains("no ldmux desk window"),
        "the fallback must be logged, got {:?}",
        r.log_line
    );
    assert!(
        r.log_line.contains("Connection refused"),
        "the log must carry the daemon's error, got {:?}",
        r.log_line
    );
    assert!(
        r.calls.lines().filter(|l| *l == "window").count() <= 12,
        "the wait is bounded; calls: {}",
        r.calls
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A daemon that answers but cannot open the window is not waited on: the
/// launcher gives up at once rather than sleeping through the retries. That
/// failure is not a race, and a terminal should appear now.
#[test]
fn a_reachable_daemon_whose_window_fails_is_not_waited_for() {
    let dir = scratch_dir();
    let ldmux = dir.join("ldmux");
    write_script(
        &ldmux,
        &format!(
            "#!/bin/sh\necho \"$1\" >> {}\ncase \"$1\" in ping) exit 0;; window) echo 'stub: no terminal' >&2; exit 1;; esac\nexit 0\n",
            dir.join("calls").display()
        ),
    );
    write_fallback_stubs(&dir);
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let start = std::time::Instant::now();
    let out = std::process::Command::new(launcher())
        .env("LDMUX", &ldmux)
        .env("PATH", &path)
        .env_remove("LDMUX_TERMINAL")
        .env_remove("LDMUX_TERMINAL_CMD")
        .env_remove("TERMINAL")
        .current_dir(&dir)
        .output()
        .unwrap();
    let took = start.elapsed();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        dir.join("alacritty-ran").exists(),
        "it must still give a terminal"
    );
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
    assert_eq!(
        calls.lines().filter(|l| *l == "window").count(),
        1,
        "a reachable daemon's window failure must not be retried; calls: {calls}"
    );
    assert!(
        took < std::time::Duration::from_secs(2),
        "a reachable daemon's window failure must not be waited on, took {took:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
/// The fallback honours the same selection as the daemon: `LDMUX_TERMINAL` beats
/// the autodetect order (which would have picked the `alacritty` stub), and the
/// terminal is run by absolute path.
#[test]
fn the_fallback_honours_ldmux_terminal() {
    let dir = scratch_dir();
    write_fallback_stubs(&dir);
    write_script(
        &dir.join("myterm"),
        &format!(
            "#!/bin/sh\necho \"$0\" > {}\n",
            dir.join("myterm-ran").display()
        ),
    );
    write_ldmux_stub(&dir, 99, true);
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(launcher())
        .env("LDMUX", dir.join("ldmux"))
        .env("PATH", &path)
        .env("LDMUX_TERMINAL", "myterm")
        .env_remove("LDMUX_TERMINAL_CMD")
        .env_remove("TERMINAL")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.join("alacritty-ran").exists(),
        "the autodetect must not override the choice"
    );
    let ran = std::fs::read_to_string(dir.join("myterm-ran")).unwrap_or_default();
    assert_eq!(
        ran.trim(),
        dir.join("myterm").display().to_string(),
        "run by absolute path"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The directory argument reaches `ldmux window` intact, spaces and all.
#[test]
fn the_directory_argument_is_passed_through() {
    let dir = scratch_dir();
    write_fallback_stubs(&dir);
    write_script(
        &dir.join("ldmux"),
        &format!(
            "#!/bin/sh\nprintf '%s|' \"$@\" >> {}\nexit 0\n",
            dir.join("args").display()
        ),
    );
    let out = std::process::Command::new(launcher())
        .arg("/some dir/with space")
        .env("LDMUX", dir.join("ldmux"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let args = std::fs::read_to_string(dir.join("args")).unwrap_or_default();
    assert_eq!(args, "window|/some dir/with space|");
    let _ = std::fs::remove_dir_all(&dir);
}
