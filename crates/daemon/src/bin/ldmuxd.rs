//! Daemon entrypoint:
//!   ldmuxd
//!
//! Port of `packages/daemon/src/main.ts` (115 lines). The daemon's `main`
//! function: parses env vars, validates the port and host, sets up signal
//! handlers (registered before the first await), creates the daemon, and
//! starts it.
//!
//! Environment:
//! - `LDMUX_STATE_DIR` — the state directory (default: `~/.ldmux`).
//! - `LDMUX_SOCK` — the control socket path (default: `<state_dir>/ldmuxd.sock`);
//!   resolved by `ldmux_protocol::paths`, the same resolver the CLI and the attach
//!   client use.
//! - `LDMUX_REMOTE_HOST` — the tailnet or loopback address (default: `127.0.0.1`).
//! - `LDMUX_REMOTE_PORT` — the remote port (default: `7623`).
//! - `LDMUX_PERSIST_DIR` — where credentials persist across a reboot (default:
//!   `$XDG_STATE_HOME/ldmux`, else `$HOME/.local/state/ldmux`). The state dir is a tmpfs.
//! - `LDMUX_TOKEN_FILE` — the bearer token's path (default: `<persist_dir>/token`).

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_protocol::net::is_tailnet_or_loopback;
use std::path::{Path, PathBuf};
use tokio::signal::unix::SignalKind;

/// The keeper binary to spawn: the one **beside this daemon**, else the bare
/// name for `PATH` to resolve.
///
/// A bare name is what this used to be, and it means the daemon and its keeper
/// can be different builds with nothing saying so. That is not hypothetical
/// here: the keeper outlives the daemon by design, so a keeper started before an
/// upgrade keeps serving the new daemon (measured 2026-10-04: a keeper running
/// since 13:39:30 served a keeper built at 23:02:49). Installing both binaries
/// into one directory is the deployment's own rule (`scripts/install.sh`), so a
/// sibling is the build that belongs with this daemon; the fallback keeps a
/// hand-run daemon from a target dir working the way it always did.
fn keeper_beside(daemon: &Path) -> PathBuf {
    let sibling = daemon
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("ldmux-keeper");
    if sibling.exists() {
        sibling
    } else {
        PathBuf::from("ldmux-keeper")
    }
}

