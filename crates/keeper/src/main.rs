//! The `ldmux-keeper` binary: bind the keeper socket and serve.
//!
//! The daemon auto-spawns this detached (phase 5); the socket path is given on
//! the command line (the daemon's state dir). Under systemd it is
//! `ldmux-keeper.service` (ADR-0021) and shells are put in their own scopes;
//! otherwise it is an ordinary detached process. `LDMUX_NO_SCOPE=1` disables
//! the scopes.
//!
//! The socket path: `$LDMUX_KEEPER_SOCKET` or the first CLI arg.

use std::path::PathBuf;
use std::sync::Arc;

use keeper as k; // the library

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut socket = std::env::var("LDMUX_KEEPER_SOCKET").unwrap_or_default();
    if socket.is_empty() {
        let mut args = std::env::args().skip(1);
        match args.next() {
            Some(p) => socket = p,
            None => {
                eprintln!("usage: ldmux-keeper <socket-path>");
                std::process::exit(2);
            }
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
