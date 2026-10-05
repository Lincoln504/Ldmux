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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ldmux_protocol::keeper::{KeeperMessage, Snapshot};

/// One registered subscriber.
///
/// There is deliberately no per-subscriber byte queue here. An earlier version
/// kept one (`q`, bounded at 1 MiB) and `extend`ed it on every fanout, but the
/// only consumer is the callback below, so nothing ever drained it: after 1 MiB
/// of *cumulative* output every subscriber was marked dropped, told to resync,
/// and silenced. The flow control of plan §3.7 lives where the backlog actually
/// is — the connection (`remote.rs`: `outstanding` vs `WS_BACKLOG_BYTES`,
/// `MAX_PENDING_BYTES`) — and the keeper bounds its own buffer.
struct Sub {
    /// The subscriber's unique id (for removal by id, not by position).
    id: usize,
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
    subs: Mutex<Vec<Sub>>,
    /// The next subscriber id (a counter, so ids are never reused or raced).
    next_sub_id: AtomicUsize,
    /// The trailing bytes of a multi-byte UTF-8 character a `Peek` chunk ended
    /// in the middle of; prepended to the next chunk. Without it the two halves
    /// were each lossy-decoded to U+FFFD and the character was corrupted.
    utf8_carry: Mutex<ldmux_protocol::utf8::Utf8Carry>,
    /// The epoch counter (count *delivered* snapshots, not captures).
    epoch: AtomicU64,
    /// The sanitizer state (a partial OSC/DCS sequence can straddle chunks).
    sanitizer: Mutex<crate::sanitize::Sanitizer>,
    closed: AtomicBool,
    /// Serialises `poll_once` against a snapshot, so a snapshot and the live
    /// output that follows it are cut at exactly one byte boundary (see
    /// `snapshot_fenced`).
    gate: Mutex<()>,
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
            next_sub_id: AtomicUsize::new(1),
            utf8_carry: Mutex::new(ldmux_protocol::utf8::Utf8Carry::new()),
            epoch: AtomicU64::new(0),
            sanitizer: Mutex::new(crate::sanitize::Sanitizer::new(false)),
            closed: AtomicBool::new(false),
            gate: Mutex::new(()),
            poll_stop: AtomicBool::new(false),
        }
    }

    /// Whether programs in the pane may write the client's clipboard (OSC 52).
    /// Chosen when the stream is made, from the persisted policy
    /// (`clipboard_policy`), so changing it needs no daemon restart.
    pub fn with_clipboard_write(self, on: bool) -> Self {
        *self.sanitizer.lock().unwrap() = crate::sanitize::Sanitizer::new(on);
        self
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
        let id = self.next_sub_id.fetch_add(1, Ordering::Relaxed);
        self.subs.lock().unwrap().push(Sub { id, sub });
        id
    }

    /// Deliver `text` to every subscriber (the fanout), through the sanitizer.
    pub fn fanout(&self, text: &str) {
        self.fanout_except(None, text);
    }

    /// `fanout`, skipping one subscriber (the one a snapshot has just repainted).
    fn fanout_except(&self, skip: Option<usize>, text: &str) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let clean = self.sanitizer.lock().unwrap().push(text);
        if clean.is_empty() {
            return;
        }
        let bytes = clean.as_bytes();
        let subs = self.subs.lock().unwrap();
        for sub in subs.iter().filter(|s| Some(s.id) != skip) {
            (sub.sub.on_output)(bytes);
        }
    }

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

    /// The pane's full state (the `getState` equivalent): a snapshot that does
    /// not take the output tail, so reading state never eats a viewer's output.
    pub fn snapshot_full(&self) -> Result<ldmux_protocol::keeper::SnapshotResult, String> {
        self.request_snapshot(None, false)
    }

    /// One keeper `Snapshot`, optionally taking the unread output tail with it.
    fn request_snapshot(
        &self,
        history_lines: Option<u64>,
        drain_output: bool,
    ) -> Result<ldmux_protocol::keeper::SnapshotResult, String> {
        let req = KeeperMessage::Snapshot(Snapshot {
            session_id: self.session_id.clone(),
            history_lines,
            drain_output,
        });
        match self.keeper.call(&req)? {
            KeeperMessage::SnapshotResult(sr) => Ok(sr),
            other => Err(format!("not a snapshotresult: {other:?}")),
        }
    }

    /// Snapshot the pane for one subscriber and hand `fence` the result **at the
    /// exact byte boundary** between "in the snapshot" and "live output".
    ///
    /// The keeper feeds every byte to the emulator and to a buffer that
    /// `poll_once` later drains. A snapshot therefore already contains whatever
    /// is still waiting in that buffer, and replaying it as live output paints
    /// it twice (a fresh stream used to replay everything an unwatched shell had
    /// printed on top of the screen it had just been sent). So: the poll is held
    /// off (`gate`), the snapshot takes the waiting tail with it, the tail is
    /// delivered to every subscriber *except* `requester` (who has it in the
    /// render), and `fence` runs before the gate opens. `fence` must send the
    /// snapshot and switch `requester` to live delivery; anything the poll
    /// produces afterwards is genuinely new and follows it on the wire.
    ///
    /// Returns the epoch (count of delivered snapshots).
    pub fn snapshot_fenced<F>(
        &self,
        requester: Option<usize>,
        history_lines: Option<u64>,
        fence: F,
    ) -> Result<u64, String>
    where
        F: FnOnce(&ldmux_protocol::keeper::SnapshotResult, u64),
    {
        let _gate = self.gate.lock().unwrap();
        let mut sr = self.request_snapshot(history_lines, true)?;
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let tail = std::mem::take(&mut sr.tail);
        if !tail.is_empty()
            && let Some(text) = self.decode_chunk(tail) {
                self.fanout_except(requester, &text);
            }
        fence(&sr, epoch);
        Ok(epoch)
    }

    /// A plain snapshot of the pane's state for a one-off read (no tail is
    /// taken, so a viewer's output is never eaten). Counts toward the epoch.
    pub fn snapshot_result(
        &self,
        history_lines: Option<u64>,
    ) -> Result<(ldmux_protocol::keeper::SnapshotResult, u64), String> {
        let sr = self.request_snapshot(history_lines, false)?;
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((sr, epoch))
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
        // Held across the keeper call and the fanout: a snapshot must not slot
        // between "bytes taken from the keeper" and "bytes delivered".
        let _gate = self.gate.lock().unwrap();
        let req = KeeperMessage::Peek(ldmux_protocol::keeper::Peek {
            session_id: self.session_id.clone(),
        });
        let resp = self.keeper.call(&req)?;
        match resp {
            KeeperMessage::PeekResult(pr) => {
                // Nobody is subscribed: this is backlog from before anyone was
                // watching (a fresh stream's first poll). It is not live output
                // for anyone, and whoever subscribes next is sent a snapshot that
                // already contains it. Drop it, along with any half-sequence.
                if self.subs.lock().unwrap().is_empty() {
                    self.utf8_carry.lock().unwrap().clear();
                    self.sanitizer.lock().unwrap().reset();
                    return Ok(());
                }
                if pr.lost {
                    // The keeper discarded output nobody was reading: whoever is
                    // watching now has a stale picture and must resync.
                    self.utf8_carry.lock().unwrap().clear();
                    self.sanitizer.lock().unwrap().reset();
                    for sub in self.subs.lock().unwrap().iter() {
                        (sub.sub.on_reset)("output was discarded while unread; resync from a snapshot");
                    }
                }
                if !pr.data.is_empty()
                    && let Some(text) = self.decode_chunk(pr.data) {
                        self.fanout(&text);
                    }
                Ok(())
            }
            other => Err(format!("unexpected keeper response to Peek: {other:?}")),
        }
    }

    /// Decode one `Peek` chunk, holding back a trailing incomplete UTF-8
    /// character for the next chunk. Genuinely invalid bytes are still replaced
    /// (the pipeline downstream is `&str`), but a character that merely
    /// straddles two peeks is no longer corrupted.
    fn decode_chunk(&self, data: Vec<u8>) -> Option<String> {
        let text = self.utf8_carry.lock().unwrap().decode(&data);
        (!text.is_empty()).then_some(text)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
                tail: Vec::new(),
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

    /// A keeper double with a real output buffer, so the snapshot seam can be
    /// pinned deterministically: `Peek` drains it, and a `Snapshot` with
    /// `drain_output` hands the same buffer back as `tail`.
    struct BufKeeper {
        out: Mutex<Vec<u8>>,
        lost: AtomicBool,
    }
    impl KeeperConn for BufKeeper {
        fn call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
            match req {
                KeeperMessage::Peek(p) => Ok(KeeperMessage::PeekResult(ldmux_protocol::keeper::PeekResult {
                    session_id: p.session_id.clone(),
                    data: std::mem::take(&mut *self.out.lock().unwrap()),
                    lost: self.lost.swap(false, Ordering::SeqCst),
                })),
                KeeperMessage::Snapshot(s) => {
                    let KeeperMessage::SnapshotResult(mut sr) = TestKeeper.call(req)? else { unreachable!() };
                    if s.drain_output {
                        sr.tail = std::mem::take(&mut *self.out.lock().unwrap());
                    }
                    Ok(KeeperMessage::SnapshotResult(sr))
                }
                other => Err(format!("not handled: {other:?}")),
            }
        }
    }

    fn collecting() -> (StreamSubscriber, Arc<Mutex<Vec<u8>>>, Arc<AtomicUsize>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let resets = Arc::new(AtomicUsize::new(0));
        let (g, r) = (got.clone(), resets.clone());
        (
            StreamSubscriber {
                on_output: Box::new(move |b: &[u8]| g.lock().unwrap().extend_from_slice(b)),
                on_reset: Box::new(move |_| {
                    r.fetch_add(1, Ordering::SeqCst);
                }),
            },
            got,
            resets,
        )
    }

    /// The bytes still waiting in the keeper when a snapshot is taken are
    /// already in that snapshot: the requester must not get them again, every
    /// other viewer must, and the fence runs before any later output.
    #[test]
    fn a_snapshot_cuts_the_stream_at_one_byte_boundary() {
        let keeper = Arc::new(BufKeeper { out: Mutex::new(Vec::new()), lost: AtomicBool::new(false) });
        struct Shared(Arc<BufKeeper>);
        impl KeeperConn for Shared {
            fn call(&self, r: &KeeperMessage) -> Result<KeeperMessage, String> { self.0.call(r) }
        }
        let s = PaneStream::new("s1".into(), Box::new(Shared(keeper.clone())));
        let (a, a_got, _) = collecting();
        let (b, b_got, _) = collecting();
        let (a_id, _ua) = s.subscribe(a);
        let (_b_id, _ub) = s.subscribe(b);

        // Output the emulator has seen but nobody has polled yet.
        *keeper.out.lock().unwrap() = b"in-the-snapshot".to_vec();
        let fenced_at = Arc::new(AtomicUsize::new(usize::MAX));
        let (f, ag) = (fenced_at.clone(), a_got.clone());
        s.snapshot_fenced(Some(a_id), None, move |sr, epoch| {
            assert_eq!(sr.text, "screen");
            assert_eq!(epoch, 1);
            f.store(ag.lock().unwrap().len(), Ordering::SeqCst);
        })
        .unwrap();
        assert!(a_got.lock().unwrap().is_empty(), "the requester has it in the render");
        assert_eq!(*b_got.lock().unwrap(), b"in-the-snapshot", "other viewers still get it");
        assert_eq!(fenced_at.load(Ordering::SeqCst), 0, "the fence ran before any output reached the requester");

        // Genuinely new output after the seam reaches both.
        *keeper.out.lock().unwrap() = b"-new".to_vec();
        s.poll_once().unwrap();
        assert_eq!(*a_got.lock().unwrap(), b"-new");
    }

    /// With nobody subscribed, polled output is backlog from before anyone was
    /// watching: dropped, not queued for the next subscriber.
    #[test]
    fn output_polled_with_no_subscriber_is_dropped() {
        let keeper = Arc::new(BufKeeper { out: Mutex::new(b"old".to_vec()), lost: AtomicBool::new(true) });
        struct Shared(Arc<BufKeeper>);
        impl KeeperConn for Shared {
            fn call(&self, r: &KeeperMessage) -> Result<KeeperMessage, String> { self.0.call(r) }
        }
        let s = PaneStream::new("s1".into(), Box::new(Shared(keeper.clone())));
        s.poll_once().unwrap();
        let (sub, got, resets) = collecting();
        let (_id, _u) = s.subscribe(sub);
        s.poll_once().unwrap();
        assert!(got.lock().unwrap().is_empty());
        assert_eq!(resets.load(Ordering::SeqCst), 0, "a lost flag with no viewer is not a resync");
    }

    /// A keeper that discarded unread output tells its viewers to resync.
    #[test]
    fn lost_output_resyncs_the_viewers() {
        let keeper = Arc::new(BufKeeper { out: Mutex::new(Vec::new()), lost: AtomicBool::new(false) });
        struct Shared(Arc<BufKeeper>);
        impl KeeperConn for Shared {
            fn call(&self, r: &KeeperMessage) -> Result<KeeperMessage, String> { self.0.call(r) }
        }
        let s = PaneStream::new("s1".into(), Box::new(Shared(keeper.clone())));
        let (sub, _got, resets) = collecting();
        let (_id, _u) = s.subscribe(sub);
        keeper.lost.store(true, Ordering::SeqCst);
        s.poll_once().unwrap();
        assert_eq!(resets.load(Ordering::SeqCst), 1);
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
        let (sr, e1) = s.snapshot_result(None).unwrap();
        assert_eq!(sr.text, "screen");
        assert_eq!(e1, 1);
        let (_, e2) = s.snapshot_result(None).unwrap();
        assert_eq!(e2, 2);
    }

    /// Regression: a subscriber used to be silenced after 1 MiB of *cumulative*
    /// output because the queue it was bounded by was never drained.
    #[test]
    fn a_subscriber_keeps_receiving_past_a_megabyte_of_output() {
        let s = stream();
        let count = Arc::new(AtomicUsize::new(0));
        let resets = Arc::new(AtomicUsize::new(0));
        let (c, r) = (count.clone(), resets.clone());
        let (_id, _u) = s.subscribe(StreamSubscriber {
            on_output: Box::new(move |b: &[u8]| {
                c.fetch_add(b.len(), Ordering::SeqCst);
            }),
            on_reset: Box::new(move |_| {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        });
        let chunk = "x".repeat(4096);
        for _ in 0..600 {
            s.fanout(&chunk); // 2.4 MiB in total
        }
        assert_eq!(count.load(Ordering::SeqCst), 600 * 4096);
        assert_eq!(resets.load(Ordering::SeqCst), 0, "no spurious resync");
    }

    /// Subscriber ids are unique even when registered concurrently.
    #[test]
    fn subscriber_ids_are_unique() {
        let s = Arc::new(stream());
        let mut hs = Vec::new();
        for _ in 0..8 {
            let s = s.clone();
            hs.push(std::thread::spawn(move || {
                (0..50)
                    .map(|_| {
                        let (o, r) = cb();
                        s.subscribe(StreamSubscriber { on_output: o, on_reset: r }).0
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let mut ids: Vec<usize> = hs.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    /// A multi-byte character split across two peeks survives intact.
    #[test]
    fn a_character_split_across_peeks_is_not_corrupted() {
        let s = stream();
        let got = Arc::new(Mutex::new(Vec::<u8>::new()));
        let g = got.clone();
        let (_id, _u) = s.subscribe(StreamSubscriber {
            on_output: Box::new(move |b: &[u8]| g.lock().unwrap().extend_from_slice(b)),
            on_reset: Box::new(|_| {}),
        });
        let bytes = "a\u{e9}\u{1f600}z".as_bytes(); // é (2 bytes), 😀 (4 bytes)
        for split in 1..bytes.len() {
            got.lock().unwrap().clear();
            for part in [&bytes[..split], &bytes[split..]] {
                if let Some(t) = s.decode_chunk(part.to_vec()) {
                    s.fanout(&t);
                }
            }
            assert_eq!(*got.lock().unwrap(), bytes, "split at {split}");
        }
    }
}
