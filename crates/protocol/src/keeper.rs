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
    Snapshot(Snapshot),
    State(State),
    List(List),
    ChildInfo(ChildInfo),
    ChildExited(ChildExited),
    Close(Close),
}

/// Keeper message tags.
pub mod k {
    pub const SPAWN: &str = "spawn";
    pub const KILL: &str = "kill";
    pub const WRITE: &str = "write";
    pub const RESIZE: &str = "resize";
    pub const READ: &str = "read";
    pub const SNAPSHOT: &str = "snapshot";
    pub const STATE: &str = "state";
    pub const LIST: &str = "list";
    pub const CHILD_INFO: &str = "childinfo";
    pub const CHILD_EXITED: &str = "childexited";
    pub const CLOSE: &str = "close";
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

/// Keeper → daemon: report state for a session (the answer to a Read/State).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    pub dead: bool,
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
