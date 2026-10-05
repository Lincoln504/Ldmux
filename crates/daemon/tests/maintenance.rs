//! The maintenance pass (the TS `liveTimer`, every 2 s).
//!
//! The port ran `reconcile` at start-up and nowhere else, so a shell that typed
//! `exit` read `running` until the daemon was restarted, its pane sat in the
//! keeper for the life of the process, and a desk window the user had closed
//! stayed recorded for good. These tests pin each of those.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use ldmux_daemon::daemon::LdmuxDaemon;

fn scratch() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-maint-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

/// The window-manager stub is reached through the **process** environment
/// (`live_window_state` reads `DISPLAY` and runs `i3-msg` from `PATH`), so the
/// two tests that install one must not run at the same time as each other. They
/// did, and the one whose window was "still there" read the other's tree.
fn window_manager_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

/// The keeper's session list, read straight off its socket: the daemon's registry
/// is the thing under test, so it is not asked.
fn keeper_sessions(sock: &std::path::Path) -> Vec<(String, bool)> {
    use std::io::{BufRead, BufReader, Write};
    let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
    s.write_all(b"{\"type\":\"list\"}\n").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    (
                        s["sessionId"].as_str().unwrap_or("").to_string(),
                        s["dead"].as_bool().unwrap_or(false),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

struct Fixture {
    dir: PathBuf,
    daemon_sock: String,
    keeper_sock: PathBuf,
    arc: Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    keeper: std::process::Child,
}

impl Fixture {
    async fn new() -> Self {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let keeper = std::process::Command::new(keeper_bin())
            .arg(keeper_sock.to_string_lossy().as_ref())
            .spawn()
            .unwrap();
        wait_socket(&keeper_sock).await;
        let d = LdmuxDaemon::new(dir.clone(), keeper_bin())
            // No remote: the maintenance pass is the daemon's, not the socket's.
            .with_live_window_grace_ms(1);
        let daemon_sock = d.socket_path().to_string_lossy().into_owned();
        let arc = Arc::new(tokio::sync::Mutex::new(d));
        {
            let mut d = arc.lock().await;
            d.start(&arc).await.unwrap();
        }
        Self { dir, daemon_sock, keeper_sock, arc, keeper }
    }

    async fn new_session(&self) -> String {
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        self.arc
            .lock()
            .await
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap()
            .id
    }

    /// Wait for the *keeper* to report the exit. That is the precondition; the
    /// daemon's reaction to it is what the tests then assert, in a single pass,
    /// so a pass that got it right for the wrong reason cannot pass.
    async fn wait_keeper_dead(&self, keeper_name: &str) -> bool {
        for _ in 0..100 {
            if keeper_sessions(&self.keeper_sock)
                .iter()
                .any(|(n, dead)| n == keeper_name && *dead)
            {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }

    async fn status_of(&self, id: &str) -> String {
        self.arc
            .lock()
            .await
            .registry()
            .get(id)
            .map(|r| r.status.clone())
            .unwrap_or_default()
    }

    async fn teardown(self) {
        self.arc.lock().await.stop();
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(self.keeper.id().to_string())
            .output();
        let mut k = self.keeper;
        let _ = k.wait();
        let sock = self.keeper_sock.to_string_lossy().into_owned();
        let _ = std::process::Command::new("pkill").args(["-f", &sock]).output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A shell that exits ends its session, and the daemon has to notice without
/// being asked. It read `running` forever: `reconcile` ran at start-up only, and
/// it counted a keeper state with `dead: true` as live.
#[tokio::test]
async fn a_shell_that_exits_is_marked_dead() {
    let f = Fixture::new().await;
    let id = f.new_session().await;

    // Type `exit` through the real control socket, as a client would.
    let r: Result<serde_json::Value, _> = ldmux_protocol::socket::call(
        &f.daemon_sock,
        "send",
        &serde_json::json!({"id": id, "text": "exit", "keys": ["Enter"]}),
        5000,
    )
    .await;
    assert!(r.is_ok(), "the send must reach the pane: {r:?}");

    assert!(
        f.wait_keeper_dead(&format!("ldmux-{id}")).await,
        "the keeper must report the exit before the daemon is asked about it"
    );
    // One pass, and only one: a keeper state with `dead: true` is not a live
    // session, and the record must say so on the tick that sees it.
    f.arc.lock().await.maintain().await.unwrap();
    assert_eq!(
        f.status_of(&id).await,
        "dead",
        "one maintenance pass must mark an exited shell dead"
    );
    f.teardown().await;
}

/// The keeper marks an exited pane `dead` and keeps it — the emulator, the
/// master pty and the child's slot all stay. Nothing released them, so every
/// shell that had ever exited stayed in the keeper for the life of the process
/// (measured live: 20 sessions and 51 MB where 2 and 26 MB were real).
#[tokio::test]
async fn a_dead_pane_is_released_from_the_keeper() {
    let f = Fixture::new().await;
    let id = f.new_session().await;
    let keeper_name = format!("ldmux-{id}");
    assert!(
        keeper_sessions(&f.keeper_sock).iter().any(|(n, _)| n == &keeper_name),
        "the session must exist before it is exited"
    );

    let _: Result<serde_json::Value, _> = ldmux_protocol::socket::call(
        &f.daemon_sock,
        "send",
        &serde_json::json!({"id": id, "text": "exit", "keys": ["Enter"]}),
        5000,
    )
    .await;

    assert!(
        f.wait_keeper_dead(&keeper_name).await,
        "the keeper must report the exit before the daemon is asked to release it"
    );
    // One pass must both notice and release it.
    f.arc.lock().await.maintain().await.unwrap();
    assert!(
        !keeper_sessions(&f.keeper_sock).iter().any(|(n, _)| n == &keeper_name),
        "the dead pane must be released from the keeper, not held for the life of the process"
    );
}

/// Closing the desk window with the window manager must free the record.
///
/// `live_windows` was written by `makeLive` and cleared only by `closeLive`, and
/// a user closes a window with the window manager, not the CLI. Live: the record
/// outlived the window for good and `makeLive` then refused with "that shell
/// already has a desk window" — the shell could never be made live again.
#[tokio::test]
async fn a_closed_desk_window_frees_the_record_and_ends_the_shell() {
    let _env = window_manager_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new().await;
    let id = f.new_session().await;

    // `live_window_state` asks i3 over PATH, so a stub stands in for the window
    // manager and answers the two questions the pass asks: "is a window with
    // this class in the tree?"
    let stub_dir = f.dir.join("bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("i3-msg");
    std::fs::write(
        &stub,
        "#!/bin/sh\ncase \"$*\" in\n  *get_tree*) printf '%s' \"$I3_TREE\"; exit \"${I3_TREE_CODE:-0}\";;\n  *) exit 0;;\nesac\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: single-threaded test process for this variable's lifetime here.
    unsafe { std::env::set_var("PATH", format!("{}:{}", stub_dir.display(), old_path)) };
    unsafe { std::env::set_var("DISPLAY", ":0") };

    // A tree that DOES contain the window, so `makeLive` records it. The exit
    // code is set explicitly: these tests share the process environment, and a
    // leaked non-zero from the other one read as "the window manager did not
    // answer", which is deliberately *not* a closed window.
    unsafe { std::env::set_var("I3_TREE_CODE", "0") };
    unsafe {
        std::env::set_var(
            "I3_TREE",
            format!(
                r#"{{"nodes":[{{"window":123,"window_properties":{{"class":"{}"}},"nodes":[],"floating_nodes":[]}}],"floating_nodes":[]}}"#,
                format!("ldmux-live-{id}")
            ),
        )
    };
    let live = f
        .arc
        .lock()
        .await
        .handle("makeLive", serde_json::json!({"id": id, "workspace": "10"}))
        .await
        .unwrap();
    assert_eq!(live["ok"], true, "makeLive must open and record the window: {live}");
    assert_eq!(f.arc.lock().await.live_window_ids(), vec![id.clone()]);

    // The window goes away (the user closed it). The grace period is 1 ms.
    unsafe { std::env::set_var("I3_TREE", r#"{"nodes":[],"floating_nodes":[]}"#) };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    f.arc.lock().await.maintain().await.unwrap();

    assert!(
        f.arc.lock().await.live_window_ids().is_empty(),
        "the record must not outlive the window"
    );
    // "Closing a desktop window closes the session totally" (the user, twice).
    let record = f.arc.lock().await.registry().get(&id).cloned();
    assert_eq!(
        record.map(|r| r.status).unwrap_or_default(),
        "dead",
        "closing the desk window must end the shell"
    );

    unsafe { std::env::set_var("PATH", old_path) };
    unsafe { std::env::remove_var("I3_TREE") };
    unsafe { std::env::remove_var("I3_TREE_CODE") };
    unsafe { std::env::remove_var("DISPLAY") };
    f.teardown().await;
}

/// A window manager that does not answer must never be read as a closed window:
/// `unknown` is not `absent`, and ending a session on it would be the worst
/// possible failure mode of this pass.
#[tokio::test]
async fn an_unanswered_window_manager_never_ends_a_session() {
    let _env = window_manager_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let f = Fixture::new().await;
    let id = f.new_session().await;

    let stub_dir = f.dir.join("bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("i3-msg");
    // `get_tree` exits non-zero: the window manager did not answer. `exec` still
    // succeeds, so the window can be opened for the test to then not find.
    std::fs::write(
        &stub,
        "#!/bin/sh\ncase \"$*\" in\n  *get_tree*) printf '%s' \"$I3_TREE\"; exit \"${I3_TREE_CODE:-0}\";;\n  *) exit 0;;\nesac\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    let old_path = std::env::var("PATH").unwrap_or_default();
    unsafe { std::env::set_var("PATH", format!("{}:{}", stub_dir.display(), old_path)) };
    unsafe { std::env::set_var("DISPLAY", ":0") };
    // No tree and a non-zero exit: the window manager is not answering.
    unsafe { std::env::set_var("I3_TREE", "{}") };
    unsafe { std::env::set_var("I3_TREE_CODE", "1") };

    let live = f
        .arc
        .lock()
        .await
        .handle("makeLive", serde_json::json!({"id": id, "workspace": "10"}))
        .await
        .unwrap();
    assert_eq!(live["ok"], true, "makeLive must open: {live}");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    f.arc.lock().await.maintain().await.unwrap();

    let record = f.arc.lock().await.registry().get(&id).cloned();
    assert_eq!(
        record.map(|r| r.status).unwrap_or_default(),
        "running",
        "an unanswered window manager is not a closed window"
    );

    unsafe { std::env::set_var("PATH", old_path) };
    unsafe { std::env::remove_var("I3_TREE") };
    unsafe { std::env::remove_var("I3_TREE_CODE") };
    unsafe { std::env::remove_var("DISPLAY") };
    f.teardown().await;
}