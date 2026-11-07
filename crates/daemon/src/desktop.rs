//! Desktop windows (ADR-0010, `docs/ARCHITECTURE.md#6-ownership`).
//!
//! Port of the pure functions from `packages/daemon/src/desktop.ts`.
//!
//! **Opening a window needs no window manager.** The daemon spawns a terminal emulator
//! directly (detached, own process group, own systemd scope, absolute path, the session
//! environment). Placing it on a workspace is an optional adapter (i3, sway, hyprland)
//! that degrades to "opened, not placed".
//!
//! **The terminal is the user's own, found by convention, not by a ldmux knob.** The
//! order is `$TERMINAL`, then `xdg-terminal-exec` when the desktop provides it (it reads
//! the user's terminal configuration, so it *is* the configured default), then this
//! desktop's configured terminal (GNOME's gsettings, KDE's `kdeglobals`, XFCE's xfconf
//! and `helpers.rc`), then an emulator already running in this session (ten alacritty
//! windows open means alacritty is the terminal here, whatever a config file says), then
//! the known emulators on `PATH`. An emulator that needs unusual flags is a wrapper
//! script and `$TERMINAL` points at it.
//!
//! **There is no terminal-multiplexer mirror.** A read-only view is `ldmux attach <id> --readonly`
//! — the same protocol client as a desk window, told not to take a claim — run in the terminal
//! the reader already has.

use std::collections::HashMap;
use std::sync::Mutex;

/// Shells that cannot use a mouse click.
const SHELLS: &[&str] = &[
    "bash", "sh", "dash", "ash", "zsh", "fish", "ksh", "ksh93", "mksh", "tcsh", "csh", "yash",
    "nu", "nushell", "elvish", "xonsh", "pwsh",
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
    window_class("ldmux-live-", session_id)
}

fn window_class(prefix: &str, session_id: &str) -> String {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    format!("{prefix}{safe}")
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

// ---------------------------------------------------------------------------
// The terminal emulator
// ---------------------------------------------------------------------------

/// How one emulator is told to run a command under a window class.
///
/// `{class}` becomes the window class (X11 class / Wayland app-id) the placement
/// adapter finds the window by; `{cmd}` becomes the attach command as separate
/// arguments. Each template also keeps the emulator in the foreground (konsole
/// `--nofork`, gnome-terminal `--wait`, xfce4-terminal `--disable-server`,
/// wezterm `--always-new-process`): the daemon tells "the window is open" by "the
/// process is running", and an emulator that hands the window to a server and
/// exits would read as a closed window.
struct Spec {
    name: &'static str,
    args: &'static [&'static str],
    /// The emulator can set the window class, so a window manager can find it.
    class: bool,
    /// The process it is launched as lives as long as the window does.
    foreground: bool,
}

const SPECS: &[Spec] = &[
    Spec {
        name: "alacritty",
        args: &["--class", "{class}", "-e", "{cmd}"],
        class: true,
        foreground: true,
    },
    Spec {
        name: "kitty",
        args: &["--class", "{class}", "{cmd}"],
        class: true,
        foreground: true,
    },
    Spec {
        name: "foot",
        args: &["--app-id", "{class}", "{cmd}"],
        class: true,
        foreground: true,
    },
    Spec {
        name: "wezterm",
        args: &[
            "start",
            "--always-new-process",
            "--class",
            "{class}",
            "--",
            "{cmd}",
        ],
        class: true,
        foreground: true,
    },
    Spec {
        name: "gnome-terminal",
        args: &["--class={class}", "--wait", "--", "{cmd}"],
        class: true,
        foreground: true,
    },
    Spec {
        name: "konsole",
        args: &["--nofork", "-e", "{cmd}"],
        class: false,
        foreground: true,
    },
    Spec {
        name: "xfce4-terminal",
        args: &["--disable-server", "-x", "{cmd}"],
        class: false,
        foreground: true,
    },
    Spec {
        name: "xterm",
        args: &["-class", "{class}", "-e", "{cmd}"],
        class: true,
        foreground: true,
    },
    // Wrappers: whatever they run is not ours to know, so they are neither
    // class-addressable nor trusted to stay in the foreground.
    Spec {
        name: "x-terminal-emulator",
        args: &["-e", "{cmd}"],
        class: false,
        foreground: false,
    },
    Spec {
        name: "xdg-terminal-exec",
        args: &["{cmd}"],
        class: false,
        foreground: false,
    },
];

/// What an emulator this table does not know is run as: `-e` is the one spelling
/// nearly all of them share.
const GENERIC_ARGS: &[&str] = &["-e", "{cmd}"];

/// The last resort: the known emulators, real ones first.
///
/// `x-terminal-emulator` is **last** and not first: on Debian it is an
/// `update-alternatives` symlink (it resolves to `xfce4-terminal.wrapper` on this
/// machine), it cannot name its window class, and it hides which program actually
/// opens. `xdg-terminal-exec` is not here at all — it is tried before any of these,
/// because when it is installed it *is* the configured terminal.
const AUTODETECT: &[&str] = &[
    "alacritty",
    "kitty",
    "foot",
    "wezterm",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
];

/// The XDG terminal convention's launcher. When a desktop ships it, it reads the
/// user's own configuration and every application that opens a terminal goes
/// through it, so it is the configured default by definition.
const XDG_TERMINAL_EXEC: &str = "xdg-terminal-exec";

/// How long a configuration probe may take. A user manager that cannot answer in
/// this has no answer, and opening a window must not wait for it.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

fn spec_for(program: &str) -> Option<&'static Spec> {
    let base = std::path::Path::new(program).file_name()?.to_str()?;
    SPECS.iter().find(|s| s.name == base)
}

fn executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The absolute path of `name`: itself when it is already an absolute
/// executable, else the first match on `path`.
fn which_in(name: &str, path: &str) -> Option<String> {
    if name.starts_with('/') {
        return executable(std::path::Path::new(name)).then(|| name.to_string());
    }
    if name.is_empty() || name.contains('/') {
        return None;
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(d).join(name))
        .find(|p| executable(p))
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}

fn which(name: &str) -> Option<String> {
    which_in(name, &std::env::var("PATH").unwrap_or_default())
}

/// The first word of a command line: `$TERMINAL` may carry flags, and only the
/// program is used (`TERMINAL="alacritty --some-flag"` names alacritty).
fn first_word(s: &str) -> Option<&str> {
    s.split_whitespace().next()
}

/// Where the answer came from, for `ldmux terminal --explain`.
///
/// A resolution nobody can interrogate is a resolution nobody can trust: "the window opened in
/// the wrong terminal" has no next question without this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `ldmux terminal set <program>` — this user's explicit choice.
    Pinned,
    /// `$TERMINAL`.
    Environment,
    /// `xdg-terminal-exec`, which reads the XDG terminal configuration.
    XdgExec,
    /// The desktop session's own configured terminal (GNOME, KDE, XFCE, MATE), for the
    /// desktop this session is actually on.
    Desktop,
    /// An emulator already running for this user.
    Running,
    /// The distribution's `x-terminal-emulator` alternative, resolved to the real program.
    Alternative,
    /// The built-in list of known emulators.
    Known,
    /// Nothing resolved.
    None,
}

impl Source {
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Pinned => "set with `ldmux terminal set`",
            Self::Environment => "$TERMINAL",
            Self::XdgExec => "xdg-terminal-exec (the XDG terminal configuration)",
            Self::Desktop => "this desktop session's configured terminal",
            Self::Running => "an emulator already running for this user",
            Self::Alternative => "the system's x-terminal-emulator alternative",
            Self::Known => "the built-in list of known emulators",
            Self::None => "nothing: no terminal found",
        }
    }
}

/// The terminal a window opens in, and where that answer came from.
#[must_use]
pub fn terminal_with_source() -> (String, Source) {
    let path = std::env::var("PATH").unwrap_or_default();
    let pinned = pinned_terminal(&path);
    let term = std::env::var("TERMINAL").ok();
    let xdg = which_in(XDG_TERMINAL_EXEC, &path);
    // The session's desktop is asked only when the cheaper, more explicit answers are absent,
    // and each probe is resolved once per process: `window_state` asks this question on every
    // poll. The pin, `$TERMINAL` and the launcher stay live (a file read and two PATH
    // lookups), so an explicit choice applies without a restart.
    let desktop = if pinned.is_none() && xdg.is_none() {
        cached_desktop()
    } else {
        None
    };
    let running = if pinned.is_none() && xdg.is_none() && desktop.is_none() {
        cached_running()
    } else {
        Vec::new()
    };
    resolve_terminal_full(pinned, term.as_deref(), xdg, desktop, &running, &path)
}

/// The terminal a window opens in: an absolute path, or "" when nothing was found.
///
/// The order is the one in the module comment. Every step is skipped silently when it does not
/// resolve: a stale `$TERMINAL`, a desktop with no configured terminal, and a probe whose tool
/// is not installed are all ordinary, and none of them is a reason to refuse to open a window in
/// something that does exist.
#[must_use]
pub fn terminal() -> String {
    terminal_with_source().0
}

