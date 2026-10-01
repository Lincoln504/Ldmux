//! The keeper boots, serves the protocol, and supervises a real shell in a
//! real PTY — no live system, no live daemon: a scratch socket in a temp dir
//! and a scratch PTY pair. This is the gate "keeper boots, serves List/Spawn".

use ldmux_protocol::keeper::{
    self as km, Kill, KeeperMessage, List, Peek, Read, Spawn,
};

/// The corpus the §3.11 harness pins (the same four styled cells): bold B,
/// underline U, 256-colour C, 24-bit D.
const CORPUS: &[u8] = b"\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[0m\x1b[38;2;255;128;0mD\x1b[0m\n";
/// What `snapshot::render` produces for the corpus row (our format —
/// plan §3.11(3): the cells are the source of truth, the format is ours).
const CORPUS_LINE: &str = "\x1b[0;1mB\x1b[0;4mU\x1b[0;38;5;123mC\x1b[0;38;2;255;128;0mD\x1b[0m";
/// The corpus as it appears in the byte stream: the body (no trailing
/// `\n`), because the pty's output post-processing (ONLCR) turns the
/// trailing `\n` into `\r\n` on the way out.
const CORPUS_BODY: &[u8] = b"\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[0m\x1b[38;2;255;128;0mD\x1b[0m";
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

