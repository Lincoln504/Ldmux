//! The `ldmux-keeper` binary: bind the keeper socket and serve.
//!
//! The daemon auto-spawns this detached (phase 5); the socket path is given on
//! the command line (the daemon's state dir). Under systemd it is
//! `ldmux-keeper.service` (ADR-0021) and shells are put in their own scopes;
//! otherwise it is an ordinary detached process. `LDMUX_NO_SCOPE=1` disables
//! the scopes.
//!
//! The socket path: `$LDMUX_KEEPER_SOCKET`, else the first CLI arg, else the same resolver the
//! daemon uses (`<LDMUX_STATE_DIR>/keeper.sock`). The fallback is what keeps the two ends
//! together when `LDMUX_STATE_DIR` is overridden: the unit used to pass a hard-coded
//! `%t/ldmux/keeper.sock`, so a daemon on another state directory waited for a socket the
//! keeper never bound and spawned a second keeper.

use std::path::PathBuf;
use std::sync::Arc;

use keeper as k; // the library

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut socket = std::env::var("LDMUX_KEEPER_SOCKET").unwrap_or_default();
    if socket.is_empty() {
        let mut args = std::env::args().skip(1);
        if let Some(p) = args.next() {
            socket = p;
        } else {
            socket = ldmux_protocol::paths::resolve_state_dir() + "/keeper.sock";
        }
    }
    let socket = PathBuf::from(socket);
    // The shell the keeper spawns per session (policy: `k::default_shell`).
    let shell = k::default_shell();
    let keeper = Arc::new(k::Keeper::new(shell).with_scope(k::scope::ScopeEnv::detect()));
    if let Err(e) = k::serve(&keeper, &socket).await {
        eprintln!("ldmux-keeper: serve failed: {e}");
        std::process::exit(1);
    }
}
