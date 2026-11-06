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
    Ack(Ack),
    Error(Error),
}

/// The terminal type the session's emulator actually implements
/// (libghostty-vt is an xterm-256color-compatible terminal). Both the keeper
/// (which pins it onto every child) and the daemon (which states it in the
/// `Spawn.env` it sends) resolve it from here, so the two can never drift.
pub const CHILD_TERM: &str = "xterm-256color";
/// The emulator renders 24-bit colour, so say so.
pub const CHILD_COLORTERM: &str = "truecolor";

/// Daemon → keeper: spawn a session (a PTY + shell + one emulator task).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Spawn {
    pub session_id: String,
    pub cols: u64,
    pub rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The argv to run instead of the keeper's login shell. `command[0]` is the program.
    ///
    /// **The daemon never sets it** — every session is the user's shell, and the `pi` kind
    /// that used it is gone. It stays because the keeper's own tests need it: the child
    /// supervision rules (SIGPIPE at its default, a shell that ignores SIGHUP, a writer that
    /// blocks, exit codes, zombies) can only be exercised by spawning a specific program, and
    /// a test that cannot name one cannot test the keeper.
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
    /// Take at most this many bytes, and answer in the compact form (`data64`,
    /// `more`). Absent = the original contract: take everything, answer with a
    /// JSON byte array. A keeper that predates the field ignores it, which is how
    /// the daemon recognises one (see [`PeekResult::bytes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

impl Peek {
    /// The original, unbounded peek.
    pub fn all(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            max_bytes: None,
        }
    }
    /// A bounded peek (the compact answer).
    pub fn bounded(session_id: impl Into<String>, max_bytes: u64) -> Self {
        Self {
            session_id: session_id.into(),
            max_bytes: Some(max_bytes),
        }
    }
}

/// The most a bounded peek returns, whatever the request asked.
pub const PEEK_MAX_BYTES: u64 = 256 * 1024;
/// What the daemon asks for per peek.
pub const PEEK_CHUNK_BYTES: u64 = 64 * 1024;

/// Keeper → daemon: the drained bytes (empty when the buffer is empty).
///
/// `data` is the raw pty bytes as a JSON byte array (the skeleton's encoding;
/// a framed binary stream replaces it when the stream direction goes live).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PeekResult {
    pub session_id: String,
    /// The bytes as a JSON array of integers: the original encoding, used when the
    /// request had no `max_bytes`.
    #[serde(default)]
    pub data: Vec<u8>,
    /// The bytes, base64 (RFC 4648, padded): the answer to a bounded peek. About
    /// a third the size of the array form and one allocation to decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data64: Option<String>,
    /// Bytes remain buffered after this chunk (a bounded peek only): ask again
    /// without waiting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    /// Output was discarded because nobody was reading it fast enough and the
    /// keeper's buffer hit its cap. A viewer must resync from a snapshot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,
}

impl PeekResult {
    /// The raw bytes, from whichever encoding the keeper answered in.
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        match &self.data64 {
            Some(b) => crate::base64::decode(b).ok_or_else(|| "peek: invalid base64".to_string()),
            None => Ok(self.data.clone()),
        }
    }

    /// Consume into the raw bytes without copying the array form.
    pub fn into_bytes(self) -> Result<Vec<u8>, String> {
        match self.data64 {
            Some(b) => crate::base64::decode(&b).ok_or_else(|| "peek: invalid base64".to_string()),
            None => Ok(self.data),
        }
    }

    /// A compact answer carrying `bytes`.
    pub fn compact(session_id: String, bytes: &[u8], more: bool, lost: bool) -> Self {
        Self {
            session_id,
            data: Vec::new(),
            data64: Some(crate::base64::encode(bytes)),
            more,
            lost,
        }
    }
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
    /// Hand back (and clear) the session's not-yet-peeked output with the
    /// render. Those bytes are already in the render — the emulator was fed
    /// them — so a stream that has just taken a snapshot must not replay them
    /// as live output. Off for every other caller (a CLI capture must not eat
    /// a viewer's output).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drain_output: bool,
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
    /// Every line the pane has, history and live: `first_line = total_rows - rows`. The keeper's
    /// own numbering, so a client can name a line rather than count rows it happens to hold.
    /// Absent from an older keeper's answer, which a client must treat as "no line identity".
    #[serde(default)]
    pub total_rows: u64,
    pub text: String,
    // The pane's state (plan §3.4: the `display-message` fields, read direct
    // from ghostty-vt — not a parsed format string). See each field's source.
    pub cursor_x: u64,
    pub cursor_y: u64,
    /// On the alternate screen (DEC 1047) — no scrollback, so `history_lines`
    /// is meaningless here and the client must not request it.
    pub alternate_on: bool,
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
    /// The output that was buffered but not yet peeked when the render was taken
    /// (only when the request set `drain_output`). Already part of `text`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<u8>,
}

