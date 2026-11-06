//! JSON control messages carried as text frames. Pane output travels as binary
//! OUTPUT frames; everything in the other direction is one of these JSON
//! shapes.
//!
//! Port of `packages/protocol/src/messages.ts` (the TS original for the
//! client-facing union; deleted at D4, readable at git commit `6b3efb8`) plus the keeper
//! messages, which have no TS source of record and are specified here.
//!
//! The wire tag is `type`, lower-cased, and each message's fields are
//! camelCase — matching the TS exactly. The one modelling choice over the TS:
//! `DesktopRequest` (`{type:"desktop"}`) and `DesktopMessage`
//! (`{type:"desktop",inventory}`) both use the tag `desktop`, and a Rust
//! serde union cannot carry two variants under one tag, so they are one
//! `Desktop` variant with an optional `inventory` (absent on a request, present
//! on the answer). The emitted JSON is identical in both directions.

use serde::{Deserialize, Serialize};

use crate::rpc::{DesktopInventory, DeviceInfo, SessionInfo, SessionOwner};

/// The protocol version, carried by `Caps.protocolVersion` and
/// `PingResult.version` (see `rpc::DAEMON_PROTOCOL_VERSION`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The control-message union. Only these shapes are sent in v1: the v0.1
/// draft's `HELLO`/`resumeToken`, `SESSION_STATE` and `CLOSE` were cut
/// (see `docs/ARCHITECTURE.md#4-the-wire`), so they are not modelled here: a type nothing can send
/// or receive is a contract nobody can keep.
///
/// **The wire itself is written by `crates/daemon/src/remote.rs`** (raw `serde_json::json!`)
/// and parsed by the app's own readers. This union is the *test* side of the contract: it
/// pins the key names and the tag vocabulary, so a rename shows up here before it shows up
/// as a phone that cannot attach.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Caps(Caps),
    Snapshot(Snapshot),
    Ping(Ping),
    Pong(Pong),
    Error(Error),
    Hello(Hello),
    Attach(Attach),
    Owned(Owned),
    MouseDelivered(MouseDelivered),
    Resize(Resize),
    MouseClick(MouseClick),
    Input(Input),
    Key(Key),
    Scrollback(Scrollback),
    List(List),
    New(New),
    Created(Created),
    Kill(Kill),
    Close(Close),
    Sessions(Sessions),
    Desktop(Desktop),
    MakeLive(MakeLive),
    CloseLive(CloseLive),
    Live(Live),
}

/// The control-message tag, as a const set (for the daemon's dispatch).
pub mod m {
    pub const CAPS: &str = "caps";
    pub const SNAPSHOT: &str = "snapshot";
    pub const PING: &str = "ping";
    pub const PONG: &str = "pong";
    pub const ERROR: &str = "error";
    pub const HELLO: &str = "hello";
    pub const ATTACH: &str = "attach";
    pub const OWNED: &str = "owned";
    pub const MOUSE_DELIVERED: &str = "mousedelivered";
    pub const RESIZE: &str = "resize";
    pub const MOUSE_CLICK: &str = "mouseclick";
    pub const INPUT: &str = "input";
    pub const KEY: &str = "key";
    pub const SCROLLBACK: &str = "scrollback";
    pub const LIST: &str = "list";
    pub const NEW: &str = "new";
    pub const CREATED: &str = "created";
    pub const KILL: &str = "kill";
    pub const CLOSE: &str = "close";
    pub const SESSIONS: &str = "sessions";
    pub const DESKTOP: &str = "desktop";
    pub const MAKE_LIVE: &str = "makelive";
    pub const CLOSE_LIVE: &str = "closelive";
    pub const LIVE: &str = "live";
}

/// Server → client: the connection's capabilities and starting geometry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    pub protocol_version: u32,
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    pub snapshot_epoch: u64,
    pub capabilities: CapsCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CapsCapabilities {
    /// Whether this server serves older pages of a pane's scrollback (the
    /// `scrollback` message). The sibling field was `controlMode`, a tmux
    /// mechanism that no longer exists and that nothing read; it was removed
    /// with the rest of the tmux vocabulary (2026-10-05).
    pub scrollback: bool,
    /// Whether `key` accepts the `S-` (shift) modifier on named keys. An older daemon
    /// refuses a whole `key` message it does not know, so a client sends `S-` only
    /// when this is true; absent reads as false.
    #[serde(default)]
    pub shift_keys: bool,
}

