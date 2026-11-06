//! Desktop live-window e2e (plan §6): Xvfb + i3 + xterm, all scratch. The window
//! is opened by direct spawn, found and placed by the i3 adapter, observed, and
//! closed, against a real X server and window manager.
//!
//! Skips, with the reason printed, when Xvfb, i3 or xterm is not installed. The
//! test is hermetic: it sets its own DISPLAY, HOME and XDG_RUNTIME_DIR and removes
//! every other session variable, so it never touches the developer's desktop.
//!
//! **What this does not cover:** the ownership/geometry semantics of a window
//! (size-on-attach, handover, close≠detach) live in `handover.rs` and
//! `attach_client.rs`, which drive real clients against a daemon with no X server.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ldmux_daemon::desktop;

fn on_path(tool: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| !d.is_empty() && std::path::Path::new(d).join(tool).is_file())
}

/// Kill the children when the test ends, however it ends.
struct Guard(Vec<std::process::Child>);
impl Drop for Guard {
    fn drop(&mut self) {
        for c in self.0.iter_mut().rev() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Poll `cond` every 50 ms until it holds or `secs` pass.
async fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

#[tokio::test]
async fn a_window_is_opened_found_placed_and_closed_under_i3() {
    for tool in ["Xvfb", "i3", "i3-msg", "xterm"] {
        if !on_path(tool) {
            eprintln!("SKIP desktop_e2e: {tool} is not installed");
            return;
        }
    }
    let scratch = std::env::temp_dir().join(format!(
        "ldmux-deske2e-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(scratch.join("run")).unwrap();
    std::fs::set_permissions(
        scratch.join("run"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();

    // A random display so two runs never collide on the WM-selection atom.
    let n = 90 + (rand::random::<u8>() % 8) as u32;
    let display = format!(":{n}");
    let mut guard = Guard(Vec::new());
    guard.0.push(
        Command::new("Xvfb")
            .args([&display, "-screen", "0", "1024x768x24"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start Xvfb"),
    );
    let socket = format!("/tmp/.X11-unix/X{n}");
    if !wait_until(10, || std::path::Path::new(&socket).exists()).await {
        eprintln!("SKIP desktop_e2e: Xvfb did not come up on {display}");
        return;
    }

    // SAFETY: this file has one test, so nothing else reads the environment.
    unsafe {
        std::env::set_var("DISPLAY", &display);
        std::env::set_var("HOME", &scratch);
        std::env::set_var("XDG_RUNTIME_DIR", scratch.join("run"));
        for v in [
            "WAYLAND_DISPLAY",
            "SWAYSOCK",
            "I3SOCK",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "NIRI_SOCKET",
            "TERMINAL",
        ] {
            std::env::remove_var(v);
        }
        // The terminal is the standard variable, and it is named `xterm` so the
        // daemon's template for xterm applies: `-class {class}` is what the i3
        // adapter finds the window by. The script ignores the arguments the daemon
        // appends (including the attach command, which has no daemon here) and runs
        // the real xterm with a class of its own and a window that stays up.
        let bin = scratch.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let term = bin.join("xterm");
        // The real xterm by absolute path: the stub is named `xterm` and sits first
        // on PATH, so a bare name inside it would exec itself.
        let real = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| std::path::Path::new(d).join("xterm"))
            .find(|p| p.is_file())
            .expect("xterm was found on PATH above");
        std::fs::write(
            &term,
            format!(
                "#!/bin/sh\nexec {} -class ldmux-live-e2e1 -e /bin/sh -c 'sleep 30'\n",
                real.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &term,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::env::set_var("PATH", path);
        std::env::set_var("TERMINAL", "xterm");
    }

    guard.0.push(
        Command::new("i3")
            .env("DISPLAY", &display)
            .env("HOME", &scratch)
            .env("XDG_RUNTIME_DIR", scratch.join("run"))
            .env_remove("I3SOCK")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start i3"),
    );
    // i3 is ready when it answers on IPC.
    // 45 s, not 15: `i3-msg` answering is bounded by how fast i3 starts, and in the
    // parallel gate every binary runs at once. Observed failing at 14-way parallelism
    // with "i3 never answered", then passing alone; the wait costs nothing when i3 is
    // quick, which is the normal case.
    let ready = wait_until(45, || {
        Command::new("i3-msg")
            .args(["-t", "get_version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
    .await;
    assert!(ready, "i3 never answered on {display}");

    let id = "e2e1";
    let opened = desktop::open_live(id, "2")
        .await
        .expect("the window must open");
    assert!(opened.ok);
    assert_eq!(
        opened.placed,
        Some(true),
        "i3 placed it: {:?}",
        opened.detail
    );
    assert!(opened.window_id.is_some());

    // On screen, by class, as the daemon asks i3.
    let mut state = "";
    for _ in 0..100 {
        state = desktop::live_window_state(id).await.unwrap();
        if state == "present" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(state, "present", "the window must be in i3's tree");
    assert_eq!(desktop::live_window_ids().await, vec![id.to_string()]);

    // ... and on the workspace asked for.
    let tree = Command::new("i3-msg")
        .args(["-t", "get_tree"])
        .output()
        .unwrap();
    let tree: serde_json::Value = serde_json::from_slice(&tree.stdout).unwrap();
    fn workspace_of(node: &serde_json::Value, ws: Option<&str>, class: &str) -> Option<String> {
        let ws = if node["type"] == "workspace" {
            node["name"].as_str()
        } else {
            ws
        };
        if node["window_properties"]["class"] == class {
            return ws.map(|s| s.to_string());
        }
        for k in ["nodes", "floating_nodes"] {
            for c in node[k].as_array().into_iter().flatten() {
                if let Some(w) = workspace_of(c, ws, class) {
                    return Some(w);
                }
            }
        }
        None
    }
    assert_eq!(
        workspace_of(&tree, None, "ldmux-live-e2e1").as_deref(),
        Some("2")
    );

    assert_eq!(desktop::close_live(id).await, Ok(true));
    let mut gone = false;
    for _ in 0..100 {
        if desktop::live_window_state(id).await.unwrap() == "absent" {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "the window must be gone after close_live");
    let _ = std::fs::remove_dir_all(&scratch);
}
