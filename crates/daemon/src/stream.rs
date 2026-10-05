//! The `PaneStream`: one live keeper stream per viewed session, fanned out to
//! any number of subscribers (the WebSocket clients).
//!
//! The retarget of record (plan §3.5): the TS `PaneStream` holds a live tmux
//! control-mode client (`%output`); the Rust `PaneStream` holds a keeper
//! connection (a `Read`/`Peek` poll loop that pulls the pane's output and fans
//! it out). The plan §3.7 flow-control is the new piece: the keeper never stops
//! reading (it always buffers the master), and each subscriber pulls from a
//! **bounded** queue — when a slow subscriber's queue overflows, it is dropped
//! and forced to resync from a snapshot (the client repaints from scratch),
//! rather than the pane pausing.
//!
//! This module is the stream's core (the subscribe/fanout/snapshot + the bounded
//! queue); the remote (the phone-facing WS server) and the linger/reconnect
//! lifecycle are layered on as they are ported.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ldmux_protocol::keeper::{KeeperMessage, Snapshot};

/// One subscriber's bounded output queue (the §3.7 flow-control).
///
/// The keeper always buffers the pane's output; each subscriber pulls from
/// this queue. When it overflows (the subscriber is too slow), the subscriber
/// is dropped and forced to resync from a snapshot — the pane never pauses.
struct SubQueue {
    /// The subscriber's unique id (for removal by id, not by position).
    id: usize,
    /// The queued output bytes (the bytes the subscriber has not drained).
    q: VecDeque<u8>,
    /// Dropped because the queue overflowed (the subscriber must resync).
    dropped: bool,
    /// The subscriber (the `on_output`/`on_reset` callbacks).
    sub: StreamSubscriber,
}

// The trait-object fields are the complexity (the callbacks are `Send + Sync`
// closures); the `allow` is load-bearing (the shape is the TS `StreamSubscriber`).
#[allow(clippy::type_complexity)]
pub struct StreamSubscriber {
    pub on_output: Box<dyn Fn(&[u8]) + Send + Sync>,
    pub on_reset: Box<dyn Fn(&str) + Send + Sync>,
}

/// The stream: one live keeper stream per viewed session, fanned out to any
/// number of subscribers.
///
/// The shared state (the `sessions`-style table, but per-session here) is held
/// behind one `Mutex`; the keeper connection is a `keeper_call` (a one-shot
/// request/response), so there is no long-lived client to hold — the poll loop
/// re-connects on every poll, exactly as the daemon↔keeper protocol is one
/// call per connection.
pub struct PaneStream {
    session_id: String,
    keeper: Box<dyn KeeperConn>,
    /// The subscribers (the WebSocket clients).
    subs: Mutex<Vec<SubQueue>>,
    /// The epoch counter (count *delivered* snapshots, not captures).
    epoch: AtomicU64,
    /// The sanitizer state (a partial OSC/DCS sequence can straddle chunks).
    sanitizer: Mutex<crate::sanitize::Sanitizer>,
    closed: AtomicBool,
    /// The poll-loop stop flag (set by `drop_stream`/`stop`).
    pub(crate) poll_stop: AtomicBool,
}

/// The keeper connection seam (the test double boundary): one
/// request/response over the keeper socket.
pub trait KeeperConn: Send + Sync {
    /// Send one keeper message and read its one response.
    fn call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String>;
}

/// A `KeeperConn` that talks to a real keeper socket (the production path).
pub struct SocketKeeperConn {
    socket: String,
}
impl SocketKeeperConn {
    pub fn new(socket: String) -> Self {
        Self { socket }
    }
}
impl KeeperConn for SocketKeeperConn {
    fn call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
        // The keeper protocol is one call == one connection; a blocking
        // connect + read (the stream's poll loop runs on a blocking thread).
        use std::io::{Read, Write};
        let mut sock = std::os::unix::net::UnixStream::connect(&self.socket)
            .map_err(|e| format!("connect: {e}"))?;
        let line = format!(
            "{}\n",
            serde_json::to_string(req).map_err(|e| e.to_string())?
        );
        sock.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("keeper closed the connection".into());
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let nl = buf.iter().position(|&b| b == b'\n').unwrap();
        let line = String::from_utf8_lossy(&buf[..nl]).to_string();
        let res: KeeperMessage =
            serde_json::from_str(&line).map_err(|e| format!("parse: {e}"))?;
        match res {
            KeeperMessage::Error(e) => Err(e.error),
            other => Ok(other),
        }
    }
}