/// One rendered snapshot (the answer to a `Snapshot`).
async fn snapshot(socket: &str, id: &str) -> km::SnapshotResult {
    match keeper_call(
        socket,
        &KeeperMessage::Snapshot(km::Snapshot {
            session_id: id.to_string(),
            history_lines: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap()
    {
        KeeperMessage::SnapshotResult(sr) => sr,
        other => panic!("expected SnapshotResult, got {other:?}"),
    }
}

/// The per-session emulator renders the pane: the corpus row appears in our
/// format (plan §3.11(3): the cells are the source of truth; the format is
/// ours — `snapshot::render`).
#[tokio::test]
async fn the_emulator_renders_the_pane_output() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s1".to_string();
    let s = socket.to_string_lossy();

    // Spawn 80x24.
    let res = keeper_call(
        &s,
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
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // Emit the corpus through the pane: write the raw bytes to a file and
    // `cat` it (cat never interprets escapes, so the pane gets the corpus
    // byte-for-byte).
    let corpus_file = std::env::temp_dir().join(format!("ldmux-keeper-corpus-{}.bin", std::process::id()));
    std::fs::write(&corpus_file, CORPUS).unwrap();
    let res = keeper_call(
        &s,
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: format!("cat {}\n", corpus_file.display()),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }

    // The emulator renders the pane: one line is the corpus in our format
    // (the prompt/echo occupy the leading lines; their count is not
    // pinned, so look for the corpus line rather than a fixed index).
    let mut got = None;
    for _ in 0..200 {
        let sr = snapshot(&s, &id).await;
        if sr.text.lines().any(|l| l == CORPUS_LINE) {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else {
        panic!("the emulator never rendered the corpus row")
    };
    assert_eq!((sr.cols, sr.rows), (80, 24), "the grid is the pane size");

    // Kill the session (the shell exits; the reader thread's read returns EOF
    // and the thread exits — without a kill the process hangs on exit).
    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
    let _ = std::fs::remove_file(&corpus_file);
}

/// The emulator's grid follows a `Resize` (FIFO: the resize is applied
/// before a later `Snapshot`).
#[tokio::test]
async fn the_emulator_resizes() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s2".to_string();
    let s = socket.to_string_lossy();

    let res = keeper_call(
        &s,
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
        KeeperMessage::State(_) => {}
        other => panic!("expected State, got {other:?}"),
    }

    // Resize to 120x40.
    let res = keeper_call(
        &s,
        &KeeperMessage::Resize(km::Resize {
            session_id: id.clone(),
            cols: 120,
            rows: 40,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert_eq!((st.cols, st.rows), (120, 40), "resize took"),
        other => panic!("expected State, got {other:?}"),
    }

    // The emulator's grid follows (the geometry in a snapshot is re-read
    // from the terminal after the resize).
    let mut got = None;
    for _ in 0..200 {
        let sr = snapshot(&s, &id).await;
        if (sr.cols, sr.rows) == (120, 40) {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else { panic!("the emulator never resized") };
    assert_eq!((sr.cols, sr.rows), (120, 40), "the grid follows the resize");

    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
}

/// The attach's stream (the `out` buffer) and the emulator see the *same*
/// bytes — both are fed by the one always-reads-the-master read.
#[tokio::test]
async fn the_buffer_and_the_emulator_see_the_same_bytes() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s3".to_string();
    let s = socket.to_string_lossy();

    let res = keeper_call(
        &s,
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
        KeeperMessage::State(_) => {}
        other => panic!("expected State, got {other:?}"),
    }

    let corpus_file = std::env::temp_dir().join(format!("ldmux-keeper-corpus-{}.bin", std::process::id()));
    std::fs::write(&corpus_file, CORPUS).unwrap();
    let _ = keeper_call(
        &s,
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: format!("cat {}\n", corpus_file.display()),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;

    // `Peek` drains the buffer, so accumulate the drained bytes: the corpus
    // must appear in the attach's stream (the same bytes the emulator
    // rendered).
    let mut acc: Vec<u8> = Vec::new();
    let mut ok = false;
    for _ in 0..200 {
        if let Ok(KeeperMessage::PeekResult(pr)) = keeper_call(
            &s,
            &KeeperMessage::Peek(Peek { session_id: id.clone() }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            acc.extend(pr.data);
            if acc.windows(CORPUS_BODY.len()).any(|w| w == CORPUS_BODY) {
                ok = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(ok, "the attach's stream must contain the bytes the pane emitted");

    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
    let _ = std::fs::remove_file(&corpus_file);
}

/// A `Snapshot` with `history_lines` returns that many history lines above
/// the visible screen (the `capture-pane -S -N -E -` contract): the client
/// replays history first, then the live screen.
#[tokio::test]
async fn the_emulator_captures_history_above_the_screen() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s4".to_string();
    let s = socket.to_string_lossy();

    // Spawn 80x24.
    let res = keeper_call(
        &s,
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
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // Emit 30 numbered lines (H0001..H0030) into the pane. The echoed command
    // occupies one line; the 30 lines fill the rest. In an 80x24 pane the
    // live screen holds the last 23 content lines (H0008..H0030) + the empty
    // cursor row; the scrollback holds the echoed line + H0001..H0007.
    let data = "i=1; while [ $i -le 30 ]; do printf \"H%04d\\n\" $i; i=$((i+1)); done\n";
    let res = keeper_call(
        &s,
        &KeeperMessage::Write(km::Write { session_id: id.clone(), data: data.to_string() }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }

    // Ask for 5 history lines. The answer is 5 history lines + 24 live lines
    // = 29 lines: the run immediately above the live screen (H0003..H0007),
    // then the live screen (H0008..H0030 + the empty cursor row).
    let mut got = None;
    for _ in 0..200 {
        let sr = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot { session_id: id.clone(), history_lines: Some(5) }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .ok()
        .and_then(|m| match m {
            KeeperMessage::SnapshotResult(sr) if sr.history_lines == 5 && sr.text.lines().count() == 29 => Some(sr),
            _ => None,
        });
        if let Some(sr) = sr {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else { panic!("the emulator never returned a 5-history, 29-line capture") };
    assert_eq!((sr.cols, sr.rows), (80, 24), "the grid is the pane size");
    let lines: Vec<&str> = sr.text.lines().collect();
    assert_eq!(lines.len(), 29, "5 history + 24 live");
    // The history run (oldest first) and the history/live boundary.
    assert_eq!(lines[0], "H0003");
    assert_eq!(lines[4], "H0007", "last history line");
    assert_eq!(lines[5], "H0008", "first live line");
    // The last live content line is the last line emitted; the cursor row
    // holds the shell's prompt (a shell detail, not the contract).
    assert_eq!(lines[27], "H0030");
    assert!(!lines[28].is_empty(), "cursor row holds the prompt");

    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
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
