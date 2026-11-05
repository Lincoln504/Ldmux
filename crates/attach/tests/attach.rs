//! The attach client's local-terminal helpers. The full daemon↔attach loop is
//! tested in `crates/daemon/tests/attach_client.rs` (which can start a daemon;
//! the attach crate cannot, without a dependency cycle).

/// A tty that will not say its size is reported as unknown, never as an invented 80x24.
///
/// The old fallback was the bug: `80x24` was sent as the attach and re-sent on every
/// snapshot, so a window whose ioctl failed was told to be 24x80 and agreed, and stayed
/// that way in a frame many times larger until the user resized it by hand.
#[test]
fn tty_size_is_unknown_rather_than_invented() {
    match attach::tty_size_for_test() {
        // No opinion, which is honest and lets the pane keep its own size.
        None => {}
        // A real answer is a real size.
        Some((c, r)) => assert!(c > 0 && r > 0, "a measured size is positive, got {c}x{r}"),
    }
}

use std::os::fd::AsFd;
use std::process::{Command, Stdio};

fn scratch() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "ldmux-attach-t-{}-{}",
        std::process::id(),
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Not a terminal: refuse, say so, and say what to do instead.
#[test]
fn refuses_when_stdin_is_not_a_terminal() {
    let dir = scratch();
    let out = Command::new(env!("CARGO_BIN_EXE_ldmux-attach"))
        .arg("some-session")
        .env("LDMUX_STATE_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not a terminal") && err.contains("ldmux shell"),
        "{err}"
    );
}

/// The window puts the terminal in raw mode, and SIGTERM puts it back: cooked
/// flags restored, and the mode resets written to the terminal.
#[test]
fn a_termination_signal_restores_the_terminal() {
    use nix::sys::termios::{LocalFlags, tcgetattr};
    use std::io::Read;
    let dir = scratch();
    let pty = nix::pty::openpty(None, None).unwrap();
    let cooked = tcgetattr(pty.slave.as_fd()).unwrap();
    assert!(
        cooked
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ECHO),
        "a pty starts cooked"
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_ldmux-attach"))
        .arg("some-session")
        // No daemon here: the client enters raw mode and then retries, which is
        // exactly the state a signal has to be survivable in.
        .env("LDMUX_STATE_DIR", &dir)
        .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut raw = false;
    for _ in 0..200 {
        let t = tcgetattr(pty.slave.as_fd()).unwrap();
        if !t
            .local_flags
            .intersects(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG)
        {
            raw = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(raw, "the client must put the terminal in raw mode");

    // SIGTERM goes to the tokio handler only once it is installed, which happens
    // right after raw mode is set; a signal that lands earlier would kill the
    // process with the default action. Poll for the retry note as the proof.
    let mut master = std::fs::File::from(pty.master);
    let mut seen = Vec::new();
    for _ in 0..200 {
        let mut buf = [0u8; 512];
        let fd = master.as_fd();
        let mut fds = [nix::poll::PollFd::new(fd, nix::poll::PollFlags::POLLIN)];
        if nix::poll::poll(&mut fds, 25u16).unwrap() > 0 {
            let n = master.read(&mut buf).unwrap_or(0);
            seen.extend_from_slice(&buf[..n]);
        }
        if String::from_utf8_lossy(&seen).contains("retrying") {
            break;
        }
    }
    assert!(
        String::from_utf8_lossy(&seen).contains("retrying"),
        "client never reached its loop"
    );

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "terminated by a signal is reported, not a crash: {status:?}"
    );

    let after = tcgetattr(pty.slave.as_fd()).unwrap();
    assert!(
        after
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG),
        "cooked flags restored"
    );
    // The resets were written before the process ended.
    let mut tail = Vec::new();
    for _ in 0..40 {
        let mut buf = [0u8; 512];
        let fd = master.as_fd();
        let mut fds = [nix::poll::PollFd::new(fd, nix::poll::PollFlags::POLLIN)];
        if nix::poll::poll(&mut fds, 25u16).unwrap() == 0 {
            break;
        }
        let n = master.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..n]);
    }
    seen.extend(tail);
    let text = String::from_utf8_lossy(&seen);
    for needle in ["\x1b[?2004l", "\x1b[?1000l", "\x1b[?1049l", "\x1b[?25h"] {
        assert!(
            text.contains(needle),
            "missing reset {needle:?} in {text:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