/// Where the user's explicit choice lives: `$XDG_CONFIG_HOME/ldmux/terminal`.
///
/// A file rather than a variable because nobody discovers a variable: the daemon unit runs
/// with the user manager's environment, so `$TERMINAL` exported in a shell never reaches the
/// process that opens the window. One line, the program and nothing else.
#[must_use]
pub fn terminal_config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty() && v.starts_with('/'))
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|h| !h.is_empty())
                .map(|h| format!("{h}/.config"))
        })?;
    Some(std::path::Path::new(&base).join("ldmux/terminal"))
}

/// Read the pinned program, if there is one.
fn pinned_terminal_raw() -> Option<String> {
    let path = terminal_config_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    Some(line.to_string())
}

/// The pinned program resolved against `path`.
fn pinned_terminal(path: &str) -> Option<String> {
    let raw = pinned_terminal_raw()?;
    Some(resolve_alias(&raw, path)?.0)
}

/// Pin the terminal a window opens in, or clear the pin with `None`.
///
/// The program must resolve now: a pin that names something that is not installed would open
/// nothing, later, with no obvious cause. Returns the file written.
pub fn set_terminal(program: Option<&str>) -> Result<std::path::PathBuf, String> {
    let path = terminal_config_path().ok_or("no HOME or XDG_CONFIG_HOME to keep the setting in")?;
    match program {
        None => {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("could not remove {}: {e}", path.display())),
            }
            Ok(path)
        }
        Some(program) => {
            let program = program.trim();
            if program.is_empty() {
                return Err("which terminal? name a program, for example `alacritty`".to_string());
            }
            let path_var = std::env::var("PATH").unwrap_or_default();
            let resolved = resolve_alias(program, &path_var).ok_or_else(|| {
                format!("{program:?} is not a program on PATH or an absolute path to one")
            })?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            }
            // Trailing newline, so `cat` and an editor both leave it alone.
            std::fs::write(&path, format!("{}\n", resolved.0))
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
            Ok(path)
        }
    }
}

/// The rule behind [`terminal_with_source`], pure so every step is testable without mutating
/// the process environment (which tests share) or owning a desktop.
fn resolve_terminal_full(
    pinned: Option<String>,
    term: Option<&str>,
    xdg_terminal_exec: Option<String>,
    desktop: Option<String>,
    running: &[String],
    path: &str,
) -> (String, Source) {
    // 1. This user said so, in the one place the daemon can read.
    if let Some(t) = pinned.filter(|t| !t.is_empty()) {
        return (t, Source::Pinned);
    }
    // 2. The standard variable every other tool already sets.
    if let Some(t) = term
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| resolve_alias(v, path))
    {
        return (t.0, Source::Environment);
    }
    // 3. The XDG launcher. When it is installed it *is* the configured default: it reads the
    //    user's own terminal configuration and every desktop application goes through it.
    if let Some(x) = xdg_terminal_exec {
        return (x, Source::XdgExec);
    }
    // 4. This desktop session's configured terminal — the session that is actually running,
    //    which is the bug this order fixes: a GNOME schema read on an XFCE session answered
    //    `x-terminal-emulator`, and that answer opened xfce4-terminal over ten alacritty
    //    windows.
    if let Some(t) = desktop.and_then(|d| resolve_alias(&d, path)) {
        return (t.0, Source::Desktop);
    }
    // 5. An emulator the user is running right now, most frequent first. Ten windows of one
    //    emulator is a stronger statement of intent than any settings key, and it is what makes
    //    a bare window manager (/i3 with no configured default) resolve to reality.
    if let Some((name, abs)) = running
        .iter()
        .find_map(|n| which_in(n, path).map(|abs| (n.clone(), abs)))
    {
        let _ = name;
        return (abs, Source::Running);
    }
    // 6. The distribution's alternative, resolved to the program it actually runs: the
    //    alternative is a system default, and on Debian it is a Perl wrapper around one.
    if let Some(t) = resolve_alias(ALTERNATIVE, path) {
        return (t.0, Source::Alternative);
    }
    // 7. The known names.
    match AUTODETECT.iter().find_map(|n| which_in(n, path)) {
        Some(t) => (t, Source::Known),
        None => (String::new(), Source::None),
    }
}

/// `x-terminal-emulator`: Debian's `update-alternatives` name for the system default terminal.
const ALTERNATIVE: &str = "x-terminal-emulator";

/// Resolve an indirection to the program that actually opens: `(absolute path, kind)`.
///
/// Two indirections exist in practice and both have to be followed, or the window opens in a
/// program nobody named:
///
/// - `x-terminal-emulator` is a symlink to `<program>.wrapper`, a packaging shim (perl on
///   Debian) that execs the real emulator;
/// - an emulator whose name ends `.wrapper` (the shim itself) is not in `SPECS`, so its
///   template would fall back to `-e` and lose the class and `--wait`.
///
/// `None` when the value names nothing runnable: a probe that answered with prose, a
/// `.desktop` id, or a program that is not installed is not an answer.
fn resolve_alias(value: &str, path: &str) -> Option<(String, &'static str)> {
    let first = first_word(value)?.trim_matches('\'');
    if first.is_empty() || first.ends_with(".desktop") {
        return None;
    }
    let abs = which_in(first, path)?;
    // Follow symlinks to the packaging shim and strip the suffix to the real program.
    let real = std::fs::canonicalize(&abs).unwrap_or_else(|_| std::path::PathBuf::from(&abs));
    let base = real.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let program = base.strip_suffix(".wrapper").unwrap_or(base);
    // The shim is a script: the program it runs is the name it was installed as. Look that up
    // in PATH, and fall back to the shim itself so a renamed packaging shim still opens.
    if let Some(found) = which_in(program, path) {
        return Some((found, program_kind(program)));
    }
    Some((abs, "wrapper"))
}

/// Which template applies: a known emulator's name, `wrapper`, or `unknown`.
fn program_kind(program: &str) -> &'static str {
    if SPECS.iter().any(|s| s.name == program) {
        "known"
    } else {
        "unknown"
    }
}

/// The desktop session's configured terminal, read from **that desktop's** configuration.
///
/// Reading another desktop's key is how this machine answered `x-terminal-emulator`: a GNOME
/// schema (`org.gnome.desktop.default-applications.terminal`) consulted on an XFCE session,
/// whose value is the Debian alternative rather than a user's choice. Each probe is now behind
/// the session it belongs to.
fn desktop_configured_terminal(
    desktop: &str,
    run: &dyn Fn(&str, &[&str]) -> Option<String>,
    helpers_rc: Option<&str>,
) -> Option<String> {
    let is = |names: &[&str]| names.iter().any(|n| desktop.eq_ignore_ascii_case(n));
    if is(&["gnome", "unity", "budgie", "pantheon"]) {
        return run(
            "gsettings",
            &["get", "org.gnome.desktop.default-applications.terminal", "exec"],
        )
        .and_then(|out| parse_quoted_probe(&out));
    }
    if is(&["cinnamon"]) {
        return run(
            "gsettings",
            &["get", "org.cinnamon.desktop.default-applications.terminal", "exec"],
        )
        .and_then(|out| parse_quoted_probe(&out));
    }
    if is(&["mate"]) {
        return run(
            "gsettings",
            &["get", "org.mate.desktop.default-applications.terminal", "exec"],
        )
        .and_then(|out| parse_quoted_probe(&out));
    }
    if is(&["kde", "plasma"]) {
        for tool in ["kreadconfig6", "kreadconfig5"] {
            if let Some(v) = run(
                tool,
                &["--file", "kdeglobals", "--group", "General", "--key", "TerminalApplication"],
            )
            .and_then(|out| parse_word_probe(&out))
            {
                return Some(v);
            }
        }
        return None;
    }
    if is(&["xfce", "x-cinnamon"]) {
        if let Some(v) = run(
            "xfconf-query",
            &["-c", "xfce4-session", "-p", "/general/TerminalEmulator"],
        )
        .and_then(|out| parse_word_probe(&out))
        {
            return Some(v);
        }
        return helpers_rc.and_then(parse_helpers_rc);
    }
    // A bare window manager (i3, sway, hyprland) and any desktop this build does not know:
    // there is no session-wide setting to read, and the running-emulator step answers instead.
    None
}

/// Run one configuration probe, bounded by [`PROBE_TIMEOUT`], with no output on
/// failure: a missing tool, a desktop that does not answer, and a tool that hangs
/// are all "this machine has no answer here".
fn run_probe(program: &str, args: &[&str]) -> Option<String> {
    let exe = which(program)?;
    let mut child = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_string(&mut out);
                }
                return Some(out);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                // A probe that will not answer is killed, not waited for.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// [`desktop_configured_terminal`] for this session, resolved once: see
/// [`terminal_with_source`] for why.
fn cached_desktop() -> Option<String> {
    static CONFIGURED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CONFIGURED.get_or_init(session_configured_terminal).clone()
}