/// Server → client: snapshot of the visible screen + terminal state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    /// Visible screen with SGR escapes preserved.
    pub screen: String,
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub alternate_on: bool,
    pub bracketed_paste: bool,
    pub mouse: bool,
    /// The program negotiated SGR mouse reporting, so a synthesized click is
    /// valid.
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub history_size: u64,
    pub snapshot_epoch: u64,
    /// True when `screen` includes paged scrollback (answer to `scrollback`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<bool>,
}

/// A liveness question; `Pong` answers it with the same id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Ping {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pong {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    pub code: String,
    pub message: String,
}

/// Remote client → server: who this connection is (ADR-0010). Sent once, before
/// anything else.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub device: DeviceInfo,
}

/// Remote client → server: subscribe this connection to one session's stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attach {
    pub session_id: String,
    /// The grid the client will paint. Omitted = leave the pane alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cols: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// The user was told who holds this shell and chose to take it anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takeover: Option<bool>,
}

/// Server → client: a shell is held by another device, or was taken from you.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Owned {
    pub session_id: String,
    /// `null` when nobody holds it (the holder left).
    pub owner: Option<SessionOwner>,
}

/// The answer to a click: the server put it into a pane whose program asked
/// for mouse input, so the click is the tap and nothing else should follow it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MouseDelivered {
    pub col: u64,
    pub row: u64,
}

/// Remote client → server: the viewport changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Resize {
    pub cols: u64,
    pub rows: u64,
}

/// Remote client → server: the user tapped a cell. Coordinates are 0-based.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MouseClick {
    pub col: u64,
    pub row: u64,
}

/// Remote client → server: type literal text into the attached pane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub data: String,
}

/// Remote client → server: send named keys (Enter, C-c, Up, ...).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    pub keys: Vec<String>,
}

/// Remote client → server: re-send the pane including paged scrollback.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Scrollback {
    pub session_id: String,
    /// Lines of history to request. `0` (or omitted) = re-send just the
    /// visible screen (the return-to-bottom action).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
}

/// Remote client → server: list sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct List {}

/// Remote client → server: open a new session (FR-10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct New {
    /// Accepted and ignored: a session is a shell (2026-11-05). The field stays on the
    /// wire so an older client is not refused, and nothing spawns a program by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The answer to `new`: which session was just made.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub session_id: String,
    pub cwd: String,
}

/// Remote client → server: end a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Kill {
    pub session_id: String,
}

/// Remote client → server: the user closed this shell in the UI.
/// Deliberately **not** `kill` (ADR-0010).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Close {
    pub session_id: String,
}

/// Server → client: answer to `list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Sessions {
    pub sessions: Vec<SessionInfo>,
}

/// `desktop` on the wire, both directions: a request has no `inventory`, the
/// answer carries it. (See the module doc for why this is one variant.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Desktop {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<DesktopInventory>,
}

/// Remote client → server: put this shell on the desktop's screen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MakeLive {
    pub session_id: String,
    pub workspace: String,
}

/// Remote client → server: take this shell's desktop window away.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CloseLive {
    pub session_id: String,
}

/// Server → client: what came of a make-live or close-live request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Live {
    pub session_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// The keeper (daemon↔keeper) messages live in the `crate::keeper` module:
// they share names with some client messages (`Kill`, `Resize`, `Snapshot`,
// `List`, `Close`) but have different fields, so they are modelled separately.

#[cfg(test)]
mod tests {
    use super::*;

    /// The control union's wire shape, pinned from the TS source of record.
    /// These are the same cases the TS `messages` contract asserts: the tag is
    /// `type`, the fields are camelCase, and the `desktop` tag carries an
    /// optional inventory.

