//! The fake keeper: a test double for the keeper (plan §3.5).
//!
//! It speaks the keeper protocol (the tagged [`KeeperMessage`]s) over a Unix
//! socket, with an **in-memory** session table — no real PTY, no real shell, no
//! real ghostty-vt. The daemon's logic tests against it: a `Spawn` creates a
//! session, a `Read`/`List` returns its `State`, a `Peek` drains the buffered
//! output, a `Snapshot` returns the rendered content, a `Kill` marks it dead.
//!
//! It mirrors the real keeper's `handle` (the request→response shapes, one
//! request line → one response line per connection) exactly, so the daemon
//! under test cannot tell the difference. The "pane" is an in-memory buffer
//! (what a test injects via [`FakeKeeper::inject_output`]), not a PTY.
//!
//! It is a test utility that lives in the daemon crate so both the unit tests
//! (`src/*.rs`) and any integration tests can reach it; the name says what it
//! is.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use ldmux_protocol::keeper::{
    self as km, Ack, KeeperMessage, ListResult, PeekResult, SnapshotResult, State,
};

/// One fake session (a pane the fake keeper has spawned).
///
/// The fields mirror the keeper's `State`/`SnapshotResult` vocabulary: the
/// geometry (`cols`/`rows`), liveness (`dead`/`exit_code`), the buffered output
/// (what a `Peek` drains), the rendered content (what a `Snapshot` returns),
/// and the display-message state fields (the cursor, the modes, the history).
#[derive(Default)]
pub struct FakeSession {
    pub id: String,
    /// The shell's pid, as the real keeper reports it (`0` unless a test sets
    /// one — the fake has no processes).
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    pub dead: bool,
    pub exit_code: i32,
    /// The signal that killed the shell, when one did (`exit_code` is then unused).
    pub exit_signal: Option<i32>,
    /// The buffered output (what a `Peek` drains).
    pub out: VecDeque<u8>,
    /// The rendered content (what a `Snapshot` returns, `\n`-joined).
    pub snapshot_text: String,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub alternate_on: bool,
    pub mouse: bool,
    pub mouse_sgr: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub history_size: u16,
    /// The pane's foreground command (`/proc/<pid>/comm`); the fake reports a
    /// fixed name for a live pane and empty for a dead one (a reaped pid may
    /// be recycled, so the real keeper reports empty for a dead pane).
    pub foreground: String,
    /// The pane's working directory (`/proc/<pid>/cwd`).
    pub cwd: String,
    /// The pane's activity clock (unix ms), exactly as the real keeper's: the
    /// time of the last input or output. `0` until the fake pane is used, so a
    /// test that never writes sees "no activity" rather than a fabricated now.
    pub activity: u64,
}

/// The fake keeper: a test double for the keeper.
///
/// One shared `sessions` table, exactly as the real keeper's; every request
/// takes it briefly. `serve` runs the accept loop (one connection == one
/// request line → one response line); the control methods (`inject_output`,
/// `kill`, `set_snapshot`) let a test script the fake pane.
pub struct FakeKeeper {
    socket_path: std::path::PathBuf,
    sessions: Mutex<HashMap<String, FakeSession>>,
    /// The history count of the last `Snapshot` request, so a test can see what
    /// a caller asked for (the response carries the count *returned*).
    last_history_request: Mutex<Option<u64>>,
}

impl FakeKeeper {
    /// A fake keeper bound to `socket_path` (not yet serving).
    pub fn new(socket_path: std::path::PathBuf) -> Self {
        Self {
            socket_path,
            sessions: Mutex::new(HashMap::new()),
            last_history_request: Mutex::new(None),
        }
    }

