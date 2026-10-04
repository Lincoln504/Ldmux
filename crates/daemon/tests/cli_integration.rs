//! CLI integration test: the CLI binary talks to the daemon over the control
//! socket.

use std::path::PathBuf;

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_daemon::fake_keeper::FakeKeeper;

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
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
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

    // The CLI `ping` command.
    let (code, out, err) = run_cli(&["ping"], &dir.to_string_lossy()).await;
    assert_eq!(code, 0, "ping failed: {out} {err}");
    assert!(out.contains("pong") || out.contains("true"), "unexpected: {out} {err}");

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
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
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
    let dir_str = dir.to_string_lossy().into_owned();

    // `new` — create a session. The output is the session id (a hex string).
    let (code, out, err) = run_cli(&["new", "shell", &dir_str], &dir_str).await;
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

    // `snapshot` — snapshot the session.
    let (code, out, err) = run_cli(&["snapshot", &session_id], &dir_str).await;
    assert_eq!(code, 0, "snapshot failed: {out} {err}");
    assert!(out.contains("cols"), "no cols in snapshot: {out} {err}");

    // `kill` — kill the session.
    let (code, out, err) = run_cli(&["kill", &session_id], &dir_str).await;
    assert_eq!(code, 0, "kill failed: {out} {err}");

    // `list` — should show the session as dead.
    let (code, out, err) = run_cli(&["list"], &dir_str).await;
    assert_eq!(code, 0, "list failed: {out} {err}");
    assert!(out.contains("dead"), "session not dead: {out} {err}");

    // `forget` — forget the dead session.
    let (code, out, err) = run_cli(&["forget", &session_id], &dir_str).await;
    assert_eq!(code, 0, "forget failed: {out} {err}");

    // `list` — should be empty.
    let (code, out, err) = run_cli(&["list"], &dir_str).await;
    assert_eq!(code, 0, "list failed: {out} {err}");
    assert!(out.trim() == "(no sessions)", "session still in list: {out} {err}");

    drop(fk);
    let _ = std::fs::remove_dir_all(&dir);
}
