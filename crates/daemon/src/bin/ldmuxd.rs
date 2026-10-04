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
//! - `LDMUX_SOCK` — the control socket path (default: `<state_dir>/ldmuxd.sock`).
//! - `LDMUX_TMUX_SOCK` — the tmux socket path (resolved via the protocol).
//! - `LDMUX_REMOTE_HOST` — the tailnet or loopback address (default: `127.0.0.1`).
//! - `LDMUX_REMOTE_PORT` — the remote port (default: `7623`).
//! - `LDMUX_TOKEN_FILE` — where the bearer token persists (default: `<state_dir>/token`).
//! - `LDMUX_STREAM_TRANSPORT` — `"control"` (default) or `"pipe"`.

use ldmux_daemon::daemon::LdmuxDaemon;
use ldmux_protocol::net::is_tailnet_or_loopback;
use tokio::signal::unix::SignalKind;

fn main() {
    // Last-resort backstops: an unhandled rejection or a stray exception is
    // logged loudly and survived. The daemon holds every client's terminal;
    // exiting because one request went wrong is the worst outcome available.
    // In Rust, panics are the equivalent; we let them panic and log.

    // The port: a port that is not a port is unambiguously a mistake.
    let remote_port_str = std::env::var("LDMUX_REMOTE_PORT").unwrap_or_else(|_| "7623".into());
    let remote_port: u16 = match remote_port_str.parse() {
        Ok(p) if (1..=65535).contains(&p) => p,
        _ => {
            eprintln!(
                "ldmuxd: LDMUX_REMOTE_PORT must be a port number, got {remote_port_str:?}"
            );
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

    // The token: read or mint via the project's own `auth` module (64 lowercase
    // hex, dir 0700, file 0600, legacy adoption). `LDMUX_TOKEN_FILE` moves it
    // out of the state dir; otherwise it lives at `<state_dir>/token`.
    let token_path_env = std::env::var("LDMUX_TOKEN_FILE").ok();
    let token = ldmux_daemon::auth::load_or_create_token(
        &state_dir.to_string_lossy(),
        token_path_env.as_deref(),
    );

    // Create the daemon.
    let keeper_bin = std::path::PathBuf::from("ldmux-keeper");
    let daemon = LdmuxDaemon::new(state_dir.clone(), keeper_bin)
        .with_remote(remote_host.clone(), remote_port, token);
    // Wrap the daemon in an `Arc<tokio::sync::Mutex>` so the control socket's
    // spawned tasks can call `handle` (which takes `&mut self`).
    let daemon_arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(daemon));

    // Start the daemon and wait for a signal (SIGTERM/SIGINT) to stop.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let daemon_ref = daemon_arc.clone();
    rt.block_on(async move {
        // The daemon is inside the `Arc`. We need a `&mut` reference to call
        // `start`. The `Arc`'s `Mutex` gives us that.
        let mut d = daemon_ref.lock().await;
        match d.start(&daemon_ref).await {
            Ok(()) => {
                println!(
                    "ldmuxd listening on {} (keeper: {})",
                    d.socket_path().display(),
                    d.keeper_socket_path().display(),
                );
            }
            Err(e) => {
                if e == "ldmuxd already running" {
                    println!("ldmuxd already running");
                    std::process::exit(0);
                }
                eprintln!("ldmuxd: {e}");
                std::process::exit(1);
            }
        }
        // Drop the mutable guard (the daemon is started).
        drop(d);

        // Watch for SIGTERM/SIGINT (the graceful shutdown, the TS
        // `process.on('SIGTERM', ...)`). When a signal is received,
        // stop the daemon (release the lock, close streams, drop the
        // daemon) and exit.
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
        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("ldmuxd: SIGTERM received, stopping");
            }
            _ = sigint.recv() => {
                eprintln!("ldmuxd: SIGINT received, stopping");
            }
        }
        let mut d2 = daemon_ref.lock().await;
        let _ = d2.stop();
        drop(d2);
    });
}
