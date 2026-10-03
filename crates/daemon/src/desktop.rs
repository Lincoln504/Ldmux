//! Desktop windows (ADR-0010, `docs/DESKTOP-INTEGRATION.md`).
//!
//! Port of the pure functions from `packages/daemon/src/desktop.ts` (428 lines).
//! The side-effect functions (`openMirror`, `openLive`, `closeLive`, `prepareLive`)
//! spawn a terminal emulator (alacritty) and manage the desktop window; they are
//! not yet ported (they are side-effect-heavy and hard to test headlessly).
//!
//! The pure functions (command construction) are ported 1:1 and unit-tested.

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
}
