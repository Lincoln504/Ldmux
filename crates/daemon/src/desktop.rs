//! Desktop windows (ADR-0010, `docs/DESKTOP-INTEGRATION.md`).
//!
//! Port of the pure functions from `packages/daemon/src/desktop.ts`.
//!
//! **There is no terminal-multiplexer mirror.** `ldmux mirror <id>` runs `ldmuxd attach <id> --readonly` —
//! the same protocol client as a desk window, told not to take a claim.

/// Shells that cannot use a mouse click.
const SHELLS: &[&str] = &[
    "bash", "sh", "dash", "ash", "zsh", "fish", "ksh", "ksh93", "mksh",
    "tcsh", "csh", "yash", "nu", "nushell", "elvish", "xonsh", "pwsh",
];

/// Is this pane's foreground command an interactive shell?
pub fn is_shell_foreground(command: &str) -> bool {
    let trimmed = command.trim().to_lowercase();
    let stripped = trimmed.strip_prefix('-').unwrap_or(&trimmed);
    SHELLS.contains(&stripped)
}

/// The i3 window class for a session's live window.
///
/// Derived from the session id, and sanitised: the class is interpolated into an
/// i3 criterion (`[class="…"]`), so anything outside `[A-Za-z0-9_-]` has to go.
pub fn live_window_class(session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    format!("ldmux-live-{safe}")
}

/// An i3 workspace target, or None when the name could be i3 syntax.
///
/// The name reaches the window manager as a criterion in `closeLive`, so it is
/// restricted to the characters real workspace names use.
pub fn safe_workspace(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 32 {
        return None;
    }
    let valid = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if valid {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// The desk window's argv: alacritty running the attach client (plan §3.9).
///
/// The window is a protocol client of the daemon (`ldmuxd attach <id>`), not a
/// multiplexer client: it claims the shell (ADR-0010), streams the pane, and handles
/// the handover. `ldmuxd` is resolved on PATH (the unit puts `~/.local/bin`
/// first).
pub fn live_argv(session_id: &str, terminal: &str) -> Vec<String> {
    vec![
        terminal.to_string(),
        "--class".to_string(),
        live_window_class(session_id),
        "-e".to_string(),
        attach_bin(),
        "attach".to_string(),
        session_id.to_string(),
    ]
}

/// The absolute path to the attach client (`ldmuxd`).
///
/// **Absolute, not `ldmuxd`.** A window is launched by `i3-msg exec`, and i3's
/// own environment has no `~/.local/bin` on `PATH` — the same failure that once
/// made every `$mod+Return` fall back to a bare shell (`node: not found`) and
/// then again put a window up that closed the instant it opened.
///
/// Resolution order: this executable when it *is* `ldmuxd` (the daemon's own
/// case), then a `ldmuxd` beside it (the install layout puts all three binaries
/// in `~/.local/bin`), then `$HOME/.local/bin/ldmuxd`. The last resort is this
/// executable's own path, which is absolute — never a bare name.
pub fn attach_bin() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().and_then(|n| n.to_str()) == Some("ldmuxd")
            && let Some(s) = exe.to_str() {
                return s.to_string();
            }
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("ldmuxd");
            if candidate.exists()
                && let Some(s) = candidate.to_str() {
                    return s.to_string();
                }
        }
        if let Some(s) = exe.to_str()
            && s.starts_with('/') {
                return s.to_string();
            }
    }
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => format!("{h}/.local/bin/ldmuxd"),
        _ => "/usr/local/bin/ldmuxd".to_string(),
    }
}


/// Find a window id in an i3 tree by window class.
///
/// Walks the whole tree because a class match is the only reliable criterion:
/// the window may be floating, or nested in a split, by the time it is found.
pub fn find_window_id_by_class(tree_json: &str, class_name: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(tree_json).ok()?;
    visit(&root, class_name)
}

fn visit(node: &serde_json::Value, class_name: &str) -> Option<String> {
    let n = node.as_object()?;
    if let Some(w) = n.get("window").and_then(|w| w.as_u64())
        && w != 0 {
            let cls = n
                .get("app_id")
                .and_then(|a| a.as_str())
                .or_else(|| {
                    n.get("window_properties")
                        .and_then(|p| p.get("class"))
                        .and_then(|c| c.as_str())
                })
                .unwrap_or("");
            if cls == class_name {
                return Some(format!("0x{:x}", w));
            }
        }
    let mut children = Vec::new();
    if let Some(nodes) = n.get("nodes").and_then(|n| n.as_array()) {
        children.extend(nodes.iter());
    }
    if let Some(floating) = n.get("floating_nodes").and_then(|n| n.as_array()) {
        children.extend(floating.iter());
    }
    for child in &children {
        if let Some(id) = visit(child, class_name) {
            return Some(id);
        }
    }
    None
}

