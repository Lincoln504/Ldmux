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

/// The longest a Unix socket path may be: `sun_path` is 108 bytes on Linux and
/// the kernel needs room for the NUL.
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

/// `LDMUX_STATE_DIR`, else `$XDG_RUNTIME_DIR/ldmux`, else `/tmp/ldmux-<uid>`.
///
/// An empty variable is unset (`LDMUX_STATE_DIR=` must not make the state dir
/// `""`, nor `XDG_RUNTIME_DIR=` make it `/ldmux`). The last resort is per-uid,
/// never a bare `/tmp/ldmux` that every user on the box shares and the first
/// to create it owns; [`try_resolve_state_dir`] also creates and verifies it.
pub fn state_dir_from(state_dir: Option<&str>, xdg_runtime_dir: Option<&str>) -> String {
    state_dir_for_uid(state_dir, xdg_runtime_dir, nix::unistd::getuid().as_raw())
}

fn state_dir_for_uid(state_dir: Option<&str>, xdg_runtime_dir: Option<&str>, uid: u32) -> String {
    match (non_empty(state_dir), non_empty(xdg_runtime_dir)) {
        (Some(s), _) => s.to_string(),
        (None, Some(x)) => format!("{}/ldmux", x.trim_end_matches('/')),
        (None, None) => format!("/tmp/ldmux-{uid}"),
    }
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// Create `dir` (0700) if absent, and verify it is a real directory, owned by
/// this uid, with no group/other access. Used for the `/tmp` fallback, where
/// another user could have pre-created the path (or planted a symlink).
fn ensure_private_dir(dir: &std::path::Path, uid: u32) -> Result<(), String> {
    ensure_private_dir_named(dir, uid, "state dir")
}

/// As [`ensure_private_dir`], naming the directory in the message: the persist dir and the
/// state dir fail the same way, and an error that calls the persist dir a state dir sends the
/// reader looking at the wrong path.
fn ensure_private_dir_named(dir: &std::path::Path, uid: u32, what: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(format!("cannot create the {what} {}: {e}", dir.display()));
        }
    }
    // `symlink_metadata`: a symlink planted at the path is refused, not followed.
    let md = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot stat the {what} {}: {e}", dir.display()))?;
    if !md.is_dir() {
        return Err(format!("the {what} {} is not a directory", dir.display()));
    }
    if md.uid() != uid {
        return Err(format!(
            "the {what} {} is owned by uid {}, not {uid}; refusing to use it",
            dir.display(),
            md.uid()
        ));
    }
    if md.mode() & 0o077 != 0 {
        return Err(format!(
            "the {what} {} has mode {:o}; it must not be accessible to group or others",
            dir.display(),
            md.mode() & 0o777
        ));
    }
    Ok(())
}

/// `LDMUX_PERSIST_DIR`, else `$XDG_STATE_HOME/ldmux`, else `$HOME/.local/state/ldmux`.
///
/// **The state dir is a tmpfs and the persist dir is not.** `$XDG_RUNTIME_DIR` is wiped
/// on every boot, which is right for what is rebuildable or per connection (sockets, the
/// registry cache, ownership claims) and wrong for a credential: a token or a session key
/// kept there is silently re-minted and every paired phone is locked out. Those live here.
///
/// The pure core takes the environment as arguments so tests never race on it; the
/// `resolve_` wrapper reads the process environment.
pub fn persist_dir_from(
    persist_dir: Option<&str>,
    state_dir: Option<&str>,
    xdg_state_home: Option<&str>,
    home: Option<&str>,
) -> String {
    if let Some(p) = non_empty(persist_dir) {
        return p.trim_end_matches('/').to_string();
    }
    // **A daemon given its own state directory gets its own credentials.** A scratch daemon
    // (a test, a second instance) sets `LDMUX_STATE_DIR` and nothing else, and inheriting the
    // real persist directory made it adopt the live machine's token, mesh PSK and radio key —
    // measured 2026-11-06: two scratch runs grabbed the deployed host radio out from under the
    // daemon that owns it. The deployed unit sets `LDMUX_PERSIST_DIR` explicitly, so this only
    // ever applies to a run that is already not the deployment.
    if let Some(st) = non_empty(state_dir) {
        return format!("{}/persist", st.trim_end_matches('/'));
    }
    if let Some(s) = non_empty(xdg_state_home) {
        return format!("{}/ldmux", s.trim_end_matches('/'));
    }
    if let Some(h) = non_empty(home) {
        return format!("{}/.local/state/ldmux", h.trim_end_matches('/'));
    }
    // No HOME and no XDG_STATE_HOME: the runtime dir is the only writable place we were
    // given. Better a credential that does not survive a reboot than a path we invent.
    let fallback = state_dir_from(None, env_var("XDG_RUNTIME_DIR").as_deref());
    format!("{fallback}/persist")
}

