//! The keeper boots, serves the protocol, and supervises a real shell in a
//! real PTY — no live system, no live daemon: a scratch socket in a temp dir
//! and a scratch PTY pair. This is the gate "keeper boots, serves List/Spawn".
//!
//! **The waits are generous on purpose.** Every poll below is bounded by an
//! iteration count with a sleep, not by a wall clock, because `cargo test
//! --workspace` runs these alongside every other crate on one machine: a real
//! shell and a real `cat` that take milliseconds alone can take seconds when the
//! whole workspace is running. A tight budget made this file flaky, not fast.

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
    for _ in 0..1200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    k
}

/// Poll until `pred` holds (the shell needs a moment to echo into the PTY).
async fn wait_until(pred: impl Fn() -> bool) -> bool {
    for _ in 0..1200 {
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
            command: None,
            env: None,
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
            command: None,
            env: None,
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
            drain_output: false,
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
            command: None,
            env: None,
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
    for _ in 0..1200 {
        let sr = snapshot(&s, &id).await;
        // `ends_with`, not `==`: the row carries the shell prompt when the
        // corpus happens to be printed on the prompt's own line, which is a
        // matter of what the shell had already written when the `cat` ran. The
        // equality check made this test depend on that, and it failed in a full
        // workspace run for exactly that reason (the pane held
        // `$ \x1b[0;1mB\x1b[0;4mU…`). What is being tested is that the emulator
        // rendered the corpus in our format, not where the prompt was.
        if sr.text.lines().any(|l| l.ends_with(CORPUS_LINE)) {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else {
        // Under a loaded machine (`--test-threads` with the whole workspace
        // running) a real shell and its `cat` can take seconds. Say what the
        // pane actually held, so a failure is diagnosable rather than a message
        // that only says "no".
        let last = snapshot(&s, &id).await;
        panic!(
            "the emulator never rendered the corpus row in 30s; the pane held:\n{:?}",
            last.text
        )
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
            command: None,
            env: None,
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
    for _ in 0..1200 {
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
            command: None,
            env: None,
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
    for _ in 0..1200 {
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
            command: None,
            env: None,
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
    for _ in 0..1200 {
        let sr = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot { session_id: id.clone(), history_lines: Some(5), drain_output: false }),
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

/// A `Snapshot` reports the pane's state (plan §3.4 — the `display-message`
/// fields, read direct from ghostty-vt, not a parsed format string): the
/// cursor, the terminal modes (off in a fresh shell), the alternate screen,
/// `dead`, and the `/proc` fields (foreground command + cwd).
#[tokio::test]
async fn the_emulator_reports_the_pane_state() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s5".to_string();
    let s = socket.to_string_lossy();

    let res = keeper_call(
        &s,
        &KeeperMessage::Spawn(Spawn { session_id: id.clone(), cols: 80, rows: 24, cwd: None,
            command: None,
            env: None,
            }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    let mut got = None;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::SnapshotResult(sr)) = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot { session_id: id.clone(), history_lines: None, drain_output: false }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else { panic!("never got a snapshot") };

    // Geometry + liveness.
    assert_eq!((sr.cols, sr.rows), (80, 24));
    assert!(!sr.dead, "a fresh pane is alive");
    // A fresh shell is on the primary screen, not in a mode, and has not
    // negotiated mouse / bracketed paste.
    assert!(!sr.alternate_on);
    assert!(!sr.mouse && !sr.mouse_sgr && !sr.app_cursor && !sr.bracketed_paste);
    // The cursor is inside the grid; nothing has scrolled yet.
    assert!(sr.cursor_x < 80 && sr.cursor_y < 24);
    assert_eq!(sr.history_size, 0);
    // The `/proc` fields: the shell's command name and the session's cwd.
    assert!(!sr.foreground.is_empty(), "the pane's foreground is the shell: {}", sr.foreground);
    assert!(!sr.cwd.is_empty(), "the pane has a cwd");

    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
}

/// The `State`/`List` liveness report carries the pane's state (the
/// display-message fields, read from the emulator, no render) — the same
/// fields a `Snapshot` answer carries, so the daemon reads liveness without
/// paying for a render. A fresh pane: the state fields are the initial ones
/// and the `/proc` fields name the shell + cwd.
#[tokio::test]
async fn the_state_query_carries_the_pane_state() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s7".to_string();
    let s = socket.to_string_lossy();

    let res = keeper_call(
        &s,
        &KeeperMessage::Spawn(Spawn { session_id: id.clone(), cols: 80, rows: 24, cwd: None,
            command: None,
            env: None,
            }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // The liveness query (`Read` → `State`) carries the pane's state.
    let mut st = None;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::State(x)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read { session_id: id.clone() }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            st = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(st) = st else { panic!("never got a State") };

    // Geometry + liveness.
    assert_eq!((st.cols, st.rows), (80, 24));
    assert!(!st.dead, "a fresh pane is alive");
    // A fresh shell: primary screen, no mode, no negotiated mouse / paste.
    assert!(!st.alternate_on);
    assert!(!st.mouse && !st.mouse_sgr && !st.app_cursor && !st.bracketed_paste);
    // The cursor is inside the grid; nothing has scrolled.
    assert!(st.cursor_x < 80 && st.cursor_y < 24);
    assert_eq!(st.history_size, 0);
    // The `/proc` fields: the shell's command name and the session's cwd.
    assert!(!st.foreground.is_empty(), "the pane's foreground is the shell: {}", st.foreground);
    assert!(!st.cwd.is_empty(), "the pane has a cwd");

    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
}

/// A natural shell exit (the shell exited on its own, not a `Kill`) is
/// reported `dead` by the raw `waitpid` in `reap()` — the keeper is the sole
/// reaper (the raw `fork`/`exec` spawn, not `tokio::process::Command`, whose
/// global `ChildReaper` would have reaped the child first). A dead pane
/// still answers a `Snapshot`: the emulator thread runs on after the master
/// EOF, `dead` is reported, and the `/proc` fields are empty (a reaped pid
/// may be recycled, so we don't read it).
///
/// This test was `#[ignore]`d as "flaky under the crate's concurrent test load".
/// It was not timing: `reap()` called `waitpid(-1)`, which reaps *any* child of
/// the process, and every test here runs its own keeper in one process — so a
/// neighbour's `reap()` consumed this shell's exit status and the keeper that
/// owned it never saw it die. `reap()` now waits on its own sessions' pids, and
/// this passes under full load (8/8 consecutive runs; 1/8 before).
#[tokio::test]
async fn a_natural_shell_exit_is_reported_dead_and_the_pane_still_snapshots() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s6".to_string();
    let s = socket.to_string_lossy();

    let res = keeper_call(
        &s,
        &KeeperMessage::Spawn(Spawn { session_id: id.clone(), cols: 80, rows: 24, cwd: None,
            command: None,
            env: None,
            }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // The shell exits on its own (not a Kill — the session stays in the map).
    let _ = keeper_call(
        &s,
        &KeeperMessage::Write(km::Write { session_id: id.clone(), data: "exit\n".to_string() }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;

    // Wait for the keeper's raw `waitpid` to reap it and report dead.
    let mut dead = false;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::State(st)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read { session_id: id.clone() }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            dead = st.dead;
            if dead {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(dead, "the natural shell exit is reaped and reported dead");

    // A snapshot of the dead pane: dead, empty `/proc` fields, last content.
    let mut sr = None;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::SnapshotResult(x)) = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot { session_id: id.clone(), history_lines: None, drain_output: false }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            sr = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = sr else { panic!("the dead pane never answered a snapshot") };
    assert!(sr.dead, "a dead pane reports dead");
    assert_eq!(sr.foreground, "", "a dead pane has no /proc/<pid>/comm");
    assert_eq!(sr.cwd, "", "a dead pane has no /proc/<pid>/cwd");
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

/// The `Spawn.command` + `Spawn.env` (plan §2.6): the command is what runs, and
/// the child sees the `LDMUX_*` variables the daemon sets. This is the
/// per-session environment (`K=V`) overlay.
#[tokio::test]
async fn spawn_runs_the_command_and_passes_the_env() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "env-1".to_string();
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: id.clone(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'MARK=%s\\n' \"$LDMUX_PROBE\"; sleep 30".into(),
            ]),
            env: Some(vec![("LDMUX_PROBE".into(), "hello-env".into())]),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(res, KeeperMessage::State(_)), "spawn answers State, got {res:?}");

    let mut seen = Vec::new();
    for _ in 0..1200 {
        let r = keeper_call(
            &socket.to_string_lossy(),
            &KeeperMessage::Peek(Peek { session_id: id.clone() }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();
        if let KeeperMessage::PeekResult(pr) = r {
            seen.extend_from_slice(&pr.data);
        }
        if String::from_utf8_lossy(&seen).contains("MARK=hello-env") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let text = String::from_utf8_lossy(&seen);
    assert!(
        text.contains("MARK=hello-env"),
        "the command's output with its env must appear; got {text:?}"
    );
}

/// Run `printf` of TERM/COLORTERM in a spawned session and return what it printed.
async fn term_seen(id: &str, env: Option<Vec<(String, String)>>) -> String {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let _: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: id.to_string(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'T=%s;C=%s;\\n' \"$TERM\" \"$COLORTERM\"; sleep 30".into(),
            ]),
            env,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let mut seen = Vec::new();
    for _ in 0..1200 {
        if let KeeperMessage::PeekResult(pr) = keeper_call(
            &socket.to_string_lossy(),
            &KeeperMessage::Peek(Peek { session_id: id.to_string() }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap()
        {
            seen.extend_from_slice(&pr.data);
        }
        // The line ends `;\r\n`, not `;\n`: the pty's ONLCR turns the child's
        // newline into CRLF on the way out. Waiting for `;\n` therefore never
        // matched, and the wait always ran out its whole budget instead of
        // returning as soon as the child had printed — 30 s per call, twice.
        let text = String::from_utf8_lossy(&seen);
        if text.contains(";\r\n") || text.contains(";\n") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// A session's child sees a 256-colour TERM and a COLORTERM, never `dumb`, so
/// programs turn colour on. (Bug found live 2026-10-05: the systemd unit has no
/// TERM, so every shell ran with TERM=dumb.)
#[tokio::test]
async fn a_spawned_child_sees_a_colour_capable_term() {
    let t = term_seen("term-1", None).await;
    assert!(t.contains("T=xterm-256color;C=truecolor;"), "got {t:?}");
}

#[tokio::test]
async fn spawn_env_overrides_term() {
    let t = term_seen("term-2", Some(vec![("TERM".into(), "vt100".into())])).await;
    assert!(t.contains("T=vt100;C=truecolor;"), "got {t:?}");
}

/// A `Snapshot` with `drain_output` hands back the not-yet-peeked output and
/// clears it (those bytes are already in the render); a plain `Snapshot` leaves
/// the buffer for the viewer that is polling it.
#[tokio::test]
async fn a_snapshot_can_take_the_unpeeked_tail() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    let id = "tail-1".to_string();
    let spawned = keeper_call(
        &s,
        &KeeperMessage::Spawn(Spawn {
            session_id: id.clone(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), "printf 'TAIL-MARK\\n'; sleep 30".into()]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(spawned, KeeperMessage::State(_)));

    // Let the output land in the render without peeking it.
    let mut rendered = false;
    for _ in 0..200 {
        if snapshot(&s, &id).await.text.contains("TAIL-MARK") {
            rendered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(rendered, "the output reached the emulator");

    // A plain snapshot leaves the buffer alone.
    assert!(snapshot(&s, &id).await.tail.is_empty());
    let take = |drain: bool| {
        let (s, id) = (s.clone(), id.clone());
        async move {
            match keeper_call(
                &s,
                &KeeperMessage::Snapshot(km::Snapshot { session_id: id, history_lines: None, drain_output: drain }),
                DEFAULT_TIMEOUT_MS,
            )
            .await
            .unwrap()
            {
                KeeperMessage::SnapshotResult(sr) => sr,
                other => panic!("expected SnapshotResult, got {other:?}"),
            }
        }
    };
    let first = take(true).await;
    assert!(
        String::from_utf8_lossy(&first.tail).contains("TAIL-MARK"),
        "the tail is exactly the bytes the render already shows: {:?}",
        String::from_utf8_lossy(&first.tail)
    );
    assert!(first.text.contains("TAIL-MARK"));
    // Taken once: a second drain and a Peek both come back empty.
    assert!(take(true).await.tail.is_empty());
    match keeper_call(&s, &KeeperMessage::Peek(Peek { session_id: id.clone() }), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap()
    {
        KeeperMessage::PeekResult(pr) => assert!(pr.data.is_empty() && !pr.lost),
        other => panic!("expected PeekResult, got {other:?}"),
    }
    let _ = keeper_call(&s, &KeeperMessage::Kill(Kill { session_id: id }), DEFAULT_TIMEOUT_MS).await;
}

#[tokio::test]
async fn the_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let socket = socket_path();
    let _k = boot(&socket).await;
    // `boot` returns once the path exists; the chmod follows the bind, so poll.
    let ok = wait_until(|| {
        std::fs::metadata(&socket)
            .map(|m| m.permissions().mode() & 0o777 == 0o600)
            .unwrap_or(false)
    })
    .await;
    assert!(ok, "the keeper socket must be 0600 whatever the umask");
}

#[tokio::test]
async fn a_request_line_without_a_newline_is_cut_off_at_the_cap() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let socket = socket_path();
    let _k = boot(&socket).await;
    let mut c = tokio::net::UnixStream::connect(&socket).await.unwrap();
    // 5 MiB with no newline: past the 4 MiB cap the keeper must drop the connection
    // rather than buffer until it is out of memory.
    let chunk = vec![b'a'; 64 * 1024];
    let mut dropped = false;
    for _ in 0..80 {
        if c.write_all(&chunk).await.is_err() {
            dropped = true;
            break;
        }
    }
    if !dropped {
        let mut b = [0u8; 1];
        dropped = matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), c.read(&mut b)).await,
            Ok(Ok(0)) | Ok(Err(_))
        );
    }
    assert!(dropped, "an unterminated oversized request must close the connection");
    // And the keeper still serves.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(res, KeeperMessage::ListResult(_)));
}

/// A job the shell leaves behind must die with the session, not survive as an
/// orphan with no controlling terminal.
///
/// The shell exits on its own here (the natural-death path), which used to do
/// nothing at all: the session was marked dead and every child of the shell
/// kept running, unreachable and invisible. Job control can also put a
/// foreground job in its own process group, where killing only the shell's
/// group misses it, so the keeper tears the whole session down.
#[tokio::test]
async fn a_background_job_does_not_survive_the_shell() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let dir = std::env::temp_dir().join(format!("ldmux-orphan-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let pidfile = dir.join("child.pid");
    let _ = std::fs::remove_file(&pidfile);
    // `set -m` asks for job control, so the background job gets its own process
    // group when the environment allows it. Either way the job outlives the
    // `sleep 0.4` shell, and the keeper must not leave it behind.
    let script = format!(
        "set -m; sleep 300 & echo $! > {}; sleep 0.4",
        pidfile.display()
    );
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: "orphan-test".to_string(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec![
                "/bin/bash".to_string(),
                "-c".to_string(),
                script,
            ]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(res, KeeperMessage::State(_)), "spawn failed: {res:?}");

    let mut child_pid = 0i32;
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&pidfile)
            && let Ok(p) = s.trim().parse()
        {
            child_pid = p;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(child_pid > 0, "the job never reported its pid");

    // Let the shell exit, then make the keeper notice: it reaps on every
    // message (the daemon polls List, which is how the real orphan was marked
    // dead), and the natural-death path is what must take the session down.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let _ = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();

    let mut gone = false;
    for _ in 0..160 {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid), None)
            .is_err_and(|e| e == nix::errno::Errno::ESRCH)
        {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    if !gone {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child_pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(gone, "a job outlived the shell that started it");
}
