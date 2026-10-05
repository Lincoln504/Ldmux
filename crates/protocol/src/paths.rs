//! Single source of truth for ldmux runtime paths. Every consumer (daemon,
//! CLI, attach) resolves state/socket paths through these helpers so the
//! fallback chain can never drift between components.
//!
//! Port of `packages/protocol/src/paths.ts` (the source of record, deleted at
//! D4: read it at git commit `6b3efb8`). The env names are the same. There is
//! no multiplexer-socket resolver: the keeper socket is the only session socket.
//!
//! The pure cores take the env values as arguments so tests never race on the
//! process environment; the `resolve_*` wrappers read it.

use std::env;

/// `LDMUX_STATE_DIR`, else `$XDG_RUNTIME_DIR/ldmux` (default `/tmp/ldmux`).
pub fn state_dir_from(state_dir: Option<&str>, xdg_runtime_dir: Option<&str>) -> String {
    match state_dir {
        Some(s) => s.to_string(),
        None => format!("{}/ldmux", xdg_runtime_dir.unwrap_or("/tmp")),
    }
}

/// The resolved state dir, from the process environment.
pub fn resolve_state_dir() -> String {
    let s = env_var("LDMUX_STATE_DIR");
    let x = env_var("XDG_RUNTIME_DIR");
    state_dir_from(s.as_deref(), x.as_deref())
}

/// `LDMUX_SOCK`, else `<state_dir>/ldmuxd.sock`.
pub fn socket_path_from(sock: Option<&str>, state_dir: &str) -> String {
    match sock {
        Some(s) => s.to_string(),
        None => format!("{state_dir}/ldmuxd.sock"),
    }
}

/// The resolved daemon socket path, from the process environment.
pub fn resolve_socket_path() -> String {
    let s = env_var("LDMUX_SOCK");
    let state = resolve_state_dir();
    socket_path_from(s.as_deref(), &state)
}

fn env_var(name: &str) -> Option<String> {
    env::var(name).ok()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_falls_back_to_xdg_then_tmp() {
        assert_eq!(state_dir_from(Some("/s"), None), "/s");
        assert_eq!(state_dir_from(None, Some("/run/u0")), "/run/u0/ldmux");
        assert_eq!(state_dir_from(None, None), "/tmp/ldmux");
    }

    #[test]
    fn socket_path_falls_back_to_state_dir() {
        assert_eq!(socket_path_from(Some("/x"), "/s"), "/x");
        assert_eq!(socket_path_from(None, "/s"), "/s/ldmuxd.sock");
    }

}
