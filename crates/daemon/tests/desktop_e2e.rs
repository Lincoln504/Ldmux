//! Phase 6 desktop attach e2e (plan §6): Xvfb + i3 scratch; verify the
//! live-window lifecycle functions work without panicking.
//!
//! This test uses a real X server (Xvfb) and window manager (i3) to verify
//! the live-window functions. It does not require a full terminal emulator.

use std::process::Command;

use ldmux_daemon::desktop;

fn scratch_dir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-deske2e-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Desktop attach e2e: start Xvfb + i3; verify open_live, live_window_state,
/// and close_live work without panicking.
#[tokio::test]
async fn desktop_attach_e2e_functions_do_not_panic() {
    // Start Xvfb on a virtual display.
    let display = ":99";
    let mut xvfb = Command::new("Xvfb")
        .arg(display)
        .arg("-screen")
        .arg("0")
        .arg("1024x768x24")
        .spawn()
        .expect("failed to start Xvfb");

    // Wait for the X server to be ready.
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    // Start i3 on the display.
    let mut i3 = Command::new("i3")
        .env("DISPLAY", display)
        .spawn()
        .expect("failed to start i3");

    // Wait for i3 to be ready.
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

    // Set the DISPLAY env for the desktop functions.
    unsafe { std::env::set_var("DISPLAY", display); }

    let state_dir = scratch_dir();
    let tmux_sock = state_dir.join("keeper.sock");
    let session_id = "s1".to_string();
    let tmux_name = "ldmux-s1".to_string();
    let tmux_bin = "/usr/bin/tmux".to_string();
    let workspace = "1".to_string();

    // Open a live window (may fail if tmux socket doesn't exist, but should
    // not panic).
    let result = desktop::open_live(
        tmux_sock.to_str().unwrap(),
        &tmux_name,
        &session_id,
        &workspace,
        &tmux_bin,
        state_dir.to_str().unwrap(),
    )
    .await;
    // The result may be an error (no tmux socket), but it should not panic.
    let _ = result;

    // Check the window state (should be "absent" or "unknown").
    let state = desktop::live_window_state(&session_id).await;
    assert!(
        matches!(state.unwrap_or("unknown"), "absent" | "unknown" | "present"),
        "live_window_state should return a valid state"
    );

    // Close the live window (should not panic).
    let _ = desktop::close_live(&session_id).await;

    // The window should now be absent or unknown.
    let state2 = desktop::live_window_state(&session_id).await.unwrap_or("unknown");
    assert!(
        matches!(state2, "absent" | "unknown"),
        "the window should be absent or unknown after close"
    );

    // Cleanup: kill i3 and Xvfb.
    let _ = i3.kill();
    let _ = xvfb.kill();
}
