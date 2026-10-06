//! The clipboard-write policy (`ldmux clipboard-write on|off`).
//!
//! Whether a program in a pane may put text on the client's clipboard (OSC 52)
//! is a security decision the user makes deliberately (see `sanitize.rs`). It is
//! one word in a file in the state directory, read when a stream is made, so
//! changing it does not need a daemon restart. The daemon used to *write* this
//! file and nothing ever read it: every stream was built with the policy off, so
//! `ldmux clipboard-write on` was silently a no-op.

use std::path::{Path, PathBuf};

/// The policy file's name inside the state directory.
pub const CLIPBOARD_WRITE_FILE: &str = "clipboard-write";

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(CLIPBOARD_WRITE_FILE)
}

/// Whether clipboard writes are allowed. Missing, unreadable or anything but
/// `on` means closed.
pub fn read(state_dir: &Path) -> bool {
    std::fs::read_to_string(path(state_dir))
        .map(|s| s.trim().eq_ignore_ascii_case("on"))
        .unwrap_or(false)
}

/// Persist the policy (mode 0600, like every other file the daemon keeps).
pub fn write(state_dir: &Path, on: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path(state_dir))?;
    f.write_all(if on { b"on\n" } else { b"off\n" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ldmux-clip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_policy_defaults_closed_and_round_trips() {
        let d = scratch();
        assert!(!read(&d), "missing file means closed");
        write(&d, true).unwrap();
        assert!(read(&d));
        write(&d, false).unwrap();
        assert!(!read(&d));
        std::fs::write(path(&d), "garbage").unwrap();
        assert!(!read(&d), "anything but `on` is closed");
        use std::os::unix::fs::PermissionsExt;
        write(&d, true).unwrap();
        let mode = std::fs::metadata(path(&d)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }
}
