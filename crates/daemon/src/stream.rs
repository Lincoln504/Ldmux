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
use std::sync::Mutex;

use ldmux_protocol::keeper::{KeeperMessage, Snapshot};

/// One subscriber's bounded output queue (the §3.7 flow-control).
///
/// The keeper always buffers the pane's output; each subscriber pulls from
/// this queue. When it overflows (the subscriber is too slow), the subscriber
/// is dropped and forced to resync from a snapshot — the pane never pauses.
struct SubQueue {
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
    pub fn subscribe(&self, sub: StreamSubscriber) -> Box<dyn Fn() + Send + Sync + '_> {
        let mut subs = self.subs.lock().unwrap();
        subs.push(SubQueue {
            q: VecDeque::new(),
            dropped: false,
            sub,
        });
        Box::new(move || {
            let mut subs = self.subs.lock().unwrap();
            // Drop the last queue (the subscriber is the one just pushed).
            subs.pop();
            drop(subs);
        })
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
    pub fn drain(&self, idx: usize) -> Vec<u8> {
        let mut subs = self.subs.lock().unwrap();
        match subs.get_mut(idx) {
            Some(sub) => {
                let d: Vec<u8> = sub.q.drain(..).collect();
                d
            }
            None => Vec::new(),
        }
    }

    /// A snapshot (the full-screen capture, via the keeper's `Snapshot`).
    ///
    /// Returns the rendered screen + the epoch (count *delivered* snapshots).
    pub fn snapshot(&self) -> Result<(String, u64), String> {
        let req = KeeperMessage::Snapshot(Snapshot {
            session_id: self.session_id.clone(),
            history_lines: None,
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
        let mut subs = self.subs.lock().unwrap();
        for sub in subs.iter() {
            (sub.sub.on_reset)(reason);
        }
        subs.clear();
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
        let _ = s.subscribe(StreamSubscriber {
            on_output: o1,
            on_reset: r1,
        });
        let (o2, r2) = cb();
        let _ = s.subscribe(StreamSubscriber {
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
        let _ = s.subscribe(StreamSubscriber {
            on_output: o,
            on_reset: r,
        });
        s.fanout("abc");
        let d = s.drain(0);
        assert_eq!(d, b"abc");
        // A second drain is empty (the queue is drained).
        assert!(s.drain(0).is_empty());
    }
}
