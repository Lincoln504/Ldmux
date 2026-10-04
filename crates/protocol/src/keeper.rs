//! Keeper messages — the daemon↔keeper control vocabulary (new; plan §3.5).
//!
//! These have **no TS source of record** (the keeper does not exist in the TS
//! tree). They travel on the same frame codec as the client messages, over the
//! daemon↔keeper connection (the same client protocol, minus auth). The names
//! are fixed by plan §3.5; the field sets are specified here and finalised by
//! the keeper phase (plan §7 phase 3). They share names with some client
//! messages (`Kill`, `Resize`, `Snapshot`, `List`, `Close`) but have different
//! fields, so they are modelled separately from [`crate::messages`].

use serde::{Deserialize, Serialize};

/// A daemon↔keeper control message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum KeeperMessage {
    Spawn(Spawn),
    Kill(Kill),
    Write(Write),
    Resize(Resize),
    Read(Read),
    Peek(Peek),
    PeekResult(PeekResult),
    Snapshot(Snapshot),
    SnapshotResult(SnapshotResult),
    State(State),
    List(List),
    ListResult(ListResult),
    ChildInfo(ChildInfo),
    ChildExited(ChildExited),
    Close(Close),
    Ack(Ack),
    Error(Error),
}

/// Keeper message tags.
pub mod k {
    pub const SPAWN: &str = "spawn";
    pub const KILL: &str = "kill";
    pub const WRITE: &str = "write";
    pub const RESIZE: &str = "resize";
    pub const READ: &str = "read";
    pub const PEEK: &str = "peek";
    pub const PEEK_RESULT: &str = "peekresult";
    pub const SNAPSHOT: &str = "snapshot";
    pub const SNAPSHOT_RESULT: &str = "snapshotresult";
    pub const STATE: &str = "state";
    pub const LIST: &str = "list";
    pub const CHILD_INFO: &str = "childinfo";
    pub const CHILD_EXITED: &str = "childexited";
    pub const CLOSE: &str = "close";
    pub const LIST_RESULT: &str = "listresult";
    pub const ACK: &str = "ack";
    pub const ERROR: &str = "error";
}

/// Daemon → keeper: spawn a session (a PTY + shell + one emulator task).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Spawn {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The argv to run instead of the keeper's default login shell (the `pi`
    /// kind runs `pi`). `command[0]` is the program.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Extra environment for the child (the `LDMUX_*` spawn env, plan §2.6).
    /// The keeper inherits its own environment and overlays these.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<(String, String)>>,
}

/// Daemon → keeper: kill a session (group-kill the child, drop the emulator).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Kill {
    pub session_id: String,
}

/// Daemon → keeper: input for a session (the Write direction). Carried as a
/// UTF-8 string, exactly as the client's `Input.data` is: the bytes-to-PTY
/// conversion is the keeper's job, and the message stays valid JSON (the codec
/// is JSON text, so raw bytes would need an encoding the client does not use).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Write {
    pub session_id: String,
    pub data: String,
}

/// Daemon → keeper: the attached client's viewport changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Resize {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
}

/// Daemon → keeper: a client is attaching (the Read/stream direction).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Read {
    pub session_id: String,
}

/// Daemon → keeper: drain the session's buffered output bytes (the attach's
/// stream direction — the bytes the shell has written to the pty since the
/// last peek). The attach polls this to pull the pane; the keeper always
/// reads the master into the buffer, so a peek is a bounded, non-blocking
/// pull.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Peek {
    pub session_id: String,
}

/// Keeper → daemon: the drained bytes (empty when the buffer is empty).
///
/// `data` is the raw pty bytes as a JSON byte array (the skeleton's encoding;
/// a framed binary stream replaces it when the stream direction goes live).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PeekResult {
    pub session_id: String,
    pub data: Vec<u8>,
}

/// Daemon → keeper: re-send the pane, optionally with paged scrollback
/// (the keeper-side of the client's `scrollback` request).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session_id: String,
    /// Lines of history to include. `0`/absent = visible screen only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_lines: Option<u64>,
}

/// Keeper → daemon: the rendered snapshot (the answer to a `Snapshot`): the
/// pane's geometry + the rendered screen (one SGR line per row, `\n`-joined —
/// the keeper's format, `ldmux_vt_sys::snapshot::render`). When
/// `history_lines > 0`, `text` is that many history lines (oldest first) above
/// the `rows` live lines — the `capture-pane -S -N -E -` contract. The count
/// is reported from what was actually captured (clamped to the scrollback),
/// not from what was asked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    /// The number of history lines actually captured (0 = live screen only).
    pub history_lines: u64,
    pub text: String,
    // The pane's state (plan §3.4: the `display-message` fields, read direct
    // from ghostty-vt — not a parsed format string). See each field's source.
    pub cursor_x: u64,
    pub cursor_y: u64,
    /// On the alternate screen (DEC 1047) — no scrollback, so `history_lines`
    /// is meaningless here and the client must not request it.
    pub alternate_on: bool,
    pub pane_in_mode: bool,
    /// Any mouse-tracking mode active (`mouse_any_flag`).
    pub mouse: bool,
    /// SGR (1006) mouse reporting negotiated — the only mouse encoding we
    /// synthesize for.
    pub mouse_sgr: bool,
    /// Application cursor-key mode (DEC 25, `keypad_cursor_flag`).
    pub app_cursor: bool,
    /// Bracketed paste mode (DEC 2004).
    pub bracketed_paste: bool,
    pub history_size: u64,
    pub dead: bool,
    /// The pane's foreground command name (`/proc/<pid>/comm`), e.g. `dash`.
    pub foreground: String,
    /// The pane's working directory (`/proc/<pid>/cwd`).
    pub cwd: String,
}