/// [`running_emulators`], resolved once: see [`terminal_with_source`] for why.
fn cached_running() -> Vec<String> {
    static RUNNING: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    RUNNING.get_or_init(running_emulators).clone()
}

/// The configured terminal of the desktop **this session is on**.
///
/// `XDG_CURRENT_DESKTOP` is a colon-separated list (`XFCE:GNOME`), and the first entry is the
/// session's own. An empty or unknown value means a bare window manager, where no desktop key
/// applies — and that is a real answer, not a reason to read someone else's schema.
fn session_configured_terminal() -> Option<String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = desktop.split(':').next().unwrap_or("").trim();
    if session.is_empty() {
        return None;
    }
    let helpers = std::env::var("HOME").ok().filter(|h| !h.is_empty()).and_then(|h| {
        std::fs::read_to_string(format!("{h}/.config/xfce4/helpers.rc")).ok()
    });
    desktop_configured_terminal(session, &|program, args| run_probe(program, args), helpers.as_deref())
}

/// `gsettings get` prints a quoted value (`'alacritty'`); an empty setting prints
/// `''`. Anything else — an error message, a missing schema — is not an answer.
fn parse_quoted_probe(out: &str) -> Option<String> {
    let t = out.trim();
    let inner = t.strip_prefix('\'')?.strip_suffix('\'')?;
    let inner = inner.trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

/// A probe that prints one word (`alacritty`, `/usr/bin/kitty`) on success and an
/// error sentence on failure: only a single token is taken as an answer.
fn parse_word_probe(out: &str) -> Option<String> {
    let t = out.trim();
    if t.is_empty() || t.contains(' ') && t.contains("Property") {
        return None;
    }
    let first = t.split_whitespace().next()?;
    // xfconf-query answers errors in prose too; a path or a program name never
    // contains ':' at the start or ends in a full stop.
    if first.ends_with('.') || first.starts_with(':') {
        return None;
    }
    Some(first.to_string())
}

/// XFCE's own helpers file: one `Key=Value` per line, `TerminalEmulator=` being the
/// configured terminal.
fn parse_helpers_rc(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(value) = line.strip_prefix("TerminalEmulator=") else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Emulator process names running for this uid, most frequent first.
///
/// The heuristic that makes a bare window manager (/i3, no desktop session, no
/// configured default) open the terminal the user is actually using: ten alacritty
/// windows open is a stronger statement of intent than any config file.
fn running_emulators() -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = nix::unistd::getuid().as_raw();
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Another user's emulator is not this user's terminal.
        if std::fs::metadata(entry.path()).is_ok_and(|m| m.uid() != uid) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim();
        if AUTODETECT.contains(&comm) {
            *counts.entry(comm.to_string()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    // Most frequent first; ties broken by name so the answer is stable.
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().map(|(n, _)| n).collect()
}

/// Why `terminal` cannot be launched, or None when it looks runnable.
///
/// A spawn that fails is reported, but one that succeeds and dies in the same
/// instant (a `$TERMINAL` naming something that is not a terminal) is a window
/// that never appears with nothing said — the failure this project has paid for
/// twice. A PATH lookup up front names the cause.
pub fn terminal_problem(terminal: &str) -> Option<String> {
    let t = terminal.trim();
    if t.is_empty() {
        return Some(format!(
            "no terminal emulator found (looked for {}, and at this user's pin, $TERMINAL, xdg-terminal-exec and this desktop session's configured terminal); install one, or name the program you use with `ldmux terminal set <program>`",
            AUTODETECT.join(", ")
        ));
    }
    if t.starts_with('/') {
        return if executable(std::path::Path::new(t)) {
            None
        } else {
            Some(format!("no such terminal: {t}"))
        };
    }
    if t.contains('/') {
        return Some(format!(
            "the terminal must be a name on PATH or an absolute path: {t}"
        ));
    }
    if which(t).is_some() {
        None
    } else {
        Some(format!("no such terminal on PATH: {t}"))
    }
}

/// Expand a template: `{cmd}` as a whole word becomes the attach command's
/// arguments, and `{class}` is substituted anywhere. No shell is involved, so no
/// quoting is needed; every template in [`SPECS`] keeps `{cmd}` as its own word.
fn expand(template: &[String], class: &str, attach: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < template.len() {
        let w = &template[i];
        if w == "{cmd}" {
            out.extend(attach.iter().cloned());
            i += 1;
            continue;
        }
        // **A window with no name gets no class flag at all.** `ldmux terminal --exec` runs the
        // operator's own terminal and has no window id to name, so the class is empty; passing
        // `--class ""` set an empty class, which the window manager cannot match and which showed up
        // in `ps` as `alacritty --class  -e …` (measured 2026-11-06). A template names the flag and
        // its value as two elements (`--class`, `{class}`), so both go together; one that wrote the
        // value inline (`--class={class}`) is dropped as a single element.
        if class.is_empty() {
            if template.get(i + 1).is_some_and(|next| next.contains("{class}")) {
                i += 2;
                continue;
            }
            if w.contains("{class}") {
                i += 1;
                continue;
            }
        }
        out.push(w.replace("{class}", class));
        i += 1;
    }
    out
}

/// The argv that runs `command` inside `program`, with the emulator's own template applied.
///
/// **The one place that knows how each emulator is told to run something**, exposed so a
/// caller can exec it itself instead of asking the daemon to open a window: the keybinding
/// wrapper does exactly that, and it used to carry a second, drifting copy of this knowledge
/// in shell. No shell is involved; every element reaches `execve` as written here.
#[must_use]
pub fn exec_argv(program: &str, class: &str, command: &[String]) -> Vec<String> {
    build_argv(program, class, command)
}

/// The whole argv for one window: the program, then its template expanded. No
/// shell is involved: each element reaches `execve` as it is written here.
fn build_argv(program: &str, class: &str, attach: &[String]) -> Vec<String> {
    let template: Vec<String> = spec_for(program)
        .map(|s| s.args)
        .unwrap_or(GENERIC_ARGS)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut argv = vec![program.to_string()];
    argv.extend(expand(&template, class, attach));
    argv
}

/// Can a window manager find this terminal's windows by class?
fn terminal_has_class(program: &str) -> bool {
    spec_for(program).map(|s| s.class).unwrap_or(false)
}

fn terminal_is_foreground(program: &str) -> bool {
    spec_for(program).map(|s| s.foreground).unwrap_or(true)
}

/// The desk window's argv: the terminal running the attach client (plan §3.9).
///
/// The window is a protocol client of the daemon (`ldmuxd attach <id>`): it
/// claims the shell (ADR-0010), streams the pane, and handles the handover. The
/// attach binary is absolute (see [attach_bin]) so the window does not depend on
/// the launching environment's `PATH`.
pub fn live_argv(session_id: &str, terminal: &str) -> Vec<String> {
    let attach = vec![attach_bin(), "attach".to_string(), session_id.to_string()];
    build_argv(terminal, &live_window_class(session_id), &attach)
}

pub fn attach_bin() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().and_then(|n| n.to_str()) == Some("ldmuxd")
            && let Some(s) = exe.to_str()
        {
            return s.to_string();
        }
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("ldmuxd");
            if candidate.exists()
                && let Some(s) = candidate.to_str()
            {
                return s.to_string();
            }
        }
        if let Some(s) = exe.to_str()
            && s.starts_with('/')
        {
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
        && w != 0
    {
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

/// The result of a live-window operation.
pub struct LiveResult {
    pub ok: bool,
    pub placed: Option<bool>,
    pub window_id: Option<String>,
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// The session environment
// ---------------------------------------------------------------------------

type Env = Vec<(String, String)>;

/// The variables that say which desktop session a window belongs to. A daemon
/// started by the user manager before login has none of them; the user manager
/// learns them from `systemctl --user import-environment`.
const SESSION_VARS: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "SWAYSOCK",
    "I3SOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "NIRI_SOCKET",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
];

fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// `own` plus, for each session variable it lacks, the value in the user
/// manager's `show-environment` output. Pure. A value systemd shows in `$'…'`
/// form (it contains something unprintable) is skipped rather than guessed at.
fn merge_session_env(own: &[(String, String)], manager: &str) -> Env {
    let mut out: Env = own.to_vec();
    for line in manager.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if !SESSION_VARS.contains(&k) || v.starts_with("$'") || v.is_empty() {
            continue;
        }
        if env_get(&out, k).is_none() {
            out.retain(|(ek, _)| ek != k);
            out.push((k.to_string(), v.to_string()));
        }
    }
    out
}

/// The environment a window is launched in and the window-manager tools are run
/// in: the daemon's own, completed from the user manager when the daemon has no
/// display of its own (it was started before the session was). Cached briefly,
/// because the maintenance pass asks every couple of seconds.
pub(crate) async fn session_env() -> Env {
    // `std::env::vars()` panics on a name or value that is not valid Unicode, and
    // this builds every window's environment: one such variable would have made
    // every desk window unlaunchable. A pair with no UTF-8 representation is
    // dropped (said once), which is the same rule the session spawn uses.
    let own: Env = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    if env_get(&own, "DISPLAY").is_some() || env_get(&own, "WAYLAND_DISPLAY").is_some() {
        return own;
    }
    static CACHE: std::sync::OnceLock<Mutex<Option<(std::time::Instant, String)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let fresh = {
        let g = cache.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some((at, text)) if at.elapsed() < std::time::Duration::from_secs(10) => {
                Some(text.clone())
            }
            _ => None,
        }
    };
    let text = match fresh {
        Some(t) => t,
        None => {
            let r = crate::exec::run(
                vec![
                    "systemctl".into(),
                    "--user".into(),
                    "show-environment".into(),
                ],
                crate::exec::ExecOpts {
                    timeout_ms: Some(2000),
                    env: None,
                },
            )
            .await;
            let t = if r.code == 0 { r.out } else { String::new() };
            *cache.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((std::time::Instant::now(), t.clone()));
            t
        }
    };
    merge_session_env(&own, &text)
}

const NO_DISPLAY: &str = "no graphical session: neither DISPLAY nor WAYLAND_DISPLAY is set for the daemon \
(start it from the session, or run `systemctl --user import-environment DISPLAY WAYLAND_DISPLAY XAUTHORITY` \
in the session's startup so the user manager passes them on)";

// ---------------------------------------------------------------------------
// Placement adapters (optional, best effort)
// ---------------------------------------------------------------------------

/// A window manager ldmux can ask where windows are and move them. None of this
/// is needed to open a window: without an adapter a window is "opened, not
/// placed". Niri has no class-addressed move, so it has no adapter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wm {
    I3,
    Sway,
    Hyprland,
}

/// Which adapter applies to `env`, if any. `have` says whether a tool is
/// installed. Sway, Hyprland and i3 are told apart by their own sockets; with
/// only a DISPLAY, an installed `i3-msg` is tried (it finds i3 through the X
/// root window, and answers non-zero under any other WM).
pub fn detect_wm(env: &[(String, String)], have: &dyn Fn(&str) -> bool) -> Option<Wm> {
    if env_get(env, "SWAYSOCK").is_some() && have("swaymsg") {
        Some(Wm::Sway)
    } else if env_get(env, "HYPRLAND_INSTANCE_SIGNATURE").is_some() && have("hyprctl") {
        Some(Wm::Hyprland)
    } else if (env_get(env, "I3SOCK").is_some() || env_get(env, "DISPLAY").is_some())
        && have("i3-msg")
    {
        Some(Wm::I3)
    } else {
        None
    }
}

async fn context() -> (Env, Option<Wm>) {
    let env = session_env().await;
    let path = env_get(&env, "PATH").unwrap_or("").to_string();
    let wm = detect_wm(&env, &|t| which_in(t, &path).is_some());
    (env, wm)
}

/// One window a manager reports: its class (or app-id) and the criterion that
/// addresses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win {
    pub class: String,
    pub handle: String,
}

