//! The keeper boots, serves the protocol, and supervises a real shell in a
//! real PTY — no live system, no live daemon: a scratch socket in a temp dir
//! and a scratch PTY pair. This is the gate "keeper boots, serves List/Spawn".
//!
//! **The waits are generous on purpose.** Every poll below is bounded by an
//! iteration count with a sleep, not by a wall clock, because `cargo test
//! --workspace` runs these alongside every other crate on one machine: a real
//! shell and a real `cat` that take milliseconds alone can take seconds when the
//! whole workspace is running. A tight budget made this file flaky, not fast.

use ldmux_protocol::keeper::{self as km, KeeperMessage, Kill, List, Peek, Read, Spawn, Write};

/// The corpus the §3.11 harness pins (the same four styled cells): bold B,
/// underline U, 256-colour C, 24-bit D.
const CORPUS: &[u8] =
    b"\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[0m\x1b[38;2;255;128;0mD\x1b[0m\n";
/// What `snapshot::render` produces for the corpus row (our format —
/// plan §3.11(3): the cells are the source of truth, the format is ours).
const CORPUS_LINE: &str = "\x1b[0;1mB\x1b[0;4mU\x1b[0;38;5;123mC\x1b[0;38;2;255;128;0mD\x1b[0m";
/// The corpus as it appears in the byte stream: the body (no trailing
/// `\n`), because the pty's output post-processing (ONLCR) turns the
/// trailing `\n` into `\r\n` on the way out.
const CORPUS_BODY: &[u8] =
    b"\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[0m\x1b[38;2;255;128;0mD\x1b[0m";
