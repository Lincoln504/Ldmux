//! The keeper boots, serves the protocol, and supervises a real shell in a
//! real PTY — no live system, no live daemon: a scratch socket in a temp dir
//! and a scratch PTY pair. This is the gate "keeper boots, serves List/Spawn".

use ldmux_protocol::keeper::{
    self as km, Kill, KeeperMessage, List, Read, Spawn,
};
use ldmux_protocol::keeper_call::{keeper_call, DEFAULT_TIMEOUT_MS};

use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch socket path, unique per test (tests run in parallel).
fn socket_path() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ldmux-keeper-test-{}-{}.sock",
        std::process::id(),
        n
    ))
}

/// Boot the keeper on `socket` (in-process) and hand back the `Arc` so the
/// test can inspect it directly (e.g. the buffered stream).
async fn boot(socket: &std::path::Path) -> std::sync::Arc<keeper::Keeper> {
    let k = std::sync::Arc::new(keeper::Keeper::new(vec!["/bin/sh".to_string()]));
    let k2 = k.clone();
    let sock = socket.to_path_buf();
    tokio::spawn(async move {
        // Runs to process exit; the task is aborted when the runtime drops.
        let _ = keeper::serve(&k2, &sock).await;
    });
    // Give the listener a moment to bind.
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    k
}

/// Poll until `pred` holds (the shell needs a moment to echo into the PTY).
async fn wait_until(pred: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if pred() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    pred()
}

#[tokio::test]
async fn boots_and_serves_an_empty_list() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::ListResult(lr) => assert!(lr.sessions.is_empty(), "no sessions yet"),
        other => panic!("expected ListResult, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_runs_a_shell_and_the_keeper_reads_the_master() {
    let socket = socket_path();
    let k = boot(&socket).await;
    let id = "s1".to_string();

    // Spawn a shell.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: id.clone(),
            cols: 80,
            rows: 24,
            cwd: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => {
            assert!(!st.dead, "a fresh session is alive");
            assert_eq!((st.cols, st.rows), (80, 24));
        }
        other => panic!("expected State, got {other:?}"),
    }

    // List shows it.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::ListResult(lr) => {
            assert_eq!(lr.sessions.len(), 1, "one session");
            assert!(!lr.sessions[0].dead);
        }
        other => panic!("expected ListResult, got {other:?}"),
    }

    // Type into the shell; the keeper must read the master (the §3.3 rule).
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: "printf 'ldmux-ok\\n'".to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    ).await.unwrap();
    match res {
        KeeperMessage::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    let ok = wait_until(|| k.buffered_len(&id).unwrap_or(0) > 0).await;
    assert!(ok, "the keeper must read the master (the shell echoed into it)");

    // A read (the attach handshake) reports the session alive.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Read(Read {
            session_id: id.clone(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // Kill it: group-killed, reported dead, removed.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Kill(Kill {
            session_id: id.clone(),
        }),
        DEFAULT_TIMEOUT_MS,
    ).await.unwrap();
    match res {
        KeeperMessage::State(st) => assert!(st.dead, "kill reports dead"),
        other => panic!("expected State, got {other:?}"),
    }

    // The list is empty again.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::ListResult(lr) => assert!(lr.sessions.is_empty()),
        other => panic!("expected ListResult, got {other:?}"),
    }
}

#[tokio::test]
async fn resize_reports_the_new_state() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s2".to_string();
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: id.clone(),
            cols: 80,
            rows: 24,
            cwd: None,
        }),
        DEFAULT_TIMEOUT_MS,
    ).await.unwrap();
    match res {
        KeeperMessage::State(_) => {}
        other => panic!("expected State, got {other:?}"),
    }
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Resize(km::Resize {
            session_id: id.clone(),
            cols: 120,
            rows: 40,
        }),
        DEFAULT_TIMEOUT_MS,
    ).await.unwrap();
    match res {
        KeeperMessage::State(st) => {
            assert_eq!((st.cols, st.rows), (120, 40), "resize took");
            assert!(!st.dead);
        }
        other => panic!("expected State, got {other:?}"),
    }
    // Kill the session: the shell exits, the master gets EOF, and the reader
    // task's blocking read returns — without this the runtime cannot be dropped
    // (a blocking-pool thread wedged in read(pty)) and the process hangs on exit.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(st.dead),
        other => panic!("expected State, got {other:?}"),
    }
}

/// A request for a session that was never spawned is an error answer.
#[tokio::test]
async fn an_unknown_session_is_an_error_answer() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let res = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Read(Read {
            session_id: "nope".to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
    match res {
        Err(e) => assert!(!e.0.is_empty(), "a failure is reported: {e}"),
        Ok(m) => panic!("expected an error, got {m:?}"),
    }
}