fn wm_tool(wm: Wm) -> &'static str {
    match wm {
        Wm::I3 => "i3-msg",
        Wm::Sway => "swaymsg",
        Wm::Hyprland => "hyprctl",
    }
}

fn list_argv(wm: Wm) -> Vec<String> {
    let v: &[&str] = match wm {
        Wm::I3 => &["i3-msg", "-t", "get_tree"],
        Wm::Sway => &["swaymsg", "-t", "get_tree"],
        Wm::Hyprland => &["hyprctl", "clients", "-j"],
    };
    v.iter().map(|s| s.to_string()).collect()
}

fn move_argv(wm: Wm, win: &Win, workspace: &str) -> Vec<String> {
    match wm {
        Wm::I3 | Wm::Sway => vec![
            wm_tool(wm).to_string(),
            format!("[{}] move to workspace number {workspace}", win.handle),
        ],
        Wm::Hyprland => vec![
            "hyprctl".into(),
            "dispatch".into(),
            "movetoworkspacesilent".into(),
            format!("{workspace},{}", win.handle),
        ],
    }
}

fn kill_argvs(wm: Wm, class: &str) -> Vec<Vec<String>> {
    match wm {
        Wm::I3 => vec![vec!["i3-msg".into(), format!("[class=\"{class}\"] kill")]],
        Wm::Sway => vec![
            vec!["swaymsg".into(), format!("[app_id=\"{class}\"] kill")],
            vec!["swaymsg".into(), format!("[class=\"{class}\"] kill")],
        ],
        Wm::Hyprland => vec![vec![
            "hyprctl".into(),
            "dispatch".into(),
            "closewindow".into(),
            format!("class:^({class})$"),
        ]],
    }
}

/// The windows in an i3/sway tree or a hyprland client list.
pub fn parse_windows(wm: Wm, json: &str) -> Vec<Win> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match wm {
        Wm::Hyprland => {
            for c in root.as_array().into_iter().flatten() {
                let class = c.get("class").and_then(|x| x.as_str()).unwrap_or("");
                let addr = c.get("address").and_then(|x| x.as_str()).unwrap_or("");
                if !class.is_empty() && !addr.is_empty() {
                    out.push(Win {
                        class: class.to_string(),
                        handle: format!("address:{addr}"),
                    });
                }
            }
        }
        Wm::I3 | Wm::Sway => collect_windows(wm, &root, &mut out),
    }
    out
}

fn collect_windows(wm: Wm, node: &serde_json::Value, out: &mut Vec<Win>) {
    let Some(n) = node.as_object() else { return };
    let class = n
        .get("app_id")
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            n.get("window_properties")
                .and_then(|p| p.get("class"))
                .and_then(|c| c.as_str())
        })
        .unwrap_or("");
    if !class.is_empty() {
        let window = n.get("window").and_then(|w| w.as_u64()).filter(|w| *w != 0);
        let con = n.get("id").and_then(|w| w.as_u64());
        let handle = match (wm, window, con) {
            (Wm::Sway, _, Some(c)) => Some(format!("con_id={c}")),
            (_, Some(w), _) => Some(format!("id=\"0x{w:x}\"")),
            _ => None,
        };
        if let Some(handle) = handle {
            out.push(Win {
                class: class.to_string(),
                handle,
            });
        }
    }
    for key in ["nodes", "floating_nodes"] {
        for c in n.get(key).and_then(|x| x.as_array()).into_iter().flatten() {
            collect_windows(wm, c, out);
        }
    }
}

/// The manager's windows, or None when it did not answer.
async fn wm_windows(env: &Env, wm: Wm) -> Option<Vec<Win>> {
    let r = crate::exec::run(
        list_argv(wm),
        crate::exec::ExecOpts {
            timeout_ms: Some(3000),
            env: Some(env.clone()),
        },
    )
    .await;
    (r.code == 0).then(|| parse_windows(wm, &r.out))
}

// ---------------------------------------------------------------------------
// Windows the daemon launched
// ---------------------------------------------------------------------------

/// The emulator processes this process launched, by window class. A window is a
/// process: where there is no manager to ask (a bare X session, Wayland
/// compositors without an adapter) the process is the answer.
type Tracked = HashMap<String, (tokio::process::Child, bool)>;

fn tracked() -> &'static Mutex<Tracked> {
    static T: std::sync::OnceLock<Mutex<Tracked>> = std::sync::OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// "present" while the process runs, "absent" once it has exited (only for an
/// emulator that stays in the foreground), None when it is not ours to say.
fn tracked_state(class: &str) -> Option<&'static str> {
    let mut g = tracked().lock().unwrap_or_else(|e| e.into_inner());
    // Read the child's state, then drop the borrow, because an exited child is removed.
    // Nothing used to remove one: `kill_tracked` was the only path that did, so a window the
    // user closed itself left its entry and its `Child` in this process-global map for the
    // life of the daemon.
    let (state, foreground) = {
        let (child, foreground) = g.get_mut(class)?;
        (child.try_wait(), *foreground)
    };
    match state {
        Ok(None) => Some("present"),
        Ok(Some(_)) => {
            g.remove(class);
            if foreground {
                Some("absent")
            } else {
                None
            }
        }
        Err(_) => {
            g.remove(class);
            None
        }
    }
}

fn kill_tracked(class: &str) -> bool {
    let removed = tracked()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(class);
    match removed {
        Some((child, _)) => {
            if let Some(pid) = child.id() {
                crate::proc::kill_group(pid, nix::sys::signal::Signal::SIGTERM);
            }
            true
        }
        None => false,
    }
}

