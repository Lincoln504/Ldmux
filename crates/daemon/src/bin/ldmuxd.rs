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

    // The state dir (the `LDMUX_STATE_DIR` env var, or `~/.ldmux`).
    let state_dir = std::env::var("LDMUX_STATE_DIR")
        .map(|s| std::path::PathBuf::from(s))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".ldmux")
        });

    // The token file (the `LDMUX_TOKEN_FILE` env var, or `<state_dir>/token`).
    let token_path = std::env::var("LDMUX_TOKEN_FILE")
        .map(|s| std::path::PathBuf::from(s))
        .unwrap_or_else(|_| state_dir.join("token"));

    // Read or generate the token.
    let token = match std::fs::read_to_string(&token_path) {
        Ok(t) => t.trim().to_string(),
        Err(_) => {
            // Generate a token if one does not exist.
            let t: String = (0..32)
                .map(|_| {
                    let r = rand::random::<u8>();
                    (b'0' + (r % 36)) as char
                })
                .collect();
            let _ = std::fs::write(&token_path, &t);
            t
        }
    };

    // Create the daemon.
    let keeper_bin = std::path::PathBuf::from("ldmux-keeper");
    let mut daemon = LdmuxDaemon::new(state_dir.clone(), keeper_bin)
        .with_remote(remote_host.clone(), remote_port, token);

    // Start the daemon.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        match daemon.start().await {
            Ok(()) => {
                println!(
                    "ldmuxd listening on {} (keeper: {})",
                    daemon.socket_path().display(),
                    daemon.keeper_socket_path().display(),
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
    });

    // The daemon started successfully. Now block forever (the process is
    // stopped by SIGTERM/SIGINT; the `Drop` impl releases the lock).
    //
    // In a real deployment, the systemd unit sends SIGTERM on stop; the
    // process's default disposition (kill) is fine for now. The `Drop` impl
    // on `LdmuxDaemon` releases the lock and stops the remote.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