    #[test]
    fn caps_wire_shape() {
        let c = Caps {
            protocol_version: 1,
            session_id: "s1".into(),
            cols: 80,
            rows: 24,
            snapshot_epoch: 7,
            capabilities: CapsCapabilities {
                scrollback: true,
                shift_keys: true,
            },
        };
        let v: serde_json::Value = serde_json::to_value(Message::Caps(c)).unwrap();
        assert_eq!(v["type"], "caps");
        assert_eq!(v["protocolVersion"], 1);
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["capabilities"]["scrollback"], true);
        assert_eq!(v["capabilities"]["shiftKeys"], true);
    }

    #[test]
    fn caps_from_an_older_daemon_has_no_shift_keys() {
        let c: Caps = serde_json::from_str(
            r#"{"protocolVersion":1,"sessionId":"s","cols":80,"rows":24,"snapshotEpoch":1,"capabilities":{"scrollback":true}}"#,
        )
        .unwrap();
        assert!(c.capabilities.scrollback);
        assert!(!c.capabilities.shift_keys);
    }

    #[test]
    fn attach_omits_absent_optionals() {
        let a = Attach {
            session_id: "s1".into(),
            cols: None,
            rows: None,
            takeover: None,
        };
        let s = serde_json::to_string(&Message::Attach(a)).unwrap();
        assert_eq!(s, r#"{"type":"attach","sessionId":"s1"}"#);
    }

    #[test]
    fn desktop_request_has_no_inventory_result_has_one() {
        let req = Message::Desktop(Desktop { inventory: None });
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"type":"desktop"}"#
        );
        let res = Message::Desktop(Desktop {
            inventory: Some(DesktopInventory {
                wm: "i3".into(),
                workspaces: vec![],
                windows: vec![],
                detail: None,
            }),
        });
        let v: serde_json::Value = serde_json::to_value(res).unwrap();
        assert_eq!(v["type"], "desktop");
        assert_eq!(v["inventory"]["wm"], "i3");
    }

    #[test]
    fn owned_owner_is_nullable() {
        let owned = Message::Owned(Owned {
            session_id: "s1".into(),
            owner: None,
        });
        let v: serde_json::Value = serde_json::to_value(owned).unwrap();
        assert_eq!(v["type"], "owned");
        assert!(v["owner"].is_null());
    }

    #[test]
    fn snapshot_round_trips_with_history_absent() {
        let snap = Snapshot {
            session_id: "s1".into(),
            cols: 80,
            rows: 24,
            screen: "hi".into(),
            cursor_x: 0,
            cursor_y: 0,
            alternate_on: false,
            bracketed_paste: true,
            mouse: false,
            mouse_sgr: false,
            app_cursor: false,
            history_size: 0,
            snapshot_epoch: 1,
            history: None,
        };
        let s = serde_json::to_string(&Message::Snapshot(snap)).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        match back {
            Message::Snapshot(s) => {
                assert_eq!(s.session_id, "s1");
                assert!(s.history.is_none(), "history must be absent");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn new_session_kind_is_a_plain_string() {
        let new = Message::New(New {
            kind: Some("shell".into()),
            cwd: None,
            title: None,
        });
        let v: serde_json::Value = serde_json::to_value(new).unwrap();
        assert_eq!(v["type"], "new");
        assert_eq!(v["kind"], "shell");
        assert!(v.get("cwd").is_none());
    }

    #[test]
    fn snapshot_full_wire_shape_is_camel_case() {
        let snap = Snapshot {
            session_id: "s1".into(),
            cols: 80,
            rows: 24,
            screen: "x".into(),
            cursor_x: 1,
            cursor_y: 2,
            alternate_on: false,
            bracketed_paste: true,
            mouse: false,
            mouse_sgr: true,
            app_cursor: false,
            history_size: 0,
            snapshot_epoch: 5,
            history: Some(true),
        };
        let v: serde_json::Value = serde_json::to_value(Message::Snapshot(snap)).unwrap();
        let keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        let want = [
            "type",
            "sessionId",
            "cols",
            "rows",
            "screen",
            "cursorX",
            "cursorY",
            "alternateOn",
            "bracketedPaste",
            "mouse",
            "mouseSgr",
            "appCursor",
            "historySize",
            "snapshotEpoch",
            "history",
        ];
        for k in want {
            assert!(v.get(k).is_some(), "missing key {k} in {v:?}");
        }
        assert_eq!(v["type"], "snapshot");
        assert_eq!(keys.len(), want.len(), "unexpected extra keys: {keys:?}");
    }

    #[test]
    fn list_is_a_bare_tag() {
        let v: serde_json::Value = serde_json::to_value(Message::List(List {})).unwrap();
        assert_eq!(v, serde_json::json!({"type":"list"}));
    }
}