    /// The socket path.
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Process one keeper message, mirroring the real keeper's `handle`.
    ///
    /// The request→response contract is the load-bearing part: a `Spawn`
    /// answers a `State` (the new session), a `Read`/`Resize` answers a
    /// `State`, a `Peek` answers a `PeekResult` (the drained bytes), a
    /// `Snapshot` answers a `SnapshotResult` (the rendered content), a
    /// `List` answers a `ListResult` (one `State` per session), a `Kill`
    /// answers a dead `State`, and a `Write`/`Close` answers an `Ack`.
    pub fn handle(&self, msg: &KeeperMessage) -> Result<KeeperMessage, String> {
        if let km::KeeperMessage::Snapshot(req) = msg {
            *self.last_history_request.lock().unwrap() = req.history_lines;
        }
        let mut sessions = self.sessions.lock().unwrap();
        match msg {
            km::KeeperMessage::Spawn(s) => {
                if sessions.contains_key(&s.session_id) {
                    return Err(format!("session already exists: {}", s.session_id));
                }
                let sess = FakeSession {
                    id: s.session_id.clone(),
                    cols: s.cols as u16,
                    rows: s.rows as u16,
                    foreground: "fake-shell".into(),
                    cwd: "/".into(),
                    ..Default::default()
                };
                // Insert, then build the state from the fresh session.
                let mut sessions = sessions;
                sessions.insert(s.session_id.clone(), sess);
                let s = sessions.get(&s.session_id).unwrap();
                Ok(KeeperMessage::State(Self::state_fields(s)))
            }
            km::KeeperMessage::Kill(k) => {
                // The real keeper removes the session on a Kill (the emulator
                // thread exits), so a later `List` no longer includes it — the
                // daemon's `reconcile` reads liveness from the `List`, so a
                // kept session would never mark dead. Build the state first.
                let st = match sessions.get_mut(&k.session_id) {
                    Some(s) => {
                        s.dead = true;
                        Self::state_fields(s)
                    }
                    None => return Err(format!("no such session: {}", k.session_id)),
                };
                sessions.remove(&k.session_id);
                Ok(KeeperMessage::State(st))
            }
            km::KeeperMessage::Write(w) => match sessions.get_mut(&w.session_id) {
                Some(s) if !s.dead => {
                    s.out.extend(w.data.bytes());
                    // Input is activity (the real keeper's rule).
                    s.activity = crate::daemon::now_ms();
                    Ok(KeeperMessage::Ack(Ack {}))
                }
                Some(_) => Err(format!("no live session: {}", w.session_id)),
                None => Err(format!("no such session: {}", w.session_id)),
            },
            km::KeeperMessage::Resize(r) => match sessions.get_mut(&r.session_id) {
                Some(s) if !s.dead => {
                    s.cols = r.cols as u16;
                    s.rows = r.rows as u16;
                    Ok(KeeperMessage::State(Self::state_fields(s)))
                }
                Some(_) => Err(format!("no live session: {}", r.session_id)),
                None => Err(format!("no such session: {}", r.session_id)),
            },
            km::KeeperMessage::Read(rd) => match sessions.get(&rd.session_id) {
                Some(s) => Ok(KeeperMessage::State(Self::state_fields(s))),
                None => Err(format!("no such session: {}", rd.session_id)),
            },
            km::KeeperMessage::Peek(p) => match sessions.get_mut(&p.session_id) {
                Some(s) => match p.max_bytes {
                    None => {
                        let data: Vec<u8> = s.out.drain(..).collect();
                        Ok(KeeperMessage::PeekResult(PeekResult {
                            session_id: p.session_id.clone(),
                            data,
                            data64: None,
                            more: false,
                            lost: false,
                        }))
                    }
                    Some(max) => {
                        let n = s.out.len().min(max.clamp(1, km::PEEK_MAX_BYTES) as usize);
                        let data: Vec<u8> = s.out.drain(..n).collect();
                        Ok(KeeperMessage::PeekResult(PeekResult::compact(
                            p.session_id.clone(),
                            &data,
                            !s.out.is_empty(),
                            false,
                        )))
                    }
                },
                None => Err(format!("no such session: {}", p.session_id)),
            },
            km::KeeperMessage::Snapshot(req) => match sessions.get_mut(&req.session_id) {
                Some(sess) => Ok(KeeperMessage::SnapshotResult(SnapshotResult {
                    session_id: sess.id.clone(),
                    cols: sess.cols as u64,
                    rows: sess.rows as u64,
                    // The real keeper clamps the request to the rows it actually
                    // retains; echoing it here is what makes a caller's request
                    // observable to a test.
                    history_lines: req.history_lines.unwrap_or(0).min(sess.history_size as u64),
                    text: sess.snapshot_text.clone(),
                    cursor_x: sess.cursor_x as u64,
                    cursor_y: sess.cursor_y as u64,
                    alternate_on: sess.alternate_on,
                    mouse: sess.mouse,
                    mouse_sgr: sess.mouse_sgr,
                    app_cursor: sess.app_cursor,
                    bracketed_paste: sess.bracketed_paste,
                    history_size: sess.history_size as u64,
                    dead: sess.dead,
                    foreground: if sess.dead {
                        String::new()
                    } else {
                        sess.foreground.clone()
                    },
                    cwd: if sess.dead {
                        String::new()
                    } else {
                        sess.cwd.clone()
                    },
                    tail: if req.drain_output {
                        sess.out.drain(..).collect()
                    } else {
                        Vec::new()
                    },
                })),
                None => Err(format!("no such session: {}", req.session_id)),
            },
            km::KeeperMessage::List(_l) => {
                let mut states = Vec::new();
                let mut activity = std::collections::BTreeMap::new();
                for s in sessions.values() {
                    states.push(Self::state_fields(s));
                    activity.insert(s.id.clone(), s.activity);
                }
                Ok(KeeperMessage::ListResult(ListResult {
                    sessions: states,
                    activity,
                }))
            }
            km::KeeperMessage::Close(_c) => Ok(KeeperMessage::Ack(Ack {})),
            // The request-only shapes (`ChildInfo`, `ChildExited`, the result
            // types) are not request methods, exactly as the real keeper's.
            other => Err(format!("not a request: {other:?}")),
        }
    }