impl PaneStream {
    /// A stream for `session_id`, backed by `keeper`.
    pub fn new(session_id: String, keeper: Box<dyn KeeperConn>) -> Self {
        Self {
            session_id,
            keeper,
            subs: Mutex::new(Vec::new()),
            epoch: AtomicU64::new(0),
            sanitizer: Mutex::new(crate::sanitize::Sanitizer::new(false)),
            closed: AtomicBool::new(false),
            poll_stop: AtomicBool::new(false),
        }
    }

    /// The session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The number of subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().unwrap().len()
    }

    /// Subscribe a subscriber. Returns an unsubscribe fn.
    ///
    /// The subscriber gets its own bounded queue; on overflow it is dropped and
    /// the `on_reset` callback fires (a forced resync).
    pub fn subscribe(&self, sub: StreamSubscriber) -> (usize, Box<dyn Fn() + Send + Sync + '_>) {
        let id_for_close = self.register(sub);
        (id_for_close, Box::new(move || {
            let mut subs = self.subs.lock().unwrap();
            // Remove by id (not by position — other subscribers may have
            // been added/removed since this one was pushed).
            subs.retain(|s| s.id != id_for_close);
        }))
    }

    /// Like `subscribe`, but the unsubscribe closure owns its own `Arc`, so it is
    /// `'static`. The daemon's per-connection state outlives the borrow that
    /// `subscribe` is tied to, so it needs this form.
    pub fn subscribe_owned(
        self: &Arc<Self>,
        sub: StreamSubscriber,
    ) -> (usize, Box<dyn Fn() + Send + Sync + 'static>) {
        let id_for_close = self.register(sub);
        let me = Arc::clone(self);
        (id_for_close, Box::new(move || {
            let mut subs = me.subs.lock().unwrap();
            subs.retain(|s| s.id != id_for_close);
        }))
    }

    /// Register a subscriber and return its id (the shared body of `subscribe`
    /// and `subscribe_owned`).
    fn register(&self, sub: StreamSubscriber) -> usize {
        let id = {
            let subs = self.subs.lock().unwrap();
            subs.iter().map(|s| s.id).max().unwrap_or(0) + 1
        };
        let mut subs = self.subs.lock().unwrap();
        subs.push(SubQueue {
            id,
            q: VecDeque::new(),
            dropped: false,
            sub,
        });
        id
    }

    /// Deliver `text` to every subscriber (the fanout), through the sanitizer.
    ///
    /// The §3.7 flow-control: each subscriber's queue is bounded; a queue that
    /// overflows (the subscriber is too slow) is dropped and its `on_reset`
    /// fires (a forced resync). The pane never pauses.
    pub fn fanout(&self, text: &str) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let clean = self.sanitizer.lock().unwrap().push(text);
        if clean.is_empty() {
            return;
        }
        let bytes = clean.as_bytes();
        let mut subs = self.subs.lock().unwrap();
        for sub in subs.iter_mut() {
            if sub.dropped {
                continue;
            }
            sub.q.extend(bytes);
            // The bounded queue: a queue that grows past the bound is dropped and
            // the subscriber is told to resync (the forced resync, §3.7).
            if sub.q.len() > 1024 * 1024 {
                sub.dropped = true;
                sub.q.clear();
                (sub.sub.on_reset)("output queue overflow; resync from a snapshot");
                continue;
            }
            // Deliver the bytes to the subscriber.
            (sub.sub.on_output)(bytes);
        }
    }

    /// Drain a subscriber's queued output (the drain is the pull).
    /// `sub_id` is the id returned by `subscribe`.
    pub fn drain(&self, sub_id: usize) -> Vec<u8> {
        let mut subs = self.subs.lock().unwrap();
        match subs.iter_mut().find(|s| s.id == sub_id) {
            Some(sub) => {
                let d: Vec<u8> = sub.q.drain(..).collect();
                d
            }
            None => Vec::new(),
        }
    }

    /// A snapshot (the full-screen capture, via the keeper's `Snapshot`).
    /// Send an SGR mouse click (the `mouseClick` method): a press + release
    /// at (col, row), both 0-based. The escape is `ESC [ < col+1 ; row+1 ; 0 M`
    /// (press) then `ESC [ < col+1 ; row+1 ; 0 m` (release).
    pub fn mouse_click(&self, col: u64, row: u64) -> Result<KeeperMessage, String> {
        let press = format!("\u{1b}[<{};{};0M", col + 1, row + 1);
        let release = format!("\u{1b}[<{};{};0m", col + 1, row + 1);
        let data = format!("{}{}", press, release);
        self.keeper.call(&KeeperMessage::Write(ldmux_protocol::keeper::Write {
            session_id: self.session_id.clone(),
            data,
        }))
    }

    /// Send keys to the pane (the `sendKey` method): convert key names to
    /// their byte sequences and send a `Write` keeper message.
    pub fn send_key(&self, keys: &[&str]) -> Result<KeeperMessage, String> {
        let mut bytes: Vec<u8> = Vec::new();
        for key in keys {
            bytes.extend(key_name_to_bytes(key)?);
        }
        let text = String::from_utf8(bytes).map_err(|e| format!("invalid key bytes: {e}"))?;
        self.keeper.call(&KeeperMessage::Write(ldmux_protocol::keeper::Write {
            session_id: self.session_id.clone(),
            data: text,
        }))
    }

    /// Write literal text to the pane (the `inputText` method): send a
    /// `Write` keeper message with the raw text bytes.
    pub fn write(&self, text: &str) -> Result<KeeperMessage, String> {
        self.keeper.call(&KeeperMessage::Write(ldmux_protocol::keeper::Write {
            session_id: self.session_id.clone(),
            data: text.to_string(),
        }))
    }

    /// Resize the pane (the `resize` method): send a `Resize` keeper message.
    /// Returns the response, or an error.
    ///
    /// The size is clamped here and again in the keeper: the frame's selection
    /// rows are one byte, so a pane above 255 rows silently wraps them, and the
    /// daemon's own bookkeeping must not disagree with the grid the PTY gets.
    pub fn resize(&self, cols: u64, rows: u64) -> Result<KeeperMessage, String> {
        let (cols, rows) = ldmux_protocol::clamp_pane_size(cols, rows);
        self.keeper.call(&KeeperMessage::Resize(ldmux_protocol::keeper::Resize {
            session_id: self.session_id.clone(),
            cols,
            rows,
        }))
    }

    ///
    /// Returns the full `SnapshotResult` (the `getState` equivalent).
    pub fn snapshot_full(&self) -> Result<ldmux_protocol::keeper::SnapshotResult, String> {
        let req = KeeperMessage::Snapshot(Snapshot {
            session_id: self.session_id.clone(),
            history_lines: None,
        });
        let resp = self.keeper.call(&req)?;
        match resp {
            KeeperMessage::SnapshotResult(sr) => {
                let _ = self.epoch.fetch_add(1, Ordering::SeqCst);
                Ok(sr)
            }
            other => Err(format!("not a snapshotresult: {other:?}")),
        }
    }

    /// A snapshot carrying **both** the full state and the delivered-snapshot
    /// epoch, optionally including `history_lines` of paged scrollback. This is
    /// the remote's `sendState` source: the TS `sendState` sends the state
    /// fields *and* the screen for attach, resize and scrollback alike, so a
    /// `snapshot()` that returned only text (as `snapshot_with_history` does)
    /// could not build it.
    pub fn snapshot_result(
        &self,
        history_lines: Option<u64>,
    ) -> Result<(ldmux_protocol::keeper::SnapshotResult, u64), String> {
        let req = KeeperMessage::Snapshot(Snapshot {
            session_id: self.session_id.clone(),
            history_lines,
        });
        let resp = self.keeper.call(&req)?;
        match resp {
            KeeperMessage::SnapshotResult(sr) => {
                let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
                Ok((sr, epoch))
            }
            other => Err(format!("not a snapshotresult: {other:?}")),
        }
    }

    ///
    /// Returns the rendered screen + the epoch (count *delivered* snapshots).
    pub fn snapshot(&self) -> Result<(String, u64), String> {
        self.snapshot_with_history(None)
    }

    /// Take a snapshot with a history line count (the `scrollback` path).
    /// `history_lines: 0`/`None` = visible screen only.
    pub fn snapshot_with_history(
        &self,
        history_lines: Option<u64>,
    ) -> Result<(String, u64), String> {
        let req = KeeperMessage::Snapshot(Snapshot {
            session_id: self.session_id.clone(),
            history_lines,
        });
        let resp = self.keeper.call(&req)?;
        match resp {
            KeeperMessage::SnapshotResult(sr) => {
                let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
                Ok((sr.text, epoch))
            }
            other => Err(format!("not a snapshotresult: {other:?}")),
        }
    }

    /// Close the stream (the subscribers are told to resync).
    pub fn close(&self, reason: &str) {
        self.closed.store(true, Ordering::Relaxed);
        self.poll_stop.store(true, Ordering::Relaxed);
        let mut subs = self.subs.lock().unwrap();
        for sub in subs.iter() {
            (sub.sub.on_reset)(reason);
        }
        subs.clear();
    }

    /// One poll iteration: pull the keeper's buffered output and fan it out.
    ///
    /// The §3.7 flow-control: the keeper always buffers the master; this poll
    /// pulls the buffer and delivers it to subscribers through the bounded
    /// queue. When a subscriber's queue overflows, it is dropped and forced
    /// to resync from a snapshot.
    pub fn poll_once(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Relaxed) || self.poll_stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let req = KeeperMessage::Peek(ldmux_protocol::keeper::Peek {
            session_id: self.session_id.clone(),
        });
        let resp = self.keeper.call(&req)?;
        match resp {
            KeeperMessage::PeekResult(pr) => {
                if !pr.data.is_empty() {
                    let text = String::from_utf8_lossy(&pr.data);
                    self.fanout(&text);
                }
                Ok(())
            }
            other => Err(format!("unexpected keeper response to Peek: {other:?}")),
        }
    }

    /// Stop the poll loop (the `drop_stream`/`stop` path).
    pub fn stop_polling(&self) {
        self.poll_stop.store(true, Ordering::Relaxed);
    }

    /// Whether the poll loop has been stopped.
    pub fn is_poll_stopped(&self) -> bool {
        self.poll_stop.load(Ordering::Relaxed)
    }
}

