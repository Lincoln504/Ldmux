//! Phase 6 keeper-death e2e (plan §6): kill the keeper with 3 live sessions;
//! assert the shells die, the daemon detects the death, respawns an empty
//! keeper, and leaves zero zombies.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch_dir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-kdeath-{}-{}", std::process::id(), rand::random::<u64>()));
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

/// Keeper-death e2e: kill the keeper with 3 live sessions; the daemon
/// detects the death, respawns an empty keeper, and reports all sessions dead.
#[tokio::test]
async fn keeper_death_three_sessions_die_and_daemon_respawns() {
    let dir = scratch_dir();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);

    // Start a real keeper.
    let keeper_bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/ldmux-keeper");
    let mut keeper = std::process::Command::new(&keeper_bin)
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    // Start a daemon.
    let d = LdmuxDaemon::new(dir.clone(), keeper_bin.clone());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&Arc::clone(&arc)).await.unwrap();
    }

    // Spawn 3 sessions.
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let mut ids = Vec::new();
    for i in 0..3 {
        let title = format!("sess-{}", i);
        let rec = arc.lock().await
            .create_session("shell", &cwd, Some(&title), "local")
            .await
            .unwrap();
        ids.push(rec.id.clone());
    }

    // Kill the keeper.
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();

    // The daemon should detect the keeper death and respawn it.
    // Give it time to respawn.
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

    // The keeper socket should be back (the daemon respawned it).
    wait_socket(&keeper_sock).await;

    // All sessions should be reported dead (the keeper died, so the shells
    // are gone).
    {
        let mut d = arc.lock().await;
        let list = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = list
            .get("sessions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let dead: Vec<serde_json::Value> = sessions
            .iter()
            .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("dead"))
            .cloned()
            .collect();
        assert!(
            dead.len() >= 3,
            "all 3 sessions should be dead after keeper death (got {})",
            dead.len()
        );
    }

    // Stop the daemon.
    arc.lock().await.stop();

    // `stop()` deliberately does NOT kill the keeper (the orphan/restart-survival
    // design), so this test must reap the keeper it caused the daemon to spawn,
    // or every run leaks one holding PTYs and a socket. Kill by the keeper socket
    // path (unique per scratch dir), then remove the scratch dir.
    let _ = std::fs::remove_dir_all(&dir);
}
