//! CLI integration test: the CLI binary talks to the daemon over the control
//! socket.

use std::path::PathBuf;

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_testkeeper::FakeKeeper;

/// A stand-in terminal: a script that stays up for the test and needs no display.
///
/// `$TERMINAL` takes a program, and a wrapper script is a program — which is also the
/// documented way to give a real emulator flags.
fn fake_terminal(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("fake-terminal");
    std::fs::write(&p, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-cli-int-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..50 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

/// The CLI binary path (built by cargo).
fn cli_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = p.parent().unwrap().parent().unwrap().to_path_buf();
    p.join("target").join("debug").join("ldmux")
}

/// Build the CLI binary (if not already built).
fn ensure_cli_built() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("Cargo.toml");
    let status = std::process::Command::new("cargo")
        .args(["build", "--bin", "ldmux", "--manifest-path"])
        .arg(&manifest)
        .output()
        .expect("cargo build failed");
    assert!(
        status.status.success(),
        "cargo build ldmux failed: {:?}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Run the CLI binary and return (exit code, stdout, stderr).
async fn run_cli(args: &[&str], state_dir: &str) -> (i32, String, String) {
    let mut cmd = std::process::Command::new(cli_binary());
    for arg in args {
        cmd.arg(arg);
    }
    cmd.env("LDMUX_STATE_DIR", state_dir);
    // `window` asks the daemon to spawn a terminal. `/bin/true` is a terminal that opens and exits:
    // the shell is created through the same path a person uses, and the test does not need a
    // display. (The `new` verb that created a shell nothing could ever show was removed.)

    let handle = tokio::task::spawn_blocking(move || {
        let output = cmd.output().expect("CLI failed to run");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    });
    handle.await.unwrap()
}

#[tokio::test]
async fn cli_ping_over_the_control_socket() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path())
            .await
            .unwrap();
    });
    wait_socket(&keeper_sock).await;

    ensure_cli_built();
    // The desk-window spawner runs **in the daemon**, so its environment is this process's, not the
    // CLI subprocess's. The fake must *stay up*: `window` treats a terminal that exits at once as
    // one that could not open a window, which is the check that keeps a headless `window` from
    // creating an invisible shell. `$TERMINAL` names it, because that is the one variable a
    // terminal choice comes from now.
    // SAFETY: test setup, before any daemon thread reads the environment.
    unsafe { std::env::set_var("TERMINAL", fake_terminal(&dir)) };

    let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    // The CLI `ping` command.
    let (code, out, err) = run_cli(&["ping"], &dir.to_string_lossy()).await;
    assert_eq!(code, 0, "ping failed: {out} {err}");
    assert!(
        out.contains("pong") || out.contains("true"),
        "unexpected: {out} {err}"
    );

    // The CLI `list` command.
    let (code, out, err) = run_cli(&["list"], &dir.to_string_lossy()).await;
    assert_eq!(code, 0, "list failed: {out} {err}");

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cli_full_lifecycle_over_the_control_socket() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path())
            .await
            .unwrap();
    });
    wait_socket(&keeper_sock).await;

    ensure_cli_built();
    // The desk-window spawner runs **in the daemon**, so its environment is this process's, not the
    // CLI subprocess's. The fake must *stay up*: `window` treats a terminal that exits at once as
    // one that could not open a window, which is the check that keeps a headless `window` from
    // creating an invisible shell. `$TERMINAL` names it, because that is the one variable a
    // terminal choice comes from now.
    // SAFETY: test setup, before any daemon thread reads the environment.
    unsafe { std::env::set_var("TERMINAL", fake_terminal(&dir)) };

    let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }
    let dir_str = dir.to_string_lossy().into_owned();

    // `new` — create a session and print its id. No window is opened and nothing attaches:
    // the CLI stopped opening windows of its own (a desk window is `ldmux attach <id>` in the
    // terminal you already have), so this is the creating verb that returns.
    let (code, out, err) = run_cli(&["new", &dir_str], &dir_str).await;
    assert_eq!(code, 0, "new failed: {out} {err}");
    let session_id = out.trim().to_string();
    assert!(!session_id.is_empty(), "no session id: {out} {err}");

    // `list` — should contain the session.
    let (code, out, err) = run_cli(&["list"], &dir_str).await;
    assert_eq!(code, 0, "list failed: {out} {err}");
    assert!(out.contains("running"), "no session in list: {out} {err}");

    // `send` — send text to the session.
    let (code, out, err) = run_cli(&["send", &session_id, "echo hi"], &dir_str).await;
    assert_eq!(code, 0, "send failed: {out} {err}");

    // `capture` — capture the screen (the fake keeper's snapshot text is
    // empty by default, so the screen is empty).
    let (code, out, err) = run_cli(&["capture", &session_id], &dir_str).await;
    assert_eq!(code, 0, "capture failed: {out} {err}");

    // `kill` — kill the session.
    let (code, out, err) = run_cli(&["kill", &session_id], &dir_str).await;
    assert_eq!(code, 0, "kill failed: {out} {err}");

    // `list` — a dead session is hidden by default (the registry keeps every shell that ever
    // ran, and the list was a page of dead rows over the live handful), and `--all` shows it
    // with why it ended.
    let (code, out, err) = run_cli(&["list"], &dir_str).await;
    assert_eq!(code, 0, "list failed: {out} {err}");
    assert!(
        !out.contains(&session_id),
        "a dead session must not be listed by default: {out} {err}"
    );
    let (code, out, err) = run_cli(&["list", "--all"], &dir_str).await;
    assert_eq!(code, 0, "list --all failed: {out} {err}");
    assert!(out.contains("dead"), "session not dead: {out} {err}");

    // `forget` — forget the dead session.
    let (code, out, err) = run_cli(&["forget", &session_id], &dir_str).await;
    assert_eq!(code, 0, "forget failed: {out} {err}");

    // `list` — should be empty.
    let (code, out, err) = run_cli(&["list"], &dir_str).await;
    assert_eq!(code, 0, "list failed: {out} {err}");
    assert!(
        out.trim() == "(no sessions)",
        "session still in list: {out} {err}"
    );

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ldmux code` and `ldmux pair` must refuse a daemon a phone cannot reach, and
/// say what to set, rather than printing a link that goes nowhere. The port
/// printed the bare link and exited 0 for both, ignoring the listen address
/// entirely — the TS threw here, and printed a QR for `code` (the one output
/// meant to connect a phone).
#[tokio::test]
async fn cli_code_and_pair_refuse_a_daemon_a_phone_cannot_reach() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path())
            .await
            .unwrap();
    });
    wait_socket(&keeper_sock).await;

    ensure_cli_built();

    // Loopback, which is exactly the case a phone cannot use. Port 0 lets the OS
    // pick: the address is what the refusal is about.
    let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent")).with_remote(
        "127.0.0.1".to_string(),
        0,
        "0123456789abcdef".repeat(4),
    );
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    for verb in ["code", "pair"] {
        let (code, out, err) = run_cli(&[verb], &dir.to_string_lossy()).await;
        assert_eq!(code, 1, "{verb} must refuse: out={out} err={err}");
        assert!(
            err.contains("LDMUX_REMOTE_HOST"),
            "{verb} must say what to set: {err}"
        );
        assert!(out.is_empty(), "{verb} printed a link anyway: {out}");
    }

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ldmux desktop --json` must print JSON. It checked for the flag with the
/// value-taking helper, which reads the *next* argument, so `--json` as the last
/// argument never matched and the human format came out instead.
#[tokio::test]
async fn cli_list_json_is_json() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path())
            .await
            .unwrap();
    });
    wait_socket(&keeper_sock).await;

    ensure_cli_built();
    let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
    let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    // `--json` is a contract for whatever reads the CLI, not a debug pretty-print: it
    // parses, and it names the list.
    let (code, out, err) = run_cli(&["list", "--json"], &dir.to_string_lossy()).await;
    assert_eq!(code, 0, "list --json failed: {out} {err}");
    let parsed: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"));
    assert!(
        parsed.get("sessions").is_some_and(|s| s.is_array()),
        "no sessions array in {parsed}"
    );

    // Without the flag it is the human format, which is not JSON.
    let (code, out, _err) = run_cli(&["list"], &dir.to_string_lossy()).await;
    assert_eq!(code, 0);
    assert!(
        serde_json::from_str::<serde_json::Value>(&out).is_err() || out.trim().is_empty(),
        "the human format must not be JSON: {out}"
    );

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `ldmux ... | head -1`: the reader closes before the CLI writes. Rust ignores
/// SIGPIPE, so `println!` used to panic ("failed printing to stdout: Broken
/// pipe"). The CLI now dies quietly like any Unix tool: no panic text, and the
/// status is either a clean exit or termination by SIGPIPE.
#[test]
fn cli_output_into_a_closed_pipe_does_not_panic() {
    use std::os::unix::process::ExitStatusExt;
    ensure_cli_built();
    let dir = scratch();
    for args in [vec!["--help"], vec!["list", "--help"]] {
        let mut child = std::process::Command::new(cli_binary())
            .args(&args)
            .env("LDMUX_STATE_DIR", &dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn CLI");
        // The reader goes away before the process has started writing.
        drop(child.stdout.take());
        let out = child.wait_with_output().expect("wait CLI");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked"), "{args:?} panicked: {err}");
        assert!(!err.contains("Broken pipe"), "{args:?}: {err}");
        assert!(
            out.status.success() || out.status.signal() == Some(13),
            "{args:?}: unexpected status {:?}: {err}",
            out.status
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
