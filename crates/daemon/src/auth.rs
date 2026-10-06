//! Remote access token.
//!
//! Port of `packages/daemon/src/auth.ts` (the source of record).
//!
//! The daemon's WebSocket listens on the tailnet address (or loopback) and
//! answers only tailnet or loopback peers (ADR-0016). Access additionally
//! requires a bearer token stored 0600 at `LDMUX_TOKEN_FILE` (default
//! `<stateDir>/token`) and rotated only on demand.

use std::os::unix::fs::PermissionsExt;

use rand::Rng;

/// Load the token at `state_dir` (or `token_path` when given), minting one when
/// there is none.
///
/// The move-semantics of `token_path` are the load-bearing part:
///
/// - A copy left at the old place would be adopted again if the real one were
///   deleted to rotate it — silently bringing the old token back — so whenever
///   a distinct `token_path` is in play, the legacy copy is removed.
/// - Moving the token out of the state directory must not unpair a phone that
///   was paired against the old location: the first start with a new path adopts
///   the token that is already there rather than minting one.
///
/// Every failure is an error. A token that could not be persisted would work
/// until the next restart and then lock the phone out, and a token file that is
/// not ours (another uid could have planted it) must not be trusted.
pub fn load_or_create_token(state_dir: &str, token_path: Option<&str>) -> Result<String, String> {
    let legacy_path = format!("{state_dir}/token");
    let path: String = token_path
        .map(str::to_string)
        .unwrap_or_else(|| legacy_path.clone());

    let existing = read_token(&path)?;
    if !existing.is_empty() {
        // A copy left at the old place would be adopted again if the real one
        // were deleted to rotate it — silently bringing the old token back.
        if let Some(p) = token_path
            && p != legacy_path.as_str()
        {
            let _ = std::fs::remove_file(&legacy_path);
        }
        return Ok(existing);
    }

    // Moving the token out of the state directory must not unpair a phone that
    // was paired against the old location: adopt the token that is already there
    // rather than minting one. (Only consulted when a distinct path is given —
    // the TS reads the legacy spot in that case, and not otherwise.)
    let legacy = if token_path.is_some() {
        read_token(&legacy_path)?
    } else {
        String::new()
    };
    let token = if legacy.is_empty() {
        random_token()
    } else {
        legacy
    };

    // mkdir -p the parent 0700 (only when we create it: the TS mode applies to
    // created directories), then write 0600 and chmod, so umask cannot widen
    // it: the token is a credential even though it lives in a 0700 dir.
    if let Some(parent) = std::path::Path::new(&path).parent()
        && parent != std::path::Path::new("")
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod {}: {e}", parent.display()))?;
    }
    // Created 0600 from the first byte: `fs::write` then `chmod` left a window in
    // which the credential existed with the umask's mode (0644), readable by any
    // local user when the parent directory was not ours to tighten.
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("write the token {path}: {e}"))?;
        f.write_all(token.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("write the token {path}: {e}"))?;
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod the token {path}: {e}"))?;

    // Once the token has a path of its own, the legacy copy must go, so a
    // deletion of the real token really rotates it.
    if let Some(p) = token_path
        && p != legacy_path.as_str()
    {
        let _ = std::fs::remove_file(&legacy_path);
    }
    Ok(token)
}

/// Thirty-two random bytes, hex — the token's shape (64 lowercase hex chars).
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes[..]);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::new();
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// The token at `path`, or empty when there is no file. A file that exists is
/// only trusted when this uid owns it, and is tightened to 0600 (a hand-written
/// one is usually 0644); one that cannot be is an error.
fn read_token(path: &str) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let md = match std::fs::metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("stat the token {path}: {e}")),
    };
    let uid = nix::unistd::getuid().as_raw();
    if md.uid() != uid {
        return Err(format!(
            "the token file {path} is owned by uid {}, not {uid}; refusing to adopt it",
            md.uid()
        ));
    }
    if md.mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            format!("the token file {path} is accessible to others and could not be fixed: {e}")
        })?;
    }
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("read the token {path}: {e}"))
}