/// Keeper → daemon: report state for a session (the answer to a Read/State
/// and each entry of a List). It carries the pane's state (the display-message
/// fields, read direct from ghostty-vt) so the daemon can read liveness without
/// paying for a `Snapshot` render.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub session_id: String,
    /// The pane's shell pid: the session's process-group leader.
    ///
    /// The daemon does not read it (it kills by session, through `Kill`) and the phone never
    /// sees a keeper message. The keeper's own tests use it to assert that a spawn produced a
    /// process and that a kill took it, which is the one place a pid is the fact under test.
    #[serde(default)]
    pub pid: i32,
    pub cols: u64,
    pub rows: u64,
    pub dead: bool,
    /// The cursor column (0-based), from the emulator.
    pub cursor_x: u64,
    /// The cursor row (0-based), from the emulator.
    pub cursor_y: u64,
    /// The alternate screen is active (from `active_screen`).
    pub alternate_on: bool,
    /// The pane negotiated mouse reporting (from `mouse_tracking`).
    pub mouse: bool,
    /// The pane negotiated SGR mouse (DEC 1006).
    pub mouse_sgr: bool,
    /// Application cursor keys (DECCKM, DEC 1).
    pub app_cursor: bool,
    /// Bracketed paste (DEC 2004) is on.
    pub bracketed_paste: bool,
    /// The Kitty keyboard protocol flags the program has set (`CSI > Ps u`, a
    /// push, `CSI = Ps ; Pm u`, a set, `CSI < u`, a pop), as a bitset.
    ///
    /// Additive with a `serde(default)`: an older keeper does not send it and
    /// the daemon then encodes keys exactly as it did before (0 = protocol
    /// off), which is the same thing a program that never asked for the
    /// protocol sees.
    #[serde(default)]
    pub kitty_keyboard: u8,
    /// The scrollback rows (from `scrollback_rows`).
    pub history_size: u64,
    /// The foreground command (`/proc/<pid>/comm`); empty for a dead pane.
    pub foreground: String,
    /// The working directory (`/proc/<pid>/cwd`); empty for a dead pane.
    pub cwd: String,
    /// How the shell ended, for a dead pane: its exit status when it exited ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// ... or the signal that killed it. Both absent: unknown (the child was
    /// reaped by someone else) or the pane is alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
}

/// Daemon → keeper: list the keeper's sessions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct List {}