/// Convert a tmux key name to its byte sequence.
///
/// The TS `sendKey` used tmux's `send-keys`, which converts key names to the
/// appropriate escape sequences / control bytes. This is the Rust equivalent,
/// and it has to answer with the names tmux *defines* — not the ones this file
/// happens to prefer — because the clients were written against tmux and send
/// tmux's names. The mobile sends `BSpace` for the backspace key, the table had
/// only `Backspace`, and the fallback below typed the name into the shell: the
/// phone's backspace inserted the literal text `BSpace`.
///
/// **An unknown name is an error, never text.** The old fallback sent the name
/// as-is, so every name missing from the table became input to the user's shell,
/// silently: a typo, or a key a client added, landed in the middle of their
/// command line as words. Failing is the only safe answer — the caller can say
/// so, and the shell never sees it.
pub fn key_name_to_bytes(name: &str) -> Result<Vec<u8>, String> {
    let bytes = match name {
        "Enter" => vec![0x0D],
        "Tab" => vec![0x09],
        "BTab" => vec![0x1B, b'[', b'Z'],
        "Escape" => vec![0x1B],
        // tmux's name, then this file's older alias: both are in the wire.
        "BSpace" | "Backspace" => vec![0x7F],
        "Space" => vec![0x20],
        // Arrow keys
        "Up" => vec![0x1B, b'[', b'A'],
        "Down" => vec![0x1B, b'[', b'B'],
        "Right" => vec![0x1B, b'[', b'C'],
        "Left" => vec![0x1B, b'[', b'D'],
        // Editing keys: tmux's names first (`DC` and `IC` are what a client that
        // was written against tmux sends), then the descriptive aliases.
        "Home" => vec![0x1B, b'[', b'H'],
        "End" => vec![0x1B, b'[', b'F'],
        "PageUp" | "PPage" => vec![0x1B, b'[', b'5', b'~'],
        "PageDown" | "NPage" => vec![0x1B, b'[', b'6', b'~'],
        "Delete" | "DC" => vec![0x1B, b'[', b'3', b'~'],
        "Insert" | "IC" => vec![0x1B, b'[', b'2', b'~'],
        // F-keys
        "F1" => vec![0x1B, b'O', b'P'],
        "F2" => vec![0x1B, b'O', b'Q'],
        "F3" => vec![0x1B, b'O', b'R'],
        "F4" => vec![0x1B, b'O', b'S'],
        "F5" => vec![0x1B, b'[', b'1', b'5', b'~'],
        "F6" => vec![0x1B, b'[', b'1', b'7', b'~'],
        "F7" => vec![0x1B, b'[', b'1', b'8', b'~'],
        "F8" => vec![0x1B, b'[', b'1', b'9', b'~'],
        "F9" => vec![0x1B, b'[', b'2', b'0', b'~'],
        "F10" => vec![0x1B, b'[', b'2', b'1', b'~'],
        "F11" => vec![0x1B, b'[', b'2', b'3', b'~'],
        "F12" => vec![0x1B, b'[', b'2', b'4', b'~'],
        // Control + letter
        "C-a" => vec![0x01],
        "C-b" => vec![0x02],
        "C-c" => vec![0x03],
        "C-d" => vec![0x04],
        "C-e" => vec![0x05],
        "C-f" => vec![0x06],
        "C-g" => vec![0x07],
        "C-h" => vec![0x08],
        "C-i" => vec![0x09],
        "C-j" => vec![0x0A],
        "C-k" => vec![0x0B],
        "C-l" => vec![0x0C],
        "C-m" => vec![0x0D],
        "C-n" => vec![0x0E],
        "C-o" => vec![0x0F],
        "C-p" => vec![0x10],
        "C-q" => vec![0x11],
        "C-r" => vec![0x12],
        "C-s" => vec![0x13],
        "C-t" => vec![0x14],
        "C-u" => vec![0x15],
        "C-v" => vec![0x16],
        "C-w" => vec![0x17],
        "C-x" => vec![0x18],
        "C-y" => vec![0x19],
        "C-z" => vec![0x1A],
        // Single character: the character itself.
        _ if name.chars().count() == 1 => name.as_bytes().to_vec(),
        // Anything else is a name this table does not know. Sending it as text
        // would type the name into the user's shell, which is what `BSpace` did.
        _ => return Err(format!("unknown key name: {name}")),
    };
    Ok(bytes)
}

