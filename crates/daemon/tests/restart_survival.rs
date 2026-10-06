//! Phase 6 restart-survival e2e (plan §6): spawn 3 sessions in a scratch
//! keeper; kill the daemon; daemon re-attaches; all 3 shells still run.
//!
//! This test uses a **real keeper** (the `ldmux-keeper` binary), not the
//! fake keeper, because the sessions must survive the daemon restart.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ldmux-restart-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for socket {}", path.display());
}

/// Restart-survival e2e: spawn 3 sessions, kill the daemon, start a new
/// daemon, assert all 3 sessions are still alive.
#[tokio::test]
async fn restart_survival_three_sessions_survive_daemon_restart() {
    let dir = scratch_dir();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);

    // Start a real keeper (the `ldmux-keeper` binary).
    let keeper_bin =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper");
    let mut keeper = std::process::Command::new(&keeper_bin)
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();

    // Wait for the keeper socket.
    wait_socket(&keeper_sock).await;

    // Start a daemon connected to the real keeper.
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin.clone());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&Arc::clone(&arc)).await.unwrap();
    }

    // Spawn 3 sessions.
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut ids = Vec::new();
    for i in 0..3 {
        let title = format!("sess-{}", i);
        let rec = arc
            .lock()
            .await
            .create_session("shell", &cwd, Some(&title), "local")
            .await
            .unwrap();
        ids.push(rec.id.clone());
    }

    // Verify all 3 are running.
    {
        let d = arc.lock().await;
        for id in &ids {
            let rec = d.get_session(id);
            assert!(rec.is_some(), "session {id} should exist");
            assert_eq!(
                rec.unwrap().status,
                "running",
                "session {id} should be running"
            );
        }
    }

    // Kill the daemon (stop + drop).
    arc.lock().await.stop();
    drop(arc);

    // Small delay to let the keeper settle.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Start a new daemon connected to the same keeper.
    let d2 = LdmuxDaemon::new(dir.clone(), keeper_bin.clone());
    let arc2 = Arc::new(tokio::sync::Mutex::new(d2));
    {
        let mut d = arc2.lock().await;
        d.start(&Arc::clone(&arc2)).await.unwrap();
    }

    // Assert all 3 sessions are still alive (the keeper survived the
    // daemon restart; the sessions are still running).
    {
        let mut d = arc2.lock().await;
        let list = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = list
            .get("sessions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let running: Vec<serde_json::Value> = sessions
            .iter()
            .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("running"))
            .cloned()
            .collect();
        assert!(
            running.len() >= 3,
            "all 3 sessions should still be running after daemon restart (got {})",
            running.len()
        );
    }

    // Cleanup: kill the keeper.
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();

    // Wait for the keeper to exit.
    let _ = keeper.wait();

    // Stop the second daemon.
    arc2.lock().await.stop();

    // Belt-and-braces: reap any keeper still bound to this scratch socket (a
    // daemon `start()` can respawn one), then remove the scratch dir.
    let _ = std::fs::remove_dir_all(&dir);
}
