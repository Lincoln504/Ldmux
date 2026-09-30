//! Where a session's directory comes from, in one place.
//!
//! Port of `packages/daemon/src/cwd.ts` (the source of record). Two things were
//! wrong with asking the filesystem directly:
//!
//! - `~` was refused. It is how every person writes their home directory, on a
//!   phone keyboard more than anywhere, and answering `not a directory: ~` to a
//!   path that plainly names one refuses the request rather than the mistake.
//! - An absent directory meant `process.cwd()`, the daemon's own working
//!   directory, which the person asking cannot see and did not choose. A home
//!   directory is at least a directory somebody can name.
//!
//! Only a leading `~` is expanded, and only when it is the whole path or is
//! followed by a separator, so a directory genuinely called `~foo` is still
//! itself. No shell is involved and none is spawned: this is string arithmetic,
//! not an `eval`.

/// The home directory, the way the source of record gets it: `$HOME`.
///
/// The TS source's `os.homedir()` falls back to the passwd entry when `$HOME`
/// is unset; the port does not replicate the fallback (a login daemon always
/// has `$HOME`), so a missing `$HOME` is a configuration error, not a value to
/// guess at.
pub fn home_dir() -> Result<String, String> {
    std::env::var("HOME").map_err(|_| {
        "HOME is not set; the TS source of record would consult the passwd entry, \
         which this port does not replicate — set $HOME"
            .to_string()
    })
}

/// Pure core: `cwd` resolved against an explicit `home`.
///
/// `cwd` is the request's `cwd` field — `None` when absent, a string otherwise
/// (the TS `unknown` collapses to "a non-empty trimmed string or home").
pub fn resolve(cwd: Option<&str>, home: &str) -> String {
    let raw = cwd.map(str::trim).filter(|s| !s.is_empty());
    match raw {
        None => return home.to_string(),
        Some(r) if r == "~" => return home.to_string(),
        Some(r) if let Some(rest) = r.strip_prefix("~/") => {
            let rest = rest.trim_start_matches('/');
            return if rest.is_empty() {
                home.to_string()
            } else {
                format!("{home}/{rest}")
            };
        }
        Some(r) => return r.to_string(),
    }
}

/// The daemon entry point: `cwd` resolved against `$HOME`.
///
/// Panics with a clear message when `$HOME` is unset (see [`home_dir`]); that is
/// the one place the TS source's passwd fallback would matter, and it does not
/// happen for a login daemon.
pub fn resolve_cwd(cwd: Option<&str>) -> String {
    let home = match home_dir() {
        Ok(h) => h,
        Err(why) => panic!("{why}"),
    };
    resolve(cwd, &home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source of record's `const home = homedir()`: `$HOME`.
    fn home() -> String {
        std::env::var("HOME").unwrap()
    }

    #[test]
    fn a_bare_tilde_is_the_home_directory() {
        assert_eq!(resolve_cwd(Some("~")), home());
        assert_eq!(resolve(Some("~"), "/h"), "/h");
    }

    #[test]
    fn a_tilde_and_a_path_are_joined_onto_home() {
        let h = home();
        assert_eq!(resolve_cwd(Some("~/Documents")), format!("{h}/Documents"));
        assert_eq!(resolve_cwd(Some("~/a/b/c")), format!("{h}/a/b/c"));
        assert_eq!(resolve(Some("~/a/b/c"), "/h"), "/h/a/b/c");
    }

    #[test]
    fn a_tilde_with_a_slash_and_nothing_after_it_is_still_home() {
        // `~/` is what a phone keyboard produces from the autocomplete, and it
        // means home rather than a directory named the empty string.
        let h = home();
        assert_eq!(resolve_cwd(Some("~/")), h);
        assert_eq!(resolve_cwd(Some("~//")), h);
        assert_eq!(resolve(Some("~/"), "/h"), "/h");
    }

    #[test]
    fn a_tilde_not_the_whole_path_and_not_followed_by_a_separator_is_left_alone() {
        // `~foo` is a legitimate relative directory name: expanding a leading
        // tilde without checking the separator would silently send the shell to
        // `home/foo`.
        let h = home();
        assert_eq!(resolve_cwd(Some("~foo")), "~foo");
        assert_eq!(resolve_cwd(Some("~foo/bar")), "~foo/bar");
        assert_eq!(resolve_cwd(Some("a/~/b")), "a/~/b");
        assert_eq!(resolve(Some("~foo"), &h), "~foo");
    }

    #[test]
    fn an_absolute_path_is_returned_unchanged() {
        assert_eq!(resolve_cwd(Some("/tmp")), "/tmp");
        assert_eq!(
            resolve_cwd(Some("/home/someone/../someone/x")),
            "/home/someone/../someone/x"
        );
    }

    #[test]
    fn an_absent_blank_or_non_string_directory_means_home() {
        let h = home();
        assert_eq!(resolve_cwd(None), h);
        assert_eq!(resolve_cwd(Some("")), h);
        assert_eq!(resolve_cwd(Some("   ")), h);
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_a_path() {
        let h = home();
        assert_eq!(resolve_cwd(Some("  /tmp  ")), "/tmp");
        assert_eq!(resolve_cwd(Some(" ~ ")), h);
    }

    #[test]
    fn nothing_in_the_path_is_interpreted_beyond_the_leading_tilde() {
        // The expansion is arithmetic on a string; the two ways to get that
        // wrong are to pass it through a shell or to reach for a regex.
        let h = home();
        assert_eq!(resolve_cwd(Some("~/$(whoami)")), format!("{h}/$(whoami)"));
        assert_eq!(resolve_cwd(Some("~/*")), format!("{h}/*"));
        assert_eq!(resolve_cwd(Some("/tmp/../etc")), "/tmp/../etc");
        assert_eq!(resolve(Some("~/a//b"), "/h"), "/h/a//b");
    }
}