    /// Build a `State` from a `FakeSession` (the fields the fake keeper
    /// reports, mirroring the real keeper's `state_of`). A dead pane reports
    /// empty `foreground`/`cwd` (a reaped pid may be recycled).
    fn state_fields(s: &FakeSession) -> State {
        let (foreground, cwd) = if s.dead {
            (String::new(), String::new())
        } else {
            (s.foreground.clone(), s.cwd.clone())
        };
        State {
            session_id: s.id.clone(),
            pid: s.pid,
            cols: s.cols as u64,
            rows: s.rows as u64,
            dead: s.dead,
            cursor_x: s.cursor_x as u64,
            cursor_y: s.cursor_y as u64,
            alternate_on: s.alternate_on,
            mouse: s.mouse,
            mouse_sgr: s.mouse_sgr,
            app_cursor: s.app_cursor,
            bracketed_paste: s.bracketed_paste,
            history_size: s.history_size as u64,
            foreground,
            cwd,
            exit_code: (s.dead && s.exit_signal.is_none()).then_some(s.exit_code as i64),
            exit_signal: if s.dead { s.exit_signal } else { None },
        }
    }

    /// Bind the socket and run the accept loop (one connection == one request
    /// line → one response line). Runs forever; drop the task (or the process)
    /// to stop. Mirrors the real keeper's `serve`.
    pub async fn serve(
        self_: &std::sync::Arc<Self>,
        socket: &std::path::Path,
    ) -> std::io::Result<()> {
        let _ = std::fs::remove_file(socket);
        let listener = tokio::net::UnixListener::bind(socket)?;
        loop {
            let (sock, _addr) = listener.accept().await?;
            let me = self_.clone();
            tokio::spawn(async move {
                if let Err(e) = me.serve_conn(sock).await {
                    eprintln!("fake keeper: connection: {e}");
                }
            });
        }
    }