/// Keeper → daemon: report state for a session (the answer to a Read/State
/// and each entry of a List). It carries the pane's state (the display-message
/// fields, read direct from ghostty-vt) so the daemon can read liveness without
/// paying for a `Snapshot` render.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    pub dead: bool,
    /// The cursor column (0-based), from the emulator.
    pub cursor_x: u64,
    /// The cursor row (0-based), from the emulator.
    pub cursor_y: u64,
    /// The alternate screen is active (from `active_screen`).
    pub alternate_on: bool,
    /// No ghostty-vt source (the tmux "pane in a mode" concept); reported
    /// `false` (documented choice; no client reads it).
    pub pane_in_mode: bool,
    /// The pane negotiated mouse reporting (from `mouse_tracking`).
    pub mouse: bool,
    /// The pane negotiated SGR mouse (DEC 1006).
    pub mouse_sgr: bool,
    /// Application cursor keys (DECCKM, DEC 1) — tmux `keypad_cursor_flag`.
    pub app_cursor: bool,
    /// Bracketed paste (DEC 2004) is on.
    pub bracketed_paste: bool,
    /// The scrollback rows (from `scrollback_rows`).
    pub history_size: u64,
    /// The foreground command (`/proc/<pid>/comm`); empty for a dead pane.
    pub foreground: String,
    /// The working directory (`/proc/<pid>/cwd`); empty for a dead pane.
    pub cwd: String,
}

/// Daemon → keeper: list the keeper's sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct List {}

/// Keeper → daemon: the child process info for a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChildInfo {
    pub pid: u64,
}

/// Keeper → daemon: the shell exited. (A keeper crash — the documented single
/// point of failure, plan R3 — is the same event as a tmux-server crash.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChildExited {
    pub session_id: String,
    pub code: i64,
}

/// The answer to `List`: every session the keeper holds, as its state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub sessions: Vec<State>,
}

/// A bare acknowledgement for a message that carries no meaningful result
/// (a `Write`, a `Close`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Ack {}

/// A failure answer (a request the keeper could not perform): the error is
/// human-readable, the daemon surfaces it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Close the daemon↔keeper connection (not a session close).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Close {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_carries_text_as_json() {
        let w = KeeperMessage::Write(Write {
            session_id: "s1".into(),
            data: "echo hi\n".into(),
        });
        let v: serde_json::Value = serde_json::to_value(&w).unwrap();
        assert_eq!(v["type"], "write");
        assert_eq!(v["data"], "echo hi\n");
    }

    #[test]
    fn spawn_and_kill_shapes() {
        let v: serde_json::Value =
            serde_json::to_value(KeeperMessage::Spawn(Spawn {
                session_id: "s1".into(),
                cols: 80,
                rows: 24,
                cwd: None,
                command: None,
                env: None,
            }))
            .unwrap();
        assert_eq!(v["type"], "spawn");
        assert_eq!(v["sessionId"], "s1");
        let v: serde_json::Value =
            serde_json::to_value(KeeperMessage::Kill(Kill {
                session_id: "s1".into(),
            }))
            .unwrap();
        assert_eq!(v, serde_json::json!({"type":"kill","sessionId":"s1"}));
    }

    #[test]
    fn snapshot_result_shape() {
        let v: serde_json::Value =
            serde_json::to_value(KeeperMessage::SnapshotResult(SnapshotResult {
                session_id: "s1".into(),
                cols: 80,
                rows: 24,
                history_lines: 0,
                text: "\u{1b}[0;1mB".into(),
                cursor_x: 3,
                cursor_y: 7,
                alternate_on: false,
                pane_in_mode: false,
                mouse: false,
                mouse_sgr: false,
                app_cursor: false,
                bracketed_paste: false,
                history_size: 0,
                dead: false,
                foreground: "dash".into(),
                cwd: "/root".into(),
            }))
            .unwrap();
        assert_eq!(v["type"], "snapshotresult");
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["cursorX"], 3);
        assert_eq!(v["mouseSgr"], false);
        assert_eq!(v["foreground"], "dash");
        let back: KeeperMessage =
            serde_json::from_value(v).expect("snapshotresult round-trips");
        assert!(matches!(back, KeeperMessage::SnapshotResult(_)));
    }

    #[test]
    fn snapshot_history_lines_absent() {
        let v: serde_json::Value =
            serde_json::to_value(KeeperMessage::Snapshot(Snapshot {
                session_id: "s1".into(),
                history_lines: None,
            }))
            .unwrap();
        assert_eq!(v["type"], "snapshot");
        assert!(v.get("historyLines").is_none());
    }
}