/// The argv that asks i3 to run a command.
///
/// The command goes in as **separate arguments**, and that is not cosmetic.
/// Quote an argument for a POSIX shell.
pub fn shell_quote(a: &str) -> String {
    // If the string is safe (no shell metacharacters), return it as-is.
    let safe: Vec<bool> = a
        .chars()
        .map(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'))
        .collect();
    if !a.is_empty() && safe.iter().all(|&b| b) {
        return a.to_string();
    }
    // Quote it (single-quoted, with embedded single quotes escaped).
    format!("'{}'", a.replace('\'', "'\\''"))
}

pub fn live_exec_argv(command: &[&str]) -> Vec<String> {
    let mut argv = vec!["i3-msg".to_string(), "exec".to_string(), "--".to_string()];
    for arg in command {
        argv.push(shell_quote(arg));
    }
    argv
}

/// The result of a live-window operation.
pub struct LiveResult {
    pub ok: bool,
    pub placed: Option<bool>,
    pub window_id: Option<String>,
    pub detail: Option<String>,
}

/// Write a session's live-window shim and return what to run.
///
/// Written with mode 700 and overwritten on every make-live: it embeds the keeper
/// socket and session name, so a stale copy is a window that attaches to the
/// wrong thing, and a world-writable one is a command somebody else can
/// replace.
/// Close a session's live window, if it is open.
///
/// i3 answers an unmatched criterion with success too, so this says "asked",
/// not "was open" — which is the honest thing to report.
pub async fn close_live(session_id: &str) -> Result<bool, String> {
    if std::env::var("DISPLAY").is_err() {
        return Err("no DISPLAY (not an X11 session)".to_string());
    }
    let class = live_window_class(session_id);
    let result = crate::exec::run(
        vec!["i3-msg".to_string(), format!("[class=\"{class}\"] kill")],
        crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
    )
    .await;
    if result.code == 0 {
        Ok(true)
    } else {
        Err("i3-msg not available".to_string())
    }
}

/// Is a session's live window still on screen?
///
/// Three answers, not two. "Unknown" is not "absent": the caller uses this to
/// decide whether a shell was *disposed of*, and treating "i3 is not
/// answering" as "the window was closed" would kill sessions every time the
/// window manager hiccupped.
pub async fn live_window_state(session_id: &str) -> Result<&'static str, String> {
    if std::env::var("DISPLAY").is_err() {
        return Ok("unknown");
    }
    let class = live_window_class(session_id);
    let result = crate::exec::run(
        vec!["i3-msg".to_string(), "-t".to_string(), "get_tree".to_string()],
        crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
    )
    .await;
    if result.code == 0 {
        let found = find_window_id_by_class(&result.out, &class).is_some();
        Ok(if found { "present" } else { "absent" })
    } else {
        Ok("unknown")
    }
}

/// The session ids whose live window is currently mapped.
///
/// Used at startup: the live-window map is memory, so a daemon that has just
/// started adopts the windows that are actually on screen instead of forgetting
/// them (and then never ending their shells when they close).
pub async fn live_window_ids() -> Vec<String> {
    if std::env::var("DISPLAY").is_err() {
        return Vec::new();
    }
    let result = crate::exec::run(
        vec!["i3-msg".to_string(), "-t".to_string(), "get_tree".to_string()],
        crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
    )
    .await;
    if result.code != 0 {
        return Vec::new();
    }
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&result.out) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_live_ids(&root, &mut out);
    out
}

fn collect_live_ids(node: &serde_json::Value, out: &mut Vec<String>) {
    let Some(n) = node.as_object() else {
        return;
    };
    if let Some(w) = n.get("window").and_then(|w| w.as_u64())
        && w != 0 {
            let cls = n
                .get("app_id")
                .and_then(|a| a.as_str())
                .or_else(|| {
                    n.get("window_properties")
                        .and_then(|p| p.get("class"))
                        .and_then(|c| c.as_str())
                })
                .unwrap_or("");
            if let Some(id) = cls.strip_prefix("ldmux-live-")
                && !id.is_empty() {
                    out.push(id.to_string());
                }
        }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = n.get(key).and_then(|x| x.as_array()) {
            for c in children {
                collect_live_ids(c, out);
            }
        }
    }
}

