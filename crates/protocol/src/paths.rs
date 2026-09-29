//! Single source of truth for ldmux runtime paths. Every consumer (daemon,
//! CLI, attach) resolves state/socket paths through these helpers so the
//! fallback chain can never drift between components.
//!
//! Port of `packages/protocol/src/paths.ts` (the source of record). The env
//! names are the same. `tmux_socket_from` is **transitional**: the tmux
//! server leaves with the port (plan §8/§9), but it is the source of record
//! today and the A/B window still runs on it, so it is ported faithfully and
//! marked for deletion.
//!
//! The pure cores take the env values as arguments so tests never race on the
//! process environment; the `resolve_*` wrappers read it.

use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSocketError(pub String);

impl std::fmt::Display for TmuxSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for TmuxSocketError {}

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

/// The tmux server ldmux drives. **Never the user's.**
///
/// `LDMUX_TMUX_SOCK` (default `ldmux`). Pointing it at the user's server —
/// which is the plausible mistake if you want ldmux to "see your existing
/// sessions" — would rewrite their global options, so `default` is refused
/// rather than obeyed: the mistake is plausible and the damage is silent.
///
/// **Transitional** (the tmux server leaves with the port; see the module doc).
pub fn tmux_socket_from(name: Option<String>) -> Result<String, TmuxSocketError> {
    let name = name.unwrap_or_else(|| "ldmux".to_string());
    if name == "default" {
        return Err(TmuxSocketError(
            "LDMUX_TMUX_SOCK=default refused: ldmux must use its own tmux server. \
             It sets global tmux options (status, mouse, history-limit, COLORTERM) \
             which would change the behaviour of your own tmux sessions."
                .to_string(),
        ));
    }
    Ok(name)
}

/// The resolved tmux socket name, from the process environment.
pub fn resolve_tmux_socket() -> Result<String, TmuxSocketError> {
    tmux_socket_from(env_var("LDMUX_TMUX_SOCK"))
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

    #[test]
    fn tmux_socket_default_name_and_refusal() {
        assert_eq!(tmux_socket_from(None).unwrap(), "ldmux");
        assert_eq!(tmux_socket_from(Some("mync".to_string())).unwrap(), "mync");
        assert!(tmux_socket_from(Some("default".to_string())).is_err());
    }
}
