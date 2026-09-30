//! The attach client boots headless and pipes a session's bytes (the gate:
//! "attach boots headless"). No live system, no live daemon: a scratch keeper
//! in-process and a scratch pty pair as the attach's local terminal.
//!
//! The piping is verified through the keeper (the attach sends input to the
//! session, the shell echoes, and the attach reads the session's output),
//! which is the meaningful direction; the pty is the attach's local terminal
//! (its in/out fds).

use ldmux_protocol::keeper::{KeeperMessage, Kill, Spawn};
use ldmux_protocol::keeper_call::{keeper_call, DEFAULT_TIMEOUT_MS};

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn socket_path() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ldmux-attach-test-{}-{}.sock",
        std::process::id(),
        n
    ))
}

async fn boot(socket: &std::path::Path) -> std::sync::Arc<keeper::Keeper> {
    let k = std::sync::Arc::new(keeper::Keeper::new(vec!["/bin/sh".to_string()]));
    let k2 = k.clone();
    let sock = socket.to_path_buf();
    tokio::spawn(async move {
        let _ = keeper::serve(&k2, &sock).await;
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    k
}

/// A scratch pty pair for the attach's local terminal. Returns the slave fd
/// (the attach reads/writes it) and both `OwnedFd`s (kept open for the test's
/// duration, so the pty stays alive and the slave fd number remains valid).
fn local_terminal() -> (i32, std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    let ws = nix::libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = nix::pty::openpty(Some(&ws), None).unwrap();
    (opened.slave.as_raw_fd(), opened.master, opened.slave)
}

#[tokio::test]
async fn attach_boots_headless_and_pipes_bytes() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let id = "s1".to_string();

    // Spawn a session.
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
        KeeperMessage::State(st) => assert!(!st.dead),
        other => panic!("expected State, got {other:?}"),
    }

    // The attach's local terminal (a scratch pty pair).
    let (slave, _master, _slave_keep) = local_terminal();

    // BOOT the attach (headless): connect + attach to the session. This is the
    // gate — "attach boots headless".
    let a = attach::Attach::connect(&socket.to_string_lossy(), &id, slave, slave)
        .await
        .unwrap();
    assert_eq!(a.session_id(), id);

    // The attach sends input to the session (the Write direction).
    a.send_input("printf 'ldmux-hi\\n'")
        .await
        .unwrap();

    // The shell runs it and echoes; the keeper buffers the output. The attach
    // reads it back (the Peek direction). Poll for it.
    let mut out: Vec<u8> = Vec::new();
    for _ in 0..400 {
        out = a.read_output().await.unwrap();
        if String::from_utf8_lossy(&out).contains("ldmux-hi") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        String::from_utf8_lossy(&out).contains("ldmux-hi"),
        "the attach read the session's output: {out:?}"
    );

    // The attach pushes the output to its local terminal (write to the slave).
    a.write_output(&out);

    // Kill the session: the shell exits, the keeper's reader unblocks, and the
    // process can exit.
    let res: KeeperMessage = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Kill(Kill {
            session_id: id,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    match res {
        KeeperMessage::State(st) => assert!(st.dead),
        other => panic!("expected State, got {other:?}"),
    }
}

/// The attach rejects a session that is unknown (the Read handshake errors).
#[tokio::test]
async fn attach_rejects_an_unknown_session() {
    let socket = socket_path();
    let _k = boot(&socket).await;
    let (slave, _master, _slave_keep) = local_terminal();
    let res = attach::Attach::connect(&socket.to_string_lossy(), "nope", slave, slave).await;
    assert!(res.is_err(), "attaching an unknown session is an error");
}