/// Launch a session's live window and put it on a chosen workspace.
///
/// The launch goes through `i3-msg exec` (which is how a window gets made at all
/// under i3) but the *placement* is `place_live`, so the user's view never moves.
/// Open a session's desk window.
///
/// The window runs the attach client, which speaks the daemon's own protocol
/// (plan §3.9): no shim script, no state dir, no socket or binary parameters.
pub async fn open_live(session_id: &str, workspace: &str) -> Result<LiveResult, String> {
    // An empty workspace means "leave it where it opens" (the current one):
    // that is the desk-window case (`$mod+Return`), which must not move the
    // user's view. A non-empty value must be a real i3 workspace name.
    let target = match safe_workspace(workspace) {
        Some(t) => Some(t),
        None if workspace.trim().is_empty() => None,
        None => return Err("invalid workspace name".to_string()),
    };
    if std::env::var("DISPLAY").is_err() {
        return Err("no DISPLAY (not an X11 session)".to_string());
    }
    let argv = live_argv(session_id, "alacritty");
    let exec_argv = live_exec_argv(&argv.iter().map(|s| s.as_str()).collect::<Vec<&str>>());
    let result = crate::exec::run(
        exec_argv.clone(),
        crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
    )
    .await;
    if result.code != 0 {
        return Err("i3-msg not available".to_string());
    }
    let placed = match &target {
        Some(t) => place_live(session_id, t, 5000).await,
        None => (true, None, Some("left on the current workspace".to_string())),
    };
    Ok(LiveResult {
        ok: true,
        placed: Some(placed.0),
        window_id: placed.1.clone(),
        detail: placed.2.clone(),
    })
}