use ldmux_protocol::keeper_call::{DEFAULT_TIMEOUT_MS, keeper_call};

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
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    let ok = wait_until(|| k.buffered_len(&id).unwrap_or(0) > 0).await;
    assert!(
        ok,
        "the keeper must read the master (the shell echoed into it)"
    );

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
    )
    .await
    .unwrap();
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
    )
    .await
    .unwrap();
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
    )
    .await
    .unwrap();
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
    let corpus_file =
        std::env::temp_dir().join(format!("ldmux-keeper-corpus-{}.bin", std::process::id()));
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
    let Some(sr) = got else {
        panic!("the emulator never resized")
    };
    assert_eq!((sr.cols, sr.rows), (120, 40), "the grid follows the resize");

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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

    let corpus_file =
        std::env::temp_dir().join(format!("ldmux-keeper-corpus-{}.bin", std::process::id()));
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
            &KeeperMessage::Peek(Peek::all(id.clone())),
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
    assert!(
        ok,
        "the attach's stream must contain the bytes the pane emitted"
    );

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: data.to_string(),
        }),
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
            &KeeperMessage::Snapshot(km::Snapshot {
                session_id: id.clone(),
                history_lines: Some(5),
                drain_output: false,
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .ok()
        .and_then(|m| match m {
            KeeperMessage::SnapshotResult(sr)
                if sr.history_lines == 5 && sr.text.lines().count() == 29 =>
            {
                Some(sr)
            }
            _ => None,
        });
        if let Some(sr) = sr {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else {
        panic!("the emulator never returned a 5-history, 29-line capture")
    };
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

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
}

/// A history capture that is longer than the screen is lines, not blanks.
///
/// The render state holds one screen, so a capture that indexed row `y` for `y`
/// up to the requested count read past it: 60 asked for, 24 answered with content
/// and 36 with empties (measured 2026-10-05 against the running keeper). The
/// capture walks the window a screenful at a time instead.
#[tokio::test]
async fn a_history_capture_longer_than_the_screen_is_all_lines() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s11".to_string();
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
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // 120 numbered lines: far more than the 24-row screen, so the history holds
    // a hundred-odd of them.
    let data = "i=1; while [ $i -le 120 ]; do printf \"K%04d\\n\" $i; i=$((i+1)); done\n";
    keeper_call(
        &s,
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: data.to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Ask for 60 history lines: more than one screen, fewer than the history.
    let mut got = None;
    for _ in 0..80 {
        let res = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot {
                session_id: id.clone(),
                history_lines: Some(60),
                drain_output: false,
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();
        if let KeeperMessage::SnapshotResult(sr) = res
            && sr.history_lines == 60
            && sr.text.lines().count() == 84
        {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else {
        panic!("the emulator never returned a 60-history, 84-line capture")
    };

    let lines: Vec<&str> = sr.text.lines().collect();
    assert_eq!(lines.len(), 84, "60 history + 24 live");
    // Every history line is a line of output, not an empty row: the count asked
    // for is the count delivered.
    let blank = lines[..60].iter().filter(|l| l.is_empty()).count();
    assert_eq!(blank, 0, "history lines must be content: {lines:?}");
    // And they are the run immediately above the live screen, oldest first.
    let nums: Vec<u32> = lines[..60]
        .iter()
        .filter_map(|l| l.trim().strip_prefix('K')?.parse::<u32>().ok())
        .collect();
    assert_eq!(
        nums.len(),
        60,
        "history is numbered output: {:?}",
        &lines[..60]
    );
    assert!(
        nums.windows(2).all(|w| w[1] == w[0] + 1),
        "and in order, oldest first: {nums:?}"
    );
    // The history ends where the live screen begins: the last history line is the
    // one immediately above the screen's first numbered line.
    let live_first = lines[60..]
        .iter()
        .find_map(|l| l.trim().strip_prefix('K')?.parse::<u32>().ok())
        .expect("the live screen holds numbered output");
    assert_eq!(
        nums[59] + 1,
        live_first,
        "the history ends where the screen begins: {nums:?}"
    );

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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

    let mut got = None;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::SnapshotResult(sr)) = keeper_call(
            &s,
            &KeeperMessage::Snapshot(km::Snapshot {
                session_id: id.clone(),
                history_lines: None,
                drain_output: false,
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            got = Some(sr);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = got else {
        panic!("never got a snapshot")
    };

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
    assert!(
        !sr.foreground.is_empty(),
        "the pane's foreground is the shell: {}",
        sr.foreground
    );
    assert!(!sr.cwd.is_empty(), "the pane has a cwd");

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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

    // The liveness query (`Read` → `State`) carries the pane's state.
    let mut st = None;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::State(x)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read {
                session_id: id.clone(),
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            st = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(st) = st else {
        panic!("never got a State")
    };

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
    assert!(
        !st.foreground.is_empty(),
        "the pane's foreground is the shell: {}",
        st.foreground
    );
    assert!(!st.cwd.is_empty(), "the pane has a cwd");

    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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

    // The shell exits on its own (not a Kill — the session stays in the map).
    let _ = keeper_call(
        &s,
        &KeeperMessage::Write(km::Write {
            session_id: id.clone(),
            data: "exit\n".to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;

    // Wait for the keeper's raw `waitpid` to reap it and report dead.
    let mut dead = false;
    for _ in 0..1200 {
        if let Ok(KeeperMessage::State(st)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read {
                session_id: id.clone(),
            }),
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
            &KeeperMessage::Snapshot(km::Snapshot {
                session_id: id.clone(),
                history_lines: None,
                drain_output: false,
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            sr = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let Some(sr) = sr else {
        panic!("the dead pane never answered a snapshot")
    };
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
        Err(e) => assert!(!e.message.is_empty(), "a failure is reported: {e}"),
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
    assert!(
        matches!(res, KeeperMessage::State(_)),
        "spawn answers State, got {res:?}"
    );

    let mut seen = Vec::new();
    for _ in 0..1200 {
        let r = keeper_call(
            &socket.to_string_lossy(),
            &KeeperMessage::Peek(Peek::all(id.clone())),
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
            &KeeperMessage::Peek(Peek::all(id.to_string())),
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
            command: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'TAIL-MARK\\n'; sleep 30".into(),
            ]),
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
                &KeeperMessage::Snapshot(km::Snapshot {
                    session_id: id,
                    history_lines: None,
                    drain_output: drain,
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
    match keeper_call(
        &s,
        &KeeperMessage::Peek(Peek::all(id.clone())),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap()
    {
        KeeperMessage::PeekResult(pr) => assert!(pr.data.is_empty() && !pr.lost),
        other => panic!("expected PeekResult, got {other:?}"),
    }
    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill { session_id: id }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
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
    assert!(
        dropped,
        "an unterminated oversized request must close the connection"
    );
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
    // Another test's keeper binds with a tightened process-wide umask (that is how
    // its socket is born 0600), and this thread may create the directory inside
    // that window: pin the mode rather than trust the umask.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    }
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
            command: Some(vec!["/bin/bash".to_string(), "-c".to_string(), script]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(
        matches!(res, KeeperMessage::State(_)),
        "spawn failed: {res:?}"
    );

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

/// A shell gets its own pty and nothing else's.
///
/// `openpty(3)` leaves `FD_CLOEXEC` unset, so a forked shell inherited the master
/// of **every session spawned before it** — measured on the deployed keeper: a
/// shell spawned after four sessions held four `/dev/ptmx` descriptors belonging
/// to other sessions. A shell is not a view of its siblings, and the inheritance
/// is also why a killed keeper left its shells running (the master's last
/// reference stayed open in a sibling, so no `SIGHUP` was ever sent).
#[tokio::test]
async fn a_shell_inherits_no_other_sessions_pty() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let mut pids = Vec::new();
    for id in ["inherit-a", "inherit-b"] {
        let res: KeeperMessage = keeper_call(
            &socket.to_string_lossy(),
            &KeeperMessage::Spawn(Spawn {
                session_id: id.to_string(),
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
            KeeperMessage::State(st) => pids.push(st.pid),
            other => panic!("expected State, got {other:?}"),
        }
    }
    // The second shell is the one that would have inherited the first's master.
    let pid = pids[1];
    let fds: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("the shell's fds")
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let masters: Vec<&String> = fds.iter().filter(|p| p.contains("ptmx")).collect();
    assert!(
        masters.is_empty(),
        "pid {pid} holds a sibling's pty master: {masters:?}"
    );
    assert!(
        fds.iter().any(|p| p.contains("/dev/pts/")),
        "the shell still has its own tty: {fds:?}"
    );
    for id in ["inherit-a", "inherit-b"] {
        let _: KeeperMessage = keeper_call(
            &socket.to_string_lossy(),
            &KeeperMessage::Kill(Kill {
                session_id: id.to_string(),
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();
    }
}

/// A killed keeper ends its shells — even when a *later* session ignores `SIGHUP`.
///
/// The kernel sends `SIGHUP` when the last descriptor for a pty master closes,
/// and the keeper holds exactly one per session. The inheritance broke that:
/// because a fork inherited *every earlier* master, the last-spawned shell was
/// the real holder of all the others, so the keeper's death closed nothing by
/// itself and the hangup travelled backwards along a chain — every earlier shell
/// dying only once the later one did. One shell that ignores `SIGHUP` (which a
/// program is allowed to do) snapped the chain at its end, and every session
/// spawned before it stayed alive with its master held open by a stranger.
///
/// **This is the test that fails without the fix** (checked against the
/// unfixed keeper: the two normal shells are still running at the end of the
/// wait). The `no-hup` shell is spawned last on purpose, so it holds both
/// earlier masters; it is expected to survive and is killed by the test.
#[tokio::test]
async fn a_killed_keeper_ends_its_shells_even_when_a_later_one_ignores_hup() {
    let socket = socket_path();
    let bin = env!("CARGO_BIN_EXE_ldmux-keeper");
    let mut keeper = std::process::Command::new(bin)
        .arg(&socket)
        .env("SHELL", "/bin/bash")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ldmux-keeper");
    for _ in 0..600 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(socket.exists(), "the keeper bound its socket");

    let spawn = |session_id: &str, command: Option<Vec<String>>| {
        let socket = socket.clone();
        let session_id = session_id.to_string();
        async move {
            let res: KeeperMessage = keeper_call(
                &socket.to_string_lossy(),
                &KeeperMessage::Spawn(Spawn {
                    session_id,
                    cols: 80,
                    rows: 24,
                    cwd: None,
                    command,
                    env: None,
                }),
                DEFAULT_TIMEOUT_MS,
            )
            .await
            .unwrap();
            match res {
                KeeperMessage::State(st) => st.pid,
                other => panic!("expected State, got {other:?}"),
            }
        }
    };
    // Two shells that behave normally, then one that will not take the hangup.
    let normal_a = spawn("hup-a", None).await;
    let normal_b = spawn("hup-b", None).await;
    let stubborn = spawn(
        "hup-c",
        Some(vec![
            "/bin/bash".into(),
            "-c".into(),
            "trap '' HUP; while :; do sleep 1; done".into(),
        ]),
    )
    .await;

    let alive = |pid: i32| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
    assert!(alive(normal_a) && alive(normal_b) && alive(stubborn));

    let _ = keeper.kill();
    let _ = keeper.wait();
    let mut gone = 0;
    for _ in 0..200 {
        gone = [normal_a, normal_b].iter().filter(|p| !alive(**p)).count();
        if gone == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(stubborn),
        nix::sys::signal::Signal::SIGKILL,
    );
    let _ = std::fs::remove_file(&socket);
    assert_eq!(
        gone,
        2,
        "the shells of a dead keeper: hup-a={} hup-b={} alive",
        alive(normal_a),
        alive(normal_b)
    );
}

/// A write that blocks does not block the keeper.
///
/// A pty's input buffer is small and a foreground program that is not reading
/// fills it, at which point `write(2)` waits for the reader, not for the kernel.
/// The keeper's runtime is single-threaded, so a write done inline waited for the
/// *keeper*: one paste into one such pane stopped every session's `List`,
/// `Snapshot` and `Peek` until the program read or died. Measured on the deployed
/// keeper, 1.6 MB of lines into a `sleep 60`: the keeper answered nothing for
/// 20 s, including a `Kill`, and the write was still blocked when the client gave
/// up.
///
/// **This is the test that fails without the fix**, and it has to run the keeper
/// in its own process to be able to see it: a stall inside the test's own runtime
/// freezes the client too, so the client's timeout never fires and the test would
/// pass whatever the keeper did. Here the client is in the test and the keeper is
/// in another process, so "the keeper answered while a write was blocked" is a
/// real observation. Checked against an unfixed keeper: the `List` times out.
///
/// The program sleeps 3 s and *then* reads, so the write finishes on its own and
/// no blocked thread is left behind.
#[tokio::test]
async fn a_write_that_blocks_does_not_block_the_keeper() {
    let socket = socket_path();
    let bin = env!("CARGO_BIN_EXE_ldmux-keeper");
    let mut keeper = std::process::Command::new(bin)
        .arg(&socket)
        .env("SHELL", "/bin/bash")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ldmux-keeper");
    for _ in 0..600 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(socket.exists(), "the keeper bound its socket");
    let socket = socket.to_string_lossy().into_owned();

    let res: KeeperMessage = keeper_call(
        &socket,
        &KeeperMessage::Spawn(Spawn {
            session_id: "blocked".to_string(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 3; cat > /dev/null".to_string(),
            ]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(res, KeeperMessage::State(_)), "spawned: {res:?}");

    // 1 MiB of *complete lines*. The newlines matter: with them the line
    // discipline has whole lines to hand the reader, so a reader that is not
    // reading fills the queue and the write blocks. Without them the canonical
    // line buffer overruns and the kernel discards the excess instead — measured
    // 2026-10-05: 1.6 MB of `"x"*79 + "\n"` blocked for as long as the program
    // slept (10.34 s against a `sleep 10`), while 1.6 MB of one unbroken line was
    // accepted in 0.82 s and the tail was thrown away by the tty.
    let payload = ("x".repeat(79) + "\n").repeat(16 * 1024);
    let writer_socket = socket.clone();
    let started = std::time::Instant::now();
    let writer = tokio::spawn(async move {
        let _: Result<KeeperMessage, _> = keeper_call(
            &writer_socket,
            &KeeperMessage::Write(Write {
                session_id: "blocked".to_string(),
                data: payload,
            }),
            30_000,
        )
        .await;
    });
    // Let the write reach the pty and block.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let t0 = std::time::Instant::now();
    let answered = keeper_call(&socket, &KeeperMessage::List(List {}), 2_000).await;
    let took = t0.elapsed();
    // The write must still be blocked, or the test proved nothing.
    assert!(
        answered.is_ok(),
        "the keeper answered while a write was blocked (after {took:?}): {answered:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(2_500),
        "the write had already finished; the test proved nothing"
    );
    // The program wakes and reads, so the write completes.
    let _ = writer.await;
    let _: Result<KeeperMessage, _> = keeper_call(
        &socket,
        &KeeperMessage::Kill(Kill {
            session_id: "blocked".to_string(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
    let _ = keeper.kill();
    let _ = keeper.wait();
    let _ = std::fs::remove_file(&socket);
}

// ---- audit hardening -------------------------------------------------------

fn spawn_req(
    id: &str,
    cols: u64,
    rows: u64,
    cwd: Option<&str>,
    command: Option<&[&str]>,
) -> KeeperMessage {
    KeeperMessage::Spawn(Spawn {
        session_id: id.to_string(),
        cols,
        rows,
        cwd: cwd.map(str::to_string),
        command: command.map(|c| c.iter().map(|s| s.to_string()).collect()),
        env: None,
    })
}

/// Peek the session until `needle` shows up (or the budget runs out); returns
/// everything seen.
/// True when `haystack` contains a cursor-position report: `CSI <row> ; <col> R`.
///
/// The digits are checked for shape, not for a position, for the reason given where this
/// is used: the report's cell depends on whether the emulator answered the query before
/// the pane's own echo of that query moved the cursor.
fn has_cursor_report(haystack: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            let (mut digits, mut semi) = (false, false);
            while j < bytes.len() {
                match bytes[j] {
                    b'0'..=b'9' => digits = true,
                    b';' if digits => semi = true,
                    b'R' if digits && semi => return true,
                    _ => break,
                }
                j += 1;
            }
        }
        i += 1;
    }
    false
}

async fn peek_for(socket: &str, id: &str, needle: &str) -> String {
    peek_until(socket, id, |text| text.contains(needle)).await
}

/// Peek `id` until `done` accepts the accumulated text, then return it.
///
/// A needle alone is not enough for output that arrives in pieces: `peek_for`
/// answered "SigIgn:" the moment the first fragment landed, and the caller then
/// parsed a line with no value in it. A predicate lets a caller wait for the
/// *complete* line it is about to parse.
async fn peek_until(
    socket: &str,
    id: &str,
    done: impl Fn(&str) -> bool,
) -> String {
    let mut seen = Vec::new();
    for _ in 0..1200 {
        if let Ok(KeeperMessage::PeekResult(pr)) = keeper_call(
            socket,
            &KeeperMessage::Peek(Peek::all(id.to_string())),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            seen.extend_from_slice(&pr.data);
        }
        if done(&String::from_utf8_lossy(&seen)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    String::from_utf8_lossy(&seen).into_owned()
}

fn pid_of(st: &KeeperMessage) -> i32 {
    match st {
        KeeperMessage::State(s) => s.pid,
        other => panic!("expected State, got {other:?}"),
    }
}

/// True while `/proc/<pid>` exists — a zombie still has an entry, so this is
/// "has not been reaped".
fn proc_exists(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test]
async fn a_duplicate_session_id_is_refused_and_the_first_survives() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    let first = keeper_call(
        &s,
        &spawn_req("dup", 80, 24, None, None),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let pid = pid_of(&first);
    let err = keeper_call(
        &s,
        &spawn_req("dup", 80, 24, None, None),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("already exists"),
        "got: {}",
        err.message
    );
    // The original shell is still the session's, and still alive.
    let st = keeper_call(
        &s,
        &KeeperMessage::Read(Read {
            session_id: "dup".into(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match st {
        KeeperMessage::State(st) => assert!(!st.dead && st.pid == pid),
        other => panic!("expected State, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_clamps_the_size_like_resize() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    match keeper_call(&s, &spawn_req("tiny", 1, 1, None, None), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap()
    {
        KeeperMessage::State(st) => assert_eq!((st.cols, st.rows), (20, 5)),
        other => panic!("expected State, got {other:?}"),
    }
    match keeper_call(
        &s,
        &spawn_req("huge", 9999, 9999, None, None),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap()
    {
        // The protocol's ceiling, not the old 500 that sat below a real wide window
        // (`ldmux_protocol::MAX_PANE_COLS`, raised to 2000 on 2026-11-02).
        KeeperMessage::State(st) => assert_eq!((st.cols, st.rows), (2000, 255)),
        other => panic!("expected State, got {other:?}"),
    }
    // The emulator's grid is the clamped one too.
    let snap = snapshot(&s, "tiny").await;
    assert_eq!((snap.cols, snap.rows), (20, 5));
}

/// Rust's runtime ignores SIGPIPE and an ignored disposition survives exec, so
/// until the fork child reset it every shell started with SIGPIPE ignored.
#[tokio::test]
async fn a_child_starts_with_sigpipe_at_its_default() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    // The test process itself ignores it (the harness is a Rust main), so this
    // is a real before/after.
    let ours = std::fs::read_to_string("/proc/self/status").unwrap();
    let mask = |status: &str| {
        let line = status.lines().find(|l| l.starts_with("SigIgn:")).unwrap();
        u64::from_str_radix(line.split_whitespace().nth(1).unwrap(), 16).unwrap()
    };
    const SIGPIPE_BIT: u64 = 1 << 12;
    assert!(
        mask(&ours) & SIGPIPE_BIT != 0,
        "the premise: the test process ignores SIGPIPE"
    );
    keeper_call(
        &s,
        &spawn_req(
            "sp",
            200,
            24,
            None,
            Some(&[
                "/bin/sh",
                "-c",
                "grep '^SigIgn:' /proc/self/status; sleep 30",
            ]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    // Wait for a *complete* line: the mask is hex digits after the label, and a peek that
    // returns only "SigIgn:" is a fragment of the same line, not an answer.
    let complete = |text: &str| {
        text.lines().any(|l| {
            l.strip_prefix("SigIgn:")
                .map(|v| !v.trim().is_empty() && v.trim().chars().all(|c| c.is_ascii_hexdigit()))
                .unwrap_or(false)
        })
    };
    let text = peek_until(&s, "sp", complete).await;
    let line = text
        .lines()
        .find(|l| l.starts_with("SigIgn:"))
        .unwrap_or_else(|| panic!("no SigIgn in {text:?}"));
    let child = u64::from_str_radix(line.split_whitespace().nth(1).unwrap().trim(), 16).unwrap();
    assert_eq!(
        child & SIGPIPE_BIT,
        0,
        "SIGPIPE must be default in the child: {line}"
    );
}

#[tokio::test]
async fn a_chdir_failure_fails_the_spawn_and_leaves_no_session_or_zombie() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    let err = keeper_call(
        &s,
        &spawn_req("badcwd", 80, 24, Some("/definitely/not/a/dir"), None),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("chdir"), "got: {}", err.message);
    assert!(
        err.message.contains("/definitely/not/a/dir"),
        "got: {}",
        err.message
    );
    match keeper_call(&s, &KeeperMessage::List(List {}), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap()
    {
        KeeperMessage::ListResult(lr) => assert!(lr.sessions.is_empty()),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_exec_failure_fails_the_spawn() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    let err = keeper_call(
        &s,
        &spawn_req("noexec", 80, 24, None, Some(&["/definitely/not/a/program"])),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("exec"), "got: {}", err.message);
    assert!(
        err.message.contains("/definitely/not/a/program"),
        "got: {}",
        err.message
    );
    // The keeper still works for the next spawn.
    keeper_call(
        &s,
        &spawn_req("after", 80, 24, None, None),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_valid_cwd_is_entered() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    keeper_call(
        &s,
        &spawn_req(
            "cwd",
            200,
            24,
            Some("/usr"),
            Some(&["/bin/sh", "-c", "echo HERE=$(pwd); sleep 30"]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let text = peek_for(&s, "cwd", "HERE=/usr").await;
    assert!(text.contains("HERE=/usr"), "got {text:?}");
}

#[tokio::test]
async fn kill_reaps_the_shell_so_no_zombie_remains() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    let pid = pid_of(
        &keeper_call(&s, &spawn_req("kz", 80, 24, None, None), DEFAULT_TIMEOUT_MS)
            .await
            .unwrap(),
    );
    assert!(proc_exists(pid));
    keeper_call(
        &s,
        &KeeperMessage::Kill(Kill {
            session_id: "kz".into(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(
        !proc_exists(pid),
        "the killed shell must be waited on, not left a zombie"
    );
}

#[tokio::test]
async fn killing_an_already_dead_session_succeeds_without_signalling() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    keeper_call(
        &s,
        &spawn_req("kd", 80, 24, None, Some(&["/bin/sh", "-c", "exit 3"])),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let dead = wait_until_async(|| async {
        matches!(
            keeper_call(&s, &KeeperMessage::Read(Read { session_id: "kd".into() }), DEFAULT_TIMEOUT_MS).await,
            Ok(KeeperMessage::State(st)) if st.dead
        )
    })
    .await;
    assert!(dead, "the exit is noticed");
    let r = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill {
            session_id: "kd".into(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(r, KeeperMessage::State(st) if st.dead));
    assert!(
        keeper_call(
            &s,
            &KeeperMessage::Read(Read {
                session_id: "kd".into()
            }),
            DEFAULT_TIMEOUT_MS
        )
        .await
        .is_err()
    );
}

async fn wait_until_async<F, Fut>(f: F) -> bool
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..1200 {
        if f().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    false
}

/// With nobody calling the keeper, an exited shell is still collected: the
/// reaper is periodic, not request-driven.
#[tokio::test]
async fn an_exited_shell_is_reaped_with_no_request_to_trigger_it() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy();
    let pid = pid_of(
        &keeper_call(
            &s,
            &spawn_req("rp", 80, 24, None, Some(&["/bin/sh", "-c", "sleep 0.3"])),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap(),
    );
    // No keeper_call from here on.
    assert!(
        wait_until(|| !proc_exists(pid)).await,
        "the zombie must be reaped by the keeper itself"
    );
}

#[tokio::test]
async fn a_second_keeper_refuses_a_live_socket_and_leaves_it_working() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let k2 = std::sync::Arc::new(keeper::Keeper::new(vec!["/bin/sh".to_string()]));
    let err = keeper::serve(&k2, &socket).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "got {err}");
    assert!(err.to_string().contains("already serving"), "got {err}");
    // The first is untouched.
    let r = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::List(List {}),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
    assert!(matches!(r, Ok(KeeperMessage::ListResult(_))));
}

#[tokio::test]
async fn a_stale_socket_is_replaced_and_is_owner_only_from_the_start() {
    use std::os::unix::fs::PermissionsExt;
    let socket = socket_path();
    // A bound-then-dropped listener leaves the file behind with nothing accepting.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    let _k = boot(&socket).await;
    // `boot` returns as soon as the path exists, and the stale file already does:
    // wait for the keeper to be what answers on it.
    let sock = socket.to_string_lossy().to_string();
    let up = wait_until_async(|| async {
        matches!(
            keeper_call(&sock, &KeeperMessage::List(List {}), DEFAULT_TIMEOUT_MS).await,
            Ok(KeeperMessage::ListResult(_))
        )
    })
    .await;
    assert!(up, "the keeper must come up beside a stale socket");
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn an_overlong_socket_path_is_a_clear_error() {
    let long = std::env::temp_dir().join("x".repeat(120));
    let k = std::sync::Arc::new(keeper::Keeper::new(vec!["/bin/sh".to_string()]));
    let err = keeper::serve(&k, &long).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("limit"), "got {err}");
}

/// A bounded peek answers in the compact form, never more than asked, and says
/// when more is waiting; the original peek still answers with the whole buffer as
/// a byte array (a daemon that predates `max_bytes` depends on it).
#[tokio::test]
async fn a_bounded_peek_is_chunked_compact_and_lossless_and_the_old_peek_still_works() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    let id = "bp1";
    keeper_call(
        &s,
        &spawn_req(
            id,
            80,
            24,
            None,
            Some(&[
                "/bin/sh",
                "-c",
                "head -c 300000 /dev/zero | tr '\\0' 'x'; echo END_MARK; sleep 30",
            ]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    // Let the 300 KB land in the keeper's buffer (not peeking meanwhile).
    for _ in 0..400 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if let Ok(KeeperMessage::State(_)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read {
                session_id: id.into(),
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            // buffered_len is not on the wire; the first bounded peek below says more=true once it is full enough
        }
        let probe = keeper_call(
            &s,
            &KeeperMessage::Peek(Peek::bounded(id, 1)),
            DEFAULT_TIMEOUT_MS,
        )
        .await;
        if let Ok(KeeperMessage::PeekResult(pr)) = probe
            && pr.more
        {
            // took 1 byte and more is waiting: continue below, counting that byte.
            let mut got = pr.into_bytes().unwrap();
            let mut chunks = 0;
            loop {
                let KeeperMessage::PeekResult(pr) = keeper_call(
                    &s,
                    &KeeperMessage::Peek(Peek::bounded(id, 64 * 1024)),
                    DEFAULT_TIMEOUT_MS,
                )
                .await
                .unwrap() else {
                    panic!("not a peekresult")
                };
                assert!(pr.data.is_empty(), "the compact form carries no byte array");
                assert!(pr.data64.is_some());
                let more = pr.more;
                let b = pr.into_bytes().unwrap();
                assert!(b.len() <= 64 * 1024, "a chunk is bounded: {}", b.len());
                got.extend(b);
                chunks += 1;
                if !more && String::from_utf8_lossy(&got).contains("END_MARK") {
                    break;
                }
                if !more {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            }
            assert!(chunks >= 2, "300 KB is more than one 64 KiB chunk");
            let xs = got.iter().filter(|&&c| c == b'x').count();
            assert_eq!(xs, 300_000, "no byte lost or duplicated across chunks");
            break;
        }
    }
    // The original contract on the same session.
    let KeeperMessage::PeekResult(pr) =
        keeper_call(&s, &KeeperMessage::Peek(Peek::all(id)), DEFAULT_TIMEOUT_MS)
            .await
            .unwrap()
    else {
        panic!()
    };
    assert!(pr.data64.is_none() && !pr.more);
    let _ = keeper_call(
        &s,
        &KeeperMessage::Kill(Kill {
            session_id: id.into(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
}

/// The keeper reports how the shell ended: its exit status, or the signal.
#[tokio::test]
async fn a_dead_pane_reports_its_exit_status_or_signal() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    for (id, cmd, code, sig) in [
        ("ex3", "exit 3", Some(3i64), None),
        ("ex0", "exit 0", Some(0), None),
        ("exk", "kill -9 $$", None, Some(9i32)),
    ] {
        keeper_call(
            &s,
            &spawn_req(id, 80, 24, None, Some(&["/bin/sh", "-c", cmd])),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();
        let mut seen = None;
        for _ in 0..400 {
            if let Ok(KeeperMessage::State(st)) = keeper_call(
                &s,
                &KeeperMessage::Read(Read {
                    session_id: id.into(),
                }),
                DEFAULT_TIMEOUT_MS,
            )
            .await
                && st.dead
            {
                seen = Some(st);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let st = seen.unwrap_or_else(|| panic!("{id} never reported dead"));
        assert_eq!((st.exit_code, st.exit_signal), (code, sig), "{id}");
    }
}

/// An OSC 8 hyperlink survives into the snapshot, on the live screen and after it
/// has scrolled into history: the replay a reconnecting client reads must link the
/// same text to the same target, or the link is lost on every reconnect.
#[tokio::test]
async fn a_hyperlink_survives_into_the_snapshot_live_and_in_history() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    let id = "link-1".to_string();
    let script = "printf '\\033]8;;https://example.com/a\\033\\\\LINKTEXT\\033]8;;\\033\\\\ plain\\n'; \
                  i=1; while [ $i -le 40 ]; do printf 'row%03d\\n' $i; i=$((i+1)); done; sleep 30";
    let spawned = keeper_call(
        &s,
        &KeeperMessage::Spawn(Spawn {
            session_id: id.clone(),
            cols: 80,
            rows: 10,
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), script.into()]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(spawned, KeeperMessage::State(_)));

    // The output has landed once the last row is on screen.
    let mut landed = false;
    for _ in 0..200 {
        if snapshot(&s, &id).await.text.contains("row040") {
            landed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(landed, "the output reached the emulator");

    // The link scrolled off a 10-row screen into history: ask for the history.
    let with_history = match keeper_call(
        &s,
        &KeeperMessage::Snapshot(km::Snapshot {
            session_id: id.clone(),
            history_lines: Some(100),
            drain_output: false,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap()
    {
        KeeperMessage::SnapshotResult(sr) => sr,
        other => panic!("expected SnapshotResult, got {other:?}"),
    };
    let open = "\x1b]8;;https://example.com/a\x1b\\";
    let close = "\x1b]8;;\x1b\\";
    let at = with_history.text.find(open).unwrap_or_else(|| {
        panic!(
            "the link target is in the snapshot: {:?}",
            with_history.text
        )
    });
    let linked = &with_history.text[at + open.len()..];
    assert!(
        linked.starts_with("LINKTEXT"),
        "the link wraps its own text: {linked:?}"
    );
    assert!(
        linked["LINKTEXT".len()..].starts_with(close),
        "the link is closed right after its text: {linked:?}"
    );
    // A row with no link carries no OSC 8 at all.
    assert_eq!(with_history.text.matches(open).count(), 1);
}

// ---- the Kitty keyboard protocol -----------------------------------------

/// A program that asks the Kitty keyboard protocol question is answered by the
/// keeper, and the flags it pushed are what it is told.
///
/// This is the one thing that makes `Shift+Enter` reachable at all: `pi` gates
/// its Shift+Enter handling on this reply and treats silence as "this terminal
/// cannot tell the keys apart", which is why it submitted the line instead of
/// inserting a newline. The engine parses the program's push but answers
/// queries only through a write-back callback, and the keeper registers none.
///
/// `stty raw -echo` because the point is to see the bytes the program reads: in
/// canonical mode the line discipline echoes escape characters as `^[` and the
/// A program that asks for the Kitty keyboard protocol is answered, and the pane's
/// state reports the flags it asked for.
///
/// The emulator parses a program's push (`CSI > <flags> u`) but answers queries only
/// through a write-back callback, and the keeper used to register none — so the query
/// went unanswered, `pi` treated the terminal as unable to tell a modified key from a
/// plain one, and `Shift+Enter` submitted the line instead of inserting a newline.
///
/// **The program prints the query**, the way a real one does, and reads the answer.
/// (The previous version wrote the query into the pane's *input* and relied on `cat`
/// echoing it back for the emulator to see, which is not a program asking a
/// terminal; it also meant the answer was re-answered for ever.)
///
/// `cat -v` because the keeper drops a *reply* from the pane's output: a reply is not
/// content, and an echoed one would be answered again (see `without_kitty_replies`).
/// `-v` renders the answer cat read as `^[[?0u`, which is not a reply and is kept.
#[tokio::test]
async fn a_kitty_keyboard_query_is_answered_by_the_keeper() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();

    // Nothing pushed yet: the answer says so, rather than not arriving.
    keeper_call(
        &s,
        &spawn_req(
            "kitty0",
            80,
            24,
            None,
            Some(&[
                "/bin/sh",
                "-c",
                "printf '\\033[?u'; stty raw -echo; exec cat -v",
            ]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let seen = peek_for(&s, "kitty0", "^[[?0u").await;
    assert!(seen.contains("^[[?0u"), "no answer to the query: {seen:?}");

    // A program that pushes the disambiguate flag first, then asks.
    keeper_call(
        &s,
        &spawn_req(
            "kitty1",
            80,
            24,
            None,
            Some(&[
                "/bin/sh",
                "-c",
                "printf '\\033[>1u\\033[?u'; stty raw -echo; exec cat -v",
            ]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();

    let mut flags = 0u8;
    for _ in 0..200 {
        if let Ok(KeeperMessage::State(st)) = keeper_call(
            &s,
            &KeeperMessage::Read(Read {
                session_id: "kitty1".to_string(),
            }),
            DEFAULT_TIMEOUT_MS,
        )
        .await
        {
            flags = st.kitty_keyboard;
            if flags == 1 {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(flags, 1, "the pushed flag is in the pane state");

    // ...and the answer carries it.
    let seen = peek_for(&s, "kitty1", "^[[?1u").await;
    assert!(
        seen.contains("^[[?1u"),
        "the answer does not carry the pushed flag: {seen:?}"
    );
}

/// A terminal reply that comes back as *output* is not answered again.
///
/// The write-back writes `CSI ? 1 u` into the pty master, i.e. into the program's
/// input. A program that echoes what it reads (`cat`, a pager, a shell with echo on)
/// turns that into output, the emulator parses it — and, because the parameterised
/// form is query-shaped, answers it again. That loop never stops: measured
/// 2026-10-25 at about sixty replies a second through a pane running `cat`, filling
/// the pane and the wire at once. A reply is not content, so the keeper drops it from
/// the output stream before the emulator sees it.
#[tokio::test]
async fn the_keeper_does_not_answer_its_own_reply() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    keeper_call(
        &s,
        &spawn_req(
            "replyloop",
            80,
            24,
            None,
            // The program asks (on stdout), then echoes its input raw — which is
            // what turned a reply into a question before the filter existed.
            Some(&[
                "/bin/sh",
                "-c",
                "printf '\\033[?u'; stty raw -echo; exec cat",
            ]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let write = |data: &str| {
        KeeperMessage::Write(Write {
            session_id: "replyloop".to_string(),
            data: data.to_string(),
        })
    };

    // The answer to the program's own query was written into the pane's input, and
    // `cat` echoes it raw: without the filter this is where the loop began.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // A reply written into the pane as the write-back would, followed by a marker the
    // echo will carry. The marker proves the pane is alive and echoing; the reply
    // itself must never come back as output for the emulator to answer again.
    keeper_call(&s, &write("\x1b[?1uMARKER"), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap();
    let after = peek_for(&s, "replyloop", "MARKER").await;
    assert!(after.contains("MARKER"), "the pane stopped echoing: {after:?}");
    // The reply form is `CSI ? <flags> u`; the program's own *query* (`CSI ? u`) is
    // in the same buffer and is not a reply. The caret-notation echo of the answer
    // (`^[[?0u`) is text, not a sequence, and is not one either.
    for reply in ["\u{1b}[?0u", "\u{1b}[?1u"] {
        assert!(
            !after.contains(reply),
            "a reply reached the emulator as output ({reply:?}), which is the loop: {after:?}"
        );
    }
}

/// a cursor-position report blocked or guessed.
#[tokio::test]
async fn a_cursor_report_and_a_device_attributes_query_are_answered() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let s = socket.to_string_lossy().into_owned();
    keeper_call(
        &s,
        &spawn_req(
            "queries",
            80,
            24,
            None,
            Some(&["/bin/sh", "-c", "stty raw -echo; exec cat"]),
        ),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    let write = |data: &str| {
        KeeperMessage::Write(Write {
            session_id: "queries".to_string(),
            data: data.to_string(),
        })
    };

    // DSR 6 = cursor position report. **The cell is not asserted**: the pane runs `cat`,
    // which prints the query it reads, and that echo moves the cursor before or after
    // the emulator answers. Whichever wins decides whether the report says `1;1R` or the
    // column the echo reached, so asserting `1;1R` made this test pass alone and fail in
    // a parallel suite while the emulator answered correctly every time. The property the
    // test is named for is that the query is answered at all.
    keeper_call(&s, &write("\x1b[6n"), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap();
    let seen = peek_for(&s, "queries", "R").await;
    assert!(has_cursor_report(&seen), "no cursor report: {seen:?}");

    // DA1 = `CSI c`, answered with a device-attributes report.
    keeper_call(&s, &write("\x1b[c"), DEFAULT_TIMEOUT_MS)
        .await
        .unwrap();
    let seen = peek_for(&s, "queries", "\x1b[?").await;
    assert!(
        seen.contains("\x1b[?") && seen.contains("c"),
        "no device attributes: {seen:?}"
    );
}