/// Close a session's live window, if it is open.
///
/// The emulator this daemon launched is signalled by process group; a manager
/// adapter, if there is one, is also asked to close by class (covering a window
/// opened before this daemon started). The window manager answers an unmatched
/// criterion with success too, so this says "asked", not "was open".
pub async fn close_live(session_id: &str) -> Result<bool, String> {
    // One class: a desk window. A read-only view is `ldmux attach <id> --readonly` in a
    // terminal you already have, so there is no second class a window could hide under.
    let classes = [live_window_class(session_id)];
    let killed = classes.iter().any(|c| kill_tracked(c));
    let (env, wm) = context().await;
    let mut asked = false;
    if let Some(wm) = wm {
        for argv in classes.iter().flat_map(|class| kill_argvs(wm, class)) {
            let r = crate::exec::run(
                argv,
                crate::exec::ExecOpts {
                    timeout_ms: Some(3000),
                    env: Some(env.clone()),
                },
            )
            .await;
            asked |= r.code == 0;
        }
    }
    if killed || asked {
        Ok(true)
    } else {
        Err("nothing to close: this daemon did not open that window and no window manager adapter (i3, sway, hyprland) is reachable".to_string())
    }
}

/// Is a session's live window still on screen?
///
/// Three answers, not two. "Unknown" is not "absent": the caller uses this to
/// decide whether a shell was *disposed of*, and treating "the window manager is
/// not answering" as "the window was closed" would kill sessions every time it
/// hiccupped. A manager that answers is believed (when the emulator names its
/// windows by class); otherwise the process this daemon launched is.
pub async fn live_window_state(session_id: &str) -> Result<&'static str, String> {
    let (env, wm) = context().await;
    Ok(window_state_in(&env, wm, &live_window_class(session_id)).await)
}

async fn window_state_in(env: &Env, wm: Option<Wm>, class: &str) -> &'static str {
    if let Some(wm) = wm
        && terminal_has_class(&terminal())
        && let Some(list) = wm_windows(env, wm).await
    {
        return if list.iter().any(|w| w.class == class) {
            "present"
        } else {
            "absent"
        };
    }
    tracked_state(class).unwrap_or("unknown")
}

/// The session ids whose live window is currently mapped, as far as a window
/// manager adapter can tell (none: empty).
///
/// Used at startup: the live-window map is memory, so a daemon that has just
/// started adopts the windows that are actually on screen instead of forgetting
/// them (and then never ending their shells when they close).
pub async fn live_window_ids() -> Vec<String> {
    let (env, wm) = context().await;
    let Some(wm) = wm else { return Vec::new() };
    let Some(list) = wm_windows(&env, wm).await else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|w| w.class.strip_prefix("ldmux-live-"))
        .filter(|id| !id.is_empty())
        .map(|id| id.to_string())
        .collect()
}

/// Open a session's desk window, on a chosen workspace.
///
/// An empty workspace means "leave it where it opens": that is the `$mod+Return`
/// case, which must not move the user's view. A non-empty one must be a real
/// workspace name, and is honoured only where a placement adapter exists.
///
/// The window runs the attach client, which speaks the daemon's own protocol
/// (plan §3.9): no shim script, no state dir, no socket or binary parameters.
pub async fn open_live(session_id: &str, workspace: &str) -> Result<LiveResult, String> {
    let argv = live_argv(session_id, &terminal());
    open_window(&argv, &live_window_class(session_id), workspace).await
}

/// Open a session's read-only window: the same machinery with a different argv and class.
/// Read-only is not a joke: a viewer that holds the shell would take it from whoever is
/// typing, and the CLI never opens this one — `ldmux attach <id> --readonly` is the same
/// client in a terminal you already have.
/// The user manager this daemon can ask for a transient scope, if any.
///
/// `LDMUX_NO_SCOPE=1` turns it off, and a container or a session without a user bus has no
/// answer — in both cases a window still has to open, so this is `Option`, not an error.
fn window_scope() -> Option<crate::scope::ScopeEnv> {
    if std::env::var("LDMUX_NO_SCOPE").is_ok_and(|v| v == "1") {
        return None;
    }
    crate::scope::ScopeEnv::detect()
}

/// A pipe the child blocks on until its scope is in place: (read end, write end).
///
/// `CLOEXEC` on both, so a window that reaches `exec` cannot leak either into the emulator's
/// children; the child closes the write end itself in `pre_exec`, which is what makes the
/// parent's death release it.
fn go_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a two-element array the kernel fills; the call cannot fail in a way
    // that leaves it uninitialised on success.
    let rc = unsafe { nix::libc::pipe2(fds.as_mut_ptr(), nix::libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned by this process.
    unsafe { Ok((std::os::fd::OwnedFd::from_raw_fd(fds[0]), std::os::fd::OwnedFd::from_raw_fd(fds[1]))) }
}

/// The parent's end of the go-pipe. Releasing it lets the child `exec`; dropping it without a
/// release does the same, because the child's read returns 0 when every write end is closed.
///
/// That is why this is a guard and not a bare `write`: an early return, a `spawn` failure or a
/// panic must not leave a window hanging before `exec`.
struct GoPipe(Option<std::os::fd::OwnedFd>);

impl GoPipe {
    fn release(&mut self) {
        if let Some(fd) = self.0.take() {
            use std::io::Write as _;
            let mut file = std::fs::File::from(fd);
            // One byte is the signal; the child ignores its value.
            let _ = file.write_all(&[1u8]);
        }
    }
}

impl Drop for GoPipe {
    fn drop(&mut self) {
        self.release();
    }
}

async fn open_window(argv: &[String], class: &str, workspace: &str) -> Result<LiveResult, String> {
    let (env, wm) = context().await;
    open_window_in(&env, wm, argv, class, workspace).await
}

/// Launch a window: spawn the emulator directly, detached, with an absolute path
/// and the session environment; then, if a placement adapter exists and a
/// workspace was asked for, move it.
///
/// There is no shell and no window-manager exec in the launch, so nothing in the
/// argv is re-parsed (i3 splits `exec` on `,` and `;`), and the emulator is a
/// child of this process in its own process group. Placement failing is not a
/// launch failure: the answer is `ok` with `placed: false` and the reason.
async fn open_window_in(
    env: &Env,
    wm: Option<Wm>,
    argv: &[String],
    class: &str,
    workspace: &str,
) -> Result<LiveResult, String> {
    let target = match safe_workspace(workspace) {
        Some(t) => Some(t),
        None if workspace.trim().is_empty() => None,
        None => return Err("invalid workspace name".to_string()),
    };
    // Refuse a terminal that cannot run, and refuse it *here* rather than in the
    // CLI: every window path (`$mod+Return`, `ldmux new`, a phone asking for a
    // desk window) comes through, and a missing emulator should say so, not
    // surface as a window that never appears. `argv[0]` is the terminal.
    let Some(program) = argv.first() else {
        return Err("no terminal command".to_string());
    };
    if let Some(problem) = terminal_problem(program) {
        return Err(problem);
    }
    if env_get(env, "DISPLAY").is_none() && env_get(env, "WAYLAND_DISPLAY").is_none() {
        return Err(NO_DISPLAY.to_string());
    }
    let path = env_get(env, "PATH")
        .map(|s| s.to_string())
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    let mut argv = argv.to_vec();
    if let Some(abs) = which_in(&argv[0], &path) {
        argv[0] = abs;
    }

    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(false);
    // **The window can start only after it has been charged to its own scope.**
    // A terminal emulator forks the attach client immediately, and a child forked
    // before the move lands in the daemon's cgroup — the attribution this exists to
    // fix. The child blocks on this pipe until the move is visible (or the request
    // failed), exactly as a kept shell does (`crates/keeper/src/lib.rs`). The guard
    // releases it on every path, including an early return or a panic, so a daemon
    // that dies mid-request cannot leave a window hanging before `exec`.
    let scope = window_scope();
    let mut go: Option<GoPipe> = None;
    if scope.is_some() {
        match go_pipe() {
            Ok((read, write)) => {
                let (r, w) = (
                    std::os::fd::AsRawFd::as_raw_fd(&read),
                    std::os::fd::AsRawFd::as_raw_fd(&write),
                );
                // SAFETY: the child may only call async-signal-safe functions before
                // `exec`; `close` and `read` are, and both descriptors are open in
                // this process. Closing the write end in the child is what makes the
                // parent's death release it (a read of a pipe whose every write end
                // is closed returns 0).
                unsafe {
                    cmd.pre_exec(move || {
                        nix::libc::close(w);
                        // **A window that cannot be scoped still has to open.** The parent
                        // writes one byte as soon as the move is visible, and closing the
                        // write end unblocks this read anyway (EOF). The `poll` timeout is
                        // the third case: a parent that died, or a task cancelled, between
                        // `spawn` and the release would otherwise leave this child blocked
                        // for ever on a pipe nobody writes — which is a window that never
                        // appears, and (measured 2026-11-06) a leaked process for as long as
                        // the machine stays up.
                        let mut pfd = nix::libc::pollfd {
                            fd: r,
                            events: nix::libc::POLLIN,
                            revents: 0,
                        };
                        loop {
                            // Raw errno, not `io::Error`: the child may not allocate between
                            // fork and exec (the keeper makes the same choice).
                            let n = nix::libc::poll(&mut pfd, 1, 2_000);
                            if n >= 0 || *nix::libc::__errno_location() != nix::libc::EINTR {
                                break;
                            }
                        }
                        nix::libc::close(r);
                        Ok(())
                    });
                }
                go = Some(GoPipe(Some(write)));
                // The parent only writes; this copy of the read end is not needed.
                drop(read);
            }
            Err(e) => eprintln!("ldmuxd: window not scoped (no pipe): {e}"),
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", argv[0]))?;
    if let (Some(scope), Some(pid)) = (scope, child.id()) {
        let scope = scope.clone();
        let class = class.to_string();
        // Off the runtime: `busctl` is a process, and the window must not pay for
        // a user manager that is slow to answer. A failure is logged and the window
        // runs unscoped — which is what happened to every window before this.
        match tokio::task::spawn_blocking(move || crate::scope::enter(&scope, &class, pid)).await {
            Ok(Ok(_name)) => {}
            Ok(Err(e)) => eprintln!("ldmuxd: window not scoped: {e}"),
            Err(e) => eprintln!("ldmuxd: window scope request failed: {e}"),
        }
    }
    if let Some(g) = go.as_mut() {
        g.release();
    }
    // A foreground emulator that is already gone has failed to open its window
    // (no such display, a bad flag): say so rather than report a window.
    let foreground = terminal_is_foreground(&argv[0]);
    if foreground {
        for _ in 0..8 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "{} exited at once ({status}); it could not open a window on {}",
                    argv[0],
                    env_get(env, "WAYLAND_DISPLAY")
                        .or(env_get(env, "DISPLAY"))
                        .unwrap_or("the display")
                ));
            }
        }
    }
    tracked()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(class.to_string(), (child, foreground));

    let placed = match &target {
        Some(t) => place_with(env, wm, class, t, 5000).await,
        None => (
            true,
            None,
            Some("left on the current workspace".to_string()),
        ),
    };
    Ok(LiveResult {
        ok: true,
        placed: Some(placed.0),
        window_id: placed.1.clone(),
        detail: placed.2.clone(),
    })
}

