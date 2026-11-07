//! A scratch directory that removes itself, for the unit tests in this crate.
//!
//! The same type lives in `tests/common/mod.rs` for the integration tests, because a `cfg(test)`
//! item is not visible from a separate test crate. Both are needed: every test that keeps state in
//! a scratch directory leaks one directory per run unless the directory removes itself, and by
//! 2026-11-07 the temp dir on the machine this was measured on held 184 `ldmux-auth-*`, 146
//! `ldmux-keeper-supervisor-*`, 231 `ldmux-risk-*`, 73 `ldmux-qr-*` and 6187
//! `ldmux-daemon-core-*` directories. A `Drop` guard is the only version that also cleans up when
//! the test panics, which is when a leak matters most.

use std::path::Path;

/// A scratch directory, removed when this drops.
pub(crate) struct Scratch(std::path::PathBuf);

impl Scratch {
    /// Create a directory named `<prefix>-<pid>-<unique>` under the temp dir.
    pub(crate) fn dir(prefix: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).expect("a scratch directory");
        Self(p)
    }

    /// The path, for the call that needs an owned `PathBuf`.
    pub(crate) fn to_path_buf(&self) -> std::path::PathBuf {
        self.0.clone()
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
