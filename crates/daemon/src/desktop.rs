//! Desktop windows (ADR-0010, `docs/DESKTOP-INTEGRATION.md`).
//!
//! Port of the pure functions from `packages/daemon/src/desktop.ts` (428 lines).
//! The side-effect functions (`openMirror`, `openLive`, `closeLive`, `prepareLive`)
//! spawn a terminal emulator (alacritty) and manage the desktop window; they are
//! not yet ported (they are side-effect-heavy and hard to test headlessly).
//!
//! The pure functions (command construction) are ported 1:1 and unit-tested.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;

/// The default mirror workspace (the last one, 10).
pub const DEFAULT_MIRROR_WORKSPACE: u32 = 10;
/// The default mirror window class.
pub const DEFAULT_MIRROR_CLASS: &str = "ldmux-mirror";

/// Shells that cannot use a mouse click.
const SHELLS: &[&str] = &[
    "bash", "sh", "dash", "ash", "zsh", "fish", "ksh", "ksh93", "mksh",
    "tcsh", "csh", "yash", "nu", "nushell", "elvish", "xonsh", "pwsh",
];

/// Is this pane's foreground command an interactive shell?
pub fn is_shell_foreground(command: &str) -> bool {
    let trimmed = command.trim().to_lowercase();
    let stripped = trimmed.strip_prefix('-').unwrap_or(&trimmed);
    SHELLS.iter().any(|s| *s == stripped)
}

/// Mirror options (the TS `MirrorOptions`).
#[derive(Debug, Clone, Default)]
pub struct MirrorOptions {
    /// Read-only client (default true): mirror, never steal input.
    pub read_only: bool,
    /// i3 workspace number to place the mirror on (default 10).
    pub workspace: u32,
    /// Terminal emulator binary.
    pub terminal: Option<String>,
    /// X window class, so i3 can target/float it.
    pub window_class: Option<String>,
    /// The tmux binary to attach with.
    pub tmux_bin: Option<String>,
}

impl MirrorOptions {
    pub fn new() -> Self {
        Self {
            read_only: true,
            workspace: DEFAULT_MIRROR_WORKSPACE,
            terminal: None,
            window_class: None,
            tmux_bin: None,
        }
    }
}

/// tmux argv for a mirror client (no emulator wrapper).
pub fn mirror_tmux_args(tmux_socket: &str, tmux_name: &str, opts: &MirrorOptions) -> Vec<String> {
    let mut args = vec![
        "-L".to_string(),
        tmux_socket.to_string(),
        "attach".to_string(),
    ];
    if opts.read_only {
        args.push("-r".to_string());
    }
    args.push("-t".to_string());
    args.push(tmux_name.to_string());
    args
}

/// Quote an argument for a POSIX shell.
pub fn shell_quote(a: &str) -> String {
    // If the string is safe (no shell metacharacters), return it as-is.
    let safe: Vec<bool> = a
        .chars()
        .map(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '=' | '.' | '/' | '-'))
        .collect();
    if !a.is_empty() && safe.iter().all(|&b| b) {
        return a.to_string();
    }
    // Quote it (single-quoted, with embedded single quotes escaped).
    format!("'{}'", a.replace('\'', "'\\''"))
}

/// Full shell-less argv: `<terminal> --class <class> -e <tmux> <args...>`.
pub fn mirror_argv(tmux_socket: &str, tmux_name: &str, opts: &MirrorOptions) -> Vec<String> {
    let terminal = opts.terminal.clone().unwrap_or_else(|| "alacritty".to_string());
    let window_class = opts.window_class.clone().unwrap_or_else(|| DEFAULT_MIRROR_CLASS.to_string());
    let tmux_bin = opts.tmux_bin.clone().unwrap_or_else(|| "tmux".to_string());
    let mut args = vec![
        terminal,
        "--class".to_string(),
        window_class,
        "-e".to_string(),
        tmux_bin,
    ];
    args.extend(mirror_tmux_args(tmux_socket, tmux_name, opts));
    args
}

