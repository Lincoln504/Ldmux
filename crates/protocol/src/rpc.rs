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

/// One version, two names, and **one literal**.
///
/// `Caps.protocolVersion` and `PingResult.version` are different fields on the wire. Two
/// independent `= 1`s meant a bump could update one and not the other, so a client got a
/// different answer depending on which field it happened to read. This is an *alias* of the
/// number in `messages.rs`; `rpc::tests::the_two_protocol_version_names_are_one_number` holds
/// them together, and `ldmux --version` and `ldmux ping` report the same value `caps` does.
pub const DAEMON_PROTOCOL_VERSION: u32 = crate::messages::PROTOCOL_VERSION;

/// The method names of the daemon RPC, as a const set (the wire is a string).
/// **The control-socket method names, as the CLI spells them.**
///
/// The daemon's dispatch (`crates/daemon/src/daemon.rs`) is the server-side authority and matches
/// these strings; this module is the client side, so a rename is a compile error on one half
/// rather than a runtime "unknown method" on the other.
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
    /// Who holds a shell, from the daemon's in-memory claim table:
    /// `{"holder": "mesh" | "desktop" | "phone" | ""}`. Separate from `list` because the radio's
    /// write path asks it before every keystroke, and `list` asks the keeper about every shell.
    pub const HOLDER: &str = "holder";
    pub const FORGET: &str = "forget";
    pub const ADOPT: &str = "adopt";
    pub const DESKTOP: &str = "desktop";
    pub const MAKE_LIVE: &str = "makeLive";
    pub const CLOSE_LIVE: &str = "closeLive";
    pub const DESK_WINDOW: &str = "deskWindow";
    pub const CLIPBOARD_WRITE: &str = "clipboardWrite";
    pub const REMOTE_INFO: &str = "remote_info";
    pub const KEY: &str = "key";
    pub const RELEASE: &str = "release";
    pub const RESIZE_PANE: &str = "resizePane";
    pub const MESH_STATUS: &str = "mesh.status";
    pub const MESH_PAIRING: &str = "mesh.pairing";
    pub const MESH_PRESET: &str = "mesh.preset";
    /// Ring the phone about a shell: `{"session", "kind": "attention" | "done", "note"}`.
    ///
    /// A **mark in the app's shell list**, not a write: it touches no claim, no pty and no output.
    /// The shell names itself, so a process inside one sends it with no argument at all
    /// (`$LDMUX_SESSION_ID`, set by `session_spawn_env`).
    pub const BUZZ: &str = "buzz";
    /// Name a shell after the conversation in it (`ldmux title`), 1-3 words.
    pub const TITLE: &str = "title";
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

    /// Whether this shell has ever had a desk window.
    ///
    /// `live_workspace` answers "has one now"; this answers "ever had one", and the pair is
    /// what tells a shell that was created headlessly (from `ldmux new`, never shown anywhere)
    /// from one whose window went away. The phone draws the second as an anomaly and the
    /// first as an ordinary row, and without this it called both "no window" in amber.
    #[serde(default)]
    pub had_window: bool,

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

    /// Whether this shell has **ever** had a desk window.
    ///
    /// `live_workspace` answers "has one now"; this answers "ever had one", and the phone
    /// needs the pair: a shell created headlessly (`ldmux new`, never shown anywhere) is an
    /// ordinary row, while a shell whose window went away is the anomaly worth an amber
    /// label. Without this the two looked identical and every headless shell was drawn as a
    /// fault. `#[serde(default)]` so a record written before this reads as "no window" and
    /// simply is not flagged.
    #[serde(default)]
    pub had_window: bool,

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInventory {
    /// "i3" | "sway" | "niri" | "hyprland" | "ewmh" | "unknown".
    pub wm: String,
    pub workspaces: Vec<DesktopWorkspace>,
    pub windows: Vec<DesktopWindow>,
    /// Set when the window manager could not be enumerated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The answer to `ping`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    /// Always `true` on the wire.
    pub pong: bool,
    pub version: u32,
    pub keeper_socket: String,
    /// The daemon's build (`ldmux_protocol::BUILD_ID`).
    ///
    /// **Additive.** An older daemon omits it and this side reads `None`; an older CLI ignores a
    /// field it does not know, which is the same rule every other wire change here follows. It is
    /// what lets a client ask "is the daemon I am talking to the build I just installed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

#[cfg(test)]
mod ping_tests {
    use super::*;

    /// The built-in is additive: a daemon too old to send `build` still parses, with `None`.
    #[test]
    fn an_older_ping_answer_without_a_build_still_parses() {
        let old = r#"{"pong":true,"version":1,"keeperSocket":"/run/user/1000/ldmux/keeper.sock"}"#;
        let r: PingResult = serde_json::from_str(old).unwrap();
        assert!(r.pong);
        assert_eq!(r.build, None);
    }

    /// **One number under two names.** `Caps.protocolVersion` and `PingResult.version` are
    /// separate wire fields, and the two Rust constants were separate literals until this
    /// test existed: a bump to one and not the other told a client two different versions.
    #[test]
    fn the_two_protocol_version_names_are_one_number() {
        assert_eq!(DAEMON_PROTOCOL_VERSION, crate::messages::PROTOCOL_VERSION);
        let ping = PingResult {
            pong: true,
            version: DAEMON_PROTOCOL_VERSION,
            keeper_socket: "/k".into(),
            build: None,
        };
        assert_eq!(ping.version, crate::messages::PROTOCOL_VERSION);
        assert!(
            crate::version_line().ends_with(&format!("(protocol {DAEMON_PROTOCOL_VERSION})")),
            "the version line must name the same number: {}",
            crate::version_line()
        );
    }

    /// And a build that carries one writes it, so the client can ask "what is running".
    #[test]
    fn a_ping_answer_carries_the_build_when_there_is_one() {
        let r = PingResult {
            pong: true,
            version: DAEMON_PROTOCOL_VERSION,
            keeper_socket: "/k".into(),
            build: Some("abc12345".into()),
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains(r#""build":"abc12345""#), "{text}");
        assert_eq!(serde_json::from_str::<PingResult>(&text).unwrap(), r);
    }
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
            had_window: false,
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
            had_window: false,
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
            had_window: false,
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