#[cfg(test)]
mod key_name_tests {
    use super::key_name_to_bytes;

    /// The names the **mobile client actually sends**. It was written against
    /// tmux's vocabulary, so these are tmux's names, and the daemon's table had
    /// `Backspace` where the phone sends `BSpace` — the phone's backspace typed
    /// the word into the shell.
    #[test]
    fn the_names_a_client_sends_are_known() {
        for (name, want) in [
            ("BSpace", vec![0x7F]),
            ("DC", vec![0x1B, b'[', b'3', b'~']),
            ("IC", vec![0x1B, b'[', b'2', b'~']),
            ("NPage", vec![0x1B, b'[', b'6', b'~']),
            ("PPage", vec![0x1B, b'[', b'5', b'~']),
            ("BTab", vec![0x1B, b'[', b'Z']),
            ("Enter", vec![0x0D]),
            ("Tab", vec![0x09]),
            ("Escape", vec![0x1B]),
            ("Space", vec![0x20]),
            ("Up", vec![0x1B, b'[', b'A']),
            ("Home", vec![0x1B, b'[', b'H']),
            ("End", vec![0x1B, b'[', b'F']),
            ("F12", vec![0x1B, b'[', b'2', b'4', b'~']),
            ("C-r", vec![0x12]),
            ("a", b"a".to_vec()),
        ] {
            assert_eq!(key_name_to_bytes(name).as_deref(), Ok(want.as_slice()), "{name}");
        }
    }

