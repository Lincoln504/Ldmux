//! The `LdmuxDaemon`: the daemon's main state (the glue).
//!
//! It owns the keeper (auto-spawned, [`KeeperSupervisor`]), the session
//! registry (the persisted table), and the ownership (who holds which shell).
//! The retarget of record (plan §3.5): today's tmux control-plane calls
//! (`cp.listSessions()`, `cp.hasSession()`, `cp.listPanes()`) become keeper
//! calls over the socket — a `List` for the live set, a `Read` for liveness,
//! a `State` for the pane facts — so the daemon under test runs against a
//! [fake_keeper](crate::fake_keeper), not a live tmux server.
//!
//! This module is the CORE of the loop (the `keeper_call` helper +
//! `reconcile`); the remote, the streams, the desktop, and the CLI are
//! layered on as they are ported.

use std::collections::HashSet;
use std::path::PathBuf;

use ldmux_protocol::keeper::{self as km, KeeperMessage};
use serde_json::Value;

use std::collections::HashMap;

use ldmux_protocol::rpc::SessionRecord;
use ldmux_protocol::rpc::{SessionInfo, SessionOwner};

use crate::daemon_lock::DaemonLock;
use crate::keeper_supervisor::KeeperSupervisor;
use crate::ownership::Ownership;
use crate::registry::Registry;
use crate::stream::PaneStream;

/// Generate a short id (the TS `shortId`): a UUID with dashes removed,
/// truncated to 10 characters.
fn short_id() -> String {
    uuid::Uuid::new_v4().to_string().replace("-", "")[..10].to_string()
}

/// The result of a `reconcile` pass (the dead/adopted/pruned sets).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReconcileResult {
    /// Records marked dead (their shell exited).
    pub dead: Vec<String>,
    /// Live sessions the registry did not know (the adoption candidates).
    pub adopted: Vec<String>,
    /// Records pruned (past their dead TTL, over the cap).
    pub pruned: Vec<String>,
}

/// The row title for a session (the TS `sessionTitle`): the explicit title when
/// it is more than the kind, else the directory's basename, else the record's.
fn session_title(record: &SessionRecord, cwd: &str) -> String {
    if !record.title.is_empty() && record.title != "shell" && record.title != "pi" {
        return record.title.clone();
    }
    let base = std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !base.is_empty() {
        base
    } else if !record.title.is_empty() {
        record.title.clone()
    } else {
        record.id.clone()
    }
}

/// The daemon's main state: the keeper, the registry, the ownership, and the
/// single-instance lock.
pub struct LdmuxDaemon {
    state_dir: PathBuf,
    keeper: KeeperSupervisor,
    registry: Registry,
    ownership: Ownership,
    dead_record_ttl_ms: u64,
    max_records: usize,
    /// The single-instance lock (the `ldmuxd.lock` pid lockfile); `Drop`
    /// releases it.
    lock: DaemonLock,
    /// The remote WS server (the phone-facing endpoint); `Option` because the
    /// remote is optional (a bind failure must not take down local control).
    #[allow(dead_code)]
    remote: Option<crate::remote::RemoteServer>,
    /// The remote config (the hostname, port, token).
    #[allow(dead_code)]
    remote_cfg: Option<crate::remote::RemoteConfig>,
    /// The per-session streams (the `Arc<PaneStream>` per tmux session).
    /// The `Arc` is shared with the poll thread (which holds its own `Arc`),
    /// so the stream outlives the daemon's map entry (the poll thread keeps
    /// it alive until it exits).
    streams: HashMap<String, std::sync::Arc<PaneStream>>,
    /// Sessions with a window open on the desktop's screen (the TS
    /// `liveWindows`): one window per shell, recorded so the daemon can
    /// close it and so a phone that opens it is asked first (ADR-0010).
    live_windows: HashMap<String, (String, u64)>,
    /// The desktop's device identity (the ADR-0010 claim source for desk
    /// windows; the TS `desktopDevice`).
    desktop_device: ldmux_protocol::rpc::DeviceInfo,
}

impl Drop for LdmuxDaemon {
    fn drop(&mut self) {
        // The lock is released by its own `Drop` (the `DaemonLock`'s `Drop`).
        // The remote is stopped here (its `stop()` sets the stopped flag).
        if let Some(r) = self.remote.as_mut() {
            r.stop();
        }
    }
}

impl LdmuxDaemon {
    /// A daemon rooted at `state_dir`, whose keeper binary is `keeper_bin`.
    ///
    /// The keeper's socket and the registry live under `state_dir`, exactly as
    /// the TS daemon's (the `ldmuxd.sock`, the `ldmuxd.lock`, and the
    /// `registry.json` all live there).
    pub fn new(state_dir: PathBuf, keeper_bin: PathBuf) -> Self {
        Self {
            state_dir: state_dir.clone(),
            keeper: KeeperSupervisor::new(state_dir.clone(), keeper_bin),
            registry: Registry::new(state_dir.join("registry.json").to_string_lossy()),
            ownership: Ownership::new(),
            dead_record_ttl_ms: 24 * 60 * 60 * 1000,
            max_records: 500,
            lock: DaemonLock::new(&state_dir),
            remote: None,
            remote_cfg: None,
            streams: HashMap::new(),
            live_windows: HashMap::new(),
            desktop_device: ldmux_protocol::rpc::DeviceInfo {
                id: "desktop".into(),
                name: "desktop".into(),
                kind: "desktop".into(),
            },
        }
    }

    /// Configure the remote (the hostname, port, token) before `start`.
    pub fn with_remote(mut self, hostname: String, port: u16, token: String) -> Self {
        self.remote_cfg = Some(crate::remote::RemoteConfig::new(hostname, port, token));
        self
    }

    /// The control socket path (the `ldmuxd.sock`).
    pub fn socket_path(&self) -> std::path::PathBuf {
        self.state_dir.join("ldmuxd.sock")
    }

    /// The state dir.
    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    /// The keeper's socket path (the daemon's connection to the keeper).
    pub fn keeper_socket_path(&self) -> &std::path::Path {
        self.keeper.socket_path()
    }

    /// The keeper's binary path.
    pub fn keeper_bin(&self) -> &std::path::Path {
        self.keeper.keeper_bin()
    }