/// Place a window on a chosen workspace, found by its class.
///
/// Polls for the window rather than sleeping a guessed interval: a terminal can
/// take a moment to map, and the move is a no-op until it exists. With no
/// adapter, or an emulator that cannot set a class, the window is opened and not
/// placed, and the detail says why.
async fn place_with(
    env: &Env,
    wm: Option<Wm>,
    class: &str,
    workspace: &str,
    timeout_ms: u64,
) -> (bool, Option<String>, Option<String>) {
    let target = match safe_workspace(workspace) {
        Some(t) => t,
        None => return (false, None, Some("invalid workspace name".to_string())),
    };
    let Some(wm) = wm else {
        return (
            false,
            None,
            Some(
                "opened, not placed: no window manager adapter (i3, sway, hyprland) is reachable"
                    .to_string(),
            ),
        );
    };
    if !terminal_has_class(&terminal()) {
        return (
            false,
            None,
            Some("opened, not placed: this terminal cannot name its windows".to_string()),
        );
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let started = std::time::Instant::now();
    let mut answered = false;
    while std::time::Instant::now() < deadline {
        if let Some(list) = wm_windows(env, wm).await {
            answered = true;
            if let Some(win) = list.iter().find(|w| w.class == class) {
                let moved = crate::exec::run(
                    move_argv(wm, win, &target),
                    crate::exec::ExecOpts {
                        timeout_ms: Some(3000),
                        env: Some(env.clone()),
                    },
                )
                .await;
                let id = Some(win.handle.clone());
                return if moved.code == 0 {
                    (true, id, None)
                } else {
                    (
                        false,
                        id,
                        Some("opened, not placed: the move failed".to_string()),
                    )
                };
            }
        }
        // A manager that has not answered once in a second is not going to.
        if !answered && started.elapsed() > std::time::Duration::from_millis(1000) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let why = if answered {
        "the window did not appear"
    } else {
        "the window manager did not answer"
    };
    (false, None, Some(format!("opened, not placed: {why}")))
}

#[cfg(test)]
mod expand_tests {
    use super::*;

    fn argv(program: &str, class: &str) -> Vec<String> {
        build_argv(program, class, &["ldmux".to_string(), "shell".to_string()])
    }

    /// A window with a name gets its class, as it always did.
    #[test]
    fn a_named_window_keeps_its_class() {
        assert_eq!(
            argv("alacritty", "ldmux-live-ab12"),
            vec!["alacritty", "--class", "ldmux-live-ab12", "-e", "ldmux", "shell"]
        );
    }

    /// **A window with no name gets no class flag.** `ldmux terminal --exec` runs the operator's own
    /// terminal and has nothing to name; `--class ""` set an empty class, which the window manager
    /// could not match (`alacritty --class  -e …` in `ps`, measured 2026-11-06).
    #[test]
    fn an_unnamed_window_drops_the_class_flag_and_its_value() {
        assert_eq!(
            argv("alacritty", ""),
            vec!["alacritty", "-e", "ldmux", "shell"],
            "the flag and its value go together"
        );
        // The same shape in another template: wezterm puts the class after two other flags.
        assert_eq!(
            argv("wezterm", ""),
            vec!["wezterm", "start", "--always-new-process", "--", "ldmux", "shell"]
        );
        // ...and kitty, which has nothing else.
        assert_eq!(argv("kitty", ""), vec!["kitty", "ldmux", "shell"]);
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
    fn live_window_class_sanitizes() {
        assert_eq!(live_window_class("abc123"), "ldmux-live-abc123");
        assert_eq!(live_window_class("abc-123"), "ldmux-live-abc-123");
        // Quotes and spaces are stripped.
        assert_eq!(live_window_class("abc\"def"), "ldmux-live-abcdef");
        // Longer than 40 chars is truncated.
        let long = "a".repeat(100);
        assert_eq!(
            live_window_class(&long),
            format!("ldmux-live-{}", "a".repeat(40))
        );
    }

    #[test]
    fn safe_workspace_valid() {
        assert_eq!(safe_workspace("1"), Some("1".to_string()));
        assert_eq!(
            safe_workspace("workspace-1"),
            Some("workspace-1".to_string())
        );
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
        assert!(
            args[4].starts_with('/'),
            "the attach binary must be absolute: {}",
            args[4]
        );
    }

    /// A terminal the window manager cannot run is refused before it is asked
    /// to: `i3-msg exec` reports success for a command it merely queued.
    #[test]
    fn a_terminal_that_cannot_be_run_is_named() {
        assert!(terminal_problem("").is_some());
        assert!(terminal_problem("   ").is_some());
        assert!(terminal_problem("/nonexistent-terminal-xyz").is_some());
        assert!(terminal_problem("definitely-not-a-real-binary-xyz").is_some());
        assert!(
            terminal_problem("bin/sh").is_some(),
            "a relative path is not a name on PATH"
        );
        assert!(terminal_problem("/bin/sh").is_none());
        // A bare name is looked for on PATH.
        assert!(terminal_problem("sh").is_none(), "PATH must be searched");
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
        assert_eq!(
            find_window_id_by_class(tree, "ldmux-live-abc123").unwrap(),
            "0x3e7"
        );
    }

    /// A terminal that cannot run is refused before the window manager is asked,
    /// and the refusal names it.
    ///
    /// The first assertion is the ordering: with no `DISPLAY` in the test process
    /// a later check would answer with the display instead, which is the error
    /// nobody can act on when the real problem is a terminal name.
    #[tokio::test]
    async fn a_terminal_that_cannot_run_is_refused_before_the_window_manager() {
        let argv = vec![
            "/nope/alacritty".to_string(),
            "--class".to_string(),
            "ldmux-live-x".to_string(),
            "-e".to_string(),
            "/bin/true".to_string(),
        ];
        let err = open_window(&argv, "ldmux-live-x", "")
            .await
            .err()
            .expect("a terminal that is not there must be refused");
        assert!(
            err.contains("no such terminal"),
            "the refusal names it: {err}"
        );
    }

    /// A program that can use a click is not mistaken for a shell (the TS
    /// `a program that can use a click is not mistaken for a shell`).
    #[test]
    fn a_non_shell_is_not_a_shell_foreground() {
        for prog in [
            "top", "vim", "emacs", "htop", "less", "nano", "git", "make", "cargo",
        ] {
            assert!(!is_shell_foreground(prog), "{prog} should not be a shell");
        }
        // The login shell with a leading dash is a shell.
        assert!(is_shell_foreground("-bash"));
        // An uppercase shell name is a shell.
        assert!(is_shell_foreground("BASH"));
        // A shell with a path is not a shell (the foreground is the basename).
        assert!(!is_shell_foreground("/usr/bin/bash"));
    }

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A comma or semicolon in a path must stay inside one argv element: nothing
    /// re-parses it (i3's `exec` splits on both), and the quoting used for
    /// The attach command reaches `execve` as separate arguments, byte for byte.
    /// Nothing in a path or a session id is re-parsed, because no shell is involved.
    #[test]
    fn nothing_in_the_argv_can_be_split_or_injected() {
        let attach = strs(&["/home/u/a,b;c d/ldmuxd", "attach", "s1"]);
        let argv = build_argv("/usr/bin/alacritty", "ldmux-live-s1", &attach);
        assert_eq!(
            argv[4], "/home/u/a,b;c d/ldmuxd",
            "one element, byte for byte: {argv:?}"
        );
        assert_eq!(argv[5..], attach[1..]);
    }

    /// Each emulator's template puts the class and the command where that
    /// emulator wants them.
    #[test]
    fn every_known_terminal_has_a_template_with_the_command() {
        let attach = strs(&["/x/ldmuxd", "attach", "s1"]);
        for spec in SPECS {
            let argv = build_argv(&format!("/usr/bin/{}", spec.name), "ldmux-live-s1", &attach);
            assert!(
                argv.ends_with(&attach),
                "{}: the attach command must come last: {argv:?}",
                spec.name
            );
            assert_eq!(
                argv.iter().any(|a| a.contains("ldmux-live-s1")),
                spec.class,
                "{}: class placeholder iff it can name windows: {argv:?}",
                spec.name
            );
        }
        let kitty = build_argv("/usr/bin/kitty", "C", &attach);
        assert_eq!(kitty[..3], strs(&["/usr/bin/kitty", "--class", "C"])[..]);
        let foot = build_argv("/usr/bin/foot", "C", &attach);
        assert_eq!(foot[1], "--app-id");
        // An emulator the table has never heard of gets the common `-e`.
        let other = build_argv("/opt/weird-term", "C", &attach);
        assert_eq!(other[..2], strs(&["/opt/weird-term", "-e"])[..]);
    }

    /// `$TERMINAL` > `xdg-terminal-exec` > the desktop's configured terminal > an
    /// emulator already running > the known names, each resolved to an absolute
    /// path. Every step falls through silently when it does not resolve: a stale
    /// `$TERMINAL` is common and must not stop a window opening.
    #[test]
    fn terminal_selection_order() {
        let dir = std::env::temp_dir().join(format!(
            "ldmux-term-sel-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        for n in ["xterm", "kitty", "mine", "xdg-terminal-exec", "foot"] {
            let f = dir.join(n);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Debian's alternative: `x-terminal-emulator` is a symlink to a `.wrapper` script
        // that execs the real emulator. Both indirections have to be followed.
        let wrapper = dir.join("xterm.wrapper");
        std::fs::write(&wrapper, "#!/bin/sh\nexec xterm \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&wrapper, dir.join("x-terminal-emulator")).unwrap();
        let path = dir.to_string_lossy().into_owned();
        let at = |n: &str| dir.join(n).to_string_lossy().into_owned();
        // The rule takes each answer as an argument; passing one is what selects it.
        assert_eq!(
            resolve_terminal_full(
                Some(at("kitty")),
                Some("xterm"),
                Some(at("xdg-terminal-exec")),
                Some("foot".to_string()),
                &["xterm".to_string()],
                &path,
            ),
            (at("kitty"), Source::Pinned),
            "the pin this user set wins over everything"
        );
        assert_eq!(
            resolve_terminal_full(
                None,
                Some("kitty -f"),
                Some(at("xdg-terminal-exec")),
                Some("xterm".to_string()),
                &[],
                &path,
            ),
            (at("kitty"), Source::Environment),
            "$TERMINAL is next, first word only"
        );
        assert_eq!(
            resolve_terminal_full(
                None,
                None,
                Some(at("xdg-terminal-exec")),
                Some("xterm".to_string()),
                &[],
                &path,
            ),
            (at("xdg-terminal-exec"), Source::XdgExec),
            "the XDG launcher beats the desktop's own setting"
        );
        assert_eq!(
            resolve_terminal_full(None, None, None, Some("xterm".to_string()), &[], &path),
            (at("xterm"), Source::Desktop),
            "the desktop's configured terminal, when there is nothing above it"
        );
        assert_eq!(
            resolve_terminal_full(None, None, None, None, &["foot".to_string()], &path),
            (at("foot"), Source::Running),
            "an emulator already running"
        );
        assert_eq!(
            resolve_terminal_full(
                None,
                None,
                None,
                Some("not-installed".to_string()),
                &["foot".to_string()],
                &path,
            ),
            (at("foot"), Source::Running),
            "a configured name that is missing falls through"
        );
        // The known list is reached only when there is no system alternative either.
        let plain = std::env::temp_dir().join(format!(
            "ldmux-term-known-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&plain).unwrap();
        for n in ["kitty", "xterm"] {
            let f = plain.join(n);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            resolve_terminal_full(None, None, None, None, &[], &plain.to_string_lossy()),
            (plain.join("kitty").to_string_lossy().into_owned(), Source::Known),
            "the known order, kitty before xterm"
        );
        let _ = std::fs::remove_dir_all(&plain);
        assert_eq!(
            resolve_terminal_full(None, None, None, None, &[], ""),
            (String::new(), Source::None)
        );
        // An answer that is an indirection is followed to the program, never returned raw.
        assert_eq!(
            resolve_terminal_full(None, None, None, Some("x-terminal-emulator".to_string()), &[], &path),
            (at("xterm"), Source::Desktop),
            "the alternative resolves to the program it runs"
        );
        assert!(terminal_problem("").unwrap().contains("$TERMINAL"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Only the session's own desktop is asked.** Reading a GNOME schema on an XFCE session
    /// is how this machine answered `x-terminal-emulator` over ten alacritty windows: the value
    /// belonged to a desktop nobody was running, and it named the Debian alternative rather
    /// than a terminal.
    #[test]
    fn only_the_sessions_own_desktop_is_asked() {
        use std::cell::RefCell;
        let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let probe = |program: &str, _args: &[&str]| -> Option<String> {
            asked.borrow_mut().push(program.to_string());
            match program {
                "gsettings" => Some("'gnome-terminal'\n".to_string()),
                "kreadconfig6" => Some("konsole\n".to_string()),
                "xfconf-query" => Some("xfce4-terminal\n".to_string()),
                _ => None,
            }
        };
        let helpers = Some("WebBrowser=firefox\nTerminalEmulator=alacritty\n");

        // GNOME: its own key, and nothing else.
        asked.borrow_mut().clear();
        assert_eq!(
            desktop_configured_terminal("GNOME", &probe, helpers).as_deref(),
            Some("gnome-terminal")
        );
        assert_eq!(*asked.borrow(), vec!["gsettings".to_string()]);

        // XFCE: xfconf first, then the helpers file — **never gsettings**, even though the
        // probe would answer.
        asked.borrow_mut().clear();
        assert_eq!(
            desktop_configured_terminal("XFCE", &probe, helpers).as_deref(),
            Some("xfce4-terminal")
        );
        assert_eq!(*asked.borrow(), vec!["xfconf-query".to_string()]);
        // With no xfconf answer, the helpers file decides.
        let no_xfconf = |program: &str, _args: &[&str]| -> Option<String> {
            (program == "gsettings").then(|| "'gnome-terminal'".to_string())
        };
        assert_eq!(
            desktop_configured_terminal("XFCE", &no_xfconf, helpers).as_deref(),
            Some("alacritty")
        );

        // KDE: kdeglobals, through kreadconfig6.
        asked.borrow_mut().clear();
        assert_eq!(
            desktop_configured_terminal("KDE", &probe, None).as_deref(),
            Some("konsole")
        );
        assert_eq!(*asked.borrow(), vec!["kreadconfig6".to_string()]);

        // A bare window manager: no desktop key applies, and nothing is asked.
        asked.borrow_mut().clear();
        assert_eq!(desktop_configured_terminal("i3", &probe, helpers), None);
        assert!(asked.borrow().is_empty(), "no probe on a bare session");
        assert_eq!(desktop_configured_terminal("", &probe, helpers), None);
    }

    /// An indirection is followed: `x-terminal-emulator` is a symlink to a `.wrapper` script,
    /// and the answer must be the program that script runs, or its template is lost.
    #[test]
    fn an_alternative_resolves_to_the_program_it_runs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "ldmux-alias-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, body: &str| {
            let f = dir.join(name);
            std::fs::write(&f, body).unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write("alacritty", "#!/bin/sh\n");
        write("alacritty.wrapper", "#!/bin/sh\nexec alacritty \"$@\"\n");
        std::os::unix::fs::symlink(dir.join("alacritty.wrapper"), dir.join("x-terminal-emulator"))
            .unwrap();
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(
            resolve_alias("x-terminal-emulator", &path).map(|(p, _)| p),
            Some(dir.join("alacritty").to_string_lossy().into_owned()),
            "the wrapper's program, not the wrapper"
        );
        assert_eq!(resolve_alias("alacritty.wrapper", &path).map(|(p, _)| p),
            Some(dir.join("alacritty").to_string_lossy().into_owned()));
        // Not an answer: prose, a .desktop id, a program that is not installed.
        assert_eq!(resolve_alias("Property \"/general/x\" does not exist", &path), None);
        assert_eq!(resolve_alias("org.gnome.Terminal.desktop", &path), None);
        assert_eq!(resolve_alias("nothing-here", &path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The parse of each probe's output, as the tools actually print it.
    #[test]
    fn desktop_probe_output_is_read_conservatively() {
        // gsettings quotes its value, and an unset one is the empty string.
        assert_eq!(parse_quoted_probe("'alacritty'\n").as_deref(), Some("alacritty"));
        assert_eq!(
            parse_quoted_probe("'x-terminal-emulator'\n").as_deref(),
            Some("x-terminal-emulator")
        );
        assert_eq!(parse_quoted_probe("''\n"), None);
        assert_eq!(parse_quoted_probe("No such schema “x”"), None);
        // kreadconfig prints the bare value; xfconf-query prints either a path or prose.
        assert_eq!(parse_word_probe("alacritty\n").as_deref(), Some("alacritty"));
        assert_eq!(
            parse_word_probe("/usr/bin/kitty\n").as_deref(),
            Some("/usr/bin/kitty")
        );
        assert_eq!(
            parse_word_probe("Property \"/general/TerminalEmulator\" does not exist on channel \"xfce4-session\".\n"),
            None
        );
        assert_eq!(parse_word_probe(""), None);
        // XFCE's helpers file is one Key=Value per line.
        assert_eq!(
            parse_helpers_rc("WebBrowser=firefox\nTerminalEmulator=xfce4-terminal\n").as_deref(),
            Some("xfce4-terminal")
        );
        assert_eq!(parse_helpers_rc("WebBrowser=firefox\n"), None);
        assert_eq!(parse_helpers_rc("TerminalEmulator=\n"), None);
    }

    /// An emulator already running is a stronger statement of intent than a config
    /// file: this is what makes a bare i3 session open the terminal in use.
    #[test]
    fn a_running_emulator_wins_over_the_static_list() {
        let dir = std::env::temp_dir().join(format!(
            "ldmux-term-run-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        for n in ["alacritty", "xterm"] {
            let f = dir.join(n);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = dir.to_string_lossy().into_owned();
        assert_eq!(
            resolve_terminal_full(None, None, None, None, &["xterm".to_string()], &path).0,
            format!("{path}/xterm"),
        );
        // Most frequent first is the caller's contract; a name that does not
        // resolve is skipped rather than returned.
        assert_eq!(
            resolve_terminal_full(
                None,
                None,
                None,
                None,
                &["not-a-term".to_string(), "xterm".to_string()],
                &path
            )
            .0,
            format!("{path}/xterm"),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_session_environment_is_completed_from_the_user_manager() {
        let own = env(&[("PATH", "/bin"), ("DISPLAY", ":1")]);
        let manager = "DISPLAY=:0\nWAYLAND_DISPLAY=wayland-1\nSWAYSOCK=/run/s.sock\nHOME=/x\nXAUTHORITY=$'a\\nb'\n";
        let merged = merge_session_env(&own, manager);
        assert_eq!(
            env_get(&merged, "DISPLAY"),
            Some(":1"),
            "the daemon's own value wins"
        );
        assert_eq!(env_get(&merged, "WAYLAND_DISPLAY"), Some("wayland-1"));
        assert_eq!(env_get(&merged, "SWAYSOCK"), Some("/run/s.sock"));
        assert_eq!(
            env_get(&merged, "HOME"),
            None,
            "only session variables are taken"
        );
        assert_eq!(
            env_get(&merged, "XAUTHORITY"),
            None,
            "a $'..' value is not guessed at"
        );
    }

    #[test]
    fn the_adapter_follows_the_session() {
        let all = |_: &str| true;
        let none = |_: &str| false;
        assert_eq!(
            detect_wm(&env(&[("SWAYSOCK", "/s"), ("DISPLAY", ":0")]), &all),
            Some(Wm::Sway)
        );
        assert_eq!(
            detect_wm(&env(&[("HYPRLAND_INSTANCE_SIGNATURE", "x")]), &all),
            Some(Wm::Hyprland)
        );
        assert_eq!(detect_wm(&env(&[("I3SOCK", "/i")]), &all), Some(Wm::I3));
        assert_eq!(detect_wm(&env(&[("DISPLAY", ":0")]), &all), Some(Wm::I3));
        assert_eq!(
            detect_wm(&env(&[("DISPLAY", ":0")]), &none),
            None,
            "no tool, no adapter"
        );
        assert_eq!(
            detect_wm(
                &env(&[("WAYLAND_DISPLAY", "w"), ("NIRI_SOCKET", "/n")]),
                &all
            ),
            None,
            "niri has no adapter"
        );
        assert_eq!(detect_wm(&[], &all), None);
    }

    #[test]
    fn windows_are_found_in_each_managers_output() {
        let sway = r#"{"nodes":[{"id":7,"window":null,"app_id":"ldmux-live-a","nodes":[],"floating_nodes":[]}]}"#;
        assert_eq!(
            parse_windows(Wm::Sway, sway),
            vec![Win {
                class: "ldmux-live-a".into(),
                handle: "con_id=7".into()
            }]
        );
        let i3 =
            r#"{"nodes":[{"id":9,"window":255,"window_properties":{"class":"ldmux-live-b"}}]}"#;
        assert_eq!(
            parse_windows(Wm::I3, i3),
            vec![Win {
                class: "ldmux-live-b".into(),
                handle: "id=\"0xff\"".into()
            }]
        );
        let hypr = r#"[{"address":"0xabc","class":"ldmux-live-c"},{"address":"0x1","class":""}]"#;
        assert_eq!(
            parse_windows(Wm::Hyprland, hypr),
            vec![Win {
                class: "ldmux-live-c".into(),
                handle: "address:0xabc".into()
            }]
        );
        assert!(parse_windows(Wm::I3, "not json").is_empty());
        let mv = move_argv(Wm::Sway, &parse_windows(Wm::Sway, sway)[0], "3");
        assert_eq!(
            mv,
            strs(&["swaymsg", "[con_id=7] move to workspace number 3"])
        );
    }

    /// With nothing that could open a window the answer says what to do, and the
    /// emulator is not started.
    #[tokio::test]
    async fn no_display_is_a_clear_refusal() {
        let argv = strs(&["/bin/sh", "-c", "exit 0"]);
        let err = open_window_in(&Vec::new(), None, &argv, "ldmux-live-nd", "")
            .await
            .err()
            .expect("must refuse");
        assert!(
            err.contains("DISPLAY")
                && err.contains("WAYLAND_DISPLAY")
                && err.contains("import-environment"),
            "{err}"
        );
    }

    /// Wayland only (no DISPLAY, no adapter): the window opens directly, is
    /// reported present while the process runs, absent when it ends, and a
    /// requested workspace degrades to "opened, not placed" rather than an error.
    #[tokio::test]
    async fn a_window_opens_without_any_window_manager() {
        let e = env(&[
            ("WAYLAND_DISPLAY", "wayland-test"),
            ("PATH", "/usr/bin:/bin"),
        ]);
        let class = "ldmux-live-direct1";
        let argv = strs(&["/bin/sh", "-c", "sleep 30"]);
        let r = open_window_in(&e, None, &argv, class, "2")
            .await
            .expect("opens");
        assert!(r.ok);
        assert_eq!(r.placed, Some(false));
        assert!(r.detail.unwrap().contains("opened, not placed"));
        assert_eq!(window_state_in(&e, None, class).await, "present");
        assert!(kill_tracked(class));
        // Gone from tracking: nothing to say, which is "unknown", never "absent".
        assert_eq!(window_state_in(&e, None, class).await, "unknown");
    }

    /// A foreground emulator that dies at once is a failure to open, not a window.
    #[tokio::test]
    async fn an_emulator_that_exits_at_once_is_reported() {
        let e = env(&[("DISPLAY", ":99"), ("PATH", "/usr/bin:/bin")]);
        let argv = strs(&["/bin/sh", "-c", "exit 3"]);
        let err = open_window_in(&e, None, &argv, "ldmux-live-dies", "")
            .await
            .err()
            .expect("must fail");
        assert!(err.contains("exited at once"), "{err}");
    }

    /// The emulator is launched detached, in its own process group.
    #[tokio::test]
    async fn the_emulator_has_its_own_process_group() {
        let e = env(&[("DISPLAY", ":99"), ("PATH", "/usr/bin:/bin")]);
        let class = "ldmux-live-pgrp";
        let argv = strs(&["/bin/sh", "-c", "sleep 30"]);
        open_window_in(&e, None, &argv, class, "")
            .await
            .expect("opens");
        let pid = tracked()
            .lock()
            .unwrap()
            .get(class)
            .unwrap()
            .0
            .id()
            .unwrap() as i32;
        let pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid)))
            .unwrap()
            .as_raw();
        assert_eq!(pgid, pid, "own group");
        assert_ne!(pgid, nix::unistd::getpgrp().as_raw());
        kill_tracked(class);
    }
}