/// The resolved persist dir, creating it 0700 and verifying it is ours when absent.
pub fn try_resolve_persist_dir() -> Result<String, String> {
    let dir = persist_dir_from(
        env_var("LDMUX_PERSIST_DIR").as_deref(),
        env_var("LDMUX_STATE_DIR").as_deref(),
        env_var("XDG_STATE_HOME").as_deref(),
        env_var("HOME").as_deref(),
    );
    ensure_private_dir_named(
        std::path::Path::new(&dir),
        nix::unistd::getuid().as_raw(),
        "persist dir",
    )?;
    Ok(dir)
}

/// The resolved state dir, from the process environment, or why it cannot be
/// used. When the per-uid `/tmp` fallback is in play the directory is created
/// and its owner and mode are verified.
pub fn try_resolve_state_dir() -> Result<String, String> {
    let s = env_var("LDMUX_STATE_DIR").filter(|v| !v.is_empty());
    let x = env_var("XDG_RUNTIME_DIR").filter(|v| !v.is_empty());
    let dir = state_dir_from(s.as_deref(), x.as_deref());
    if s.is_none() && x.is_none() {
        ensure_private_dir(std::path::Path::new(&dir), nix::unistd::getuid().as_raw())?;
    }
    Ok(dir)
}

/// The resolved state dir, from the process environment. An unusable fallback
/// directory is reported on stderr and ends the process: every caller is a
/// binary about to bind or connect, and carrying on would use a directory
/// another user controls.
pub fn resolve_state_dir() -> String {
    try_resolve_state_dir().unwrap_or_else(|e| exit_with(&e))
}

/// `LDMUX_SOCK`, else `<state_dir>/ldmuxd.sock`. An empty `LDMUX_SOCK` is unset.
pub fn socket_path_from(sock: Option<&str>, state_dir: &str) -> String {
    match non_empty(sock) {
        Some(s) => s.to_string(),
        None => format!("{state_dir}/ldmuxd.sock"),
    }
}

/// The daemon's local attach socket, derived from its control socket: the same
/// directory and stem with `-attach` before the extension (`ldmuxd.sock` becomes
/// `ldmuxd-attach.sock`). It carries the WebSocket protocol for the desk attach
/// client, so a window needs no TCP, no tailnet and no token — only to be this
/// user (the daemon checks the peer's uid). Derived rather than configured so the
/// daemon and the client resolve it from the one path they already share.
pub fn attach_socket_path_from(control_socket: &str) -> String {
    match control_socket.strip_suffix(".sock") {
        Some(stem) => format!("{stem}-attach.sock"),
        None => format!("{control_socket}-attach"),
    }
}

/// Refuse a socket path the kernel cannot bind: over `sun_path`'s limit,
/// which otherwise surfaces as a bare `InvalidInput` from `bind`/`connect`.
pub fn check_socket_path(path: &str) -> Result<(), String> {
    if path.len() > MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "the socket path is {} bytes, over the {MAX_SOCKET_PATH_BYTES}-byte Unix socket limit: {path} (use a shorter LDMUX_STATE_DIR or LDMUX_SOCK)",
            path.len()
        ));
    }
    if path.as_bytes().contains(&0) {
        return Err("the socket path contains a NUL byte".to_string());
    }
    Ok(())
}

/// The resolved daemon socket path, or why it is unusable.
pub fn try_resolve_socket_path() -> Result<String, String> {
    let s = env_var("LDMUX_SOCK");
    let state = try_resolve_state_dir()?;
    let path = socket_path_from(s.as_deref(), &state);
    check_socket_path(&path)?;
    Ok(path)
}