/// The answer to `List`: every session the keeper holds, as its state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub sessions: Vec<State>,
    /// The pane's own activity clock, in unix milliseconds, keyed by session id.
    ///
    /// It fills `lastActivityAt`: a session is stamped every time its pane
    /// takes input or produces output, and the launcher's notion of
    /// "recent" came from it. The keeper reads the master for every session on
    /// its own (plan §3.3 — the kernel buffer must never fill), so it is the
    /// only thing in the system that knows when a shell last did anything.
    ///
    /// It rides on `List`, not on `State`: three callers ask `List` and nothing
    /// asks a `State` for an activity clock, so the field goes where the demand
    /// is. `#[serde(default)]` so a keeper and a daemon rolled apart still talk.
    #[serde(default)]
    pub activity: std::collections::BTreeMap<String, u64>,
    /// When a **person** last touched the pane, in unix milliseconds, keyed by session id.
    ///
    /// Separate from [`Self::activity`], which is output too, and the separation is the whole
    /// point: a full-screen program that repaints — a `pi` session's spinner, a clock, a progress
    /// bar — stamps `activity` every second for ever, so ordering a launcher by it ranks programs
    /// by how much they animate rather than by what the person was last working in. Measured
    /// 2026-11-06: seven `pi` shells sat above a shell created seconds earlier, all stamped in the
    /// same second as the query. `touched` moves only on input (a keystroke, a paste, a named key)
    /// and when a client attaches, which is what "recent" means to a person.
    #[serde(default)]
    pub touched: std::collections::BTreeMap<String, u64>,
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
        let v: serde_json::Value = serde_json::to_value(KeeperMessage::Spawn(Spawn {
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
        let v: serde_json::Value = serde_json::to_value(KeeperMessage::Kill(Kill {
            session_id: "s1".into(),
        }))
        .unwrap();
        assert_eq!(v, serde_json::json!({"type":"kill","sessionId":"s1"}));
    }

    #[test]
    fn snapshot_result_shape() {
        let v: serde_json::Value =
            serde_json::to_value(KeeperMessage::SnapshotResult(SnapshotResult {
                total_rows: 0,
                session_id: "s1".into(),
                cols: 80,
                rows: 24,
                history_lines: 0,
                text: "\u{1b}[0;1mB".into(),
                cursor_x: 3,
                cursor_y: 7,
                alternate_on: false,
                mouse: false,
                mouse_sgr: false,
                app_cursor: false,
                bracketed_paste: false,
                history_size: 0,
                dead: false,
                foreground: "dash".into(),
                cwd: "/root".into(),
                tail: Vec::new(),
            }))
            .unwrap();
        assert_eq!(v["type"], "snapshotresult");
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["cursorX"], 3);
        assert_eq!(v["mouseSgr"], false);
        assert_eq!(v["foreground"], "dash");
        let back: KeeperMessage = serde_json::from_value(v).expect("snapshotresult round-trips");
        assert!(matches!(back, KeeperMessage::SnapshotResult(_)));
    }

    #[test]
    fn snapshot_history_lines_absent() {
        let v: serde_json::Value = serde_json::to_value(KeeperMessage::Snapshot(Snapshot {
            session_id: "s1".into(),
            history_lines: None,
            drain_output: false,
        }))
        .unwrap();
        assert_eq!(v["type"], "snapshot");
        assert!(v.get("historyLines").is_none());
    }

    /// The two peek encodings, and the compatibility rules the daemon and keeper
    /// rely on across a restart of only one of them.
    #[test]
    fn peek_encodings_and_compatibility() {
        // Request: `maxBytes` is absent for the original peek (an old keeper sees
        // exactly what it always saw) and present for a bounded one.
        let old = serde_json::to_value(KeeperMessage::Peek(Peek::all("s1"))).unwrap();
        assert_eq!(old, serde_json::json!({"type":"peek","sessionId":"s1"}));
        let new = serde_json::to_value(KeeperMessage::Peek(Peek::bounded("s1", 65536))).unwrap();
        assert_eq!(new["maxBytes"], 65536);
        // An old daemon's request parses on a new keeper, with no bound.
        let KeeperMessage::Peek(p) = serde_json::from_value(old).unwrap() else {
            panic!()
        };
        assert_eq!(p.max_bytes, None);

        // Answer, compact: base64, no byte array, `more` only when true.
        let c = PeekResult::compact("s1".into(), b"hello\x1b[0m", true, false);
        let v = serde_json::to_value(KeeperMessage::PeekResult(c.clone())).unwrap();
        assert_eq!(v["data64"], "aGVsbG8bWzBt");
        assert_eq!(v["more"], true);
        assert_eq!(v["data"], serde_json::json!([]));
        assert_eq!(c.bytes().unwrap(), b"hello\x1b[0m");

        // Answer, original: what an old keeper sends (no `data64`, no `more`)
        // parses on a new daemon, and a new keeper's original-form answer parses
        // on an old daemon (it has `data`).
        let legacy = serde_json::json!({"type":"peekresult","sessionId":"s1","data":[104,105]});
        let KeeperMessage::PeekResult(r) = serde_json::from_value(legacy).unwrap() else {
            panic!()
        };
        assert_eq!(r.into_bytes().unwrap(), b"hi");
        let orig = serde_json::to_value(KeeperMessage::PeekResult(PeekResult {
            session_id: "s1".into(),
            data: vec![1, 2],
            data64: None,
            more: false,
            lost: false,
        }))
        .unwrap();
        assert_eq!(orig["data"], serde_json::json!([1, 2]));
        assert!(orig.get("data64").is_none() && orig.get("more").is_none());

        // Bad base64 is an error, not silence.
        let bad = PeekResult {
            data64: Some("***".into()),
            ..PeekResult::compact("s".into(), b"", false, false)
        };
        assert!(bad.bytes().is_err());
    }
}
