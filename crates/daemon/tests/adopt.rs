//! Adopting a keeper session the daemon does not know about.
//!
//! `adopt` is what a daemon with no record of a shell uses to pick one up: the
//! registry was wiped, or the keeper outlived the daemon that started it. `start`
//! calls the same code for every session its keeper has and its registry does
//! not, so the start-up path was covered — by `restart_survival.rs` — while the
//! RPC itself, the two spellings of a keeper name, the idempotence and the
//! refusals had no test of their own.
//!
//! The sessions here are spawned **straight into the keeper**, which is the only
//! way to make one the daemon has never heard of: `forget` refuses a shell that
//! is still running ("end it with kill, not forget"), deliberately.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_protocol::keeper::{KeeperMessage, Read, Spawn, Write};
use ldmux_protocol::keeper_call::{DEFAULT_TIMEOUT_MS, keeper_call};

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-adopt-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("socket did not appear: {}", path.display());
}

/// Spawn a shell in the keeper under the name the keeper would have given it
/// (`ldmux-<id>`), and return that name.
async fn spawn_in_keeper(sock: &std::path::Path, id: &str, cwd: &str) -> String {
    let keeper_name = format!("ldmux-{id}");
    let res = keeper_call(
        sock.to_str().unwrap(),
        &KeeperMessage::Spawn(Spawn {
            session_id: keeper_name.clone(),
            cols: 80,
            rows: 24,
            cwd: Some(cwd.to_string()),
            command: None,
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(s) => assert!(!s.dead, "the shell must start alive"),
        other => panic!("expected State, got {other:?}"),
    }
    keeper_name
}

async fn keeper_state(sock: &std::path::Path, keeper_name: &str) -> Option<bool> {
    let res = keeper_call(
        sock.to_str().unwrap(),
        &KeeperMessage::Read(Read { session_id: keeper_name.to_string() }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .ok()?;
    match res {
        KeeperMessage::State(s) => Some(s.dead),
        _ => None,
    }
}

#[tokio::test]
async fn adopt_takes_a_running_shell_the_daemon_has_never_heard_of() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let d = LdmuxDaemon::new(dir.clone(), keeper_bin());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let keeper_name = spawn_in_keeper(&keeper_sock, "unknown1", &cwd).await;

    // A session the daemon does not know is not listable...
    assert!(
        arc.lock().await.get_session("unknown1").is_none(),
        "the daemon must not know this shell yet"
    );

    // ...until it is adopted, by the keeper name the keeper actually uses.
    let rec = arc
        .lock()
        .await
        .handle("adopt", serde_json::json!({"keeperName": keeper_name}))
        .await
        .unwrap();
    assert_eq!(rec["id"], "unknown1");
    assert_eq!(rec["keeperName"], keeper_name);
    assert_eq!(rec["status"], "running");
    assert_eq!(rec["createdBy"], "adopt", "an adopted shell records how it arrived");
    assert_eq!(rec["cwd"], cwd, "the cwd comes from the keeper's own view of the pane");

    // The bare id is the same session: the RPC accepts either spelling, and the
    // second adopt is the first one's record rather than a second record for one
    // shell (two records would be two rows, two claims, two windows).
    let again = arc
        .lock()
        .await
        .handle("adopt", serde_json::json!({"keeperName": "unknown1"}))
        .await
        .unwrap();
    assert_eq!(again["id"], rec["id"]);
    assert_eq!(again["createdBy"], "adopt");
    let listed = arc.lock().await.handle("list", serde_json::json!({})).await.unwrap();
    let rows = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["id"] == "unknown1")
        .count();
    assert_eq!(rows, 1, "one shell is one row: {listed}");

    // The adoption is written down, not just remembered: it is in the registry
    // file the next daemon loads. Read from disk rather than through a second
    // daemon, because a second daemon's `start` would adopt the keeper's session
    // all over again if the record were missing — which is the one thing this is
    // trying to tell apart.
    let reg = std::fs::read_to_string(dir.join("registry.json")).unwrap();
    assert!(reg.contains("unknown1"), "an adopted shell must be on disk: {reg}");
    assert!(reg.contains(keeper_name.as_str()), "with the name it was adopted under");
    assert!(reg.contains("adopt"), "and how it arrived");

    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn adopt_refuses_a_shell_that_is_not_there_and_one_that_has_exited() {
    let dir = scratch();
    let keeper_sock = dir.join("keeper.sock");
    let _reaper = common::Reaper::new(&dir, &keeper_sock);
    let mut keeper = std::process::Command::new(keeper_bin())
        .arg(keeper_sock.to_string_lossy().as_ref())
        .spawn()
        .unwrap();
    wait_socket(&keeper_sock).await;

    let d = LdmuxDaemon::new(dir.clone(), keeper_bin());
    let arc = Arc::new(tokio::sync::Mutex::new(d));
    {
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
    }

    // A name the keeper has never had.
    let err = arc
        .lock()
        .await
        .handle("adopt", serde_json::json!({"keeperName": "ldmux-ghost"}))
        .await
        .err()
        .expect("adopting a shell that does not exist must fail");
    assert!(!err.is_empty(), "and it must say so: {err}");
    assert!(arc.lock().await.registry().get("ghost").is_none(), "and record nothing");

    // A shell that has exited: the pane is still in the keeper (that is what a
    // dead pane is — the emulator keeps the last grid), and adopting it would
    // put a row in the list for a shell nobody can type into.
    let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
    let keeper_name = spawn_in_keeper(&keeper_sock, "exited1", &cwd).await;
    keeper_call(
        keeper_sock.to_str().unwrap(),
        &KeeperMessage::Write(Write {
            session_id: keeper_name.clone(),
            data: "exit\n".to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let mut dead = false;
    for _ in 0..200 {
        if keeper_state(&keeper_sock, &keeper_name).await == Some(true) {
            dead = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(dead, "the shell must have exited for this test to mean anything");

    let err = arc
        .lock()
        .await
        .handle("adopt", serde_json::json!({"keeperName": keeper_name}))
        .await
        .err()
        .expect("adopting an exited shell must fail");
    assert!(err.contains("dead"), "and it must say why: {err}");
    assert!(arc.lock().await.registry().get("exited1").is_none(), "and record nothing");

    arc.lock().await.stop();
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(keeper.id().to_string())
        .output();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