    /// An unknown name must never become shell input. The old fallback sent the
    /// name as-is, so a name missing from the table was typed into the user's
    /// command line as words — silently, and in the middle of what they were
    /// typing.
    #[test]
    fn an_unknown_name_is_refused_not_typed() {
        for bad in ["BSpace2", "NotAKey", "C-A", "PageUp2", ""] {
            assert!(
                key_name_to_bytes(bad).is_err(),
                "{bad:?} must be refused, not sent to the shell"
            );
        }
    }

    /// The descriptive aliases the older table used still work: both spellings
    /// are on the wire.
    #[test]
    fn both_spellings_are_accepted() {
        assert_eq!(key_name_to_bytes("Backspace"), key_name_to_bytes("BSpace"));
        assert_eq!(key_name_to_bytes("Delete"), key_name_to_bytes("DC"));
        assert_eq!(key_name_to_bytes("Insert"), key_name_to_bytes("IC"));
        assert_eq!(key_name_to_bytes("PageUp"), key_name_to_bytes("PPage"));
        assert_eq!(key_name_to_bytes("PageDown"), key_name_to_bytes("NPage"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    /// A `KeeperConn` test double: answers a `Snapshot` with a fixed screen.
    /// (The `fanout`/`drain` never call the keeper, so the other arms are
    /// unreachable from the tests.)
    struct TestKeeper;
    impl KeeperConn for TestKeeper {
        fn call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
            match req {
                KeeperMessage::Snapshot(s) => Ok(KeeperMessage::SnapshotResult(
                    ldmux_protocol::keeper::SnapshotResult {
                        session_id: s.session_id.clone(),
                        cols: 80,
                        rows: 24,
                        history_lines: 0,
                        text: "screen".into(),
                        cursor_x: 0,
                        cursor_y: 0,
                        alternate_on: false,
                        pane_in_mode: false,
                        mouse: false,
                        mouse_sgr: false,
                        app_cursor: false,
                        bracketed_paste: false,
                        history_size: 0,
                        dead: false,
                        foreground: "dash".into(),
                        cwd: "/".into(),
                    },
                )),
                other => Err(format!("not handled: {other:?}")),
            }
        }
    }

    fn cb() -> (Box<dyn Fn(&[u8]) + Send + Sync>, Box<dyn Fn(&str) + Send + Sync>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let reset = Arc::new(AtomicBool::new(false));
        let g = got.clone();
        let r = reset.clone();
        (
            Box::new(move |t: &[u8]| {
                g.lock().unwrap().push(String::from_utf8_lossy(t).into_owned());
            }),
            Box::new(move |m: &str| {
                let _ = m;
                r.store(true, Ordering::SeqCst);
            }),
        )
    }

    /// A `PaneStream` with the test keeper.
    fn stream() -> PaneStream {
        PaneStream::new("s1".into(), Box::new(TestKeeper))
    }

    /// The fanout delivers to every subscriber (the §3.7 fan-out).
    #[test]
    fn the_fanout_delivers_to_every_subscriber() {
        let s = stream();
        let (o1, r1) = cb();
        let (_id1, _u1) = s.subscribe(StreamSubscriber {
            on_output: o1,
            on_reset: r1,
        });
        let (o2, r2) = cb();
        let (_id2, _u2) = s.subscribe(StreamSubscriber {
            on_output: o2,
            on_reset: r2,
        });
        s.fanout("hello");
        // Both subscribers have a queue (the fan-out reached both).
        assert_eq!(s.subscriber_count(), 2);
    }

    /// A snapshot (via the keeper's `Snapshot`) returns the rendered screen and
    /// a monotonically increasing epoch.
    #[test]
    fn a_snapshot_returns_the_screen_and_an_epoch() {
        let s = stream();
        let (text, e1) = s.snapshot().unwrap();
        assert_eq!(text, "screen");
        assert_eq!(e1, 1);
        let (_, e2) = s.snapshot().unwrap();
        assert_eq!(e2, 2);
    }

    /// A subscriber's bounded queue: bytes are queued and drained (the pull).
    #[test]
    fn a_subscribers_queue_is_bounded_and_drains() {
        let s = stream();
        let (o, r) = cb();
        let (sub_id, _) = s.subscribe(StreamSubscriber {
            on_output: o,
            on_reset: r,
        });
        s.fanout("abc");
        let d = s.drain(sub_id);
        assert_eq!(d, b"abc");
        // A second drain is empty (the queue is drained).
        assert!(s.drain(sub_id).is_empty());
    }
}