    /// Serve one connection: read one request line, dispatch, write one
    /// response line. Mirrors the real keeper's `serve_conn`.
    async fn serve_conn(&self, sock: tokio::net::UnixStream) -> std::io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut rd, mut wr) = sock.into_split();
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = rd.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let nl = buf.iter().position(|&b| b == b'\n').unwrap();
        let line = String::from_utf8_lossy(&buf[..nl]).to_string();
        let req: KeeperMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let err = serde_json::json!({ "error": e.to_string() });
                let _ = wr
                    .write_all(format!("{}\n", serde_json::to_string(&err).unwrap()).as_bytes())
                    .await;
                return Ok(());
            }
        };
        let resp = match self.handle(&req) {
            Ok(m) => m,
            Err(e) => KeeperMessage::Error(km::Error {
                error: e,
                session_id: None,
            }),
        };
        let resp_line = format!("{}\n", serde_json::to_string(&resp).unwrap());
        wr.write_all(resp_line.as_bytes()).await?;
        wr.flush().await?;
        Ok(())
    }

    /// Inject output into a session's buffer (a test hook: the fake pane's
    /// "output" is whatever the test injects; a `Peek` drains it).
    pub fn inject_output(&self, session_id: &str, data: &[u8]) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(session_id) {
            s.out.extend(data.iter().copied());
            // Output is activity (the real keeper stamps its clock in the reader
            // task, the one place every byte from the master passes).
            s.activity = crate::daemon::now_ms();
        }
    }

    /// Mark a session dead (a test hook: the fake pane's "shell exited").
    ///
    /// Mirrors the keeper's authoritative death: after this, `State`/`Read`
    /// report `dead: true` and empty `foreground`/`cwd`.
    pub fn kill(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(session_id) {
            s.dead = true;
        }
    }

    /// Mark a session dead the way the real keeper's `waitpid` does: with the
    /// shell's exit status, or the signal that killed it.
    pub fn exit(&self, session_id: &str, code: Option<i32>, signal: Option<i32>) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(session_id) {
            s.dead = true;
            s.exit_code = code.unwrap_or(0);
            s.exit_signal = signal;
        }
    }

    /// Set a session's rendered content (a test hook: the fake pane's
    /// "rendered screen", what a `Snapshot` returns).
    /// The history count of the last `Snapshot` request (`None` = the live screen).
    pub fn last_history_request(&self) -> Option<u64> {
        *self.last_history_request.lock().unwrap()
    }

    /// Set the shell pid the fake reports (a test that needs an ancestry walk).
    pub fn set_pid(&self, session_id: &str, pid: i32) {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(session_id) {
            s.pid = pid;
        }
    }

    pub fn set_snapshot(&self, session_id: &str, text: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(s) = sessions.get_mut(session_id) {
            s.snapshot_text = text.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::keeper::{Kill, Peek, Read, Snapshot, Spawn};

    fn spawn_id(k: &FakeKeeper, id: &str) -> Result<KeeperMessage, String> {
        k.handle(&KeeperMessage::Spawn(Spawn {
            session_id: id.into(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: None,
            env: None,
        }))
    }

    fn is_state(m: &KeeperMessage, id: &str, dead: bool) -> bool {
        match m {
            KeeperMessage::State(s) => s.session_id == id && s.dead == dead,
            _ => false,
        }
    }

    /// A `Spawn` answers a live `State` for the new session.
    #[test]
    fn spawn_answers_a_state() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let m = spawn_id(&k, "s1").unwrap();
        assert!(is_state(&m, "s1", false));
    }

    /// A duplicate `Spawn` is an error (the real keeper would not double-spawn).
    #[test]
    fn a_duplicate_spawn_is_an_error() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        assert!(spawn_id(&k, "s1").is_err());
    }

    /// A `Read` answers the session's `State`.
    #[test]
    fn read_answers_the_state() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        let m = k
            .handle(&KeeperMessage::Read(Read {
                session_id: "s1".into(),
            }))
            .unwrap();
        assert!(is_state(&m, "s1", false));
    }

    /// A `Peek` drains the buffered output (and an empty buffer drains empty).
    #[test]
    fn peek_drains_the_buffered_output() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        k.inject_output("s1", b"hello");
        let m = k.handle(&KeeperMessage::Peek(Peek::all("s1"))).unwrap();
        match &m {
            KeeperMessage::PeekResult(p) => assert_eq!(p.data, b"hello"),
            _ => panic!("not a peekresult"),
        }
        // A second `Peek` drains nothing (the buffer is empty).
        let m = k.handle(&KeeperMessage::Peek(Peek::all("s1"))).unwrap();
        match &m {
            KeeperMessage::PeekResult(p) => assert!(p.data.is_empty()),
            _ => panic!("not a peekresult"),
        }
    }

    /// A `Write` buffers the bytes (a subsequent `Peek` drains them).
    #[test]
    fn write_buffers_the_output() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        let w = ldmux_protocol::keeper::Write {
            session_id: "s1".into(),
            data: "abc".into(),
        };
        let m = k.handle(&KeeperMessage::Write(w)).unwrap();
        assert!(matches!(m, KeeperMessage::Ack(_)));
        let m = k.handle(&KeeperMessage::Peek(Peek::all("s1"))).unwrap();
        match &m {
            KeeperMessage::PeekResult(p) => assert_eq!(p.data, b"abc"),
            _ => panic!("not a peekresult"),
        }
    }

    /// A `Snapshot` returns the rendered content and the live state fields.
    #[test]
    fn snapshot_returns_the_rendered_content() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        k.set_snapshot("s1", "line1\nline2");
        let m = k
            .handle(&KeeperMessage::Snapshot(Snapshot {
                session_id: "s1".into(),
                history_lines: None,
                drain_output: false,
            }))
            .unwrap();
        match &m {
            KeeperMessage::SnapshotResult(sr) => {
                assert_eq!(sr.text, "line1\nline2");
                assert_eq!(sr.cols, 80);
                assert_eq!(sr.rows, 24);
                assert!(!sr.dead);
                assert_eq!(sr.foreground, "fake-shell");
                assert_eq!(sr.cwd, "/");
            }
            _ => panic!("not a snapshotresult"),
        }
    }

    /// A `Kill` answers a dead `State` and removes the session (the real
    /// keeper's `Kill` removes it, so a later `List` no longer includes it — the
    /// daemon's `reconcile` reads liveness from the `List`). The dead `State`
    /// reports empty `foreground`/`cwd` (a reaped pid may be recycled).
    #[test]
    fn kill_answers_a_dead_state_and_removes_the_session() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        let m = k
            .handle(&KeeperMessage::Kill(Kill {
                session_id: "s1".into(),
            }))
            .unwrap();
        match &m {
            KeeperMessage::State(st) => {
                assert!(st.dead);
                assert_eq!(st.foreground, "");
                assert_eq!(st.cwd, "");
            }
            _ => panic!("not a state"),
        }
        // The session is removed: a later `Read` is a "no such session" error.
        let m = k
            .handle(&KeeperMessage::Read(Read {
                session_id: "s1".into(),
            }))
            .unwrap_err();
        assert!(m.contains("no such session"));
    }

    /// A `List` answers one `State` per session.
    #[test]
    fn list_answers_one_state_per_session() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let _ = spawn_id(&k, "s1").unwrap();
        let _ = spawn_id(&k, "s2").unwrap();
        let m = k
            .handle(&KeeperMessage::List(ldmux_protocol::keeper::List {}))
            .unwrap();
        match &m {
            KeeperMessage::ListResult(lr) => {
                assert_eq!(lr.sessions.len(), 2);
            }
            _ => panic!("not a listresult"),
        }
    }

    /// A `Write` to an unknown session is an error.
    #[test]
    fn write_to_an_unknown_session_is_an_error() {
        let k = FakeKeeper::new(std::path::PathBuf::from("/tmp/fk-t"));
        let w = ldmux_protocol::keeper::Write {
            session_id: "nope".into(),
            data: "x".into(),
        };
        assert!(k.handle(&KeeperMessage::Write(w)).is_err());
    }

    /// End-to-end over the socket: `keeper_call` drives the fake keeper the way
    /// the daemon does (connect, one request line, one response line). This is
    /// the plan §3.5 contract: the daemon under test cannot tell the fake from
    /// the real keeper.
    #[tokio::test]
    async fn keeper_call_drives_the_fake_over_the_socket() {
        use ldmux_protocol::keeper_call::keeper_call;
        let dir = std::env::temp_dir().join(format!("ldmux-fake-keeper-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("keeper.sock");
        let k = std::sync::Arc::new(FakeKeeper::new(socket.clone()));
        let k_serve = k.clone();
        let socket_serve = socket.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&k_serve, socket_serve.as_path())
                .await
                .unwrap();
        });

        // Wait for the fake keeper's socket file to exist (the `serve` task
        // binds it; the spawn is async, so the file appears a beat after the
        // spawn is scheduled — the same bind→connect race the keeper's
        // `is_alive` bridges). A dead keeper never creates the file, so this
        // times out (the budget is the guard).
        tokio::time::timeout(std::time::Duration::from_millis(2000), async {
            loop {
                if socket.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        // A `Spawn` over the socket answers a live `State`.
        let r = keeper_call(
            socket.to_str().unwrap(),
            &KeeperMessage::Spawn(Spawn {
                session_id: "s1".into(),
                cols: 80,
                rows: 24,
                cwd: None,
                command: None,
                env: None,
            }),
            2000,
        )
        .await
        .unwrap();
        assert!(is_state(&r, "s1", false));

        // A `Peek` over the socket drains what the test injected.
        k.inject_output("s1", b"socket-bytes");
        let r = keeper_call(
            socket.to_str().unwrap(),
            &KeeperMessage::Peek(Peek::all("s1")),
            2000,
        )
        .await
        .unwrap();
        match &r {
            KeeperMessage::PeekResult(p) => assert_eq!(p.data, b"socket-bytes"),
            _ => panic!("not a peekresult"),
        }

        drop(k);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
