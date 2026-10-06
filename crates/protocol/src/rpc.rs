//! The daemon wire contract: shared request/response types and the method
//! names.
//!
//! This is the single source of truth for the Unix-socket RPC surface, imported
//! by the daemon (server), the CLI, and tests. Port of
//! `packages/protocol/src/rpc.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`).
//!
//! The closed string unions (`kind`, `status`, device `kind`, `mode`) are modelled
//! as `String` and documented with their closed set: the wire is a plain string
//! and a Rust enum would add no wire fidelity. (They can be tightened to string
//! enums in a later pass without a wire change.)

use serde::{Deserialize, Serialize};

/// One version, two names.
///
/// `Caps.protocolVersion` and `PingResult.version` are different fields on the
/// wire, and two independent `= 1`s meant a future bump could update one and not
/// the other — so a client got a different answer depending on which one it
/// happened to check. The number lives in `messages.rs`, beside the messages
/// that carry it; this is the name the socket contract has always exposed it under.
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;

/// The method names of the daemon RPC, as a const set (the wire is a string).
pub mod method {
    pub const PING: &str = "ping";
    pub const LIST: &str = "list";
    pub const NEW: &str = "new";
    pub const SEND: &str = "send";
    pub const CAPTURE: &str = "capture";
    pub const SNAPSHOT: &str = "snapshot";
    pub const KILL: &str = "kill";
    pub const CLOSE: &str = "close";
    pub const RECLAIM: &str = "reclaim";
    pub const FORGET: &str = "forget";
    pub const ADOPT: &str = "adopt";
    pub const DESKTOP: &str = "desktop";
    pub const MAKE_LIVE: &str = "makeLive";
    pub const CLOSE_LIVE: &str = "closeLive";
    pub const DESK_WINDOW: &str = "deskWindow";
    pub const CLIPBOARD_WRITE: &str = "clipboardWrite";
    pub const REMOTE_INFO: &str = "remote_info";
}

/// A device identity, so two devices can be told apart in a message about a
/// session they both want. It is **not** an account and **not** a security
/// boundary: it is a stable per-install string, and nothing is authorised by it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable per-install id. The phone derives one once and keeps it.
    pub id: String,
    /// What a human reads in "open on …".
    pub name: String,
    pub kind: String,
}

/// A session's current holder, as advertised to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionOwner {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Epoch ms when it was taken.
    pub since: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    /// Empty string when the session is not terminal-attached (RPC-only).
    pub keeper_name: String,
    pub kind: String,
    pub cwd: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub created_by: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<i64>,
    /// The desktop workspace this shell has a window on, when it has one.
    ///
    /// Persisted, not memory: the daemon cannot tell "its window was closed
    /// while we were down" from "it never had one" after a restart otherwise,
    /// and the TS relied on the live shim's pid for exactly that. Closing a desk
    /// window ends its shell, so losing this fact leaves a shell no window and no
    /// rule will ever end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_workspace: Option<String>,

    /// The process a desk window *is*, when the daemon did not launch it.
    ///
    /// `ldmux shell` and `ldmux run` become a desk window in the terminal they
    /// were started from, so there is no window of ours to look for by class —
    /// the terminal is a stock one. The window is open while this process is,
    /// which the kernel answers exactly, and that is the same rule the
    /// TypeScript shim's recorded pid gave. `None` for a window the daemon
    /// opened through i3, which the window manager can be asked about.
    ///
    /// Persisted for the same reason as `live_workspace`: after a restart the
    /// daemon must still know this shell has a window, or it ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_pid: Option<i32>,

    /// Why it ended, when it did, in words.
    ///
    /// Four different things end a session — its shell exits, its desk window
    /// closes, someone kills it, the keeper comes back without it — and the
    /// record said only `dead`. The journal said nothing at all: an evening of
    /// use ended two shells and the only way to explain either was elimination.
    /// Every ending now goes through one place that writes this, so "why did my
    /// shell die" has an answer in `ldmux list` and in the journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,

    /// The signal that killed the shell, when one did (`exit_code` is then absent).
    /// `exit_code` and this are the shell's own ending, from the keeper's `waitpid`;
    /// neither is set for a session someone killed or that the keeper lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,

    /// What kind of ending it was, as a closed set:
    /// `"exited"` (the shell ended itself), `"killed"` (`kill`), `"closed"`
    /// (closed from a client), `"keeper_lost"` (the keeper came back without it),
    /// `"window_closed"` (its desk window was closed). Absent while running and
    /// for records written before this existed. `ended_reason` is the same fact
    /// in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_kind: Option<String>,
}