/// Human/machine readable command line (what the CLI prints).
pub fn mirror_command(tmux_socket: &str, tmux_name: &str, opts: &MirrorOptions) -> String {
    mirror_argv(tmux_socket, tmux_name, opts)
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
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

/// argv for a session's live window.
///
/// It runs the shim (the shim path) rather than tmux directly, because a live
/// window has to survive being detached to say who took the shell.
pub fn live_argv(shim_path: &str, session_id: &str, terminal: &str) -> Vec<String> {
    vec![
        terminal.to_string(),
        "--class".to_string(),
        live_window_class(session_id),
        "-e".to_string(),
        "/bin/sh".to_string(),
        shim_path.to_string(),
    ]
}

/// Where a session's live-window shim lives. One place, so the cleanup agrees.
pub fn live_shim_path(state_dir: &str, session_id: &str) -> String {
    format!("{state_dir}/live/live-{}.sh", live_window_class(session_id))
}

/// The live window's shim, as a shell script.
///
/// A live window cannot be a bare `tmux attach`: under exclusive ownership a
/// window is *detached* when another device takes the shell over, and a bare
/// attach would exit on that. So the window runs a loop: attach, and when the
/// attach returns, say what happened and wait. The reclaim is the `read`:
/// pressing a key re-attaches, and the daemon sees the window's client appear
/// while another device holds the shell and takes it back.
pub fn live_shim_script(tmux_bin: &str, tmux_socket: &str, tmux_name: &str) -> String {
    let t = format!("{} -L {}", shell_quote(tmux_bin), shell_quote(tmux_socket));
    let name = shell_quote(tmux_name);
    let d = "\u{24}"; // `$`
    let mut s = String::new();
    s.push_str("#!/bin/sh\n");
    s.push_str("# Generated by ldmuxd for one live window. Do not edit: it is rewritten on every\n");
    s.push_str("# make-live, and a stale copy is a window that talks to the wrong session.\n");
    s.push_str("# Record which client this window is. The daemon compares it against tmux's\n");
    s.push_str("# client list, which is how it tells this window still being here (we detached\n");
    s.push_str("# it) from this window being closed (the user is done with the shell).\n");
    s.push_str(&format!("LDMUX_LIVE_TTY=({d}tty 2>/dev/null || true)\n"));
    s.push_str(&format!("if [ -n \"{d}LDMUX_LIVE_TTY\" ]; then\n"));
    s.push_str(&format!("  {t} set-option -t {name} -q @ldmux_live_tty \"{d}LDMUX_LIVE_TTY\"\n"));
    s.push_str("fi\n");
    s.push_str("# ...and which process this window is.\n");
    s.push_str(&format!("{t} set-option -t {name} -q @ldmux_live_pid \"{d}{d}\"\n"));
    s.push_str("while :; do\n");
    s.push_str(&format!("  {t} attach -t {name}\n"));
    s.push_str(&format!("  if ! {t} has-session -t {name} 2>/dev/null; then\n"));
    s.push_str("    printf '\\n  This shell has ended.\\n\\n'\n");
    s.push_str("    break\n");
    s.push_str("  fi\n");
    s.push_str("  printf '\\n  Detached. Press any key to re-attach.\\n'\n");
    s.push_str("  read -r -n 1 _\n");
    s.push_str("done");
    s
}

/// Resolve the tmux binary: prefer `LDMUX_TMUX_BIN`, then the first `tmux`
/// on `PATH`.
///
/// The TS `resolveTmuxBin` does the same: the daemon's own tmux (set by the
/// flake) is preferred over whatever the window manager's PATH resolves to.
pub fn resolve_tmux_bin() -> String {
    if let Ok(v) = std::env::var("LDMUX_TMUX_BIN") {
        if !v.is_empty() {
            return v;
        }
    }
    // Fall back to the first `tmux` on PATH.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("tmux");
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    // Last resort: bare `tmux`.
    "tmux".to_string()
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
    if let Some(w) = n.get("window").and_then(|w| w.as_u64()) {
        if w != 0 {
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
/// Written with mode 700 and overwritten on every make-live: it embeds the tmux
/// socket and session name, so a stale copy is a window that attaches to the
/// wrong thing, and a world-writable one is a command somebody else can
/// replace.
pub async fn prepare_live(
    state_dir: &str,
    tmux_socket: &str,
    tmux_name: &str,
    session_id: &str,
    tmux_bin: &str,
) -> Result<(String, Vec<String>), String> {
    let shim_path = live_shim_path(state_dir, session_id);
    // Create the live directory.
    std::fs::create_dir_all(format!("{state_dir}/live"))
        .map_err(|e| format!("create live dir: {e}"))?;
    // Write the shim (mode 700).
    let script = live_shim_script(tmux_bin, tmux_socket, tmux_name);
    std::fs::write(&shim_path, &script)
        .map_err(|e| format!("write shim: {e}"))?;
    std::fs::set_permissions(
        &shim_path,
        Permissions::from_mode(0o700),
    )
    .map_err(|e| format!("chmod shim: {e}"))?;
    let argv = live_argv(&shim_path, session_id, "alacritty");
    Ok((shim_path, argv))
}

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

/// Launch a session's live window and put it on a chosen workspace.
///
/// The launch goes through `i3-msg exec` (which is how a window gets made at all
/// under i3) but the *placement* is `place_live`, so the user's view never moves.
pub async fn open_live(
    tmux_socket: &str,
    tmux_name: &str,
    session_id: &str,
    workspace: &str,
    tmux_bin: &str,
    state_dir: &str,
) -> Result<LiveResult, String> {
    let target = safe_workspace(workspace).ok_or("invalid workspace name")?;
    if std::env::var("DISPLAY").is_err() {
        return Err("no DISPLAY (not an X11 session)".to_string());
    }
    let (shim_path, argv) = prepare_live(state_dir, tmux_socket, tmux_name, session_id, tmux_bin)
        .await?;
    let exec_argv = live_exec_argv(&argv.iter().map(|s| s.as_str()).collect::<Vec<&str>>());
    let result = crate::exec::run(
        exec_argv.clone(),
        crate::exec::ExecOpts { timeout_ms: Some(3000), env: None },
    )
    .await;
    if result.code != 0 {
        return Err("i3-msg not available".to_string());
    }
    let placed = place_live(session_id, &target, 5000).await;
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
    fn mirror_tmux_args_read_only() {
        let opts = MirrorOptions::new();
        let args = mirror_tmux_args("sock", "session", &opts);
        assert_eq!(
            args,
            vec!["-L", "sock", "attach", "-r", "-t", "session"]
        );
    }

    #[test]
    fn mirror_tmux_args_read_write() {
        let mut opts = MirrorOptions::new();
        opts.read_only = false;
        let args = mirror_tmux_args("sock", "session", &opts);
        assert_eq!(
            args,
            vec!["-L", "sock", "attach", "-t", "session"]
        );
    }

    #[test]
    fn mirror_argv_full_command() {
        let opts = MirrorOptions::new();
        let args = mirror_argv("sock", "session", &opts);
        assert_eq!(
            args,
            vec![
                "alacritty",
                "--class",
                "ldmux-mirror",
                "-e",
                "tmux",
                "-L",
                "sock",
                "attach",
                "-r",
                "-t",
                "session",
            ]
        );
    }

    #[test]
    fn mirror_command_quoted() {
        let opts = MirrorOptions::new();
        let cmd = mirror_command("sock", "session", &opts);
        assert!(cmd.contains("alacritty"));
        assert!(cmd.contains("ldmux-mirror"));
        assert!(cmd.contains("attach"));
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
    fn live_argv_constructs_the_shim_command() {
        let args = live_argv("/tmp/shim.sh", "abc123", "alacritty");
        assert_eq!(
            args,
            vec![
                "alacritty",
                "--class",
                "ldmux-live-abc123",
                "-e",
                "/bin/sh",
                "/tmp/shim.sh",
            ]
        );
    }

    #[test]
    fn live_shim_path_works() {
        let path = live_shim_path("/tmp/state", "abc123");
        assert_eq!(path, "/tmp/state/live/live-ldmux-live-abc123.sh");
    }

    #[test]
    fn live_shim_script_is_a_valid_shell_script() {
        let script = live_shim_script("tmux", "sock1", "sess1");
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("@ldmux_live_tty"));
        assert!(script.contains("@ldmux_live_pid"));
        assert!(script.contains("while :; do"));
        assert!(script.contains("attach -t"));
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
        let argv = live_exec_argv(&["tmux", "-L", "sock", "attach", "sess"]);
        assert_eq!(argv[0], "i3-msg");
        assert_eq!(argv[1], "exec");
        assert_eq!(argv[2], "--");
        assert_eq!(argv[3], "tmux");
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

    /// The shim records its tty before attaching (the TS `the shim records its
    /// tty before attaching`).
    #[test]
    fn the_shim_records_its_tty_before_attaching() {
        let script = live_shim_script("tmux", "sock", "mysession");
        // The TTY is recorded before the attach loop.
        assert!(script.contains("LDMUX_LIVE_TTY"), "shim must record the tty");
        // The TTY is set on the session before the loop.
        assert!(script.contains("@ldmux_live_tty"), "shim must set the tty option");
        // The tty line comes before the while loop.
        let tty_pos = script.find("LDMUX_LIVE_TTY").unwrap();
        let loop_pos = script.find("while :; do").unwrap();
        assert!(tty_pos < loop_pos, "tty recording must come before the attach loop");
    }

    /// The shim loops, so being detached does not close the window (the TS
    /// `the shim loops, so being detached does not close the window`).
    #[test]
    fn the_shim_loops_so_detached_does_not_close() {
        let script = live_shim_script("tmux", "sock", "mysession");
        // The loop is there.
        assert!(script.contains("while :; do"), "shim must have a loop");
        // The detach message is in the loop.
        assert!(script.contains("Detached"), "shim must report the detach");
        // The re-attach wait is in the loop.
        assert!(script.contains("read -r -n 1"), "shim must wait for a key");
    }

    /// The shim quotes what it interpolates (the TS `the shim quotes what it
    /// interpolates`). A safe name is not quoted; a name with a space is.
    #[test]
    fn the_shim_quotes_its_interpolations() {
        // A safe name is not quoted (shell_quote returns it as-is).
        let script = live_shim_script("tmux", "sock", "mysession");
        assert!(script.contains("mysession"), "safe name must appear unquoted");
        // A name with a space is quoted.
        let script2 = live_shim_script("tmux", "sock", "my session");
        assert!(script2.contains("'my session'"), "name with a space must be quoted");
        // A tmux bin with a space is quoted.
        let script3 = live_shim_script("/usr/bin/tmux 1.0", "sock", "mysession");
        assert!(script3.contains("'/usr/bin/tmux 1.0'"), "tmux bin with a space must be quoted");
    }

    /// liveExecArgv shell-quotes each element (the TS `liveExecArgv shell-quotes
    /// each element, because i3 re-parses through sh`).
    #[test]
    fn live_exec_argv_shell_quotes_each_arg() {
        // An arg with a space is quoted.
        let argv = live_exec_argv(&["tmux", "-L", "sock with space", "attach", "sess"]);
        // The quoted arg should appear as a single element.
        let joined = argv.join(" ");
        assert!(joined.contains("'sock with space'"), "args with spaces must be quoted: {joined}");
    }

    /// Resolve the tmux binary: prefer LDMUX_TMUX_BIN, then PATH order (the TS
    /// `resolveTmuxBin prefers LDMUX_TMUX_BIN, then PATH order, keeping an
    /// absolute path`).
    #[test]
    fn resolve_tmux_bin_prefers_env_then_path() {
        // Set LDMUX_TMUX_BIN to a known path.
        unsafe { std::env::set_var("LDMUX_TMUX_BIN", "/usr/bin/tmux-custom"); }
        let resolved = resolve_tmux_bin();
        assert_eq!(resolved, "/usr/bin/tmux-custom", "LDMUX_TMUX_BIN must win");
        // Clear the env var.
        unsafe { std::env::remove_var("LDMUX_TMUX_BIN"); }
    }
}