    /// A handle to the registry (test/inspection: seed records, read them).
    pub fn registry(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// The ownership (test/inspection: read the claims).
    pub fn ownership(&self) -> &Ownership {
        &self.ownership
    }

    /// Talk to the keeper: one request line → one response line.
    ///
    /// The same client protocol as the WS connection, minus auth (plan §3.5) —
    /// the [fake_keeper](crate::fake_keeper) answers it in tests, the real
    /// keeper in production. A keeper `Error` answer is a failure.
    ///
    /// Keeper-death detection (plan §6): if the call fails with a connection
    /// error (the keeper is dead), respawn it and retry once.
    async fn keeper_call(&mut self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
        match ldmux_protocol::keeper_call::keeper_call(
            self.keeper.socket_path().to_str().unwrap(),
            req,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .map_err(|e| e.0) {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // Keeper-death detection: if the socket is gone, respawn.
                if !self.keeper.is_alive().await {
                    self.keeper.ensure().await.map_err(|e| {
                        format!("keeper respawn failed: {e}")
                    })?;
                    // Small delay for the new keeper to bind the socket.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    // Retry the call against the new keeper.
                    ldmux_protocol::keeper_call::keeper_call(
                        self.keeper.socket_path().to_str().unwrap(),
                        req,
                        ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
                    )
                    .await
                    .map_err(|e| e.0)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Detect keeper death and respawn (plan §6): call the keeper; if it's
    /// dead, respawn it and reconcile (mark all sessions dead).
    async fn ensure_keeper_alive(&mut self) -> Result<(), String> {
        if self.keeper.is_alive().await {
            return Ok(());
        }
        self.keeper.ensure().await.map_err(|e| {
            format!("keeper respawn failed: {e}")
        })?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Reconcile: the new keeper is empty, so all sessions are dead.
        let _ = self.reconcile().await;
        Ok(())
    }

    /// Reconcile the registry against the keeper's live sessions.
    ///
    /// The retarget of the TS `reconcile`: the live set comes from a keeper
    /// `List` (not a tmux `list-sessions`), each keeper `session_id` mapping to
    /// its terminal name `ldmux-<id>`. The dead/adopted sets drive the
    /// ownership retain (drop claims for sessions that no longer exist) and the
    /// registry save. The `dead`/`adopted`/`pruned` sets are returned (the caller
    /// decides what is news; a change in the unknown set is news, not its mere
    /// presence).
    pub async fn reconcile(&mut self) -> Result<ReconcileResult, String> {
        let resp = self.keeper_call(&KeeperMessage::List(km::List {})).await?;
        let states = match resp {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => return Err("keeper did not answer a ListResult".into()),
        };
        // The live set, as terminal names (`ldmux-<id>`), the registry's key.
        // The keeper's `session_id` IS that name (the daemon passes the tmux
        // name at Spawn), so it is used verbatim — re-prefixing it made every
        // session look dead on restart and duplicated it via `adopt`.
        let live: std::collections::BTreeSet<String> =
            states.iter().map(|s| s.session_id.clone()).collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Reconcile the registry (the dead/adopted sets).
        let (dead, adopted) = self.registry.reconcile(Some(&live), now);
        // Prune the registry (the dead TTL, the cap).
        let pruned = self.registry.prune(
            Some(self.dead_record_ttl_ms),
            Some(self.max_records),
            now,
        );
        if !dead.is_empty() || !pruned.is_empty() {
            let _ = self.registry.save();
        }
        // Claims for sessions that no longer exist are dropped (a session killed
        // outside ldmux must not leave an owner pointing at nothing).
        let known: HashSet<String> = self
            .registry
            .list()
            .into_iter()
            .map(|r| r.id)
            .collect();
        self.ownership.retain(&known);
        Ok(ReconcileResult { dead, adopted, pruned })
    }

    /// Start the daemon's main loop (the core of the TS `start`).
    ///
    /// The sequence, retargeted onto the keeper: acquire the single-instance
    /// lock (a concurrent daemon refuses — callers treat that as success,
    /// someone else won), ensure the keeper (auto-spawn if dead), load the
    /// registry, adopt the daemon's own sessions (the keeper's `ldmux-<id>`
    /// panes the registry does not list), and reconcile. The remote is started
    /// after the core (a bind failure must not take down local control).
    /// Start the daemon. The daemon is wrapped in an `Arc<tokio::sync::Mutex>`
    /// (created by the caller) so that the control socket's spawned tasks can
    /// call `handle` (which takes `&mut self`). The `Arc` is shared across the
    /// accept loop.
    pub async fn start(&mut self, arc: &std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>>) -> Result<(), String> {
        // Create or tighten the state dir to 0700 (the TS daemon creates it
        // with `mode: 0o700`; a pre-existing dir is tightened, not trusted).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::path::Path::new(&self.state_dir).exists() {
                // Tighten an existing dir.
                let _ = std::fs::set_permissions(
                    &self.state_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            } else {
                // Create with 0700.
                std::fs::create_dir_all(&self.state_dir)
                    .map_err(|e| format!("create state dir: {e}"))?;
                let _ = std::fs::set_permissions(
                    &self.state_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            }
        }
        // Acquire the lock (the single-instance pid lockfile).
        self.lock.acquire().map_err(|e| e.to_string())?;
        // Ensure the keeper (auto-spawn if dead).
        self.keeper.ensure().await?;
        // Load the registry.
        self.registry.load();
        // Adopt the daemon's own sessions.
        self.adopt_own_sessions().await?;
        // Reconcile.
        let _ = self.reconcile().await?;
        // Start the remote (the phone-facing WS endpoint). A bind failure is
        // logged, not fatal: the remote is optional (local control must not
        // be taken down by a remote bind failure).
        if let Some(cfg) = self.remote_cfg.clone() {
            let mut rs = crate::remote::RemoteServer::new(cfg.clone());
            match rs.start(arc).await {
                Ok((host, port)) => {
                    eprintln!("ldmuxd: remote ws://{host}:{port}/ws");
                    self.remote = Some(rs);
                }
                Err(e) => {
                    // The bind can fail at boot, before tailscaled has assigned
                    // the tailnet address (the documented incident: the listener
                    // was never retried and the phone could not connect at all).
                    // Retry, so nobody has to restart the daemon to be reachable.
                    eprintln!("ldmuxd: remote bind failed ({e}); retrying every 5s");
                    let arc2 = arc.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            if arc2.lock().await.remote.is_some() {
                                break;
                            }
                            let mut rs = crate::remote::RemoteServer::new(cfg.clone());
                            match rs.start(&arc2).await {
                                Ok((host, port)) => {
                                    eprintln!("ldmuxd: remote up ws://{host}:{port}/ws");
                                    arc2.lock().await.remote = Some(rs);
                                    break;
                                }
                                Err(_) => continue,
                            }
                        }
                    });
                }
            }
        }
        // Start the control socket (the `ldmuxd.sock` Unix socket).
        // The daemon's `handle` method is the handler: each RPC call locks the
        // mutex and calls `handle`.
        let cs = crate::control_socket::ControlSocketServer::new(&self.state_dir);
        let listener = cs.bind().map_err(|e| format!("bind control socket: {e}"))?;
        let handler: crate::control_socket::RpcHandler = Box::new({
            let arc2 = arc.clone();
            move |method: &str, req: &Value| {
                let method = method.to_string();
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                let arc = arc2.clone();
                Box::pin(async move {
                    let mut daemon = arc.lock().await;
                    daemon.handle(&method, params).await
                })
            }
        });
        let handle = tokio::spawn(async move {
            loop {
                match crate::control_socket::serve_accept_with(&cs, &listener, &handler).await {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("ldmuxd: control socket: {e}");
                        break;
                    }
                }
            }
        });
        // Store the handle so the daemon can be dropped cleanly.
        // (For now, we just drop it; the accept loop runs until the process exits.)
        let _ = handle;
        Ok(())
    }

    /// Stop the daemon (the TS `stop`). Stops the remote, closes streams,
    /// removes the socket, releases the lock. Tolerates a partially-started
    /// daemon (a signal during startup).
    pub fn stop(&mut self) {
        if let Some(ref mut r) = self.remote {
            r.stop();
        }
        // Close all streams (the `close` sets `poll_stop`, so the poll
        // threads exit on their next iteration).
        for stream in self.streams.values() {
            stream.close("daemon stopped");
        }
        self.streams.clear();
        // Remove the socket file.
        let sock = self.socket_path();
        let _ = std::fs::remove_file(&sock);
        // Release the lock.
        self.lock.release();
    }

    /// Create a new session (the TS `handleNew`).
    pub async fn create_session(
        &mut self,
        kind: &str,
        cwd: &str,
        title: Option<&str>,
        created_by: &str,
    ) -> Result<SessionRecord, String> {
        let kind = if kind == "shell" || kind == "pi" {
            kind.to_string()
        } else {
            return Err(format!("invalid kind: {kind:?} (expected shell or pi)"));
        };
        let cwd = crate::cwd::resolve_cwd(Some(cwd));
        if !std::path::Path::new(&cwd).is_dir() {
            return Err(format!("not a directory: {cwd}"));
        }
        let id = short_id();
        let tmux_name = format!("ldmux-{id}");
        let command: Vec<String> = if kind == "pi" { vec!["pi".to_string()] } else { vec![] };
        let now: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let record = SessionRecord {
            id: id.clone(),
            tmux_name: tmux_name.clone(),
            kind: kind.to_string(),
            cwd: cwd.clone(),
            title: title.map(|t| t.to_string()).unwrap_or_else(|| {
                if kind == "pi" { "pi".to_string() } else { "shell".to_string() }
            }),
            created_at: now,
            updated_at: now,
            created_by: created_by.to_string(),
            status: "running".to_string(),
            exit_code: None,
            exited_at: None,
        };
        let env: Vec<(String, String)> = vec![
            ("LDMUX".to_string(), "1".to_string()),
            ("LDMUX_SESSION_ID".to_string(), id.clone()),
            ("LDMUX_KIND".to_string(), kind.to_string()),
            ("LDMUX_SOCKET".to_string(), self.socket_path().to_string_lossy().into_owned()),
            // Truthful names. `tmuxName` is the wire/registry field name and cannot
            // change without breaking the paired app, but the *value* is the keeper
            // session id and the socket is the keeper's — say so.
            ("LDMUX_SESSION_NAME".to_string(), tmux_name.clone()),
            ("LDMUX_KEEPER_SOCK".to_string(), self.keeper_socket_path().to_string_lossy().into_owned()),
            // Kept because a shell profile written against the tmux era reads them.
            // They name the same two values; nothing here is a tmux server.
            ("LDMUX_TMUX_NAME".to_string(), tmux_name.clone()),
            ("LDMUX_TMUX_SOCK".to_string(), self.keeper_socket_path().to_string_lossy().into_owned()),
        ];
        // Call the keeper's `Spawn` (the equivalent of the TS `cp.newSession`,
        // retargeted onto the keeper: a `Spawn` creates a new emulator session).
        // The `pi` kind runs `pi`; every session gets the `LDMUX_*` spawn env
        // (plan §2.6, the replacement for the tmux profile's environment).
        let spawn_resp = self.keeper_call(&km::KeeperMessage::Spawn(km::Spawn {
            session_id: tmux_name.clone(),
            cols: 80,
            rows: 24,
            cwd: Some(cwd.clone()),
            command: if command.is_empty() { None } else { Some(command.clone()) },
            env: Some(env.clone()),
        }))
        .await?;
        let _ = spawn_resp;
        self.registry.upsert(record.clone());
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        Ok(record)
    }

    /// Kill a session (the TS `handleKill`).
    pub async fn kill_session(&mut self, id: &str) -> Result<SessionRecord, String> {
        let mut record = self.registry.get(id).cloned().ok_or_else(|| format!("no such session: {id}"))?;
        if !record.tmux_name.is_empty() {
            let _ = self
                .keeper_call(&km::KeeperMessage::Kill(km::Kill {
                    session_id: record.tmux_name.clone(),
                }))
                .await;
        }
        record.status = "dead".to_string();
        let now: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        record.exited_at = Some(now as i64);
        record.updated_at = now;
        record.exit_code = None;
        self.registry.upsert(record.clone());
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        self.forget_session_state(&record.id);
        Ok(record)
    }

    /// Close a session (the TS `handleClose`).
    pub async fn close_session(&mut self, id: &str) -> Result<(bool, SessionRecord), String> {
        let record = self.registry.get(id).cloned().ok_or_else(|| format!("no such session: {id}"))?;
        if self.ownership.owner(id).is_some() {
            return Ok((false, record));
        }
        let record = self.kill_session(id).await?;
        Ok((true, record))
    }

    /// Forget a session (the TS `forget`).
    pub async fn forget_session(&mut self, id: &str) -> Result<bool, String> {
        let record = match self.registry.get(id) {
            Some(r) => r,
            None => return Ok(false),
        };
        if record.status == "running" && !record.tmux_name.is_empty() {
            return Err(format!("{id} is running; end it with kill, not forget"));
        }
        self.forget_session_state(id);
        self.registry.remove(id);
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        Ok(true)
    }

    /// Forget a session's state (the TS `forgetSessionState`).
    /// Clears the ownership record for the session.
    pub fn forget_session_state(&mut self, id: &str) {
        if let Some(claim) = self.ownership.owner(id).cloned() {
            self.ownership.release(id, &claim.device.id);
        }
    }

    /// Get or create the `PaneStream` for a session (the TS `streamFor`).
    ///
    /// The stream is created with a `SocketKeeperConn` (the production path)
    /// that talks to the keeper's socket. A poll thread is spawned that pulls
    /// the keeper's buffered output and fans it out to subscribers (the §3.7
    /// live stream). The `Arc` is shared between the daemon's map and the
    /// poll thread.
    pub fn stream_for(&mut self, tmux_name: &str) -> &std::sync::Arc<PaneStream> {
        if !self.streams.contains_key(tmux_name) {
            let conn = Box::new(crate::stream::SocketKeeperConn::new(
                self.keeper_socket_path().to_string_lossy().into_owned(),
            ));
            let stream = std::sync::Arc::new(PaneStream::new(tmux_name.to_string(), conn));
            // Spawn the poll thread (the §3.7 live stream: pulls the keeper's
            // buffered output and fans it out to subscribers).
            let poll_stream = stream.clone();
            std::thread::spawn(move || {
                loop {
                    if poll_stream.poll_stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let _ = poll_stream.poll_once();
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            });
            self.streams.insert(tmux_name.to_string(), stream);
        }
        self.streams.get(tmux_name).unwrap()
    }

    /// Get a stream by session id (the `streams` map lookup, without creating).
    pub fn get_stream(&mut self, tmux_name: &str) -> Option<&std::sync::Arc<PaneStream>> {
        self.streams.get(tmux_name)
    }

    /// Close and forget a stream (the TS `dropStream`).
    /// Its tmux session is gone.
    pub fn drop_stream(&mut self, tmux_name: &str) {
        if let Some(stream) = self.streams.remove(tmux_name) {
            stream.close("session ended");
        }
    }

    /// Release a stream only if nothing is subscribed to it (the TS
    /// `releaseStreamIfIdle`).
    pub fn release_stream_if_idle(&mut self, tmux_name: &str) {
        // Check the subscriber count without holding a mutable borrow.
        let idle = self
            .streams
            .get(tmux_name)
            .map(|s| s.subscriber_count() == 0)
            .unwrap_or(false);
        if idle {
            self.drop_stream(tmux_name);
        }
    }

    /// Evict streams whose backing session is gone (the TS `sweepStreams`).
    /// Bounded memory: a stream for a dead session is dropped.
    pub fn sweep_streams(&mut self) {
        let to_drop: Vec<String> = self
            .streams
            .keys()
            .filter(|tmux_name| {
                let record = self.registry.by_tmux_name(tmux_name);
                record.is_none() || record.as_ref().map(|r| r.status.as_str()) == Some("dead")
            })
            .cloned()
            .collect();
        for tmux_name in to_drop {
            self.drop_stream(&tmux_name);
        }
    }

    /// The RPC dispatch (the TS `handle` method): maps an RPC method name to a
    /// daemon method and returns the result as a JSON value.
    ///
    /// This is the core of the control socket server: the `serve_one` method
    /// in `control_socket.rs` calls this to handle each RPC request.
    pub async fn handle(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        match method {
            "ping" => {
                Ok(serde_json::json!({
                    "pong": true,
                    "version": ldmux_protocol::rpc::DAEMON_PROTOCOL_VERSION,
                    "tmuxSocket": self.keeper_socket_path().to_string_lossy(),
                }))
            }
            "list" => {
                // Detect keeper death (plan §6): if the keeper is dead,
                // respawn it and reconcile (mark all sessions dead).
                self.ensure_keeper_alive().await?;
                let sessions: Vec<SessionRecord> = self.registry.list();
                Ok(serde_json::json!({ "sessions": sessions }))
            }
            "new" => {
                let kind = params.get("kind").and_then(|v| v.as_str()).unwrap_or("shell");
                let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
                let title = params.get("title").and_then(|v| v.as_str());
                let created_by = params.get("createdBy").and_then(|v| v.as_str()).unwrap_or("local");
                let record = self.create_session(kind, cwd, title, created_by).await?;
                Ok(serde_json::json!({ "session": record }))
            }
            "kill" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let record = self.kill_session(id).await?;
                Ok(serde_json::json!({ "ok": true, "session": record }))
            }
            "close" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let (ended, record) = self.close_session(id).await?;
                Ok(serde_json::json!({ "ok": true, "ended": ended, "session": record }))
            }
            "forget" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let removed = self.forget_session(id).await?;
                Ok(serde_json::json!({ "ok": true, "removed": removed }))
            }
            "desktop" => {
                let inv = self.desktop_inventory().await;
                serde_json::to_value(&inv).map_err(|e| e.to_string())
            }
            "send" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let keys = params.get("keys").and_then(|v| v.as_array());
                let record = self.registry.get(id).ok_or_else(|| format!("no such session: {id}"))?;
                if record.tmux_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Build the input bytes: the text as raw bytes, then each key
                // name converted to its byte sequence (the TS `sendText` +
                // `sendKey` are separate operations, but both write to the PTY;
                // concatenating into one `Write` is equivalent).
                let mut input: Vec<u8> = Vec::new();
                if !text.is_empty() {
                    input.extend_from_slice(text.as_bytes());
                }
                if let Some(keys) = keys {
                    for k in keys {
                        let ks = k.as_str().unwrap_or("");
                        input.extend(crate::stream::key_name_to_bytes(ks));
                    }
                }
                let data = String::from_utf8(input).map_err(|e| format!("invalid input bytes: {e}"))?;
                // Send the input to the keeper (the `Write` message).
                self.keeper_call(&km::KeeperMessage::Write(km::Write {
                    session_id: record.tmux_name.clone(),
                    data,
                }))
                .await?;
                Ok(serde_json::json!({ "ok": true }))
            }
            "capture" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let history = params.get("history").and_then(|v| v.as_bool()).unwrap_or(false);
                let record = self.registry.get(id).ok_or_else(|| format!("no such session: {id}"))?;
                if record.tmux_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Take a snapshot (the keeper's `Snapshot` message).
                let resp = self.keeper_call(&km::KeeperMessage::Snapshot(km::Snapshot {
                    session_id: record.tmux_name.clone(),
                    history_lines: if history { Some(100) } else { None },
                }))
                .await?;
                let text = match &resp {
                    KeeperMessage::SnapshotResult(s) => s.text.clone(),
                    _ => return Err("expected SnapshotResult".to_string()),
                };
                Ok(serde_json::json!({ "screen": text }))
            }
            "snapshot" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let record = self.registry.get(id).ok_or_else(|| format!("no such session: {id}"))?;
                if record.tmux_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Take a snapshot (the keeper's `Snapshot` message).
                let resp = self.keeper_call(&km::KeeperMessage::Snapshot(km::Snapshot {
                    session_id: record.tmux_name.clone(),
                    history_lines: None,
                }))
                .await?;
                let result = match &resp {
                    KeeperMessage::SnapshotResult(s) => s.clone(),
                    _ => return Err("expected SnapshotResult".to_string()),
                };
                Ok(serde_json::to_value(&result).map_err(|e| e.to_string())?)
            }
            // ── Desktop window lifecycle ──
            "makeLive" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let workspace = params.get("workspace").and_then(|v| v.as_str()).unwrap_or("");
                let record = self.registry.get(id).cloned()
                    .ok_or_else(|| format!("no such session: {id}"))?;
                if record.status != "running" {
                    return Ok(serde_json::json!({
                        "ok": false,
                        "detail": format!("that shell is {}", record.status),
                    }));
                }
                if self.live_windows.contains_key(id) {
                    return Ok(serde_json::json!({
                        "ok": false,
                        "detail": "that shell already has a desk window",
                    }));
                }
                let tmux_socket = self.keeper_socket_path().to_string_lossy();
                let tmux_bin = self.keeper_bin().to_string_lossy();
                let result = crate::desktop::open_live(
                    &tmux_socket, &record.tmux_name, id, workspace, &tmux_bin,
                    &self.state_dir.to_string_lossy(),
                ).await;
                match result {
                    Ok(lr) => {
                        if lr.ok {
                            self.live_windows.insert(id.to_string(), (workspace.to_string(), std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap().as_millis() as u64));
                            let dd = self.desktop_device.clone();
                            let _ = self.claim(id, &dd, true);
                        }
                        Ok(serde_json::json!({
                            "ok": lr.ok,
                            "detail": lr.detail,
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "ok": false,
                        "detail": e,
                    })),
                }
            }
            "closeLive" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let result = crate::desktop::close_live(id).await;
                self.live_windows.remove(id);
                let dd_id = self.desktop_device.id.clone();
                let _ = self.release_claim(id, &dd_id);
                Ok(serde_json::json!({
                    "ok": result.unwrap_or(false),
                }))
            }
            "deskWindow" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let record = self.registry.get(id).cloned()
                    .ok_or_else(|| format!("no such session: {id}"))?;
                if record.status != "running" {
                    return Ok(serde_json::json!({
                        "ok": false,
                        "detail": format!("that shell is {}", record.status),
                    }));
                }
                if self.live_windows.contains_key(id) {
                    return Ok(serde_json::json!({
                        "ok": false,
                        "detail": "that shell already has a desk window",
                    }));
                }
                let tmux_socket = self.keeper_socket_path().to_string_lossy();
                let tmux_bin = self.keeper_bin().to_string_lossy();
                let result = crate::desktop::open_live(
                    &tmux_socket, &record.tmux_name, id, "", &tmux_bin,
                    &self.state_dir.to_string_lossy(),
                ).await;
                match result {
                    Ok(lr) => {
                        if lr.ok {
                            self.live_windows.insert(id.to_string(), (String::new(), std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap().as_millis() as u64));
                            let dd = self.desktop_device.clone();
                            let _ = self.claim(id, &dd, true);
                        }
                        Ok(serde_json::json!({
                            "ok": lr.ok,
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "ok": false,
                        "detail": e,
                    })),
                }
            }
            "reclaim" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let device: ldmux_protocol::rpc::DeviceInfo = params
                    .get("device")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .ok_or_else(|| "no device given".to_string())?;
                let result = self.claim(&id, &device, true);
                Ok(serde_json::json!({
                    "ok": matches!(result, crate::ownership::ClaimResult::Acquired { .. }),
                }))
            }
            // ── Remote info ──
            "remote_info" => {
                Ok(serde_json::json!({
                    "hostname": self.remote_cfg.as_ref()
                        .map(|c| c.hostname.clone())
                        .unwrap_or_else(|| "127.0.0.1".to_string()),
                    "port": self.remote_cfg.as_ref().map(|c| c.port).unwrap_or(0),
                    "path": "/ws",
                    "token": self.remote_cfg.as_ref()
                        .map(|c| c.token.clone())
                        .unwrap_or_default(),
                    "listening": self.remote.is_some(),
                }))
            }
            // ── Adopt ──
            "adopt" => {
                let tmux_name = params.get("tmuxName")
                    .and_then(|v| v.as_str()).unwrap_or("");
                let record = self.adopt(tmux_name).await?;
                Ok(serde_json::to_value(&record).map_err(|e| e.to_string())?)
            }
            // ── Clipboard write policy ──
            "clipboardWrite" => {
                let on = params.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
                let path = self.state_dir.join("clipboard-write");
                let content = if on { "on\n" } else { "off\n" };
                std::fs::write(&path, content)
                    .map_err(|e| format!("write clipboard policy: {e}"))?;
                Ok(serde_json::json!({ "on": on }))
            }
            // ── Copy mode (tmux-specific; no keeper equivalent) ──
            "copyMode" => Err("not_implemented: copyMode (tmux-specific, no keeper equivalent)".to_string()),
            // ── Stream-dependent handlers handled by the remote WS server ──
            "attach" | "resize" | "mouseclick" | "input" | "key"
            | "follow" | "scrollback" => {
                Err(format!("not_implemented: {method} (handled by the remote WS server)"))
            }
            _ => Err(format!("unknown method: {method}")),
        }
    }

    /// Adopt the daemon's own sessions (the keeper's `ldmux-<id>` panes the
    /// registry does not list). Mirrors the TS `adoptOwnSessions`: a session the
    /// registry does not know is adopted (a record is created for it), and the
    /// adoption is logged.
    ///
    /// Returns the count adopted.
    pub async fn adopt_own_sessions(&mut self) -> Result<usize, String> {
        let resp = self.keeper_call(&KeeperMessage::List(km::List {})).await?;
        let states = match resp {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => return Err("expected a listresult".into()),
        };
        let mut adopted = 0;
        for s in &states {
            // The keeper's session_id IS the terminal name (`ldmux-<id>`).
            let tmux_name = s.session_id.clone();
            if self.registry.by_tmux_name(&tmux_name).is_some() {
                continue;
            }
            let bare = tmux_name
                .strip_prefix("ldmux-")
                .unwrap_or(&tmux_name)
                .to_string();
            if self.registry.get(&bare).is_some() {
                continue;
            }
            // A failed adopt is not counted (a dead/gone session is not adopted).
            if self.adopt(&tmux_name).await.is_ok() {
                adopted += 1;
            }
        }
        Ok(adopted)
    }

    /// Adopt a single keeper session into the registry (the TS `adopt`).
    ///
    /// Verifies the keeper session is live (a `Read`), and creates a record for
    /// it if one does not already exist. The terminal name is `ldmux-<id>`;
    /// the record's `id` is the keeper `session_id` (one of this daemon's own
    /// sessions keeps its id: it is in the name, in the shell's
    /// `LDMUX_SESSION_ID`, and in any window class made for it).
    pub async fn adopt(&mut self, session_id: &str) -> Result<ldmux_protocol::rpc::SessionRecord, String> {
        // The keeper names sessions `ldmux-<id>`; accept either form (the
        // RPC passes the terminal name, `adopt_own_sessions` the same).
        let tmux_name = if session_id.starts_with("ldmux-") {
            session_id.to_string()
        } else {
            format!("ldmux-{session_id}")
        };
        // Verify the keeper session is live (a `Read`).
        let resp = self
            .keeper_call(&KeeperMessage::Read(km::Read {
                session_id: tmux_name.clone(),
            }))
            .await?;
        let state = match resp {
            KeeperMessage::State(s) => s,
            _ => return Err("expected a state".into()),
        };
        if state.dead {
            return Err("session is dead".into());
        }
        // A record already exists for this session: return it (the adoption is
        // idempotent; two concurrent adopts of one session race to one record).
        if let Some(existing) = self.registry.by_tmux_name(&tmux_name) {
            return Ok(existing.clone());
        }
        let id = tmux_name
            .strip_prefix("ldmux-")
            .unwrap_or(&tmux_name)
            .to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let record = ldmux_protocol::rpc::SessionRecord {
            id,
            tmux_name,
            kind: "shell".into(),
            cwd: state.cwd,
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
        };
        self.registry.upsert(record.clone());
        let _ = self.registry.save();
        Ok(record)
    }

    // ── Remote-facing daemon methods (the callbacks the TS `RemoteServer` takes) ──

    /// Get a session record by id (the TS `getSession`).
    pub fn get_session(&self, id: &str) -> Option<ldmux_protocol::rpc::SessionRecord> {
        self.registry.get(id).cloned()
    }

    /// The session list as clients see it (the TS `describeSessions`).
    ///
    /// One keeper `List` for the per-pane facts (the foreground command and the
    /// pane's own cwd, the TS `paneFacts()`), then the registry record plus the
    /// ownership and the live-window workspace. The facts are what make a row
    /// read `bash`/`pi` and match a desk `pi` by the directory it is in now,
    /// rather than the one it was created in.
    pub async fn describe_sessions(&mut self) -> Vec<SessionInfo> {
        let mut facts: HashMap<String, (String, String)> = HashMap::new();
        if let Ok(KeeperMessage::ListResult(lr)) =
            self.keeper_call(&KeeperMessage::List(km::List {})).await
        {
            for st in lr.sessions {
                facts.insert(st.session_id.clone(), (st.foreground.clone(), st.cwd.clone()));
            }
        }
        let records = self.registry.list();
        records
            .into_iter()
            .map(|record| {
                let fact = facts.get(&record.tmux_name);
                // `cwd` follows the pane when it is running and the keeper
                // knows where it is; otherwise the record's creation directory.
                let cwd = match fact {
                    Some((_, path)) if record.status == "running" && !path.is_empty() => {
                        path.clone()
                    }
                    _ => record.cwd.clone(),
                };
                let process = fact.map(|(cmd, _)| cmd.clone()).filter(|c| !c.is_empty());
                let owner = self.ownership.owner(&record.id).map(|c| SessionOwner {
                    id: c.device.id.clone(),
                    name: c.device.name.clone(),
                    kind: c.device.kind.clone(),
                    since: c.since.max(0) as u64,
                });
                let live_workspace = self
                    .live_windows
                    .get(&record.id)
                    .map(|(ws, _)| ws.clone())
                    .filter(|w| !w.is_empty());
                SessionInfo {
                    id: record.id.clone(),
                    kind: record.kind.clone(),
                    title: session_title(&record, &cwd),
                    cwd,
                    status: record.status.clone(),
                    tmux_name: record.tmux_name.clone(),
                    created_by: record.created_by.clone(),
                    last_activity_at: Some(record.updated_at),
                    process,
                    owner,
                    live_workspace,
                }
            })
            .collect()
    }

    /// Read the desktop inventory (the TS `desktopInventory`).
    pub async fn desktop_inventory(&self) -> ldmux_protocol::rpc::DesktopInventory {
        let home = std::env::var("HOME").unwrap_or_default();
        crate::desktop_inventory::read_desktop(&home).await
    }

    /// Claim a session for a device (the TS `claimSession`, ADR-0010).
    ///
    /// Returns the ownership result. The daemon-side side effects (size,
    /// desk-window handling, displaced notification) are layered on by the
    /// remote (they depend on the transport state the remote owns).
    pub fn claim(
        &mut self,
        session_id: &str,
        device: &ldmux_protocol::rpc::DeviceInfo,
        takeover: bool,
    ) -> crate::ownership::ClaimResult {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        self.ownership.claim(session_id, device.clone(), takeover, Some(now))
    }

    /// Release a claim (the TS `releaseClaim`).
    pub fn release_claim(&mut self, session_id: &str, device_id: &str) -> bool {
        self.ownership.release(session_id, device_id)
    }

    /// Whether a device holds a session (the TS `holds`).
    pub fn holds(&self, session_id: &str, device_id: &str) -> bool {
        self.ownership
            .owner(session_id)
            .map(|c| c.device.id == device_id)
            .unwrap_or(false)
    }

    /// Release all claims for a device (the TS `releaseDevice`).
    ///
    /// Returns the list of session ids that were freed.
    pub fn release_device(&mut self, device_id: &str) -> Vec<String> {
        self.ownership.release_device(device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_keeper::FakeKeeper;
    use ldmux_protocol::keeper::{Kill, Spawn};
    use ldmux_protocol::rpc::SessionRecord;

    fn scratch() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ldmux-daemon-core-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Wait for the fake keeper's socket to exist (the bind→connect race the
    /// fake keeper's own end-to-end test bridges).
    async fn wait_socket(p: &std::path::Path) {
        tokio::time::timeout(std::time::Duration::from_millis(2000), async {
            loop {
                if p.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    /// The daemon's `reconcile`, run against a fake keeper, mirrors the TS
    /// `reconcile`: a live keeper session keeps its record running, and killing
    /// it marks the record dead. The daemon under test cannot tell the fake from
    /// the real keeper (plan §3.5).
    #[tokio::test]
    async fn reconcile_drives_the_registry_against_the_keeper() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        assert_eq!(d.keeper_socket_path(), keeper_sock.as_path());

        // Spawn a session on the fake keeper (the daemon's keeper_call path).
        // The keeper session id IS the tmux name (the daemon passes it at Spawn).
        let spawn = KeeperMessage::Spawn(Spawn {
            session_id: "ldmux-s1".into(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: None,
            env: None,
            });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &spawn,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // A registry record for the session (the terminal name `ldmux-s1`).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        d.registry().upsert(SessionRecord {
            id: "s1".into(),
            tmux_name: "ldmux-s1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
        });

        // Pass 1: the session is live, so nothing is marked dead.
        let r = d.reconcile().await.unwrap();
        assert!(r.dead.is_empty(), "a live session is not dead: {:?}", r);
        assert!(r.adopted.is_empty());

        // Kill the session on the fake keeper.
        let kill = KeeperMessage::Kill(Kill { session_id: "ldmux-s1".into() });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &kill,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // Pass 2: the session is gone, so the record is marked dead.
        let r = d.reconcile().await.unwrap();
        assert_eq!(r.dead, vec!["s1".to_string()]);

        // The record is dead in the registry.
        assert_eq!(d.registry().get("s1").unwrap().status, "dead");

        drop(fk);
    }

    /// `start` (the main loop) against a fake keeper: a keeper session the
    /// registry does not know is adopted (a record is created for it). The
    /// daemon's `start` acquires the lock, ensures the keeper (the fake is
    /// already alive, so no spawn), loads the registry, adopts the session, and
    /// reconciles — all over the keeper socket, never a tmux control plane.
    #[tokio::test]
    async fn start_adopts_the_daemons_own_sessions_against_the_fake_keeper() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));

        // A session the registry does not know (the daemon's own session).
        // The keeper session id IS the tmux name.
        let spawn = KeeperMessage::Spawn(Spawn {
            session_id: "ldmux-s1".into(),
            cols: 80,
            rows: 24,
            cwd: None,
            command: None,
            env: None,
            });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &spawn,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // `start` (the main loop). The daemon is wrapped in an `Arc` so the
        // control socket's spawned tasks can call `handle`.
        let arc: std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>> =
            std::sync::Arc::new(tokio::sync::Mutex::new(d));
        let mut d = arc.lock().await;
        d.start(&arc).await.unwrap();
        drop(d);

        // The session was adopted: the registry has a record for it.
        let mut d = arc.lock().await;
        let r = d.registry().get("s1").unwrap();
        assert_eq!(r.tmux_name, "ldmux-s1");
        assert_eq!(r.status, "running");
        assert_eq!(r.created_by, "adopt");

        drop(fk);
    }

    /// `create_session` against the fake keeper: a new session is created on
    /// the keeper, a record is added to the registry, and the registry is saved.
    #[tokio::test]
    async fn create_session_spawns_on_the_keeper_and_records() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        // Create a session in the current dir (which exists).
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        assert_eq!(record.kind, "shell");
        assert_eq!(record.status, "running");
        assert!(!record.tmux_name.is_empty());
        // The registry has the record.
        assert!(d.registry().get(&record.id).is_some());
        // The keeper has the session.
        let resp = d.keeper_call(&KeeperMessage::List(km::List {})).await.unwrap();
        let list = match &resp {
            KeeperMessage::ListResult(l) => l.clone(),
            _ => panic!("expected ListResult"),
        };
        assert!(list.sessions.iter().any(|s| s.session_id == record.tmux_name));
        drop(fk);
    }

    /// `kill_session` against the fake keeper: the keeper session is killed,
    /// the record is marked dead, and the session state is forgotten.
    #[tokio::test]
    async fn kill_session_kills_on_the_keeper_and_marks_dead() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let id = record.id.clone();
        let _tmux_name = record.tmux_name.clone();
        // Kill the session.
        let killed = d.kill_session(&id).await.unwrap();
        assert_eq!(killed.status, "dead");
        assert!(killed.exited_at.is_some());
        // The registry has the dead record.
        assert_eq!(d.registry().get(&id).unwrap().status, "dead");
        drop(fk);
    }

    /// `close_session`: if no one holds the session, it is killed; the result
    /// is `(true, record)`. If someone holds it, it is not killed; the result
    /// is `(false, record)`.
    #[tokio::test]
    async fn close_session_kills_when_unheld_and_lets_go_when_held() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let id = record.id.clone();
        // No one holds it: close kills it.
        let (ended, _) = d.close_session(&id).await.unwrap();
        assert!(ended);
        assert_eq!(d.registry().get(&id).unwrap().status, "dead");
        // Create another, claim it, and close: it is let go, not killed.
        let record2 = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let id2 = record2.id.clone();
        let device = ldmux_protocol::rpc::DeviceInfo {
            id: "device1".into(),
            name: "test".into(),
            kind: "phone".into(),
        };
        d.claim(&id2, &device, false);
        let (ended2, _) = d.close_session(&id2).await.unwrap();
        assert!(!ended2);
        assert_eq!(d.registry().get(&id2).unwrap().status, "running");
        drop(fk);
    }

    /// `forget_session`: a dead session can be forgotten (removed from the
    /// registry); a running session cannot.
    #[tokio::test]
    async fn forget_session_removes_dead_and_refuses_running() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let id = record.id.clone();
        // A running session cannot be forgotten.
        let err = d.forget_session(&id).await.unwrap_err();
        assert!(err.contains("running"));
        // Kill it, then forget it.
        d.kill_session(&id).await.unwrap();
        let removed = d.forget_session(&id).await.unwrap();
        assert!(removed);
        assert!(d.registry().get(&id).is_none());
        drop(fk);
    }

    /// The RPC dispatch (`handle`): `ping`, `list`, `new`, `kill`, `close`,
    /// `forget`, and `desktop` are dispatched to the daemon's methods.
    #[tokio::test]
    async fn handle_dispatches_rpc_methods() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));

        // `ping` returns the tmux socket.
        let r = d.handle("ping", serde_json::json!({})).await.unwrap();
        assert!(r.get("pong").and_then(|v| v.as_bool()).unwrap_or(false));
        assert_eq!(
            r.get("tmuxSocket").and_then(|v| v.as_str()).unwrap_or(""),
            keeper_sock.to_string_lossy(),
        );

        // `new` creates a session.
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let r = d.handle("new", serde_json::json!({ "kind": "shell", "cwd": cwd })).await.unwrap();
        let id: &str = r.get("session").and_then(|s| s.get("id")).and_then(|v| v.as_str()).unwrap_or("");
        assert!(!id.is_empty());

        // `list` includes the new session.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        assert!(sessions.iter().any(|s| s.get("id").and_then(|v| v.as_str()) == Some(id)));

        // `kill` kills the session.
        let r = d.handle("kill", serde_json::json!({ "id": id })).await.unwrap();
        assert!(r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

        // `list` shows the session as dead.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        let status = sessions
            .iter()
            .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
            .and_then(|s| s.get("status").and_then(|v| v.as_str()))
            .unwrap_or("");
        assert_eq!(status, "dead");

        // `forget` removes the dead session.
        let r = d.handle("forget", serde_json::json!({ "id": id })).await.unwrap();
        assert!(r.get("removed").and_then(|v| v.as_bool()).unwrap_or(false));

        // `list` no longer includes the session.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
        assert!(!sessions.iter().any(|s| s.get("id").and_then(|v| v.as_str()) == Some(id)));

        drop(fk);
    }

    /// The stream lifecycle: `stream_for` creates a stream, `release_stream_if_idle`
    /// releases it when no one is subscribed, and `sweep_streams` drops streams
    /// whose backing session is dead.
    #[tokio::test]
    async fn stream_lifecycle_create_release_sweep() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let tmux_name = record.tmux_name.clone();

        // `stream_for` creates the stream.
        let stream = d.stream_for(&tmux_name);
        assert_eq!(stream.session_id(), tmux_name);
        assert_eq!(stream.subscriber_count(), 0);

        // `release_stream_if_idle` releases the stream (no subscribers).
        d.release_stream_if_idle(&tmux_name);
        assert!(!d.streams.contains_key(&tmux_name));

        // `stream_for` recreates the stream.
        let _stream = d.stream_for(&tmux_name);
        assert!(d.streams.contains_key(&tmux_name));

        // Kill the session, then `sweep_streams` drops the stream.
        d.kill_session(&record.id).await.unwrap();
        d.sweep_streams();
        assert!(!d.streams.contains_key(&tmux_name));

        drop(fk);
    }

    /// The `send`, `capture`, and `snapshot` RPC handlers: `send` writes input
    /// to the keeper, `capture` returns the rendered screen, and `snapshot`
    /// returns the rendered screen + state.
    #[tokio::test]
    async fn send_capture_snapshot_dispatch() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let record = d.create_session("shell", &cwd, None, "local").await.unwrap();
        let id = record.id.clone();

        // `send` writes input to the keeper.
        let r = d.handle("send", serde_json::json!({ "id": id, "text": "hello" })).await.unwrap();
        assert!(r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

        // `capture` returns the rendered screen (the fake keeper's snapshot_text
        // is empty until a test sets it, so we only assert the field exists).
        let r = d.handle("capture", serde_json::json!({ "id": id })).await.unwrap();
        assert!(r.get("screen").is_some(), "the screen field should exist");

        // `snapshot` returns the rendered screen + state.
        let r = d.handle("snapshot", serde_json::json!({ "id": id })).await.unwrap();
        assert!(r.get("cols").and_then(|v| v.as_u64()).is_some());
        assert!(r.get("rows").and_then(|v| v.as_u64()).is_some());

        drop(fk);
    }

    /// Stop removes the socket and the lock (the TS `stop removes the socket
    /// and the lock, and leaves tmux alone`).
    #[tokio::test]
    async fn stop_removes_the_socket_and_the_lock() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(d));
        let mut d = arc.lock().await;
        d.start(&std::sync::Arc::clone(&arc)).await.unwrap();
        drop(d);

        // The socket exists.
        let sock = dir.join("ldmuxd.sock");
        assert!(sock.exists(), "socket should exist after start");
        let lock = dir.join("ldmuxd.lock");
        assert!(lock.exists(), "lock should exist after start");

        // Stop.
        arc.lock().await.stop();

        // The socket is gone.
        assert!(!sock.exists(), "socket should be removed after stop");
        // The lock is gone.
        assert!(!lock.exists(), "lock should be removed after stop");
    }

    /// Stop is idempotent (the TS `stop is idempotent and safe to call twice`).
    #[tokio::test]
    async fn stop_is_idempotent() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(d));
        let mut d = arc.lock().await;
        d.start(&std::sync::Arc::clone(&arc)).await.unwrap();
        drop(d);

        arc.lock().await.stop();
        // A second stop does not panic.
        arc.lock().await.stop();
    }

    /// A state directory that already exists is tightened to 0700 (the TS
    /// `a state directory that already exists is tightened, not trusted`).
    #[tokio::test]
    async fn a_loose_state_dir_is_tightened() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // Set the dir to loose permissions (0775) before the daemon starts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        }

        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(d));
        let mut d = arc.lock().await;
        d.start(&std::sync::Arc::clone(&arc)).await.unwrap();
        drop(d);

        // The dir is tightened to 0700.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "state dir should be tightened to 0700");
        }

        arc.lock().await.stop();
    }

    /// A remote bind failure only disables remote; local control keeps working
    /// (the TS `a remote bind failure only disables remote; local control
    /// keeps working`).
    #[tokio::test]
    async fn a_remote_bind_failure_does_not_stop_local_control() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // Take a port with a raw TCP listener.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Start a daemon that wants the same port.
        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"))
            .with_remote("127.0.0.1".to_string(), port, "test-token".to_string());
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(d));
        let mut d = arc.lock().await;
        d.start(&std::sync::Arc::clone(&arc)).await.unwrap();
        drop(d);

        // The daemon started (the remote bind failed, but the daemon is alive).
        // The control socket works.
        let sock = dir.join("ldmuxd.sock");
        assert!(sock.exists(), "control socket should exist");

        // Drop the listener so the port is free.
        drop(listener);

        arc.lock().await.stop();
    }
}


