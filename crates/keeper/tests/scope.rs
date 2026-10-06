//! Each shell lives in its own systemd scope when a user manager is there, and
//! starts directly when it is not (ADR-0021).
//!
//! The scope tests need a systemd user manager with a reachable bus. They probe
//! for one and, when it is absent (a CI sandbox), print why and pass. The scopes
//! they create live under a throwaway slice and are stopped before the test ends.

use keeper::scope::{ScopeEnv, scope_name};
use ldmux_protocol::keeper::{KeeperMessage, Kill, Spawn};
use ldmux_protocol::keeper_call::{DEFAULT_TIMEOUT_MS, keeper_call};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ldmux-scope-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // The keeper swaps the process umask around `bind`, so a concurrent test can
    // leave it at 0o177 for a moment: set the mode explicitly.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    d
}

async fn boot(k: keeper::Keeper, socket: &Path) -> Arc<keeper::Keeper> {
    let k = Arc::new(k);
    let (k2, sock) = (k.clone(), socket.to_path_buf());
    tokio::spawn(async move {
        let _ = keeper::serve(&k2, &sock).await;
    });
    for _ in 0..1200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    k
}

/// Spawn `sh -c <script>` as session `id`; `script` records its pid in `pidfile`.
async fn spawn(socket: &Path, id: &str, script: String) {
    let res = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Spawn(Spawn {
            session_id: id.into(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: Some(vec!["/bin/sh".into(), "-c".into(), script]),
            env: None,
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await
    .unwrap();
    assert!(matches!(res, KeeperMessage::State(_)), "spawn: {res:?}");
}

async fn kill(socket: &Path, id: &str) {
    let _ = keeper_call(
        &socket.to_string_lossy(),
        &KeeperMessage::Kill(Kill {
            session_id: id.into(),
        }),
        DEFAULT_TIMEOUT_MS,
    )
    .await;
}

async fn read_pid(pidfile: &Path) -> i32 {
    for _ in 0..400 {
        if let Some(p) = std::fs::read_to_string(pidfile)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return p;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the shell never wrote {}", pidfile.display());
}

/// The session id of `pid` (field 6 of /proc/<pid>/stat), if it is alive.
fn session_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesised and may contain spaces: count from the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(3)?.parse().ok()
}

fn cgroup_of(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default()
}

fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A scope-capable environment, or `None` (with the reason printed).
fn real_scope_env(slice: &str) -> Option<ScopeEnv> {
    let Some(mut env) = ScopeEnv::detect() else {
        eprintln!("SKIP: no systemd user manager / bus / busctl in this environment");
        return None;
    };
    let probe = std::process::Command::new(&env.busctl)
        .args([
            "--user",
            "--quiet",
            "--timeout=3",
            "get-property",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "Version",
        ])
        .output();
    if !probe.is_ok_and(|o| o.status.success()) {
        eprintln!("SKIP: the user bus does not answer");
        return None;
    }
    env.slice = slice.to_string();
    Some(env)
}

#[tokio::test]
async fn a_shell_enters_its_own_scope_and_a_daemonized_job_outlives_the_kill() {
    let slice = format!("ldmux-test-{}.slice", std::process::id());
    let Some(env) = real_scope_env(&slice) else {
        return;
    };
    let dir = scratch("on");
    let socket = dir.join("k.sock");
    let _k = boot(
        keeper::Keeper::new(vec!["/bin/sh".into()]).with_scope(Some(env)),
        &socket,
    )
    .await;
    let (pidfile, jobfile) = (dir.join("pid"), dir.join("job"));
    // The shell records its pid, daemonizes a job in its own session, and waits.
    spawn(
        &socket,
        "scoped",
        format!(
            "echo $$ > {p}; setsid sleep 1000 & echo $! > {j}; wait",
            p = pidfile.display(),
            j = jobfile.display()
        ),
    )
    .await;
    let pid = read_pid(&pidfile).await;
    let job = read_pid(&jobfile).await;
    let name = scope_name("scoped", pid);

    // `spawn` returns after the scope request, but the manager moves the process
    // asynchronously: poll.
    let mut moved = false;
    for _ in 0..200 {
        if cgroup_of(pid).contains(&name) {
            moved = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let cg = cgroup_of(pid);
    if !moved {
        let _ = std::fs::remove_dir_all(&dir);
        kill(&socket, "scoped").await;
        panic!("the shell did not enter {name}: {cg}");
    }
    assert!(cg.contains(&slice), "the scope is under {slice}: {cg}");
    assert_ne!(
        cg,
        cgroup_of(std::process::id() as i32),
        "not the keeper's cgroup"
    );

    // The job is only "daemonized" once its `setsid` has run: until then it still
    // sits in the shell's session and a kill rightly takes it with the shell, so the
    // test waits for it to lead its own session (field 6 of /proc/<pid>/stat) before
    // it kills anything.
    let mut led = false;
    for _ in 0..200 {
        if session_of(job) == Some(job) {
            led = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(led, "the job never became a session leader");

    // Killing the session ends the shell's process group, not the scope's job.
    kill(&socket, "scoped").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let job_cg = cgroup_of(job);
    let survived = job_cg.contains(&name);
    let job_state = std::fs::read_to_string(format!("/proc/{job}/stat")).unwrap_or_default();

    // Clean up before asserting: stopping the scope kills the job.
    let _ = systemctl(&["stop", &name]);
    let _ = systemctl(&["stop", &slice]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        survived,
        "a job the shell daemonized survives the kill, in its scope: scope {name}, \
         job {job} cgroup {job_cg:?}, stat {job_state:?}"
    );
}

/// A scope that cannot be made (here: a `busctl` that always fails) must not stop
/// the shell from starting.
#[tokio::test]
async fn a_failed_scope_request_still_starts_the_shell() {
    let dir = scratch("fail");
    let socket = dir.join("k.sock");
    let env = ScopeEnv {
        busctl: PathBuf::from("/bin/false"),
        slice: "ldmux-test-never.slice".into(),
    };
    let _k = boot(
        keeper::Keeper::new(vec!["/bin/sh".into()]).with_scope(Some(env)),
        &socket,
    )
    .await;
    let pidfile = dir.join("pid");
    spawn(
        &socket,
        "nofail",
        format!("echo $$ > {}; sleep 1000", pidfile.display()),
    )
    .await;
    let pid = read_pid(&pidfile).await;
    assert!(Path::new(&format!("/proc/{pid}")).exists());
    kill(&socket, "nofail").await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// The direct path (no scope configured: what `LDMUX_NO_SCOPE=1` selects and what a
/// machine without systemd gets): the shell stays in the keeper's own cgroup.
#[tokio::test]
async fn without_a_scope_the_shell_stays_in_the_keepers_cgroup() {
    let dir = scratch("off");
    let socket = dir.join("k.sock");
    let _k = boot(
        keeper::Keeper::new(vec!["/bin/sh".into()]).with_scope(None),
        &socket,
    )
    .await;
    let pidfile = dir.join("pid");
    spawn(
        &socket,
        "direct",
        format!("echo $$ > {}; sleep 1000", pidfile.display()),
    )
    .await;
    let pid = read_pid(&pidfile).await;
    assert_eq!(cgroup_of(pid), cgroup_of(std::process::id() as i32));
    kill(&socket, "direct").await;
    let _ = std::fs::remove_dir_all(&dir);
}