/// The resolved daemon socket path, from the process environment. Exits with a
/// clear message when the path is unusable (see [`resolve_state_dir`]).
pub fn resolve_socket_path() -> String {
    try_resolve_socket_path().unwrap_or_else(|e| exit_with(&e))
}

fn exit_with(msg: &str) -> ! {
    eprintln!("ldmux: {msg}");
    std::process::exit(1)
}

fn env_var(name: &str) -> Option<String> {
    env::var(name).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_attach_socket_sits_beside_the_control_socket() {
        assert_eq!(
            attach_socket_path_from("/run/user/1000/ldmux/ldmuxd.sock"),
            "/run/user/1000/ldmux/ldmuxd-attach.sock"
        );
        assert_eq!(
            attach_socket_path_from("/tmp/x/custom"),
            "/tmp/x/custom-attach"
        );
    }

    use super::*;

    #[test]
    fn state_dir_falls_back_to_xdg_then_a_per_uid_tmp_dir() {
        assert_eq!(state_dir_for_uid(Some("/s"), None, 7), "/s");
        assert_eq!(state_dir_for_uid(None, Some("/run/u0"), 7), "/run/u0/ldmux");
        assert_eq!(state_dir_for_uid(None, None, 7), "/tmp/ldmux-7");
    }

    #[test]
    fn empty_variables_are_unset() {
        assert_eq!(
            state_dir_for_uid(Some(""), Some("/run/u0"), 7),
            "/run/u0/ldmux"
        );
        assert_eq!(state_dir_for_uid(Some(""), Some(""), 7), "/tmp/ldmux-7");
        assert_eq!(state_dir_for_uid(None, Some(""), 7), "/tmp/ldmux-7");
        assert_eq!(socket_path_from(Some(""), "/s"), "/s/ldmuxd.sock");
    }

    #[test]
    fn the_persist_dir_is_not_the_runtime_dir() {
        assert_eq!(
            persist_dir_from(Some("/p"), None, Some("/s"), Some("/h")),
            "/p"
        );
        assert_eq!(
            persist_dir_from(Some("/p/"), None, None, Some("/h")),
            "/p"
        );
        // A state directory of its own means credentials of its own: a scratch daemon must not
        // read the deployment's token, PSK or radio key.
        assert_eq!(
            persist_dir_from(None, Some("/tmp/scratch"), Some("/s"), Some("/h")),
            "/tmp/scratch/persist"
        );
        assert_eq!(
            persist_dir_from(None, None, Some("/s/"), Some("/h")),
            "/s/ldmux"
        );
        assert_eq!(
            persist_dir_from(None, None, None, Some("/h")),
            "/h/.local/state/ldmux"
        );
        // Empty is unset, exactly as for the state dir.
        assert_eq!(
            persist_dir_from(Some(""), None, Some("/s"), Some("/h")),
            "/s/ldmux"
        );
    }

    #[test]
    fn socket_path_falls_back_to_state_dir() {
        assert_eq!(socket_path_from(Some("/x"), "/s"), "/x");
        assert_eq!(socket_path_from(None, "/s"), "/s/ldmuxd.sock");
    }

    #[test]
    fn an_overlong_socket_path_is_an_error() {
        assert!(check_socket_path(&format!("/{}", "a".repeat(106))).is_ok()); // 107 bytes
        let e = check_socket_path(&format!("/{}", "a".repeat(107))).unwrap_err(); // 108
        assert!(e.contains("108 bytes") && e.contains("limit"), "{e}");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ldmux-paths-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_fallback_dir_is_created_private_and_verified() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let uid = nix::unistd::getuid().as_raw();
        let base = scratch("ok");
        let dir = base.join("state");
        ensure_private_dir(&dir, uid).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        // Idempotent.
        ensure_private_dir(&dir, uid).unwrap();
        // Loosened: refused.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&dir, uid).unwrap_err().contains("mode"));
        // Someone else's: refused.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            ensure_private_dir(&dir, uid + 1)
                .unwrap_err()
                .contains("owned by")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_symlink_at_the_fallback_path_is_refused() {
        let uid = nix::unistd::getuid().as_raw();
        let base = scratch("link");
        let target = base.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            ensure_private_dir(&link, uid)
                .unwrap_err()
                .contains("not a directory")
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