/// A session as advertised to remote clients: the registry record minus the
/// daemon's internal bookkeeping, plus the freshness signal clients need.
/// Derived from `SessionRecord` so the two cannot drift apart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub cwd: String,
    pub status: String,
    pub keeper_name: String,
    pub created_by: String,
    /// Last observed activity, epoch ms — the freshness signal for unread
    /// markers. Absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
    /// What the pane is currently running (`bash`, `vim`, `pi`), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// The device holding this shell, when one does (ADR-0010).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionOwner>,
    /// The desktop workspace this shell has a window on, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_workspace: Option<String>,

    /// Why it ended, when it has. Absent while it is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_reason: Option<String>,

    /// The shell's exit status, when it exited by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// The signal that killed the shell, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
    /// `exited | killed | closed | keeper_lost | window_closed` (see
    /// [`SessionRecord::ended_kind`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_kind: Option<String>,
}

/// The desktop inventory: what the launcher shows *before* a terminal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWorkspace {
    /// The WM's own name for it ("1", "web", "code").
    pub name: String,
    /// Numeric part when the name is a plain number, else null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    pub focused: bool,
    pub visible: bool,
    pub window_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopWindow {
    pub id: String,
    pub app_class: String,
    pub title: String,
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPiSession {
    pub pid: i64,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The ldmux session this desk `pi` runs inside, found by walking its
    /// process ancestry up to an ldmux session's shell. Null when the pi was started in a
    /// terminal that is not an ldmux session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ldmux_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// What to show in the menu: the directory's basename.
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInventory {
    /// "i3" | "sway" | "niri" | "hyprland" | "ewmh" | "unknown".
    pub wm: String,
    pub workspaces: Vec<DesktopWorkspace>,
    pub windows: Vec<DesktopWindow>,
    pub pi_sessions: Vec<DesktopPiSession>,
    /// Set when windows could not be listed; pi sessions are still reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The pane's state, as the daemon reports it for a `snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PaneState {
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub alternate_on: bool,
    pub mouse: bool,
    /// The running program negotiated SGR mouse reporting (1006).
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub history_size: u64,
    pub cols: u64,
    pub rows: u64,
    pub dead: bool,
    /// The pane's foreground command name (`pane_current_command`).
    pub foreground: String,
}

/// The answer to `ping`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    /// Always `true` on the wire.
    pub pong: bool,
    pub version: u32,
    pub keeper_socket: String,
}

/// Params for `new`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NewParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Tie the session's life to its clients (destroyed when the last one
    /// detaches).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
}

/// Params for `send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendParams {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_info_round_trips() {
        let d = DeviceInfo {
            id: "abc".into(),
            name: "My Phone".into(),
            kind: "phone".into(),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(s, r#"{"id":"abc","name":"My Phone","kind":"phone"}"#);
        let back: DeviceInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }

    /// The reason a shell ended is on the wire in camelCase, and absent while it
    /// is running — the Kotlin client's `SessionsParseTest` pins the shape of
    /// this struct, so a field that appears when it should not is a contract
    /// break, not a detail.
    #[test]
    fn session_info_carries_the_reason_only_when_it_has_one() {
        let info = SessionInfo {
            id: "s1".into(),
            kind: "shell".into(),
            title: "t".into(),
            cwd: "/".into(),
            status: "dead".into(),
            keeper_name: "ldmux-s1".into(),
            created_by: "local".into(),
            last_activity_at: None,
            process: None,
            owner: None,
            live_workspace: None,
            ended_reason: Some("its shell exited".into()),
            exit_code: None,
            exit_signal: None,
            ended_kind: None,
        };
        let s = serde_json::to_string(&info).unwrap();
        assert!(s.contains(r#""endedReason":"its shell exited""#), "got {s}");
        let mut live = info.clone();
        live.status = "running".into();
        live.ended_reason = None;
        let s = serde_json::to_string(&live).unwrap();
        assert!(
            !s.contains("endedReason"),
            "a running shell has no reason: {s}"
        );
        // Round-trips, so an older writer and a newer reader agree.
        let back: SessionInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(back, live);
    }

    #[test]
    fn session_record_omits_absent_exit_fields() {
        let r = SessionRecord {
            id: "s1".into(),
            keeper_name: "s1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "t".into(),
            created_at: 1,
            updated_at: 2,
            created_by: "d".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("exitCode"), "exitCode must be absent: {s}");
        assert!(!s.contains("exitedAt"), "exitedAt must be absent: {s}");
        // The wire is camelCase: every multi-word field matches the TS object.
        assert!(s.contains(r#""keeperName":"s1""#), "got {s}");
        assert!(s.contains(r#""createdBy":"d""#), "got {s}");
        // The wire contract the Kotlin client pins (`SessionsParseTest`): the
        // session-layer name is `keeperName`, and nothing on it says tmux.
        assert!(!s.to_ascii_lowercase().contains("tmux"), "got {s}");
    }

    #[test]
    fn session_record_serializes_exit_fields_when_present() {
        let mut r = SessionRecord {
            id: "s1".into(),
            keeper_name: "s1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "t".into(),
            created_at: 1,
            updated_at: 2,
            created_by: "d".into(),
            status: "dead".into(),
            exit_code: Some(130),
            exited_at: Some(99),
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""exitCode":130"#), "got {s}");
        assert!(s.contains(r#""exitedAt":99"#), "got {s}");
        r.exit_code = Some(0);
    }
}