/// Constant-time comparison of the secret bytes.
///
/// The length check is an early exit, which is safe *here*: the token length is
/// fixed and public, so it leaks nothing, and the byte-wise compare would
/// otherwise read a different number of bytes for a wrong-length input (the TS
/// `timingSafeEqual` throws on a length mismatch; the guard comes first).
pub fn token_matches(expected: &str, provided: &str) -> bool {
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ldmux-auth-{}", uuid::Uuid::new_v4()))
    }

    fn new_state_dir(root: &std::path::Path) -> String {
        let p = root.join(format!("s-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().to_string()
    }

    fn is_token_hex64(t: &str) -> bool {
        t.len() == 64 && t.chars().all(|c| "0123456789abcdef".contains(c))
    }

    #[test]
    fn a_token_is_32_random_bytes_in_hex_stored_owner_only() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        let token = load_or_create_token(&state_dir, None).unwrap();
        assert!(is_token_hex64(&token));
        assert_eq!(
            std::fs::read_to_string(format!("{state_dir}/token")).unwrap(),
            token
        );
        // 0600: the token is a credential even though it lives in a 0700 dir.
        let mode = std::fs::metadata(format!("{state_dir}/token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_token_is_stable_across_loads() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        let first = load_or_create_token(&state_dir, None).unwrap();
        assert_eq!(load_or_create_token(&state_dir, None).unwrap(), first);
        assert_eq!(load_or_create_token(&state_dir, None).unwrap(), first);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_daemons_get_different_tokens() {
        let root = temp_dir();
        let a = load_or_create_token(&new_state_dir(&root), None).unwrap();
        let b = load_or_create_token(&new_state_dir(&root), None).unwrap();
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_existing_token_file_is_adopted_rather_than_overwritten() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        std::fs::write(format!("{state_dir}/token"), "hand-written-token\n").unwrap();
        assert_eq!(
            load_or_create_token(&state_dir, None).unwrap(),
            "hand-written-token"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_empty_or_whitespace_token_file_is_replaced() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        std::fs::write(format!("{state_dir}/token"), "   \n").unwrap();
        let token = load_or_create_token(&state_dir, None).unwrap();
        assert!(is_token_hex64(&token));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn token_comparison_is_exact() {
        // Length, case and content all matter.
        let token = "a".repeat(64);
        assert!(token_matches(&token, &token));
        assert!(!token_matches(&token, &"a".repeat(63)));
        assert!(!token_matches(&token, &"a".repeat(65)));
        assert!(!token_matches(&token, &"A".repeat(64)));
        let b_first = "b".to_string() + &"a".repeat(63);
        assert!(!token_matches(&token, &b_first));
        let b_last = "a".repeat(63) + "b";
        assert!(!token_matches(&token, &b_last));
        assert!(!token_matches(&token, ""));
        assert!(token_matches("", ""));
        // Non-ASCII/UTF-8 must not sneak through a byte-length comparison.
        let e = "é".repeat(32);
        assert!(token_matches(&e, &e));
        assert!(!token_matches(&e, &"e".repeat(32)));
    }

    #[test]
    fn comparison_does_not_panic_on_differently_sized_buffers() {
        // The guard must come first: a byte-wise compare of differently sized
        // buffers would read out of bounds, and the TS's `timingSafeEqual`
        // throws on a length mismatch.
        assert!(!token_matches("abc", "abcdef"));
        assert!(!token_matches("abcdef", "abc"));
    }

    #[test]
    fn a_token_path_outside_the_state_dir_survives_the_state_dir_being_wiped() {
        // The state dir is a tmpfs under XDG_RUNTIME_DIR: gone on every reboot.
        let root = temp_dir();
        let token_path = root
            .join("persist/nested/token")
            .to_string_lossy()
            .to_string();
        let boot1 = new_state_dir(&root);
        let first = load_or_create_token(&boot1, Some(&token_path)).unwrap();
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let boot2 = new_state_dir(&root);
        assert_eq!(
            load_or_create_token(&boot2, Some(&token_path)).unwrap(),
            first
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn moving_the_token_to_its_own_path_adopts_the_one_already_paired() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        let paired = load_or_create_token(&state_dir, None).unwrap();
        let moved_root = root.join(format!("moved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&moved_root).unwrap();
        let token_path = moved_root.join("token").to_string_lossy().to_string();
        assert_eq!(
            load_or_create_token(&state_dir, Some(&token_path)).unwrap(),
            paired
        );
        assert_eq!(std::fs::read_to_string(&token_path).unwrap(), paired);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn once_moved_the_old_copy_is_gone_so_deleting_the_token_really_rotates_it() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        let paired = load_or_create_token(&state_dir, None).unwrap();
        let moved_root = root.join(format!("rotate-p-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&moved_root).unwrap();
        let token_path = moved_root.join("token").to_string_lossy().to_string();
        load_or_create_token(&state_dir, Some(&token_path)).unwrap();
        // The legacy copy is gone, so deleting the token really rotates it.
        assert!(!std::path::Path::new(&state_dir).join("token").exists());
        std::fs::remove_file(&token_path).unwrap();
        assert_ne!(
            load_or_create_token(&state_dir, Some(&token_path)).unwrap(),
            paired
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_token_that_cannot_be_written_is_an_error_not_a_silent_success() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        // A directory sits where the token file should be: it cannot be written.
        std::fs::create_dir_all(format!("{state_dir}/token/x")).unwrap();
        assert!(load_or_create_token(&state_dir, None).is_err());
        // A parent that cannot be created: a file in the way.
        let blocker = format!("{state_dir}/blocker");
        std::fs::write(&blocker, "x").unwrap();
        let under = format!("{blocker}/sub/token");
        assert!(load_or_create_token(&state_dir, Some(&under)).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_adopted_token_file_is_tightened_to_owner_only() {
        let root = temp_dir();
        let state_dir = new_state_dir(&root);
        let path = format!("{state_dir}/token");
        std::fs::write(&path, "hand-written-token").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            load_or_create_token(&state_dir, None).unwrap(),
            "hand-written-token"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(root);
    }
}