/// Place a session's live window on a chosen workspace.
///
/// Polls for the window rather than sleeping a guessed interval: a terminal can
/// take a moment to map, and the move is a no-op until it exists.
pub async fn place_live(
    session_id: &str,
    workspace: &str,
    timeout_ms: u64,
) -> (bool, Option<String>, Option<String>) {
    let target = match safe_workspace(workspace) {
        Some(t) => t,
        None => return (false, None, Some("invalid workspace name".to_string())),
    };
    if std::env::var("DISPLAY").is_err() {
        return (false, None, Some("no DISPLAY (not an X11 session)".to_string()));
    }
    let class = live_window_class(session_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if std::time::Instant::now() >= deadline {
            return (false, None, Some("the window did not appear".to_string()));
        }
        let result = crate::exec::run(
            vec!["i3-msg".to_string(), "-t".to_string(), "get_tree".to_string()],
            crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
        )
        .await;
        if result.code == 0 {
            if let Some(window_id) = find_window_id_by_class(&result.out, &class) {
                let moved = crate::exec::run(
                    vec![
                        "i3-msg".to_string(),
                        format!("[id=\"{window_id}\"] move to workspace number {target}"),
                    ],
                    crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
                )
                .await;
                if moved.code == 0 {
                    return (true, Some(window_id), None);
                } else {
                    return (
                        false,
                        Some(window_id),
                        Some("i3 move failed".to_string()),
                    );
                }
            }
        } else if std::time::Instant::now() > deadline - std::time::Duration::from_millis(4000) {
            return (false, None, Some("i3-msg not available".to_string()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_shell_foreground_matches_known_shells() {
        assert!(is_shell_foreground("bash"));
        assert!(is_shell_foreground("zsh"));
        assert!(is_shell_foreground(" -bash"));
        assert!(is_shell_foreground("ZSH"));
        assert!(!is_shell_foreground("node"));
        assert!(!is_shell_foreground("vim"));
        assert!(!is_shell_foreground(""));
    }

    #[test]
    fn shell_quote_safe_string() {
        assert_eq!(shell_quote("hello"), "hello");
        assert_eq!(shell_quote("path/to/file"), "path/to/file");
    }

    #[test]
    fn shell_quote_unsafe_string() {
        let q = shell_quote("hello world");
        assert!(q.starts_with('\''));
        assert!(q.ends_with('\''));
        let q2 = shell_quote("it's");
        assert!(q2.contains("'\\''"));
    }

    #[test]
    fn live_window_class_sanitizes() {
        assert_eq!(live_window_class("abc123"), "ldmux-live-abc123");
        assert_eq!(live_window_class("abc-123"), "ldmux-live-abc-123");
        // Quotes and spaces are stripped.
        assert_eq!(live_window_class("abc\"def"), "ldmux-live-abcdef");
        // Longer than 40 chars is truncated.
        let long = "a".repeat(100);
        assert_eq!(live_window_class(&long), format!("ldmux-live-{}", "a".repeat(40)));
    }

    #[test]
    fn safe_workspace_valid() {
        assert_eq!(safe_workspace("1"), Some("1".to_string()));
        assert_eq!(safe_workspace("workspace-1"), Some("workspace-1".to_string()));
        assert_eq!(safe_workspace(" 1 "), Some("1".to_string()));
    }

    #[test]
    fn safe_workspace_invalid() {
        assert_eq!(safe_workspace(""), None);
        assert_eq!(safe_workspace("has space"), None);
        assert_eq!(safe_workspace("has$var"), None);
        assert_eq!(safe_workspace(&"a".repeat(33)), None);
    }

    #[test]
    fn live_argv_runs_the_attach_client() {
        let args = live_argv("abc123", "alacritty");
        assert_eq!(
            args,
            vec![
                "alacritty",
                "--class",
                "ldmux-live-abc123",
                "-e",
                &attach_bin(),
                "attach",
                "abc123",
            ]
        );
        // The binary must be a path i3's exec can resolve without a PATH.
        assert!(args[4].starts_with('/'), "the attach binary must be absolute: {}", args[4]);
    }

    #[test]
    fn find_window_id_by_class_finds_a_window() {
        let tree = r#"{"nodes": [{"window": 12345, "app_id": "ldmux-live-abc123"}]}"#;
        let id = find_window_id_by_class(tree, "ldmux-live-abc123").unwrap();
        assert_eq!(id, "0x3039");
    }

    #[test]
    fn find_window_id_by_class_returns_none_when_absent() {
        let tree = r#"{"nodes": [{"window": 12345, "app_id": "other-class"}]}"#;
        assert_eq!(find_window_id_by_class(tree, "ldmux-live-abc123"), None);
    }

    #[test]
    fn find_window_id_by_class_finds_nested_and_floating() {
        let tree = r#"{"nodes": [{"nodes": [{"window": 999, "app_id": "ldmux-live-abc123"}]}, {"floating_nodes": [{"window": 777, "window_properties": {"class": "ldmux-live-abc123"}}]}]}"#;
        // The nested one is found first.
        assert_eq!(find_window_id_by_class(tree, "ldmux-live-abc123").unwrap(), "0x3e7");
    }

    #[test]
    fn live_exec_argv_quits_each_arg() {
        let argv = live_exec_argv(&["ldmuxd", "-L", "sock", "attach", "sess"]);
        assert_eq!(argv[0], "i3-msg");
        assert_eq!(argv[1], "exec");
        assert_eq!(argv[2], "--");
        assert_eq!(argv[3], "ldmuxd");
        assert_eq!(argv[4], "-L");
        assert_eq!(argv[5], "sock");
    }

    /// A program that can use a click is not mistaken for a shell (the TS
    /// `a program that can use a click is not mistaken for a shell`).
    #[test]
    fn a_non_shell_is_not_a_shell_foreground() {
        for prog in ["top", "vim", "emacs", "htop", "less", "nano", "git", "make", "cargo"] {
            assert!(!is_shell_foreground(prog), "{prog} should not be a shell");
        }
        // The login shell with a leading dash is a shell.
        assert!(is_shell_foreground("-bash"));
        // An uppercase shell name is a shell.
        assert!(is_shell_foreground("BASH"));
        // A shell with a path is not a shell (the foreground is the basename).
        assert!(!is_shell_foreground("/usr/bin/bash"));
    }

    /// liveExecArgv shell-quotes each element (the TS `liveExecArgv shell-quotes
    /// each element, because i3 re-parses through sh`).
    #[test]
    fn live_exec_argv_shell_quotes_each_arg() {
        // An arg with a space is quoted.
        let argv = live_exec_argv(&["ldmuxd", "-L", "sock with space", "attach", "sess"]);
        // The quoted arg should appear as a single element.
        let joined = argv.join(" ");
        assert!(joined.contains("'sock with space'"), "args with spaces must be quoted: {joined}");
    }

}