fn main() {
    // **`--version` before any startup work.** It used to fall through into the daemon's own
    // startup: asking a running daemon what it was printed "already running" and exited on the
    // lock, and `ldmuxd --version` on a machine with no daemon started one. A question about the
    // binary must not start the service.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--version" | "-V" | "version") => {
            println!(
                "ldmuxd {} (protocol {})",
                ldmux_protocol::BUILD_ID,
                ldmux_protocol::rpc::DAEMON_PROTOCOL_VERSION
            );
            return;
        }
        Some("--help" | "-h") => {
            println!("usage: ldmuxd [--version]");
            println!("       ldmuxd attach <session-id> [--readonly]");
            return;
        }
        Some("attach") => {}
        Some(other) => {
            // An argument this binary does not know is a usage error, not a daemon start: the
            // old behaviour turned a typo into a second daemon that exited on the lock, with a
            // message about "already running" that named neither the typo nor the daemon.
            eprintln!("ldmuxd: unknown argument {other:?}");
            eprintln!("usage: ldmuxd [--version]");
            eprintln!("       ldmuxd attach <session-id> [--readonly]");
            std::process::exit(2);
        }
        None => {}
    }
    // `ldmuxd attach <session> [--readonly]`: the desk-window entry point
    // (plan §3.9). alacritty runs this, so its stdio is the window's pty.
    if args.get(1).map(|s| s.as_str()) == Some("attach") {
        let readonly = args.iter().any(|a| a == "--readonly" || a == "-r");
        let session = args.iter().skip(2).find(|a| !a.starts_with('-')).cloned();
        let Some(session) = session else {
            eprintln!("usage: ldmuxd attach <session-id> [--readonly]");
            std::process::exit(2);
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(attach::run(&session, readonly)) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("ldmuxd attach: {e}");
                std::process::exit(1);
            }
        }
    }

    // Last-resort backstops: an unhandled rejection or a stray exception is
    // logged loudly and survived. The daemon holds every client's terminal;
    // exiting because one request went wrong is the worst outcome available.
    // In Rust, panics are the equivalent; we let them panic and log.

    // The port: a port that is not a port is unambiguously a mistake.
    let remote_port_str = std::env::var("LDMUX_REMOTE_PORT").unwrap_or_else(|_| "7623".into());
    let remote_port: u16 = match remote_port_str.parse() {
        Ok(p) if (1..=65535).contains(&p) => p,
        _ => {
            eprintln!("ldmuxd: LDMUX_REMOTE_PORT must be a port number, got {remote_port_str:?}");
            std::process::exit(1);
        }
    };

    // The host: the same rule the server enforces when it listens, checked
    // here so a wrong address is an exit and a message rather than a running
    // daemon with no WebSocket.
    let remote_host = std::env::var("LDMUX_REMOTE_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    if !is_tailnet_or_loopback(&remote_host) {
        eprintln!(
            "ldmuxd: LDMUX_REMOTE_HOST must be this machine's tailnet address or loopback, got {remote_host:?}"
        );
        std::process::exit(1);
    }

    // The state dir (the `LDMUX_STATE_DIR` env var, or
    // `${XDG_RUNTIME_DIR:-/tmp}/ldmux` — the TS `resolveStateDir`).
    let state_dir = std::path::PathBuf::from(ldmux_protocol::paths::resolve_state_dir());

    // Where credentials live. **Not the state dir**: that is `$XDG_RUNTIME_DIR`, a tmpfs, so
    // a token or a session key kept there is re-minted on every boot and every paired phone is
    // locked out. `LDMUX_PERSIST_DIR`, else `$XDG_STATE_HOME/ldmux`, else
    // `$HOME/.local/state/ldmux`; created 0700 when absent.
    let persist_dir = match ldmux_protocol::paths::try_resolve_persist_dir() {
        Ok(d) => std::path::PathBuf::from(d),
        Err(e) => {
            eprintln!("ldmuxd: {e}");
            std::process::exit(1);
        }
    };

    // The token: read or mint via the project's own `auth` module (64 lowercase
    // hex, dir 0700, file 0600, legacy adoption). It lives in the persist dir, so a
    // reboot does not re-mint it and lock every paired phone out; `LDMUX_TOKEN_FILE`
    // still overrides the location, and a token left at `<state_dir>/token` is adopted
    // and then removed.
    let token_path_env = std::env::var("LDMUX_TOKEN_FILE")
        .ok()
        .or_else(|| Some(persist_dir.join("token").to_string_lossy().into_owned()));
    let token = match ldmux_daemon::auth::load_or_create_token(
        &state_dir.to_string_lossy(),
        token_path_env.as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ldmuxd: {e}");
            std::process::exit(1);
        }
    };

    // The control socket: `LDMUX_SOCK` or `<state_dir>/ldmuxd.sock`, through the
    // one resolver every client uses, so they all look where the daemon binds.
    let socket_path = PathBuf::from(ldmux_protocol::paths::resolve_socket_path());

    // Create the daemon. The keeper is the binary beside this one.
    let keeper_bin = std::env::current_exe()
        .map(|exe| keeper_beside(&exe))
        .unwrap_or_else(|_| PathBuf::from("ldmux-keeper"));
    let daemon = LdmuxDaemon::new(state_dir.clone(), keeper_bin)
        .with_socket_path(socket_path.clone())
        .with_remote(remote_host.clone(), remote_port, token);
    // The radio route, if this machine has a radio. It is installed inside `block_on` below,
    // because starting it needs a reactor.
    // A misconfigured mesh link must not take the daemon with it. `exit(1)` here meant one
    // stale variable in an environment file — `LDMUX_MESH_PSK` left behind when the rest of
    // the link was removed — stopped the daemon, every shell, the tailnet listener and the
    // CLI, and the only symptom was "the app does not work". The mesh route is one feature;
    // say why it is off and start the rest.
    let mesh_cfg = match ldmux_daemon::mesh::MeshConfig::from_env() {
        Ok(cfg) => cfg.map(|mut c| {
            // Beside the other state: the record of whether this radio's path width has already
            // been decided once (see `mesh_serial::prepare`).
            c.width_marker = Some(state_dir.join("mesh-width"));
            c.airtime_record = Some(state_dir.join("mesh-airtime"));
            c
        }),
        Err(e) => {
            eprintln!("ldmuxd: mesh link disabled: {e}");
            None
        }
    };

    // **A bad mesh setting disables the mesh, it does not stop the daemon.** This was the last
    // `exit(1)` on the startup path, and it is the same class the link half was fixed for: one
    // stale or malformed `LDMUX_MESH_PSK` (or a session file the process cannot read) took down
    // every shell and the CLI along with the radio. A person can fix the key; they cannot un-kill
    // the shells it killed on the way out.
    let mesh_session = match ldmux_daemon::mesh::SessionSetup::from_env(&persist_dir, &state_dir)
    {
        Ok(setup) => setup,
        Err(e) => {
            eprintln!("ldmuxd: mesh sessions disabled: {e}");
            None
        }
    };
    // The same rule once more for the session half: a mesh session with no link to carry it is
    // useless, not fatal. Both halves used to `exit(1)` here, which is how one stale
    // `LDMUX_MESH_PSK` stopped the daemon, every shell and the CLI.
    let mut mesh_session = mesh_session;
    if mesh_session.is_some() && mesh_cfg.is_none() {
        eprintln!(
            "ldmuxd: mesh link disabled: LDMUX_MESH_PSK is set but LDMUX_MESH_SERIAL is not. \
             Remove the key or name the device; the daemon runs either way."
        );
        mesh_session = None;
    }

    // Wrap the daemon in an `Arc<tokio::sync::Mutex>` so the control socket's
    // spawned tasks can call `handle` (which takes `&mut self`).
    let daemon_arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(daemon));

    // Start the daemon and wait for a signal (SIGTERM/SIGINT) to stop.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let daemon_ref = daemon_arc.clone();
    rt.block_on(async move {
        if let Some(cfg) = mesh_cfg {
            // The one fact worth logging: which radio the daemon has taken over. There is no
            // address and no peer to name — the pairing's other end is whichever phone holds the
            // keys — and printing empty fields for them read as a fault.
            println!(
                "ldmuxd: mesh link to {} (serial, owned by this daemon)",
                cfg.serial
            );
            if let Some(session) = &mesh_session {
                println!(
                    "ldmuxd: mesh session on; radio key {}",
                    session.public_key_hex()
                );
            }
            let session_on = mesh_session.is_some();
            let runtime = ldmux_daemon::mesh::MeshRuntime::start_with_session(cfg, mesh_session);
            runtime.clone().install();
            // The shell bridge (ADR-0025): the phone attaches to a shell through the session
            // and gets plain-text rows. It only exists when there is a session to carry it.
            if session_on {
                let host = ldmux_daemon::mesh_bridge::DaemonHost(daemon_ref.clone());
                let bridge = ldmux_daemon::mesh_bridge::Bridge::new(host, runtime.clone());
                tokio::spawn(ldmux_daemon::mesh_bridge::run_publishing(
                    bridge,
                    move |stats| runtime.publish_bridge(stats),
                ));
            }
        }
        // The handlers go in **before** `start`: a SIGTERM that arrives while
        // the daemon is starting is held by the stream and acted on as soon as
        // start returns (so it is stopped, not killed half-started).
        let mut sigterm = match tokio::signal::unix::signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ldmuxd: failed to install SIGTERM handler: {e}");
                std::process::exit(1);
            }
        };
        let mut sigint = match tokio::signal::unix::signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ldmuxd: failed to install SIGINT handler: {e}");
                std::process::exit(1);
            }
        };
        // The daemon is inside the `Arc`. We need a `&mut` reference to call
        // `start`. The `Arc`'s `Mutex` gives us that.
        let mut d = daemon_ref.lock().await;
        match d.start(&daemon_ref).await {
            Ok(()) => {
                println!(
                    "ldmuxd {} listening on {} (keeper: {})",
                    ldmux_protocol::BUILD_ID,
                    d.socket_path().display(),
                    d.keeper_socket_path().display(),
                );
            }
            Err(e) => {
                if e == "ldmuxd already running" {
                    println!("ldmuxd already running");
                    std::process::exit(0);
                }
                // Half-started: give back what start took before exiting.
                d.stop();
                eprintln!("ldmuxd: {e}");
                std::process::exit(1);
            }
        }
        // Drop the mutable guard (the daemon is started).
        drop(d);

        // Watch for SIGTERM/SIGINT (the graceful shutdown). When a signal is
        // received, stop the daemon (release the lock, close streams) and exit.
        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("ldmuxd: SIGTERM received, stopping");
            }
            _ = sigint.recv() => {
                eprintln!("ldmuxd: SIGINT received, stopping");
            }
        }
        // A handler stuck holding the daemon lock (a wedged keeper, a hung window
        // manager) must not make the daemon unstoppable: wait a few seconds for
        // an orderly stop, then do what matters without the lock. The flock dies
        // with the process, so the socket file is all that has to be removed.
        match tokio::time::timeout(std::time::Duration::from_secs(3), daemon_ref.lock()).await {
            Ok(mut d2) => d2.stop(),
            Err(_) => {
                eprintln!("ldmuxd: the daemon lock is busy; stopping without it");
                let _ = std::fs::remove_file(&socket_path);
            }
        }
        // Not a return: the runtime would wait for blocking tasks (a stream poll
        // stuck on the keeper) before dropping.
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ldmuxd-bin-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The keeper is the one beside the daemon when it is there.
    #[test]
    fn the_keeper_is_the_sibling_of_the_daemon() {
        let dir = scratch_dir();
        let daemon = dir.join("ldmuxd");
        std::fs::write(&daemon, b"").unwrap();
        std::fs::write(dir.join("ldmux-keeper"), b"").unwrap();
        assert_eq!(keeper_beside(&daemon), dir.join("ldmux-keeper"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without a sibling, the bare name is kept: that is the hand-run case, and
    /// `PATH` resolving it is the old, working behaviour.
    #[test]
    fn without_a_sibling_the_path_is_used() {
        let dir = scratch_dir();
        let daemon = dir.join("ldmuxd");
        std::fs::write(&daemon, b"").unwrap();
        assert_eq!(keeper_beside(&daemon), PathBuf::from("ldmux-keeper"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
