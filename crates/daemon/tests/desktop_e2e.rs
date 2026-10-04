//! Desktop live-window e2e (plan §6): Xvfb + i3 scratch; the live-window
//! lifecycle functions (open/state/close) run against a real X server and WM
//! without panicking.
//!
//! **What this does not cover:** the ownership/geometry semantics of a window
//! (size-on-attach, handover, close≠detach) live in `handover.rs`, which drives
//! two real remote-WS clients against a daemon with no X server; the full
//! alacritty-stub handover (a real window driven through i3) is still not built.

use std::process::Command;

use ldmux_daemon::desktop;

fn scratch_dir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-deske2e-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Kill both children when the test ends, however it ends.
struct Guard(Vec<std::process::Child>);
impl Drop for Guard {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[tokio::test]
async fn desktop_attach_e2e_functions_do_not_panic() {
    // A random display, so two runs (or a leftover) never collide on the
    // X server's WM-selection atom.
    let display = format!(":{}", 90 + (rand::random::<u8>() % 8));
    let xvfb = Command::new("Xvfb")
        .arg(&display)
        .arg("-screen")
        .arg("0")
        .arg("1024x768x24")
        .spawn()
        .expect("failed to start Xvfb");
    let mut guard = Guard(vec![xvfb]);
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    let i3 = Command::new("i3")
        .env("DISPLAY", &display)
        .spawn()
        .expect("failed to start i3");
    guard.0.push(i3);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    unsafe { std::env::set_var("DISPLAY", &display) };

    let state_dir = scratch_dir();
    let tmux_sock = state_dir.join("keeper.sock");
    let session_id = "s1".to_string();
    let tmux_name = "ldmux-s1".to_string();
    let tmux_bin = "alacritty".to_string();
    let workspace = "1".to_string();

    // Open a live window (the argv is what matters; without a real session the
    // i3 exec may place nothing, which is fine).
    let _ = desktop::open_live(
        tmux_sock.to_str().unwrap(),
        &tmux_name,
        &session_id,
        &workspace,
        &tmux_bin,
        state_dir.to_str().unwrap(),
    )
    .await;

    let state = desktop::live_window_state(&session_id).await;
    assert!(
        matches!(state.unwrap_or("unknown"), "absent" | "unknown" | "present"),
        "live_window_state returns a valid state"
    );

    let _ = desktop::close_live(&session_id).await;
    // i3 processes the kill asynchronously: poll briefly for the window to go.
    let mut state2 = "present";
    for _ in 0..40 {
        state2 = desktop::live_window_state(&session_id).await.unwrap_or("unknown");
        if state2 != "present" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        matches!(state2, "absent" | "unknown"),
        "the window is absent or unknown after close (got {state2})"
    );

    let _ = std::fs::remove_dir_all(&state_dir);
}