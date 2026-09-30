//! The `ldmux-keeper` binary: bind the keeper socket and serve.
//!
//! The daemon auto-spawns this detached (phase 5); the socket path is given on
//! the command line (the daemon's state dir). It runs to process exit — the
//! keeper is an ordinary process, not a unit.
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
    // The shell the keeper spawns per session: the user's shell, else /bin/sh.
    let shell = if let Ok(sh) = std::env::var("SHELL") {
        vec![sh]
    } else {
        vec!["/bin/sh".to_string()]
    };
    let keeper = Arc::new(k::Keeper::new(shell));
    if let Err(e) = k::serve(&keeper, &socket).await {
        eprintln!("ldmux-keeper: serve failed: {e}");
        std::process::exit(1);
    }
}
