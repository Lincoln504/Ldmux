//! The `LdmuxDaemon`: the daemon's main state (the glue).
//!
//! It owns the keeper (auto-spawned, [`KeeperSupervisor`]), the session
//! registry (the persisted table), and the ownership (who holds which shell).
//! Everything session-related is a keeper call over the socket — a `List`
//! for the live set, a `Read` for liveness, a `State` for the pane facts — so
//! the daemon under test runs against a [fake_keeper](crate::fake_keeper), not
//! a live keeper (plan §3.5).
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

/// A desk window seen by a maintenance pass: its session id and the `since`
/// stamp it had, so a window re-opened meanwhile is not confused with it.
type WindowStamp = (String, u64);

/// What one maintenance tick worked out under the lock (see `maintain_plan`).
struct MaintainPlan {
    result: ReconcileResult,
    live_changed: bool,
    cwd_changed: bool,
    /// Windows only the window manager can vouch for: `(id, since)`.
    probes: Vec<WindowStamp>,
    /// Windows already known gone.
    absent: Vec<WindowStamp>,
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
    /// The adopted-session set changed since the last pass. It is reported on
    /// every pass and never recorded, so only a change is news (the TS
    /// `unknownChanged`).
    pub unknown_changed: bool,
}

/// How often the daemon reconciles the world against the keeper and the window
/// manager (the TS `setInterval(..., 2000)`).
///
/// Without this pass a shell that typed `exit` stays `running` in the registry
/// until the daemon restarts, a `cd` never reaches a client that is not polling
/// `list`, and a desk window the user closed stays recorded for good.
const MAINTENANCE_INTERVAL_MS: u64 = 2000;

/// How long a claim may stand with no connection behind it **that never had
/// one**.
///
/// A claim is taken on a window's behalf *before* the window has attached
/// (`makeLive`, `deskWindow`), so it is legitimately unbacked while the window
/// connects — a fraction of a second. Past the grace, an unbacked claim is a
/// shell no device is using and no device can take without a takeover: a
/// displaced desk window waits for its shell to be *free*, so it stays on
/// "Press r to take it back" for good. Found in use, 2026-10-05.
///
/// A claim whose connection *went away* is a different question and gets
/// [`RECONNECT_GRACE_MS`]; the daemon knows which is which from
/// [`LdmuxDaemon::ever_backed`].
pub const CLAIM_GRACE_MS: u64 = 3_000;

/// How long a claim outlives the connection that was using it, waiting for the
/// same device to come back.
///
/// Sized against the client's own reconnect policy, not against impatience:
/// when the phone's link drops it spends `NETWORK_PROBE_MS` (2.5 s) deciding the
/// old socket is dead, then `RETRY_BASE_MS` (400 ms, plus jitter) before the
/// first attach and backs off from there. The old 3 s grace landed *on* that
/// first retry, and the sweep released the shell the moment the phone tried to
/// come back — so a dropped link became a real handover to a waiting desk
/// window, and the phone reported a takeover nobody had made.
pub const RECONNECT_GRACE_MS: u64 = 15_000;

/// How much history `capture --history` asks for: the TypeScript's own cap
/// (`HISTORY_CAPTURE_LINES`, `origin/main:packages/daemon/src/tmux/control-plane.ts:200`),
/// which is what a client paging a pane's scrollback expects to be able to reach.
/// The port hard-coded 100, so the CLI showed a hundred lines of a history the
/// pane had thousands of (measured 2026-10-05: 995 rows retained, 123 returned).
pub(crate) const HISTORY_CAPTURE_LINES: u64 = 5000;

/// How long a desk window may be missing before it counts as closed.
///
/// A window is not on screen the instant `makeLive` returns: i3 maps it a beat
/// later. A pass that landed in that gap would read "absent" and end a shell the
/// user had just asked for, so a window younger than this is left alone.
const LIVE_WINDOW_GRACE_MS: u64 = 5000;

/// Epoch milliseconds.
///
/// One definition, in the crate's own module, and it does not panic: a clock
/// before 1970 is the only way `duration_since` fails, and taking the daemon
/// down over it would drop every session's stream with it. `remote.rs` had a
/// second copy of this body and `daemon.rs` had ten copies of a panicking one.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The session's desk window, as the daemon remembers it.
///
/// `pid` is set exactly when the daemon did *not* launch the window: `ldmux
/// shell` and `ldmux run` become one in the terminal they were started from,
/// so there is no window of ours to find by class and the process is the
/// window. A missing `pid` means an i3 window the daemon opened, which the
/// window manager answers for.
struct LiveWindow {
    workspace: String,
    since: u64,
    pid: Option<i32>,
}

/// Run one pass of a background loop on its own task, so a panic in it ends that
/// pass and not the loop.
///
/// The loops that keep the daemon honest (maintenance, the claim and idle sweep) are
/// detached tasks: a panic in one is a journal line and then silence, and the daemon
/// carries on answering requests while never again doing the thing the loop was for.
/// The daemon lock is a `tokio` mutex, so a pass that panics while holding it
/// releases it as it unwinds and the next pass is not blocked.
pub(crate) async fn survive<F>(what: &'static str, pass: F) -> Option<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::spawn(pass).await {
        Ok(v) => Some(v),
        Err(e) => {
            crate::stream::note_panic(what);
            eprintln!("ldmuxd: {what} pass ended abnormally ({e}); the loop continues");
            None
        }
    }
}

/// Is the process still there? The kernel's answer, and the only one for a
/// window that is not ours (the TS `processAlive`).
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // `kill(pid, 0)`: portable (no `/proc`), and EPERM still means "there".
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// One keeper call, respawning a dead keeper once (see `LdmuxDaemon::keeper_call`).
/// A free function over the supervisor so a caller that does not hold the daemon
/// lock can make it.
async fn keeper_call_via(
    keeper: &KeeperSupervisor,
    req: &KeeperMessage,
) -> Result<KeeperMessage, String> {
    // A path that is not valid UTF-8 is an error, not a panic.
    let keeper_sock = keeper
        .socket_path()
        .to_str()
        .ok_or_else(|| "the keeper socket path is not valid UTF-8".to_string())?
        .to_string();
    match ldmux_protocol::keeper_call::keeper_call(
        &keeper_sock,
        req,
        ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
    )
    .await
    {
        Ok(resp) => Ok(resp),
        Err(e) if e.is_connect() => {
            // Nothing was sent. Respawn a dead keeper (or wait for one that
            // is binding) and try once more.
            keeper
                .ensure()
                .await
                .map_err(|e| format!("keeper respawn failed: {e}"))?;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            ldmux_protocol::keeper_call::keeper_call(
                &keeper_sock,
                req,
                ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
            )
            .await
            .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Make sure the state dir exists, is ours, and is 0700.
fn ensure_private_state_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("create state dir {}: {e}", dir.display()))?;
    }
    let md =
        std::fs::metadata(dir).map_err(|e| format!("stat state dir {}: {e}", dir.display()))?;
    if !md.is_dir() {
        return Err(format!(
            "the state dir {} is not a directory",
            dir.display()
        ));
    }
    let uid = nix::unistd::getuid().as_raw();
    if md.uid() != uid {
        return Err(format!(
            "the state dir {} is owned by uid {}, not {uid}; refusing to use it",
            dir.display(),
            md.uid()
        ));
    }
    if md.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            format!(
                "the state dir {} has mode {:o} and could not be set to 700: {e}",
                dir.display(),
                md.mode() & 0o777
            )
        })?;
    }
    Ok(())
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

/// The kind of an ending that went through `kill_session`, from its reason: the
/// reasons are built by the callers (`closed from <device>`, `its desk window
/// closed`, `killed ...`), and this is the one place that classifies them.
fn ended_kind_of(reason: &str) -> &'static str {
    if reason.starts_with("closed") {
        "closed"
    } else if reason.contains("desk window closed") {
        "window_closed"
    } else {
        "killed"
    }
}

/// The daemon's main state: the keeper, the registry, the ownership, and the
/// single-instance lock.
pub struct LdmuxDaemon {
    state_dir: PathBuf,
    /// Shared (`Arc`) so a maintenance pass can talk to the keeper without
    /// holding the daemon lock.
    keeper: std::sync::Arc<KeeperSupervisor>,
    /// The control socket path: `<state_dir>/ldmuxd.sock` unless `LDMUX_SOCK`
    /// (resolved by `protocol::paths`, in `ldmuxd`) says otherwise.
    socket_path: PathBuf,
    registry: Registry,
    ownership: Ownership,
    dead_record_ttl_ms: u64,
    max_records: usize,
    /// The control socket's accept loop.
    ///
    /// `tokio::spawn` starts it and returns a handle; the handle was dropped on
    /// the floor with a comment saying so. Dropping a `JoinHandle` does not stop
    /// the task — it *detaches* it — so `stop()` removed the socket file and
    /// released the lock while the accept loop went on holding the bound
    /// listener and serving it. Nothing in production noticed (the process
    /// exits), but a stop that does not stop is the kind of thing that is only
    /// discovered when a second daemon or a test starts in the same process.
    control_handle: Option<tokio::task::JoinHandle<()>>,
    /// Whether anything a client can *see* changed since the last broadcast.
    ///
    /// Set by every mutator, consumed by `maintain`. It exists because the daemon
    /// has three doors — the control socket (the CLI), the remote WS, and its own
    /// maintenance — and only the remote's handlers pushed a fresh list. So
    /// `ldmux kill` marked a record dead, the next maintenance tick found it
    /// *already* dead (`registry.reconcile` only reports a **transition**), no
    /// change was detected, and no list was sent: measured, a connected client
    /// received nothing for 8s after a kill and went on showing a shell that no
    /// longer existed. The mutator marks the state it changed; whoever owns the
    /// clients pushes it.
    sessions_dirty: bool,
    /// Since when each claim has had no connection behind it, by session id
    /// (the device that holds it is part of the value, so a claim that moves
    /// starts its wait again). See [`LdmuxDaemon::release_unbacked_claims`].
    unbacked_since: std::collections::HashMap<String, (String, u64)>,
    /// `(session, device)` claims that have had a connection attached to that
    /// session at least once.
    ///
    /// It is what separates the two reasons a claim can have no connection
    /// behind it, and the two have different graces: a claim taken on a
    /// window's behalf that never attached is nobody's ([`CLAIM_GRACE_MS`]),
    /// while a claim whose working connection went away belongs to that device
    /// and is waiting for it ([`RECONNECT_GRACE_MS`]). Without this the sweep
    /// released a phone's shell three seconds after a dropped link — exactly
    /// when the phone re-attaches — and a waiting desk window took it, which the
    /// phone reported as "the desktop took it over".
    ever_backed: std::collections::HashSet<(String, String)>,
    /// The single-instance lock (the `ldmuxd.lock` pid lockfile); `Drop`
    /// releases it.
    lock: DaemonLock,
    /// The remote WS server (the phone-facing endpoint); `Option` because the
    /// remote is optional (a bind failure must not take down local control).
    remote: Option<crate::remote::RemoteServer>,
    /// The remote config (the hostname, port, token).
    remote_cfg: Option<crate::remote::RemoteConfig>,
    /// The connection table and sweep every listener serves into (the tailnet
    /// WebSocket and the local attach socket).
    conn_shared: Option<std::sync::Arc<crate::remote::Shared>>,
    /// The last `speech` sequence number handed out per session. Ordering on the phone is
    /// this number, not arrival order.
    speech_seq: std::collections::HashMap<String, u64>,
    /// The local attach socket's accept loop.
    local_handle: Option<tokio::task::JoinHandle<()>>,
    /// The per-session streams (the `Arc<PaneStream>` per keeper session).
    /// The `Arc` is shared with the poll thread (which holds its own `Arc`),
    /// so the stream outlives the daemon's map entry (the poll thread keeps
    /// it alive until it exits).
    streams: HashMap<String, std::sync::Arc<PaneStream>>,
    /// Sessions with a window open on the desktop's screen (the TS
    /// `liveWindows`): one window per shell, recorded so the daemon can
    /// close it and so a phone that opens it is asked first (ADR-0010).
    live_windows: HashMap<String, LiveWindow>,
    /// The desktop's device identity (the ADR-0010 claim source for desk
    /// windows; the TS `desktopDevice`).
    desktop_device: ldmux_protocol::rpc::DeviceInfo,
    /// Each running session's pane directory at the last maintenance pass: the
    /// only way to notice a `cd` and tell a client that is not polling (the TS
    /// `lastCwds`).
    last_cwds: HashMap<String, String>,
    /// The adopted-session set as of the last pass. Only a *change* is news:
    /// reacting to the set being non-empty made one stray session an endless
    /// loop of save-broadcast-reconcile (the TS `unknownSessionsKey`).
    unknown_sessions_key: String,
    /// Set by `stop`: the maintenance loop exits rather than keeping the daemon
    /// (and its keeper, and its state dir) alive after a test is done with it.
    maintenance_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// How long a recorded desk window may be missing before it counts as
    /// closed. A field so the window-close rule can be tested without waiting
    /// the real five seconds for a window that is never going to appear.
    live_window_grace_ms: u64,
    /// When the daemon first saw each keeper session dead (monotonic), by keeper
    /// name. A shell that has exited keeps its pane in the keeper for
    /// [`DEAD_KEEP_MS`] so a client that reconnects can still read its final
    /// screen and be told how it ended; only then is it released.
    dead_seen: HashMap<String, std::time::Instant>,
    dead_keep_ms: u64,
    /// The daemon's monotonic clock: milliseconds since this base. Every timer
    /// the daemon runs (claim grace, restore grace) reads this, never the wall
    /// clock, so a clock step (NTP, a suspended laptop) cannot free a claim early
    /// or hold one for ever. `now_ms` stays for what is *shown* (`since`, record
    /// times).
    mono_base: std::time::Instant,
    /// Claims reloaded at startup, by session id → the monotonic ms until which
    /// they are held for their device to come back (see `load_claims`).
    restored: HashMap<String, u64>,
}

/// How long a claim restored from disk waits for its device to reconnect after a
/// daemon restart. Longer than [`CLAIM_GRACE_MS`]: the phone backs off between
/// retries (up to ~30 s) and a daemon restart is when it is retrying.
pub const RESTORE_GRACE_MS: u64 = 60_000;

/// How long the daemon leaves an ended shell's pane in the keeper (see
/// `dead_seen`). Short on purpose: it is a window for a reconnect, not an archive.
pub const DEAD_KEEP_MS: u64 = 60_000;

impl Drop for LdmuxDaemon {
    fn drop(&mut self) {
        // The lock is released by its own `Drop` (the `DaemonLock`'s `Drop`).
        // The remote is stopped here (its `stop()` sets the stopped flag).
        if let Some(r) = self.remote.as_mut() {
            r.stop();
        }
    }
}

/// The most text one `speech` call may carry. A spoken summary is a few sentences; a runaway
/// answer is refused rather than queued on a phone.
pub const SPEECH_MAX_BYTES: usize = 4096;

impl LdmuxDaemon {
    /// A daemon rooted at `state_dir`, whose keeper binary is `keeper_bin`.
    ///
    /// The keeper's socket and the registry live under `state_dir`, exactly as
    /// the TS daemon's (the `ldmuxd.sock`, the `ldmuxd.lock`, and the
    /// `registry.json` all live there).
    pub fn new(state_dir: PathBuf, keeper_bin: PathBuf) -> Self {
        Self {
            state_dir: state_dir.clone(),
            keeper: std::sync::Arc::new(KeeperSupervisor::new(state_dir.clone(), keeper_bin)),
            socket_path: state_dir.join("ldmuxd.sock"),
            registry: Registry::new(state_dir.join("registry.json").to_string_lossy()),
            ownership: Ownership::new(),
            dead_record_ttl_ms: 24 * 60 * 60 * 1000,
            max_records: 500,
            lock: DaemonLock::new(&state_dir),
            remote: None,
            remote_cfg: None,
            conn_shared: None,
            speech_seq: std::collections::HashMap::new(),
            local_handle: None,
            streams: HashMap::new(),
            control_handle: None,
            sessions_dirty: false,
            unbacked_since: HashMap::new(),
            ever_backed: std::collections::HashSet::new(),
            live_windows: HashMap::new(),
            desktop_device: ldmux_protocol::rpc::DeviceInfo {
                id: "desktop".into(),
                name: "desktop".into(),
                kind: "desktop".into(),
            },
            last_cwds: HashMap::new(),
            unknown_sessions_key: String::new(),
            maintenance_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            live_window_grace_ms: LIVE_WINDOW_GRACE_MS,
            dead_seen: HashMap::new(),
            dead_keep_ms: DEAD_KEEP_MS,
            mono_base: std::time::Instant::now(),
            restored: HashMap::new(),
        }
    }

    /// Configure the remote (the hostname, port, token) before `start`.
    pub fn with_remote(mut self, hostname: String, port: u16, token: String) -> Self {
        self.remote_cfg = Some(crate::remote::RemoteConfig::new(hostname, port, token));
        self
    }

    /// Shorten the live-window grace period. A test hook: the rule it guards is
    /// real, and five seconds per assert is not.
    pub fn with_live_window_grace_ms(mut self, ms: u64) -> Self {
        self.live_window_grace_ms = ms;
        self
    }

    /// How long an ended shell's pane is kept in the keeper. A test hook (`0`
    /// releases it at once, as the daemon did before the final screen was kept).
    pub fn with_dead_keep_ms(mut self, ms: u64) -> Self {
        self.dead_keep_ms = ms;
        self
    }

    /// Shorten the remote's idle deadline. A test hook: the deadline is what the
    /// keep-alive contract is against, and twenty seconds is a long test.
    pub fn with_remote_idle_ms(mut self, idle_ms: u64) -> Self {
        if let Some(cfg) = self.remote_cfg.take() {
            self.remote_cfg = Some(cfg.with_idle_ms(idle_ms));
        }
        self
    }

    /// Serve the control socket at `path` instead of `<state_dir>/ldmuxd.sock`
    /// (what `ldmux_protocol::paths::resolve_socket_path` returns, so the daemon,
    /// the CLI and the attach client all agree on one place).
    pub fn with_socket_path(mut self, path: PathBuf) -> Self {
        self.socket_path = path;
        self
    }

    /// The control socket path (the `ldmuxd.sock`).
    pub fn socket_path(&self) -> std::path::PathBuf {
        self.socket_path.clone()
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
    /// Keeper-death detection (plan §6): a call that could not **connect** is
    /// retried once after `ensure` has respawned (or waited for) the keeper.
    ///
    /// Only a connect failure is retried, and that is the point of the kind:
    /// a connect failure means the request never reached the keeper, so the
    /// retry cannot apply it twice. A timeout or a read failure means the
    /// keeper may have run it already, so retrying could type a paste twice or
    /// open two shells.
    ///
    /// This used to be gated on `!is_alive()`, i.e. it retried only when the
    /// keeper looked dead. That gate was wrong in exactly the case that
    /// mattered: a daemon started next to a **stale keeper socket** (the file
    /// a killed keeper leaves behind) failed its first connect with
    /// `ECONNREFUSED`, then the fresh keeper bound the socket within the
    /// liveness probe's 500 ms of retries, so the probe said "alive" and the
    /// original refused connect was returned as fatal. The daemon exited, and
    /// systemd restarted it two seconds later. Measured on this box on
    /// 2026-10-06: `ldmuxd: connect: Connection refused (os error 111)` at
    /// 02:41:07, restart at 02:41:09. The window is a `ldmux-terminal`
    /// fallback away from a plain, session-less terminal.
    async fn keeper_call(&mut self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
        keeper_call_via(&self.keeper, req).await
    }

    /// Detect keeper death and respawn (plan §6): call the keeper; if it's
    /// dead, respawn it and reconcile (mark all sessions dead).
    async fn ensure_keeper_alive(&mut self) -> Result<(), String> {
        if self.keeper.is_alive().await {
            return Ok(());
        }
        self.keeper
            .ensure()
            .await
            .map_err(|e| format!("keeper respawn failed: {e}"))?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Reconcile: the new keeper is empty, so all sessions are dead.
        let _ = self.reconcile().await;
        Ok(())
    }

    /// Reconcile the registry against the keeper's live sessions.
    ///
    /// The retarget of the TS `reconcile`: the live set comes from a keeper
    /// `List` (each keeper `session_id` mapping to
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
        self.kill_dead_keeper_sessions(&states).await;
        Ok(self.apply_reconcile(states))
    }

    /// End a keeper session whose shell has exited.
    ///
    /// A shell that exits ends its session (`remain-on-exit off`, SPEC §5.2), so
    /// the keeper should not hold the pane afterwards — but it does: it marks the
    /// session `dead` and keeps the record so `/proc` reads do not go stale. The
    /// emulator, the master pty and the child's slot all stay allocated, and
    /// **nothing ever released them**, so every shell that had ever exited stayed
    /// in the keeper for the life of the process. The keeper has reaped the child
    /// and this is the authority on the death, so the daemon releases what is
    /// left: the terminal is the expensive part.
    ///
    /// Idempotent and self-healing: it runs on every pass, so a pane leaked
    /// before this existed is collected on the next tick.
    pub async fn kill_dead_keeper_sessions(&mut self, states: &[ldmux_protocol::keeper::State]) {
        for name in self.dead_sessions_due(states) {
            let _ = self
                .keeper_call(&KeeperMessage::Kill(km::Kill { session_id: name }))
                .await;
        }
    }

    /// The dead keeper sessions whose kept-for-a-reconnect time is up (see
    /// `dead_seen`), and the bookkeeping that decides it. Quick and synchronous, so
    /// a caller that does its keeper I/O unlocked can ask under the lock and kill
    /// outside it.
    pub(crate) fn dead_sessions_due(
        &mut self,
        states: &[ldmux_protocol::keeper::State],
    ) -> Vec<String> {
        let now = std::time::Instant::now();
        // Forget sessions the keeper no longer lists as dead.
        self.dead_seen
            .retain(|name, _| states.iter().any(|s| s.dead && &s.session_id == name));
        let mut due = Vec::new();
        for st in states.iter().filter(|s| s.dead) {
            let since = *self.dead_seen.entry(st.session_id.clone()).or_insert(now);
            if now.duration_since(since).as_millis() as u64 >= self.dead_keep_ms {
                self.dead_seen.remove(&st.session_id);
                due.push(st.session_id.clone());
            }
        }
        due
    }

    /// The reconcile pass against a keeper listing already in hand, so a
    /// maintenance tick pays for one `List` and not three.
    pub fn apply_reconcile(
        &mut self,
        states: Vec<ldmux_protocol::keeper::State>,
    ) -> ReconcileResult {
        // The live set, as terminal names (`ldmux-<id>`), the registry's key.
        // The keeper's `session_id` IS that name (the daemon passes the
        // keeper name at Spawn), so it is used verbatim — re-prefixing it made every
        // session look dead on restart and duplicated it via `adopt`.
        //
        // **`dead` is the point.** The keeper keeps a pane whose shell has exited
        // so its last screen stays readable, and reports it `dead: true`. Counting
        // it live is what left a shell that typed `exit` reading `running` until
        // the daemon was restarted.
        let live: std::collections::BTreeSet<String> = states
            .iter()
            .filter(|s| !s.dead)
            .map(|s| s.session_id.clone())
            .collect();
        let now = now_ms() as i64;
        let (dead, adopted) = self.registry.reconcile(Some(&live), now);
        let pruned =
            self.registry
                .prune(Some(self.dead_record_ttl_ms), Some(self.max_records), now);
        // A shell that ended on its own (`exit`, killed outside ldmux) has to lose
        // its claim and its window record exactly as one killed through ldmux
        // does. Only the killing paths did this, so a shell that exited kept its
        // owner until the record was pruned a day later.
        //
        // Which ending it is comes from the keeper: a pane still there and marked
        // `dead` is a shell that exited and whose last screen is being kept, and
        // a name the keeper does not have at all came back without it (a keeper
        // restart, or a pane its own reaper collected). Both were silent before.
        let gone_from_keeper: std::collections::HashSet<String> = states
            .iter()
            .filter(|s| s.dead)
            .map(|s| s.session_id.clone())
            .collect();
        for id in &dead {
            self.forget_session_state(id);
            let name = self
                .registry
                .get(id)
                .map(|r| r.keeper_name.clone())
                .unwrap_or_default();
            let exited = gone_from_keeper.contains(&name);
            let (kind, reason) = if exited {
                ("exited", "its shell exited")
            } else {
                ("keeper_lost", "the keeper no longer has it")
            };
            // How the shell went, from the keeper's `waitpid`: written before the
            // ending is noted, so everything that reads the record (the list, the
            // `session_ended` a watching client is sent) already has it.
            if let Some(st) = states.iter().find(|s| s.session_id == name)
                && let Some(mut r) = self.registry.get(id).cloned()
            {
                r.exit_code = st.exit_code;
                r.exit_signal = st.exit_signal;
                self.registry.upsert(r);
            }
            self.note_ended(id, kind, reason);
            // A shell that ended on its own is as over as one that was killed,
            // and the client watching it has to be told. `kill_session` drops the
            // stream; this path did not, so a desk window whose shell exited sat
            // on a dead pane for good — measured after typing `exit`: the window
            // still on screen with its three attach processes alive, while the
            // registry said `dead`.
            if !name.is_empty() {
                self.drop_stream(&name);
            }
        }
        // Only a *change* in the adopted set is news. Reacting to it being
        // non-empty made one stray session an endless loop: save, broadcast, and
        // the broadcast's own listing found it again.
        let unknown_key = adopted.join("\n");
        let unknown_changed = unknown_key != self.unknown_sessions_key;
        self.unknown_sessions_key = unknown_key;
        if !dead.is_empty() || unknown_changed || !pruned.is_empty() {
            let _ = self.registry.save();
        }
        // Claims for sessions that no longer exist are the other half of stale
        // state: a session killed outside ldmux leaves an owner pointing at
        // nothing, and a later session reusing that id inherits it (ADR-0010).
        let known: HashSet<String> = self.registry.list().into_iter().map(|r| r.id).collect();
        if !self.ownership.retain(&known).is_empty() {
            self.save_claims();
        }
        for id in self.live_windows.keys().cloned().collect::<Vec<_>>() {
            if !known.contains(&id) {
                self.live_windows.remove(&id);
            }
        }
        // A stream whose session has no record any more (pruned, forgotten) or
        // is dead is a poll thread polling nothing.
        self.sweep_streams();
        ReconcileResult {
            dead,
            adopted,
            pruned,
            unknown_changed,
        }
    }

    /// Reconcile the desk windows against reality: is each recorded window still
    /// on screen?
    ///
    /// `live_windows` is written by `makeLive`/`deskWindow` and, until this pass
    /// existed, cleared only by `closeLive`. A user closes a window with the
    /// window manager, not with the CLI, so the record outlived the window for
    /// good: `makeLive` then refused with "that shell already has a desk window"
    /// and the shell could never be made live again.
    ///
    /// Closing a desk window **ends the shell**, everywhere — including under a
    /// phone that has it, which is told the shell is gone and goes back to its
    /// launcher. That is the user's rule, stated twice (2026-09-17, 2026-09-26).
    ///
    /// Only an explicit "absent" may end anything: a window manager that does not
    /// answer must never be read as a closed window.
    pub async fn reconcile_live(&mut self) -> bool {
        let (mut changed, probes, mut absent) = self.live_prepass();
        for (id, since) in probes {
            if crate::desktop::live_window_state(&id).await.as_deref() == Ok("absent") {
                absent.push((id, since));
            }
        }
        changed |= self.end_absent_windows(absent).await;
        changed
    }

    /// The part of [`Self::reconcile_live`] that needs no I/O beyond the kernel.
    ///
    /// Returns `(changed, probes, absent)`: `probes` are the windows only the
    /// window manager can answer for (each a spawn of `i3-msg get_tree`, which
    /// is why they are returned instead of run here — the caller can ask without
    /// the daemon lock), and `absent` are windows already known gone (a window
    /// that is a process whose process is not there). Each carries the `since`
    /// stamp it was seen with, so a window re-opened meanwhile is left alone.
    fn live_prepass(&mut self) -> (bool, Vec<WindowStamp>, Vec<WindowStamp>) {
        let mut changed = false;
        let mut probes = Vec::new();
        let mut absent = Vec::new();
        let now = self.mono_ms();
        for id in self.live_windows.keys().cloned().collect::<Vec<_>>() {
            let record = self.registry.get(&id).cloned();
            match record {
                Some(r) if r.status == "running" => {}
                // The record is gone, or its shell has ended: the window is not
                // this daemon's business any more.
                _ => {
                    // The record is gone or the shell has ended: it has no window
                    // any more, and saying otherwise would resurrect the window in
                    // the list through the record fallback.
                    self.live_windows.remove(&id);
                    self.set_live_window(&id, None, None);
                    changed = true;
                    continue;
                }
            }
            // A window that was just opened is not on screen yet.
            let (opened_at, pid) = match self.live_windows.get(&id) {
                Some(w) => (w.since, w.pid),
                None => (0, None),
            };
            if now.saturating_sub(opened_at) < self.live_window_grace_ms {
                continue;
            }
            match pid {
                // The window is a process: the kernel answers, and it answers
                // immediately. This is the `ldmux shell`/`run` case, where there
                // is no window of ours for the window manager to find.
                Some(p) => {
                    if !process_alive(p) {
                        absent.push((id, opened_at));
                    }
                }
                None => probes.push((id, opened_at)),
            }
        }
        (changed, probes, absent)
    }

    /// End the shells of windows that are gone (see [`Self::live_prepass`]).
    async fn end_absent_windows(&mut self, absent: Vec<(String, u64)>) -> bool {
        let mut changed = false;
        for (id, since) in absent {
            // Re-opened, or already handled, while the probe ran.
            if self.live_windows.get(&id).map(|w| w.since) != Some(since) {
                continue;
            }
            self.live_windows.remove(&id);
            changed = true;
            self.set_live_window(&id, None, None);
            let _ = self.kill_session(&id, "its desk window closed").await;
        }
        changed
    }

    /// Record, in the registry, the desk window a session has (or `None` when it
    /// no longer has one), and the pid of the process that window is when the
    /// daemon did not launch it.
    ///
    /// The live-window map is memory and the windows are not, so a daemon
    /// restart needs this to tell "its window was closed while we were down"
    /// from "it never had one". See [Self::rebuild_live_windows].
    fn set_live_window(&mut self, id: &str, workspace: Option<String>, pid: Option<i32>) {
        if let Some(mut r) = self.registry.get(id).cloned() {
            if r.live_workspace == workspace && r.live_pid == pid {
                return;
            }
            r.live_workspace = workspace;
            r.live_pid = pid;
            self.registry.upsert(r);
            let _ = self.registry.save();
        }
    }

    /// Recover which sessions have desk windows after a restart.
    ///
    /// The TS rebuilt the live map from the pid its shim wrote into the session:
    /// a window still running was re-adopted, and one whose pid was gone had
    /// been closed while the daemon was down, so its shell was ended. The port
    /// has no shim, so the registry records the workspace and the window manager
    /// is asked directly. Only a definite "absent" ends a shell.
    /// Recover which sessions have desk windows after a restart.
    ///
    /// The TS rebuilt the live map from the pid its shim wrote into the session:
    /// a window still running was re-adopted, and one whose pid was gone had
    /// been closed while the daemon was down, so its shell was ended. The port
    /// keeps that pid in the record for a window the daemon did not launch
    /// (`ldmux shell`/`run`), and asks the window manager by class for one it
    /// did. Only a definite "absent" ends a shell.
    pub async fn rebuild_live_windows(&mut self) -> bool {
        let mut changed = false;
        // Adopt the windows that are actually on screen. The map is memory and a
        // daemon that has just started has none, so a window opened before this
        // process started would otherwise be untracked until its next `makeLive`,
        // and closing it would not end its shell.
        for id in crate::desktop::live_window_ids().await {
            let Some(rec) = self.registry.get(&id).cloned() else {
                continue;
            };
            if rec.status != "running" {
                continue;
            }
            if rec.live_workspace.is_none() {
                self.set_live_window(&id, Some(String::new()), None);
            }
            let now = self.mono_ms();
            self.live_windows.entry(id).or_insert(LiveWindow {
                workspace: rec.live_workspace.unwrap_or_default(),
                since: now,
                pid: None,
            });
        }
        for r in self.registry.list() {
            let Some(workspace) = r.live_workspace.clone() else {
                continue;
            };
            if r.status != "running" {
                self.set_live_window(&r.id, None, None);
                changed = true;
                continue;
            }
            let now = self.mono_ms();
            let present = match r.live_pid {
                Some(pid) => process_alive(pid),
                None => crate::desktop::live_window_state(&r.id).await.as_deref() != Ok("absent"),
            };
            if present {
                self.live_windows.entry(r.id.clone()).or_insert(LiveWindow {
                    workspace,
                    since: now,
                    pid: r.live_pid,
                });
                continue;
            }
            // The window was closed while no daemon was watching. Closing a
            // desk window ends its shell.
            eprintln!(
                "ldmuxd: {}'s desk window closed while the daemon was down; ending it",
                r.id
            );
            self.set_live_window(&r.id, None, None);
            let _ = self
                .kill_session(&r.id, "its desk window closed while the daemon was down")
                .await;
            changed = true;
        }
        changed
    }

    /// Notice a pane that changed directory (the TS `checkCwdDrift`).
    /// A client that is not asking — the mobile app sends `list` once, at
    /// connect — would otherwise see a `cd` only on the next unrelated
    /// broadcast. This is what turns that poll into one.
    pub fn check_cwd_drift(&mut self, states: &[ldmux_protocol::keeper::State]) -> bool {
        let running: Vec<String> = self
            .registry
            .list()
            .into_iter()
            .filter(|r| r.status == "running" && !r.keeper_name.is_empty())
            .map(|r| r.id)
            .collect();
        if running.is_empty() {
            let had = !self.last_cwds.is_empty();
            self.last_cwds.clear();
            return had;
        }
        let mut paths: HashMap<&str, &str> = HashMap::new();
        for st in states {
            if !st.dead {
                paths.insert(st.session_id.as_str(), st.cwd.as_str());
            }
        }
        let mut changed = false;
        let mut seen: HashSet<String> = HashSet::new();
        for id in running {
            let Some(record) = self.registry.get(&id) else {
                continue;
            };
            let Some(path) = paths.get(record.keeper_name.as_str()) else {
                continue;
            };
            seen.insert(id.clone());
            if self.last_cwds.get(&id).map(String::as_str) != Some(*path) {
                self.last_cwds.insert(id.clone(), (*path).to_string());
                changed = true;
            }
        }
        // A session that stopped running leaves a stale entry otherwise: an
        // unbounded map in a daemon meant to run for days is not a habit worth
        // keeping.
        for id in self.last_cwds.keys().cloned().collect::<Vec<_>>() {
            if !seen.contains(&id) {
                self.last_cwds.remove(&id);
            }
        }
        changed
    }

    /// One maintenance tick: reconcile the registry, the desk windows and the
    /// pane directories against the keeper and the window manager. Returns
    /// whether anything a client can see changed, which is the signal to push a
    /// fresh session list.
    pub async fn maintain(&mut self) -> Result<bool, String> {
        let states = match self.keeper_call(&KeeperMessage::List(km::List {})).await? {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => Vec::new(),
        };
        self.kill_dead_keeper_sessions(&states).await;
        let plan = self.maintain_plan(&states);
        let mut absent = plan.absent.clone();
        for (id, since) in &plan.probes {
            if crate::desktop::live_window_state(id).await.as_deref() == Ok("absent") {
                absent.push((id.clone(), *since));
            }
        }
        Ok(self.maintain_commit(plan, absent).await)
    }

    /// The in-memory half of a maintenance tick, against a keeper listing in hand.
    fn maintain_plan(&mut self, states: &[ldmux_protocol::keeper::State]) -> MaintainPlan {
        let result = self.apply_reconcile(states.to_vec());
        let (live_changed, probes, absent) = self.live_prepass();
        let cwd_changed = self.check_cwd_drift(states);
        MaintainPlan {
            result,
            live_changed,
            cwd_changed,
            probes,
            absent,
        }
    }

    /// The closing half: end the shells of windows found gone, and say whether a
    /// client has news.
    async fn maintain_commit(&mut self, plan: MaintainPlan, absent: Vec<(String, u64)>) -> bool {
        let ended = self.end_absent_windows(absent).await;
        // Anything a mutator touched since the last tick is news even when no
        // *transition* is visible here (see `sessions_dirty`).
        let dirty = std::mem::take(&mut self.sessions_dirty);
        let r = plan.result;
        dirty
            || !r.dead.is_empty()
            || r.unknown_changed
            || !r.pruned.is_empty()
            || plan.live_changed
            || ended
            || plan.cwd_changed
    }

    /// One maintenance tick that does **not** hold the daemon lock across slow
    /// I/O: the keeper `List`/`Kill`s and the window manager's per-window probes
    /// (a process spawn each) run unlocked, and the lock is taken only to read
    /// what they need and to commit what they found. Holding it across them
    /// stalled every handler (and the shutdown path) for as long as a stuck
    /// keeper or i3 took to answer.
    pub(crate) async fn maintain_shared(
        arc: &std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    ) -> Result<(bool, Option<std::sync::Arc<crate::remote::Shared>>), String> {
        let keeper = arc.lock().await.keeper.clone();
        let states = match keeper_call_via(&keeper, &KeeperMessage::List(km::List {})).await? {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => Vec::new(),
        };
        let due = arc.lock().await.dead_sessions_due(&states);
        for name in due {
            let _ =
                keeper_call_via(&keeper, &KeeperMessage::Kill(km::Kill { session_id: name })).await;
        }
        let plan = {
            let mut d = arc.lock().await;
            d.maintain_plan(&states)
        };
        let mut absent = plan.absent.clone();
        for (id, since) in &plan.probes {
            if crate::desktop::live_window_state(id).await.as_deref() == Ok("absent") {
                absent.push((id.clone(), *since));
            }
        }
        let mut d = arc.lock().await;
        let changed = d.maintain_commit(plan, absent).await;
        let shared = d.conn_shared.clone();
        Ok((changed, shared))
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
    pub async fn start(
        &mut self,
        arc: &std::sync::Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    ) -> Result<(), String> {
        // The state dir holds the control socket, the keeper socket and the
        // token: it must be ours and private. A new one is created 0700; an
        // existing one must be owned by this uid and is tightened to 0700, and
        // a dir that cannot be (not ours, chmod refused) is a startup error, not
        // something to carry on with.
        ensure_private_state_dir(&self.state_dir)?;
        // Acquire the single-instance lock (a flock).
        self.lock.acquire().map_err(|e| e.to_string())?;
        // Ensure the keeper (auto-spawn if dead).
        self.keeper.ensure().await?;
        // Load the registry.
        self.registry.load();
        // Adopt the daemon's own sessions.
        self.adopt_own_sessions().await?;
        // Reconcile.
        let _ = self.reconcile().await?;
        // Recover the desk-window bookkeeping the last daemon left in the
        // registry (the map itself is memory). A window that closed while this
        // daemon was down closes its shell now, exactly as if the daemon had
        // been watching.
        let _ = self.rebuild_live_windows().await;
        // The claims the last daemon held, for shells that are still running.
        self.load_claims();
        // Start the remote (the phone-facing WS endpoint). A bind failure is
        // logged, not fatal: the remote is optional (local control must not
        // be taken down by a remote bind failure).
        // The connection table is the daemon's, built once, so the tailnet socket
        // and the local attach socket are one population (see `Shared::new`).
        let (token, idle_ms, send_timeout_ms) = match &self.remote_cfg {
            Some(c) => (c.token.clone(), c.idle_ms, c.send_timeout_ms),
            None => (String::new(), 20_000, 10_000),
        };
        let shared = crate::remote::Shared::new(arc.clone(), token, idle_ms, send_timeout_ms);
        self.conn_shared = Some(shared.clone());
        // The local attach socket (E): the desk attach client's way in, with no
        // tailnet involved. A bind failure is logged, not fatal; the client then
        // has nothing to connect to and says so.
        let attach_path = std::path::PathBuf::from(ldmux_protocol::paths::attach_socket_path_from(
            &self.socket_path().to_string_lossy(),
        ));
        match crate::control_socket::ControlSocketServer::at(&attach_path).bind() {
            Ok(listener) => {
                self.local_handle = Some(crate::remote::serve_local(listener, shared.clone()))
            }
            Err(e) => eprintln!("ldmuxd: local attach socket {}: {e}", attach_path.display()),
        }
        if let Some(cfg) = self.remote_cfg.clone() {
            let mut rs = crate::remote::RemoteServer::new(cfg.clone()).with_shared(shared.clone());
            match rs.start(arc).await {
                Ok((host, port)) => {
                    eprintln!("ldmuxd: remote ws://{host}:{port}/ws");
                    // Remember the port that was bound. A configured 0 means the
                    // kernel chose, and every surface that tells a client where to
                    // connect — `remote_info`, the pairing link, the QR code —
                    // has to name the real one.
                    if let Some(c) = self.remote_cfg.as_mut() {
                        c.port = port;
                    }
                    self.remote = Some(rs);
                }
                Err(e) => {
                    // The bind can fail at boot, before tailscaled has assigned
                    // the tailnet address (the documented incident: the listener
                    // was never retried and the phone could not connect at all).
                    // Retry, so nobody has to restart the daemon to be reachable.
                    eprintln!("ldmuxd: remote bind failed ({e}); retrying every 5s");
                    let arc2 = arc.clone();
                    let stop = self.maintenance_stop.clone();
                    tokio::spawn(async move {
                        let mut failures = 0u64;
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            // A stopped daemon must not grow a listener.
                            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                break;
                            }
                            if arc2.lock().await.remote.is_some() {
                                break;
                            }
                            let mut rs = crate::remote::RemoteServer::new(cfg.clone())
                                .with_shared(shared.clone());
                            match rs.start(&arc2).await {
                                Ok((host, port)) => {
                                    eprintln!("ldmuxd: remote up ws://{host}:{port}/ws");
                                    let mut d = arc2.lock().await;
                                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                                        rs.stop();
                                        break;
                                    }
                                    if let Some(c) = d.remote_cfg.as_mut() {
                                        c.port = port;
                                    }
                                    d.remote = Some(rs);
                                    break;
                                }
                                // Said again every minute, with the reason: a bind
                                // that keeps failing (a taken port, no tailnet
                                // address) is not a one-line event, and a journal
                                // that stopped talking after the first line made
                                // it look resolved.
                                Err(e) => {
                                    failures += 1;
                                    if failures.is_multiple_of(12) {
                                        eprintln!(
                                            "ldmuxd: remote still not bound after {} attempts ({e}); retrying every 5s",
                                            failures + 1
                                        );
                                    }
                                    continue;
                                }
                            }
                        }
                    });
                }
            }
        }
        // The maintenance loop (the TS `liveTimer`, every 2s): reconcile the
        // registry against the keeper, the desk windows against the window
        // manager, and the pane directories against the last pass. A change is
        // pushed to every connected client, because a pager whose session died
        // is a page that silently does nothing.
        //
        // The pass is one keeper `List`, taken under the daemon lock; the
        // broadcast is outside it, since it needs the lock itself to describe
        // the sessions.
        {
            let maint_arc = arc.clone();
            let stop = self.maintenance_stop.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(MAINTENANCE_INTERVAL_MS))
                        .await;
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    // A keeper that is restarting is not news. The slow steps run
                    // without the daemon lock (see `maintain_shared`).
                    //
                    // Each pass on its own task: a panic in one is that pass's, not
                    // the loop's. This loop is what notices a shell that exited, a
                    // window that closed and a claim nobody holds, so when it ended
                    // (silently: a detached task's panic is only a journal line) the
                    // daemon went on answering while never again cleaning up.
                    let arc = maint_arc.clone();
                    let pass = survive("maintenance", async move {
                        let (changed, shared) = LdmuxDaemon::maintain_shared(&arc)
                            .await
                            .unwrap_or((false, None));
                        if changed && let Some(shared) = shared {
                            crate::remote::broadcast_sessions(&shared).await;
                        }
                    });
                    pass.await;
                }
            });
        }
        // Start the control socket (the `ldmuxd.sock` Unix socket).
        // The daemon's `handle` method is the handler: each RPC call locks the
        // mutex and calls `handle`.
        let cs = crate::control_socket::ControlSocketServer::at(self.socket_path());
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
        let handler = std::sync::Arc::new(handler);
        let handle = tokio::spawn(async move {
            // One task per connection, so a silent client holds only its own slot.
            if let Err(e) = crate::control_socket::serve_loop(&listener, handler).await {
                eprintln!("ldmuxd: control socket: {e}");
            }
        });
        // Kept, so `stop()` can end it. A detached task cannot be cancelled and
        // a stopped daemon that is still accepting is not stopped.
        self.control_handle = Some(handle);
        Ok(())
    }

    /// Stop the daemon (the TS `stop`). Stops the remote, closes streams,
    /// removes the socket, releases the lock. Tolerates a partially-started
    /// daemon (a signal during startup).
    pub fn stop(&mut self) {
        self.maintenance_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // End the accept loop, not merely its socket file: dropping the handle
        // would leave the task running with the listener still bound.
        if let Some(h) = self.control_handle.take() {
            h.abort();
        }
        if let Some(ref mut r) = self.remote {
            r.stop();
        }
        if let Some(h) = self.local_handle.take() {
            h.abort();
        }
        if let Some(shared) = self.conn_shared.take() {
            shared.stop();
        }
        // Close all streams (the `close` sets `poll_stop`, so the poll
        // threads exit on their next iteration).
        for stream in self.streams.values() {
            stream.close(crate::stream::RESET_DAEMON_STOPPED);
        }
        self.streams.clear();
        // Remove the socket file — only if the lock is ours: a daemon that was
        // refused (or never got that far) must not delete the socket of the one
        // that is running.
        if self.lock.owns() {
            let sock = self.socket_path();
            let _ = std::fs::remove_file(&sock);
            let _ = std::fs::remove_file(ldmux_protocol::paths::attach_socket_path_from(
                &sock.to_string_lossy(),
            ));
        }
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
        let cwd = crate::cwd::resolve_cwd(Some(cwd))?;
        if !std::path::Path::new(&cwd).is_dir() {
            return Err(format!("not a directory: {cwd}"));
        }
        let id = short_id();
        let keeper_name = format!("ldmux-{id}");
        let command: Vec<String> = if kind == "pi" {
            vec!["pi".to_string()]
        } else {
            vec![]
        };
        let now: u64 = now_ms();
        let record = SessionRecord {
            id: id.clone(),
            keeper_name: keeper_name.clone(),
            kind: kind.to_string(),
            cwd: cwd.clone(),
            title: title.map(|t| t.to_string()).unwrap_or_else(|| {
                if kind == "pi" {
                    "pi".to_string()
                } else {
                    "shell".to_string()
                }
            }),
            created_at: now,
            updated_at: now,
            created_by: created_by.to_string(),
            status: "running".to_string(),
            exit_code: None,
            exited_at: None,
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        };
        let env = session_spawn_env(
            &id,
            kind.as_str(),
            &keeper_name,
            self.socket_path().to_string_lossy().as_ref(),
            self.keeper_socket_path().to_string_lossy().as_ref(),
        );
        // Call the keeper's `Spawn` (the equivalent of the TS `cp.newSession`,
        // retargeted onto the keeper: a `Spawn` creates a new emulator session).
        // The `pi` kind runs `pi`; every session gets the `LDMUX_*` spawn env
        // (plan §2.6).
        let spawn_resp = self
            .keeper_call(&km::KeeperMessage::Spawn(km::Spawn {
                session_id: keeper_name.clone(),
                cols: 80,
                rows: 24,
                cwd: Some(cwd.clone()),
                command: if command.is_empty() {
                    None
                } else {
                    Some(command.clone())
                },
                env: Some(env.clone()),
            }))
            .await?;
        let _ = spawn_resp;
        self.registry.upsert(record.clone());
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        self.sessions_dirty = true;
        Ok(record)
    }

    /// Kill a session (the TS `handleKill`).
    ///
    /// `reason` is how it ended, in words — it lands in the record and in the
    /// journal. It is a required argument on purpose: a new ending path cannot
    /// compile without saying why, and before this existed no path said why at
    /// all. See [`Self::note_ended`].
    pub async fn kill_session(&mut self, id: &str, reason: &str) -> Result<SessionRecord, String> {
        let mut record = self
            .registry
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no such session: {id}"))?;
        if !record.keeper_name.is_empty() {
            let killed = self
                .keeper_call(&km::KeeperMessage::Kill(km::Kill {
                    session_id: record.keeper_name.clone(),
                }))
                .await;
            if let Err(e) = killed {
                // "No such session" means it is already gone: that is the goal.
                // Anything else, and the shell may still be running; marking
                // the record dead then lies, and the next reconcile flips it
                // back. Check, and refuse while the keeper still has it.
                if !e.contains("no such session") {
                    let still_there = matches!(
                        self.keeper_call(&KeeperMessage::List(km::List {})).await,
                        Ok(KeeperMessage::ListResult(lr))
                            if lr.sessions.iter().any(|st| st.session_id == record.keeper_name && !st.dead)
                    );
                    if still_there {
                        return Err(format!("the keeper did not end {id}: {e}"));
                    }
                }
            }
        }
        record.status = "dead".to_string();
        let now: u64 = now_ms();
        record.exited_at = Some(now as i64);
        record.updated_at = now;
        record.exit_code = None;
        record.ended_reason = Some(reason.to_string());
        let kind = ended_kind_of(reason);
        record.ended_kind = Some(kind.to_string());
        record.exit_signal = None;
        self.note_ended(&record.id, kind, reason);
        self.registry.upsert(record.clone());
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        self.forget_session_state(&record.id);
        // Tell whoever is watching that it is over. The TS dropped the stream
        // here and the port did not: measured, after `ldmux kill` the session's
        // desk window and its `ldmuxd attach` were both still alive, the client
        // holding a subscription to a pane that no longer existed. `drop_stream`
        // closes the stream, which resets every subscriber — that is the signal
        // the attach client exits on.
        self.drop_stream(&record.keeper_name);
        self.sessions_dirty = true;
        Ok(record)
    }

    /// Close a session (the TS `handleClose`).
    pub async fn close_session(
        &mut self,
        id: &str,
        reason: &str,
    ) -> Result<(bool, SessionRecord), String> {
        let record = self
            .registry
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no such session: {id}"))?;
        if self.ownership.owner(id).is_some() {
            return Ok((false, record));
        }
        let record = self.kill_session(id, reason).await?;
        Ok((true, record))
    }

    /// Say why a session ended, on the record and in the journal.
    ///
    /// The record used to say `dead` and nothing else, and the daemon's log was
    /// silent: two shells ended during an evening of use on 2026-10-05 and the
    /// only way to explain either was to rule everything else out. Every ending
    /// funnels through here, like `forget_session_state` does for the claim and
    /// the window record, for the same reason — a reason a path forgets to write
    /// is a reason nobody has later.
    ///
    /// Called from `kill_session` (which every explicit ending uses) and from the
    /// two ending paths that do not kill: the keeper reporting a session gone,
    /// and the daemon starting up without a window its record promised.
    fn note_ended(&mut self, id: &str, kind: &str, reason: &str) {
        println!("ldmuxd: session {id} ended ({kind}): {reason}");
        let Some(mut r) = self.registry.get(id).cloned() else {
            return;
        };
        if r.ended_reason.as_deref() == Some(reason) && r.ended_kind.as_deref() == Some(kind) {
            return;
        }
        r.ended_reason = Some(reason.to_string());
        r.ended_kind = Some(kind.to_string());
        self.registry.upsert(r);
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
    }

    /// Forget a session (the TS `forget`).
    pub async fn forget_session(&mut self, id: &str) -> Result<bool, String> {
        let record = match self.registry.get(id) {
            Some(r) => r,
            None => return Ok(false),
        };
        if record.status == "running" && !record.keeper_name.is_empty() {
            return Err(format!("{id} is running; end it with kill, not forget"));
        }
        self.forget_session_state(id);
        self.registry.remove(id);
        if let Err(e) = self.registry.save() {
            eprintln!("ldmuxd: registry save: {e}");
        }
        self.sessions_dirty = true;
        Ok(true)
    }

    /// Forget a session's state (the TS `forgetSessionState`).
    /// Clears the ownership record for the session.
    pub fn forget_session_state(&mut self, id: &str) {
        // Every ending funnels through here — kill, close, forget, a shell that
        // exited, a window that closed — and each of them changes what a client
        // is shown (a claim, a window record, a stream). One line here covers
        // all of them, which is the point: the last three bugs of this kind were
        // a path that forgot to.
        self.sessions_dirty = true;
        if let Some(claim) = self.ownership.owner(id).cloned() {
            self.ownership.release(id, &claim.device.id);
            self.restored.remove(id);
            self.save_claims();
        }
        // A session that is over has no window. Doing it here rather than in each
        // ending keeps the invariant in one place: `running` with a window record
        // is the only state that has one, so nothing can report a window for a
        // shell that is gone (the list falls back to the record).
        self.live_windows.remove(id);
        self.set_live_window(id, None, None);
    }

    /// Get or create the `PaneStream` for a session (the TS `streamFor`).
    ///
    /// The stream is created with a `SocketKeeperConn` (the production path)
    /// that talks to the keeper's socket. A poll thread is spawned that pulls
    /// the keeper's buffered output and fans it out to subscribers (the §3.7
    /// live stream). The `Arc` is shared between the daemon's map and the
    /// poll thread.
    pub fn stream_for(&mut self, keeper_name: &str) -> &std::sync::Arc<PaneStream> {
        if !self.streams.contains_key(keeper_name) {
            let conn = Box::new(crate::stream::SocketKeeperConn::new(
                self.keeper_socket_path().to_string_lossy().into_owned(),
            ));
            let stream = std::sync::Arc::new(
                PaneStream::new(keeper_name.to_string(), conn)
                    .with_clipboard_write(crate::clipboard_policy::read(&self.state_dir)),
            );
            // Spawn the poll thread (the §3.7 live stream: pulls the keeper's
            // buffered output and fans it out to subscribers).
            let poll_stream = stream.clone();
            std::thread::spawn(move || {
                loop {
                    if poll_stream
                        .poll_stop
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        break;
                    }
                    // A chunk with more behind it is followed at once; only an
                    // empty keeper buffer earns the sleep. A flood is therefore
                    // drained at the keeper socket's speed, in 64 KiB pieces.
                    //
                    // A panic in a pass costs that pass: the thread is what delivers
                    // this shell's output to every viewer, and when it ended the
                    // stream stayed in the map with nobody polling it (a frozen
                    // terminal on every device, with the link looking healthy).
                    let more = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll_stream.poll_once()
                    }))
                    .unwrap_or_else(|_| {
                        crate::stream::note_panic("a stream poll pass");
                        Ok(false)
                    })
                    .unwrap_or(false);
                    if !more {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            });
            self.streams.insert(keeper_name.to_string(), stream);
        }
        self.streams.get(keeper_name).unwrap()
    }

    /// The last screen of a session whose shell has ended, while the keeper still
    /// holds it (see `DEAD_KEEP`): what a reconnecting client is shown before it
    /// is told the ending. `None` once the keeper has let go of it.
    pub async fn final_screen(
        &mut self,
        keeper_name: &str,
    ) -> Option<ldmux_protocol::keeper::SnapshotResult> {
        if keeper_name.is_empty() {
            return None;
        }
        let req = KeeperMessage::Snapshot(km::Snapshot {
            session_id: keeper_name.to_string(),
            history_lines: None,
            drain_output: false,
        });
        match self.keeper_call(&req).await {
            Ok(KeeperMessage::SnapshotResult(sr)) => Some(sr),
            _ => None,
        }
    }

    /// Get a stream by session id (the `streams` map lookup, without creating).
    pub fn get_stream(&mut self, keeper_name: &str) -> Option<&std::sync::Arc<PaneStream>> {
        self.streams.get(keeper_name)
    }

    /// Close and forget a stream (the TS `dropStream`).
    /// Its keeper session is gone.
    pub fn drop_stream(&mut self, keeper_name: &str) {
        if let Some(stream) = self.streams.remove(keeper_name) {
            stream.close(crate::stream::RESET_SESSION_ENDED);
        }
    }

    /// Release a stream only if nothing is subscribed to it (the TS
    /// `releaseStreamIfIdle`).
    pub fn release_stream_if_idle(&mut self, keeper_name: &str) {
        // Check the subscriber count without holding a mutable borrow.
        let idle = self
            .streams
            .get(keeper_name)
            .map(|s| s.subscriber_count() == 0)
            .unwrap_or(false);
        if idle {
            self.drop_stream(keeper_name);
        }
    }

    /// Evict streams whose backing session is gone (the TS `sweepStreams`).
    /// Bounded memory: a stream for a dead session is dropped.
    pub fn sweep_streams(&mut self) {
        let to_drop: Vec<String> = self
            .streams
            .keys()
            .filter(|keeper_name| {
                let record = self.registry.by_keeper_name(keeper_name);
                record.is_none() || record.as_ref().map(|r| r.status.as_str()) == Some("dead")
            })
            .cloned()
            .collect();
        for keeper_name in to_drop {
            self.drop_stream(&keeper_name);
        }
    }

    /// The RPC dispatch (the TS `handle` method): maps an RPC method name to a
    /// daemon method and returns the result as a JSON value.
    ///
    /// This is the core of the control socket server: the `serve_one` method
    /// in `control_socket.rs` calls this to handle each RPC request.
    pub async fn handle(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match method {
            // The MeshMore link, if this machine has one. Always answerable, so
            // a client can tell "disabled" from "broken" without guessing.
            "mesh.status" => Ok(crate::mesh::status_json()),
            // Voice mode (docs/VOICE-MODE.md section 6): relay what an agent wants said to the
            // clients viewing the session that asked for it. Validation only; no speech logic.
            "speech" => {
                let id = params
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.trim().is_empty() {
                    return Err("speech text is empty".to_string());
                }
                if text.len() > SPEECH_MAX_BYTES {
                    return Err(format!("speech text is over {SPEECH_MAX_BYTES} bytes"));
                }
                let message_id = params.get("id").and_then(|v| v.as_str());
                if message_id.is_some_and(|m| m.is_empty() || m.len() > 64) {
                    return Err("speech id must be 1 to 64 bytes".to_string());
                }
                let is_final = params
                    .get("final")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if self.registry.get(id).is_none() {
                    return Err(format!("no such session: {id}"));
                }
                let seq = {
                    let n = self.speech_seq.entry(id.to_owned()).or_insert(0);
                    *n += 1;
                    *n
                };
                let message = serde_json::json!({
                    "type": "speech",
                    "sessionId": id,
                    "id": message_id,
                    "text": text,
                    "final": is_final,
                    "seq": seq,
                });
                let delivered = self.conn_shared.as_ref().map_or(0, |shared| {
                    crate::remote::deliver_speech(shared, id, &message)
                });
                Ok(serde_json::json!({ "seq": seq, "delivered": delivered }))
            }
            "ping" => Ok(serde_json::json!({
                "pong": true,
                "version": ldmux_protocol::rpc::DAEMON_PROTOCOL_VERSION,
                "keeperSocket": self.keeper_socket_path().to_string_lossy(),
            })),
            "list" => {
                // Detect keeper death (plan §6): if the keeper is dead,
                // respawn it and reconcile (mark all sessions dead).
                self.ensure_keeper_alive().await?;
                // The **described** view, not the raw registry — the same answer
                // the launcher and the phone get. Handing back the stored records
                // instead gave two views of one fact that disagreed: a shell
                // opened in `~/Documents/RCNT` is *stored* with the title `pi`,
                // and only `session_title` turns that into `RCNT`, so `ldmux
                // list` showed one name and the phone showed another. It also
                // dropped `owner`, `process` and the live workspace, so the CLI
                // could not show who holds a shell. The TS fixed this and said
                // why; the port went back to the registry. One view, and it is
                // the richer one.
                let sessions = self.describe_sessions().await;
                Ok(serde_json::json!({ "sessions": sessions }))
            }
            "new" => {
                let kind = params
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("shell");
                let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");
                let title = params.get("title").and_then(|v| v.as_str());
                let created_by = params
                    .get("createdBy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("local");
                let record = self.create_session(kind, cwd, title, created_by).await?;
                Ok(serde_json::json!({ "session": record }))
            }
            "kill" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let record = self.kill_session(id, "killed").await?;
                Ok(serde_json::json!({ "ok": true, "session": record }))
            }
            "close" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let (ended, record) = self.close_session(id, "closed").await?;
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
                let record = self
                    .registry
                    .get(id)
                    .cloned()
                    .ok_or_else(|| format!("no such session: {id}"))?;
                if record.keeper_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Build the input bytes: the text as raw bytes, then each key
                // name converted to its byte sequence (the TS `sendText` +
                // `sendKey` are separate operations, but both write to the PTY;
                // concatenating into one `Write` is equivalent).
                // Cursor keys depend on the pane's DECCKM mode and a modified
                // Enter on its Kitty keyboard flags (`Read` answers both).
                let (app_cursor, kitty) = match self
                    .keeper_call(&km::KeeperMessage::Read(km::Read {
                        session_id: record.keeper_name.clone(),
                    }))
                    .await
                {
                    Ok(km::KeeperMessage::State(st)) => (st.app_cursor, st.kitty_keyboard),
                    _ => (false, 0),
                };
                let mut input: Vec<u8> = Vec::new();
                if !text.is_empty() {
                    input.extend_from_slice(text.as_bytes());
                }
                if let Some(keys) = keys {
                    for k in keys {
                        let ks = k.as_str().unwrap_or("");
                        // An unknown name is refused rather than typed into the
                        // shell: see `key_name_to_bytes`.
                        input.extend(crate::stream::key_name_to_bytes_in_mode(
                            ks, app_cursor, kitty,
                        )?);
                    }
                }
                let data =
                    String::from_utf8(input).map_err(|e| format!("invalid input bytes: {e}"))?;
                // Send the input to the keeper (the `Write` message).
                self.keeper_call(&km::KeeperMessage::Write(km::Write {
                    session_id: record.keeper_name.clone(),
                    data,
                }))
                .await?;
                Ok(serde_json::json!({ "ok": true }))
            }
            "capture" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let history = params
                    .get("history")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let record = self
                    .registry
                    .get(id)
                    .ok_or_else(|| format!("no such session: {id}"))?;
                if record.keeper_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Take a snapshot (the keeper's `Snapshot` message).
                let resp = self
                    .keeper_call(&km::KeeperMessage::Snapshot(km::Snapshot {
                        session_id: record.keeper_name.clone(),
                        history_lines: if history {
                            Some(HISTORY_CAPTURE_LINES)
                        } else {
                            None
                        },
                        drain_output: false,
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
                let record = self
                    .registry
                    .get(id)
                    .ok_or_else(|| format!("no such session: {id}"))?;
                if record.keeper_name.is_empty() {
                    return Err("no terminal-backed session".to_string());
                }
                // Take a snapshot (the keeper's `Snapshot` message).
                let resp = self
                    .keeper_call(&km::KeeperMessage::Snapshot(km::Snapshot {
                        session_id: record.keeper_name.clone(),
                        history_lines: None,
                        drain_output: false,
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
                let workspace = params
                    .get("workspace")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let record = self
                    .registry
                    .get(id)
                    .cloned()
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
                let result = crate::desktop::open_live(id, workspace).await;
                match result {
                    Ok(lr) => {
                        if lr.ok {
                            self.sessions_dirty = true;
                            self.live_windows.insert(
                                id.to_string(),
                                LiveWindow {
                                    workspace: workspace.to_string(),
                                    since: self.mono_ms(),
                                    pid: None,
                                },
                            );
                            self.set_live_window(id, Some(workspace.to_string()), None);
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
                self.sessions_dirty = true;
                self.live_windows.remove(id);
                self.set_live_window(id, None, None);
                let dd_id = self.desktop_device.id.clone();
                let _ = self.release_claim(id, &dd_id);
                Ok(serde_json::json!({
                    "ok": result.unwrap_or(false),
                }))
            }
            "deskWindow" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let record = self
                    .registry
                    .get(id)
                    .cloned()
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
                // `spawn` defaults to true: the daemon opens the window, which
                // is what the window manager runs (`$mod+Return`). `spawn: false`
                // is the caller *being* the window in the terminal it is already
                // in — `ldmux shell` and `ldmux run` — so the daemon launches
                // nothing and the window is the caller's process, whose pid it
                // is given. That pid is also the model's `live_pid`, and the
                // daemon asks the kernel about it, because a stock terminal has
                // no window class of ours to find.
                let spawn = params
                    .get("spawn")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let pid = params.get("pid").and_then(|v| v.as_i64()).map(|v| v as i32);
                let (ok, detail) = if spawn {
                    match crate::desktop::open_live(id, "").await {
                        Ok(lr) => (lr.ok, None),
                        Err(e) => (false, Some(e)),
                    }
                } else if pid.is_some() {
                    (true, None)
                } else {
                    (
                        false,
                        Some(
                            "an in-place desk window needs the pid of the process it is"
                                .to_string(),
                        ),
                    )
                };
                if ok {
                    self.sessions_dirty = true;
                    self.live_windows.insert(
                        id.to_string(),
                        LiveWindow {
                            workspace: String::new(),
                            since: self.mono_ms(),
                            pid,
                        },
                    );
                    self.set_live_window(id, Some(String::new()), pid);
                    let dd = self.desktop_device.clone();
                    let _ = self.claim(id, &dd, true);
                }
                Ok(serde_json::json!({
                    "ok": ok,
                    "detail": detail,
                }))
            }
            "reclaim" => {
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let device: ldmux_protocol::rpc::DeviceInfo = params
                    .get("device")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .ok_or_else(|| "no device given".to_string())?;
                let result = self.claim(id, &device, true);
                Ok(serde_json::json!({
                    "ok": matches!(result, crate::ownership::ClaimResult::Acquired { .. }),
                }))
            }
            // ── Remote info ──
            "remote_info" => Ok(serde_json::json!({
                "hostname": self.remote_cfg.as_ref()
                    .map(|c| c.hostname.clone())
                    .unwrap_or_else(|| "127.0.0.1".to_string()),
                "port": self.remote_cfg.as_ref().map(|c| c.port).unwrap_or(0),
                "path": "/ws",
                "token": self.remote_cfg.as_ref()
                    .map(|c| c.token.clone())
                    .unwrap_or_default(),
                "listening": self.remote.is_some(),
            })),
            // ── Adopt ──
            "adopt" => {
                let keeper_name = params
                    .get("keeperName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let record = self.adopt(keeper_name).await?;
                Ok(serde_json::to_value(&record).map_err(|e| e.to_string())?)
            }
            // ── Clipboard write policy ──
            "clipboardWrite" => {
                let on = params.get("on").and_then(|v| v.as_bool()).unwrap_or(false);
                crate::clipboard_policy::write(&self.state_dir, on)
                    .map_err(|e| format!("write clipboard policy: {e}"))?;
                Ok(serde_json::json!({ "on": on }))
            }
            // ── Stream-dependent handlers handled by the remote WS server ──
            "attach" | "resize" | "mouseclick" | "input" | "key" | "follow" | "scrollback" => Err(
                format!("not_implemented: {method} (handled by the remote WS server)"),
            ),
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
            let keeper_name = s.session_id.clone();
            if self.registry.by_keeper_name(&keeper_name).is_some() {
                continue;
            }
            let bare = keeper_name
                .strip_prefix("ldmux-")
                .unwrap_or(&keeper_name)
                .to_string();
            if self.registry.get(&bare).is_some() {
                continue;
            }
            // A failed adopt is not counted (a dead/gone session is not adopted).
            if self.adopt(&keeper_name).await.is_ok() {
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
    pub async fn adopt(
        &mut self,
        session_id: &str,
    ) -> Result<ldmux_protocol::rpc::SessionRecord, String> {
        // The keeper names sessions `ldmux-<id>`; accept either form (the
        // RPC passes the terminal name, `adopt_own_sessions` the same).
        let keeper_name = if session_id.starts_with("ldmux-") {
            session_id.to_string()
        } else {
            format!("ldmux-{session_id}")
        };
        // Verify the keeper session is live (a `Read`).
        let resp = self
            .keeper_call(&KeeperMessage::Read(km::Read {
                session_id: keeper_name.clone(),
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
        if let Some(existing) = self.registry.by_keeper_name(&keeper_name) {
            return Ok(existing.clone());
        }
        let id = keeper_name
            .strip_prefix("ldmux-")
            .unwrap_or(&keeper_name)
            .to_string();
        let now = now_ms();
        let record = ldmux_protocol::rpc::SessionRecord {
            id,
            keeper_name,
            kind: "shell".into(),
            cwd: state.cwd,
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
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
        let mut facts: HashMap<String, (String, String, u64)> = HashMap::new();
        if let Ok(KeeperMessage::ListResult(lr)) =
            self.keeper_call(&KeeperMessage::List(km::List {})).await
        {
            for st in lr.sessions {
                let activity = lr.activity.get(&st.session_id).copied().unwrap_or(0);
                facts.insert(
                    st.session_id.clone(),
                    (st.foreground.clone(), st.cwd.clone(), activity),
                );
            }
        }
        let records = self.registry.list();
        let mut sessions: Vec<SessionInfo> = records
            .into_iter()
            .map(|record| {
                let fact = facts.get(&record.keeper_name);
                // `cwd` follows the pane when it is running and the keeper
                // knows where it is; otherwise the record's creation directory.
                let cwd = match fact {
                    Some((_, path, _)) if record.status == "running" && !path.is_empty() => {
                        path.clone()
                    }
                    _ => record.cwd.clone(),
                };
                let process = fact
                    .map(|(cmd, _, _)| cmd.clone())
                    .filter(|c| !c.is_empty());
                // The pane's own clock where the keeper has one, else the record's
                // last write. This was `Some(record.updated_at)` alone, which is
                // only ever the *creation* or *death* time — so "last activity"
                // was a timestamp that never moved while a shell was being used,
                // and the launcher's notion of recent was a lie. the keeper stamps
                // every session on input and output.
                let last_activity_at = match fact {
                    Some((_, _, a)) if *a > 0 => Some(*a),
                    _ => Some(record.updated_at),
                };
                let owner = self.ownership.owner(&record.id).map(|c| SessionOwner {
                    id: c.device.id.clone(),
                    name: c.device.name.clone(),
                    kind: c.device.kind.clone(),
                    since: c.since.max(0) as u64,
                });
                // Some("") is a fact, not an absence: the window exists and the
                // window manager did not say where it was placed (a `deskWindow`
                // opens on whatever workspace is focused). Filtering the blank
                // out made every `$mod+Return` window report "no window" on the
                // phone while its window sat on the desk (2026-10-05). The
                // clients already read a blank as "on desktop", no number.
                //
                // The live map is preferred because it is current; the record is
                // the fallback for the moment after a restart before the map is
                // rebuilt, and for a window opened by something other than this
                // daemon and not yet adopted.
                let live_workspace = self
                    .live_windows
                    .get(&record.id)
                    .map(|w| Some(w.workspace.clone()))
                    .unwrap_or_else(|| record.live_workspace.clone());
                SessionInfo {
                    id: record.id.clone(),
                    kind: record.kind.clone(),
                    title: session_title(&record, &cwd),
                    cwd,
                    status: record.status.clone(),
                    keeper_name: record.keeper_name.clone(),
                    created_by: record.created_by.clone(),
                    last_activity_at,
                    process,
                    owner,
                    live_workspace,
                    ended_reason: record.ended_reason.clone(),
                    exit_code: record.exit_code,
                    exit_signal: record.exit_signal,
                    ended_kind: record.ended_kind.clone(),
                }
            })
            .collect::<Vec<_>>();
        // **Running first, most recently used first.**
        //
        // The registry's own order is `updated_at` descending, and ending a
        // session *writes* `updated_at` (to record when it died). So every shell
        // a user closed jumped to the top of the list: measured, killing one put
        // it first, above every live shell. The clients render these rows the
        // same way, so the launcher led with shells that no longer existed and
        // tapping one failed — the count looked wrong because the top of the
        // list was the dead.
        //
        // A shell that is running sorts above one that is not, and within each
        // group the most recently active comes first, which is what a launcher's
        // order is for. The id breaks ties so two clients never disagree.
        sessions.sort_by(|a, b| {
            let alive = (b.status == "running").cmp(&(a.status == "running"));
            alive
                .then(b.last_activity_at.cmp(&a.last_activity_at))
                .then(a.id.cmp(&b.id))
        });
        sessions
    }

    /// The sessions recorded as having a desk window, for tests and diagnostics.
    pub fn live_window_ids(&self) -> Vec<String> {
        self.live_windows.keys().cloned().collect()
    }

    /// Read the desktop inventory (the TS `desktopInventory`).
    pub async fn desktop_inventory(&mut self) -> ldmux_protocol::rpc::DesktopInventory {
        let home = std::env::var("HOME").unwrap_or_default();
        let mut inv = crate::desktop_inventory::read_desktop(&home).await;
        // What the machine shows and what ldmux knows are two different things,
        // and a client needs the second: a desk window's terminal is not in its
        // pi's ancestry (the pi runs in the keeper's pty, the window is a separate
        // alacritty running `ldmuxd attach`), so the inventory on its own reports
        // a pi filling a window as having none. The session's shell pid is in that
        // ancestry, which is how the TypeScript linked the two with tmux's
        // `#{pane_pid}`, and a client that has the link goes to the running shell
        // instead of guessing by directory and opening a new one
        // (`origin/main:packages/daemon/src/daemon.ts:1096-1106`).
        if let Ok(ldmux_protocol::keeper::KeeperMessage::ListResult(res)) = self
            .keeper_call(&km::KeeperMessage::List(km::List {}))
            .await
        {
            let by_shell_pid = self.session_ids_by_shell_pid(&res.sessions);
            crate::desktop_inventory::link_pi_sessions(&mut inv, &by_shell_pid);
        }
        inv
    }

    /// The keeper's sessions, keyed by the shell pid it reports, valued by the
    /// session id the daemon knows them by. `pid <= 0` is dropped: a pid that is
    /// not a pid would match a lookup by luck.
    fn session_ids_by_shell_pid(
        &self,
        states: &[ldmux_protocol::keeper::State],
    ) -> std::collections::HashMap<i32, String> {
        let mut map = std::collections::HashMap::new();
        if states.is_empty() {
            return map;
        }
        let by_keeper_name: std::collections::HashMap<String, String> = self
            .registry
            .list()
            .into_iter()
            .filter(|r| !r.keeper_name.is_empty())
            .map(|r| (r.keeper_name, r.id))
            .collect();
        for st in states {
            if st.pid <= 0 {
                continue;
            }
            if let Some(id) = by_keeper_name.get(&st.session_id) {
                map.insert(st.pid, id.clone());
            }
        }
        map
    }

    /// Claim a session for a device (the TS `claimSession`, ADR-0010).
    ///
    /// Returns the ownership result. The daemon-side side effects (size,
    /// desk-window handling, displaced notification) are layered on by the
    /// remote (they depend on the transport state the remote owns).
    /// Milliseconds on the daemon's monotonic clock.
    pub fn mono_ms(&self) -> u64 {
        self.mono_base.elapsed().as_millis() as u64
    }

    fn claims_path(&self) -> PathBuf {
        self.state_dir.join("claims.json")
    }

    /// Mirror the claim table to disk (atomic, 0600). Best effort: a failure is
    /// logged and costs only the restart survival.
    fn save_claims(&self) {
        // A stopping daemon is closing its connections, and each close would
        // release that device's claim: exactly the thing the file is there to
        // survive. The file written while running is the one to keep.
        if self
            .maintenance_stop
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let rows: Vec<Value> = self
            .ownership
            .export()
            .into_iter()
            .map(|(sid, c)| {
                serde_json::json!({
                    "sessionId": sid,
                    "device": {"id": c.device.id, "name": c.device.name, "kind": c.device.kind},
                    "since": c.since,
                })
            })
            .collect();
        let path = self.claims_path();
        let tmp = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(Value::Array(rows).to_string().as_bytes())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            eprintln!("ldmuxd: claims save: {e}");
        }
    }

    /// Reload the claims the last daemon held, for sessions that are still running,
    /// each held for [`RESTORE_GRACE_MS`] for its device to come back.
    ///
    /// The clients a restart drops reconnect within seconds; without this another
    /// device could attach in that gap and the shell's holder would find it taken.
    /// A restored claim that is never re-attached is freed by the ordinary sweep
    /// once the grace has passed.
    pub fn load_claims(&mut self) {
        let Ok(text) = std::fs::read_to_string(self.claims_path()) else {
            return;
        };
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        let deadline = self.mono_ms() + RESTORE_GRACE_MS;
        for r in rows {
            let (Some(sid), Some(dev)) = (r["sessionId"].as_str(), r.get("device")) else {
                continue;
            };
            let running = self
                .registry
                .get(sid)
                .is_some_and(|rec| rec.status == "running");
            let (Some(id), Some(name), Some(kind)) = (
                dev["id"].as_str(),
                dev["name"].as_str(),
                dev["kind"].as_str(),
            ) else {
                continue;
            };
            if !running {
                continue;
            }
            self.ownership.restore(
                sid,
                crate::ownership::Claim {
                    device: ldmux_protocol::rpc::DeviceInfo {
                        id: id.into(),
                        name: name.into(),
                        kind: kind.into(),
                    },
                    since: r["since"].as_i64().unwrap_or(0),
                },
            );
            self.restored.insert(sid.to_string(), deadline);
        }
    }

    pub fn claim(
        &mut self,
        session_id: &str,
        device: &ldmux_protocol::rpc::DeviceInfo,
        takeover: bool,
    ) -> crate::ownership::ClaimResult {
        let now = now_ms() as i64;
        let r = self
            .ownership
            .claim(session_id, device.clone(), takeover, Some(now));
        // Who holds a shell is on the wire (`owner`), so a claim is a change.
        self.sessions_dirty = true;
        self.restored.remove(session_id);
        self.save_claims();
        r
    }

    /// Record that a claim has a connection behind it now.
    ///
    /// A claim with no connection behind it gets one of two graces
    /// (see [`RECONNECT_GRACE_MS`]): a claim taken on a
    /// window's behalf is nobody's after [`CLAIM_GRACE_MS`], and a claim whose
    /// connection went away belongs to its device for the reconnect grace.
    /// Called by the attach path, which is the one place a claim is made on
    /// behalf of a live connection.
    pub fn mark_backed(&mut self, session_id: &str, device_id: &str) {
        self.ever_backed
            .insert((session_id.to_string(), device_id.to_string()));
    }

    /// Release claims that no connection is behind, once they have been that way
    /// for [`CLAIM_GRACE_MS`] (or [`RECONNECT_GRACE_MS`], for a claim that had a
    /// connection before it went away).
    ///
    /// `attached` is the set of `(session, device)` pairs that have a connection
    /// *attached to that session right now* — the remote's view, since only it
    /// knows the connections. A claim with no connection behind it is a shell
    /// nobody is using: the daemon can tell, the window waiting for it cannot.
    ///
    /// The case this exists for: `makeLive`/`deskWindow` claim a shell to the
    /// desk device on a window's behalf, and the window then never attaches (it
    /// was already displaced by a phone and the daemon had lost its window
    /// record, so a second window was refused and the first one went on
    /// waiting). The claim stood for good, `list` said `desktop` held the shell,
    /// and the window — which only takes a shell that is *free* — never came
    /// back without someone pressing `r`.
    ///
    /// Returns the sessions it freed, which is news for every client.
    pub fn release_unbacked_claims(
        &mut self,
        attached: &std::collections::HashSet<(String, String)>,
        now: u64,
    ) -> Vec<String> {
        let claims: Vec<(String, String)> = self
            .ownership
            .session_ids()
            .into_iter()
            .filter_map(|sid| {
                self.ownership
                    .owner(&sid)
                    .map(|c| (sid.clone(), c.device.id.clone()))
            })
            .collect();
        // Forget what is no longer a claim, and what is now a different one: a
        // claim that moved to another device has not been unbacked since the old
        // device's timestamp.
        self.unbacked_since
            .retain(|sid, (device, _)| claims.iter().any(|(s, d)| s == sid && d == device));
        // Bounded by the claims that exist, like the clock above: a device that
        // held a shell fit only matters while the shell is still claimed.
        self.ever_backed
            .retain(|(sid, device)| claims.iter().any(|(s, d)| s == sid && d == device));
        // A restore deadline is only about a claim that is still there: one whose
        // claim was dropped (its shell ended) kept its entry for the life of the
        // daemon.
        self.restored
            .retain(|sid, _| claims.iter().any(|(s, _)| s == sid));
        let mut freed = Vec::new();
        for (sid, device) in claims {
            if attached.contains(&(sid.clone(), device.clone())) {
                self.ever_backed.insert((sid.clone(), device.clone()));
                self.unbacked_since.remove(&sid);
                self.restored.remove(&sid);
                continue;
            }
            // Restored from disk: held for its device until the restore grace
            // ends, however long it has been unbacked.
            if self.restored.get(&sid).is_some_and(|until| *until > now) {
                self.unbacked_since.remove(&sid);
                continue;
            }
            let since = match self.unbacked_since.get(&sid) {
                Some((d, t)) if d == &device => *t,
                _ => {
                    self.unbacked_since
                        .insert(sid.clone(), (device.clone(), now));
                    now
                }
            };
            // A claim that has had a connection behind it belongs to its
            // device until the reconnect grace runs out; one that never did
            // was taken on a window's behalf and is nobody's after the short
            // grace. Same question as before, but the answer depends on which
            // of the two this claim is.
            let grace = if self.ever_backed.contains(&(sid.clone(), device.clone())) {
                RECONNECT_GRACE_MS
            } else {
                CLAIM_GRACE_MS
            };
            if now.saturating_sub(since) >= grace {
                self.unbacked_since.remove(&sid);
                self.release_claim(&sid, &device);
                freed.push(sid);
            }
        }
        freed
    }

    /// Release a claim (the TS `releaseClaim`).
    pub fn release_claim(&mut self, session_id: &str, device_id: &str) -> bool {
        let r = self.ownership.release(session_id, device_id);
        if r {
            self.sessions_dirty = true;
            self.restored.remove(session_id);
            self.save_claims();
        }
        r
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
        let freed = self.ownership.release_device(device_id);
        if !freed.is_empty() {
            for sid in &freed {
                self.restored.remove(sid);
            }
            self.save_claims();
        }
        freed
    }
}

/// The environment every session's child starts with (plan §2.6).
///
/// Two groups, both load-bearing:
///
/// 1. `TERM`/`COLORTERM` pinned to what the emulator actually is. The daemon's
///    unit has no `TERM`, so a child that inherited one would get bash's
///    invented `dumb` and every colour-capable program would switch off. The
///    keeper pins the same default (`child_env`) for keepers started before
///    this entry existed, and `Spawn.env` wins there, so a session is
///    colour-correct whether or not the running keeper knows about it.
/// 2. The `LDMUX_*` identity set: the session id, kind, the keeper session
///    name, and both socket paths.
fn session_spawn_env(
    id: &str,
    kind: &str,
    keeper_name: &str,
    daemon_sock: &str,
    keeper_sock: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("TERM".to_string(), km::CHILD_TERM.to_string()),
        ("COLORTERM".to_string(), km::CHILD_COLORTERM.to_string()),
        ("LDMUX".to_string(), "1".to_string()),
        ("LDMUX_SESSION_ID".to_string(), id.to_string()),
        ("LDMUX_KIND".to_string(), kind.to_string()),
        ("LDMUX_SOCKET".to_string(), daemon_sock.to_string()),
        ("LDMUX_SESSION_NAME".to_string(), keeper_name.to_string()),
        ("LDMUX_KEEPER_SOCK".to_string(), keeper_sock.to_string()),
    ];
    env.extend(utf8_locale_env(|k| std::env::var(k).ok()));
    env
}

/// The locale a session's shell starts with.
///
/// The emulator is UTF-8, so a shell in the `C`/`POSIX` locale (what a systemd
/// user service has when the unit sets no `LANG`) mangles every non-ASCII
/// character it echoes or edits. The keeper inherits *its* environment, which is
/// whatever the daemon had when it first spawned it and can be stale, so the
/// locale is stated in `Spawn.env` from the daemon's current one: the daemon's
/// own `LANG` when the effective locale (`LC_ALL`, `LC_CTYPE`, `LANG`, in that
/// order) is already UTF-8, else `C.UTF-8`, which glibc and musl both ship. A
/// non-UTF-8 `LC_ALL`/`LC_CTYPE` would override `LANG`, so those are replaced
/// too when they are set.
pub(crate) fn utf8_locale_env(get: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let is_utf8 = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.contains("utf-8") || v.contains("utf8")
    };
    let nonempty = |k: &str| get(k).filter(|v| !v.is_empty());
    let effective = nonempty("LC_ALL")
        .or_else(|| nonempty("LC_CTYPE"))
        .or_else(|| nonempty("LANG"));
    if effective.as_deref().is_some_and(is_utf8) {
        // Already right; pin LANG only so a stale keeper environment cannot win.
        return nonempty("LANG")
            .map(|l| vec![("LANG".to_string(), l)])
            .unwrap_or_default();
    }
    let mut env = vec![("LANG".to_string(), "C.UTF-8".to_string())];
    for k in ["LC_ALL", "LC_CTYPE"] {
        if nonempty(k).is_some() {
            env.push((k.to_string(), "C.UTF-8".to_string()));
        }
    }
    if nonempty("LC_ALL").is_none() && nonempty("LC_CTYPE").is_none() {
        env.push(("LC_CTYPE".to_string(), "C.UTF-8".to_string()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_keeper::FakeKeeper;
    use ldmux_protocol::keeper::{Kill, Spawn};
    use ldmux_protocol::rpc::SessionRecord;

    fn scratch() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ldmux-daemon-core-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A session's spawn env pins the colour terminal, so a keeper that does
    /// not itself default `TERM` still hands the shell a colour-capable one.
    /// Without this the daemon's unit supplies no `TERM`, bash invents `dumb`,
    /// and every program turns colour off (observed 2026-10-05).
    #[test]
    fn the_shell_gets_a_utf8_locale_when_the_daemon_has_none() {
        let env = |pairs: &'static [(&str, &str)]| {
            utf8_locale_env(move |k| {
                pairs
                    .iter()
                    .find(|(pk, _)| *pk == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        let get = |e: &Vec<(String, String)>, k: &str| {
            e.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone())
        };
        // Nothing set (a bare systemd unit): C.UTF-8.
        let e = env(&[]);
        assert_eq!(get(&e, "LANG").as_deref(), Some("C.UTF-8"));
        assert_eq!(get(&e, "LC_CTYPE").as_deref(), Some("C.UTF-8"));
        // The classic C locale is replaced, and a non-UTF-8 LC_ALL cannot win.
        let e = env(&[("LANG", "C"), ("LC_ALL", "POSIX")]);
        assert_eq!(get(&e, "LANG").as_deref(), Some("C.UTF-8"));
        assert_eq!(get(&e, "LC_ALL").as_deref(), Some("C.UTF-8"));
        // The daemon's own UTF-8 locale is kept, not replaced.
        let e = env(&[("LANG", "ja_JP.UTF-8")]);
        assert_eq!(e, vec![("LANG".to_string(), "ja_JP.UTF-8".to_string())]);
        // LANG says UTF-8 but LC_CTYPE overrides it with something else: fixed.
        let e = env(&[("LANG", "en_US.UTF-8"), ("LC_CTYPE", "C")]);
        assert_eq!(get(&e, "LC_CTYPE").as_deref(), Some("C.UTF-8"));
    }

    #[test]
    fn the_spawn_env_pins_a_colour_terminal() {
        let env = session_spawn_env("id", "shell", "ldmux-id", "/d.sock", "/k.sock");
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str());
        assert_eq!(get("TERM"), Some(km::CHILD_TERM));
        assert_eq!(get("COLORTERM"), Some(km::CHILD_COLORTERM));
        assert_eq!(get("LDMUX_SESSION_ID"), Some("id"));
        assert_eq!(get("LDMUX_KEEPER_SOCK"), Some("/k.sock"));
        assert!(
            env.iter()
                .all(|(k, _)| !k.to_ascii_lowercase().contains("tmux"))
        );
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        assert_eq!(d.keeper_socket_path(), keeper_sock.as_path());

        // Spawn a session on the fake keeper (the daemon's keeper_call path).
        // The keeper session id IS the keeper name (the daemon passes it at Spawn).
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
        let now = now_ms();
        d.registry().upsert(SessionRecord {
            id: "s1".into(),
            keeper_name: "ldmux-s1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        });

        // Pass 1: the session is live, so nothing is marked dead.
        let r = d.reconcile().await.unwrap();
        assert!(r.dead.is_empty(), "a live session is not dead: {:?}", r);
        assert!(r.adopted.is_empty());

        // Kill the session on the fake keeper.
        let kill = KeeperMessage::Kill(Kill {
            session_id: "ldmux-s1".into(),
        });
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
    /// reconciles — all over the keeper socket, never a separate control plane.
    #[tokio::test]
    async fn start_adopts_the_daemons_own_sessions_against_the_fake_keeper() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));

        // A session the registry does not know (the daemon's own session).
        // The keeper session id IS the keeper name.
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
        assert_eq!(r.keeper_name, "ldmux-s1");
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        // Create a session in the current dir (which exists).
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        assert_eq!(record.kind, "shell");
        assert_eq!(record.status, "running");
        assert!(!record.keeper_name.is_empty());
        // The registry has the record.
        assert!(d.registry().get(&record.id).is_some());
        // The keeper has the session.
        let resp = d
            .keeper_call(&KeeperMessage::List(km::List {}))
            .await
            .unwrap();
        let list = match &resp {
            KeeperMessage::ListResult(l) => l.clone(),
            _ => panic!("expected ListResult"),
        };
        assert!(
            list.sessions
                .iter()
                .any(|s| s.session_id == record.keeper_name)
        );
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let id = record.id.clone();
        let _keeper_name = record.keeper_name.clone();
        // Kill the session.
        let killed = d.kill_session(&id, "killed").await.unwrap();
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let id = record.id.clone();
        // No one holds it: close kills it.
        let (ended, _) = d.close_session(&id, "closed").await.unwrap();
        assert!(ended);
        assert_eq!(d.registry().get(&id).unwrap().status, "dead");
        // Create another, claim it, and close: it is let go, not killed.
        let record2 = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let id2 = record2.id.clone();
        let device = ldmux_protocol::rpc::DeviceInfo {
            id: "device1".into(),
            name: "test".into(),
            kind: "phone".into(),
        };
        d.claim(&id2, &device, false);
        let (ended2, _) = d.close_session(&id2, "closed").await.unwrap();
        assert!(!ended2);
        assert_eq!(d.registry().get(&id2).unwrap().status, "running");
        drop(fk);
    }

    /// `capture --history` asks the keeper for the pre-migration cap.
    ///
    /// The TS captured with `HISTORY_CAPTURE_LINES = 5000`; the port asked for
    /// 100, so the CLI showed a hundred lines of a history the pane had
    /// thousands of (measured 2026-10-05: 995 rows retained, 123 returned).
    #[tokio::test]
    async fn capture_history_asks_for_the_plans_cap() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();

        // Without `history` the request is the live screen: no history asked for.
        d.handle("capture", serde_json::json!({ "id": rec.id }))
            .await
            .unwrap();
        assert_eq!(
            fk.last_history_request(),
            None,
            "a live capture asks for no history"
        );

        d.handle(
            "capture",
            serde_json::json!({ "id": rec.id, "history": true }),
        )
        .await
        .unwrap();
        assert_eq!(
            fk.last_history_request(),
            Some(crate::daemon::HISTORY_CAPTURE_LINES),
            "`--history` asks for the plan's cap"
        );
        drop(fk);
    }

    /// The `desktop` RPC links a running pi to the ldmux session it runs inside.
    ///
    /// The inventory cannot do this by itself: a desk window's terminal is not in
    /// the pi's ancestry (the pi runs in the keeper's pty, the window is a
    /// separate `alacritty` running `ldmuxd attach`), so a pi filling a window
    /// reports no window at all. The keeper's shell pid *is* in that ancestry —
    /// the same fact the TypeScript got from tmux's `#{pane_pid}` — and in the
    /// port `ldmuxSessionId` was hard-coded `None`, so every client that asked
    /// was told the pi was not in ldmux and guessed by directory instead
    /// (`origin/main:packages/daemon/src/daemon.ts:1106`).
    #[tokio::test]
    async fn the_desktop_inventory_links_a_running_pi_to_its_session() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A process whose `comm` is `pi`, under a shell whose parent is this test
        // process: `pi` → `sh` → the test process. `/bin/sleep` copied to a file
        // named `pi` is what makes the inventory's scan find it.
        let bin = dir.join("pi");
        std::fs::copy("/bin/sleep", &bin).unwrap();
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{} 30; :", bin.display()))
            .spawn()
            .unwrap();
        let pi_pid = {
            let mut found = 0i64;
            for _ in 0..200 {
                if let Ok(entries) = std::fs::read_dir("/proc") {
                    for e in entries.flatten() {
                        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i64>().ok())
                        else {
                            continue;
                        };
                        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                            && comm.trim() == "pi"
                        {
                            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                                .unwrap_or_default();
                            // Only the one under our own sh: its ppid is a child of
                            // this test process.
                            if std::fs::read_to_string(format!("/proc/{pid}/status"))
                                .map(|st| st.contains(&format!("PPid:\t{}", child.id())))
                                .unwrap_or(false)
                            {
                                found = pid;
                                let _ = stat;
                            }
                        }
                    }
                }
                if found > 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(found > 0, "the spawned pi process");
            found
        };

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        // The keeper reports the *shell's* pid; standing in for it with this test
        // process's pid makes the spawned pi a descendant of "the shell".
        fk.set_pid(&rec.keeper_name, std::process::id() as i32);

        let inv = d.handle("desktop", serde_json::json!({})).await.unwrap();
        let sessions = inv
            .get("piSessions")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        let mine = sessions
            .iter()
            .find(|p| p.get("pid").and_then(|x| x.as_i64()) == Some(pi_pid))
            .unwrap_or_else(|| panic!("the spawned pi in {sessions:?}"));
        assert_eq!(
            mine.get("ldmuxSessionId").and_then(|x| x.as_str()),
            Some(rec.id.as_str()),
            "the pi's ancestry leads to the session's shell: {mine}"
        );

        let _ = child.kill();
        let _ = child.wait();
        drop(fk);
    }

    /// Every ending says why, on the record and in the list the CLI prints.
    ///
    /// Before this the record said `dead` and nothing else and the journal said
    /// nothing at all: two shells ended during an evening of use (2026-10-05) and
    /// the only way to explain either was elimination.
    #[tokio::test]
    async fn an_ending_says_why() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let killed = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let closed = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();

        d.kill_session(&killed.id, "killed from the test")
            .await
            .unwrap();
        let (ended, _) = d
            .close_session(&closed.id, "closed from the test")
            .await
            .unwrap();
        assert!(ended);

        // On the record, where `ldmux list` reads it.
        assert_eq!(
            d.registry()
                .get(&killed.id)
                .unwrap()
                .ended_reason
                .as_deref(),
            Some("killed from the test")
        );
        assert_eq!(
            d.registry()
                .get(&closed.id)
                .unwrap()
                .ended_reason
                .as_deref(),
            Some("closed from the test")
        );
        // And on the advertised session, which is what the CLI prints.
        let listed = d.describe_sessions().await;
        let info = listed.iter().find(|s| s.id == killed.id).unwrap();
        assert_eq!(info.status, "dead");
        assert_eq!(info.ended_reason.as_deref(), Some("killed from the test"));
        // A running session has no reason to give.
        let live = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let listed = d.describe_sessions().await;
        let info = listed.iter().find(|s| s.id == live.id).unwrap();
        assert_eq!(info.ended_reason, None);
        drop(fk);
    }

    /// A shell that exited on its own and one the keeper came back without read
    /// differently, and the difference is the keeper's own report: a pane still
    /// there marked `dead` is a shell that exited and whose last screen is being
    /// kept, and a name the keeper does not have at all came back without it.
    #[tokio::test]
    async fn a_shell_that_exited_and_one_the_keeper_lost_read_differently() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let exited = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        // The pane stays, marked dead: the shell exited.
        fk.kill(&exited.keeper_name);
        // A record whose keeper never had it: the keeper came back without it.
        let mut ghost = exited.clone();
        ghost.id = "ghost1".into();
        ghost.keeper_name = "ldmux-ghost1".into();
        ghost.status = "running".into();
        ghost.exited_at = None;
        ghost.ended_reason = None;
        d.registry().upsert(ghost);

        let r = d.reconcile().await.unwrap();
        assert!(r.dead.contains(&exited.id), "both are dead: {:?}", r.dead);
        assert!(
            r.dead.contains(&"ghost1".to_string()),
            "both are dead: {:?}",
            r.dead
        );
        assert_eq!(
            d.registry()
                .get(&exited.id)
                .unwrap()
                .ended_reason
                .as_deref(),
            Some("its shell exited")
        );
        assert_eq!(
            d.registry().get("ghost1").unwrap().ended_reason.as_deref(),
            Some("the keeper no longer has it")
        );
        drop(fk);
    }

    /// A claim with no connection behind it is given back after the grace, and
    /// only then: the grace is what lets a window attach to the claim the daemon
    /// just took on its behalf.
    ///
    /// The state this exists for, found in use on 2026-10-05: the daemon claimed
    /// a shell to the desk device for a window that then never attached (it was
    /// already displaced, and the daemon had lost its window record). `list`
    /// said the desk held the shell, the displaced window waited for a *free*
    /// one, and the only way out was pressing `r`.
    #[test]
    fn a_claim_with_no_connection_behind_it_is_given_back() {
        let dir = scratch();
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let device = ldmux_protocol::rpc::DeviceInfo {
            id: "desktop".into(),
            name: "desktop".into(),
            kind: "desktop".into(),
        };
        let other = ldmux_protocol::rpc::DeviceInfo {
            id: "phone".into(),
            name: "phone".into(),
            kind: "phone".into(),
        };
        let attached = |pairs: &[(&str, &str)]| -> std::collections::HashSet<(String, String)> {
            pairs
                .iter()
                .map(|(s, d)| (s.to_string(), d.to_string()))
                .collect()
        };

        d.claim("s1", &device, false);
        // The claim is a moment old: the window it was taken for is still
        // connecting. Nothing is freed.
        assert!(d.release_unbacked_claims(&attached(&[]), 1_000).is_empty());
        assert!(d.holds("s1", "desktop"), "the grace must not be cut short");
        // Inside the grace, still nothing.
        assert!(
            d.release_unbacked_claims(&attached(&[]), 1_000 + CLAIM_GRACE_MS - 1)
                .is_empty()
        );
        assert!(d.holds("s1", "desktop"));
        // Past it, the claim is given back — and it is news, so the session is
        // marked dirty for the next push.
        d.sessions_dirty = false;
        assert_eq!(
            d.release_unbacked_claims(&attached(&[]), 1_000 + CLAIM_GRACE_MS),
            vec!["s1".to_string()]
        );
        assert!(!d.holds("s1", "desktop"));
        assert!(d.sessions_dirty, "who holds a shell is on the wire");

        // A connection behind the claim keeps it, however old it is.
        d.claim("s2", &device, false);
        assert!(
            d.release_unbacked_claims(&attached(&[("s2", "desktop")]), 9_999_999)
                .is_empty()
        );
        assert!(d.holds("s2", "desktop"));

        // A connection appearing inside the grace restarts the wait: the claim
        // has not been unbacked since it appeared. (`s2` stays attached from
        // here on, so what is freed is never it.)
        let with_s2 = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("s2", "desktop")];
            pairs.extend_from_slice(extra);
            attached(&pairs)
        };
        d.claim("s3", &device, false);
        assert!(d.release_unbacked_claims(&with_s2(&[]), 1_000).is_empty());
        assert!(
            d.release_unbacked_claims(&with_s2(&[("s3", "desktop")]), 4_000)
                .is_empty(),
            "an attached claim is not unbacked"
        );
        assert!(
            d.release_unbacked_claims(&with_s2(&[]), 4_100).is_empty(),
            "the wait runs from when it went unbacked again"
        );
        assert!(d.holds("s3", "desktop"));
        // This claim *has* had a connection behind it, so it is its device's to
        // keep while that device reconnects: the short grace that frees a claim
        // taken on a window's behalf does not free it.
        assert!(
            d.release_unbacked_claims(&with_s2(&[]), 4_100 + CLAIM_GRACE_MS)
                .is_empty(),
            "a claim that was backed gets the reconnect grace, not the claim grace"
        );
        assert!(d.holds("s3", "desktop"));
        assert_eq!(
            d.release_unbacked_claims(&with_s2(&[]), 4_100 + RECONNECT_GRACE_MS),
            vec!["s3".to_string()]
        );

        // A claim that moves to another device is a new claim, and waits again:
        // the old device's timestamp is not the new holder's.
        d.claim("s4", &device, false);
        assert!(d.release_unbacked_claims(&with_s2(&[]), 1_000).is_empty());
        d.claim("s4", &other, true);
        assert!(
            d.release_unbacked_claims(&with_s2(&[]), 1_000 + CLAIM_GRACE_MS)
                .is_empty(),
            "the new holder has not been unbacked for the grace yet"
        );
        assert!(d.holds("s4", "phone"));
        assert_eq!(
            d.release_unbacked_claims(&with_s2(&[]), 1_000 + 2 * CLAIM_GRACE_MS),
            vec!["s4".to_string()]
        );
    }

    fn running_record(id: &str) -> SessionRecord {
        let now = now_ms();
        SessionRecord {
            id: id.into(),
            keeper_name: format!("ldmux-{id}"),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: None,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        }
    }

    async fn spawn_fake(sock: &std::path::Path, name: &str) {
        ldmux_protocol::keeper_call::keeper_call(
            sock.to_str().unwrap(),
            &KeeperMessage::Spawn(Spawn {
                session_id: name.into(),
                cols: 80,
                rows: 24,
                cwd: None,
                command: None,
                env: None,
            }),
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();
    }

    async fn keeper_has(sock: &std::path::Path, name: &str) -> bool {
        ldmux_protocol::keeper_call::keeper_call(
            sock.to_str().unwrap(),
            &KeeperMessage::Read(km::Read {
                session_id: name.into(),
            }),
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .is_ok()
    }

    /// How a shell ended is recorded (status or signal, and the kind), and its pane
    /// stays in the keeper for the keep window so the last screen can be read.
    #[tokio::test]
    async fn an_ended_shell_records_how_it_ended_and_its_pane_outlives_the_ending() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk2 = fk.clone();
        let ks = keeper_sock.clone();
        tokio::spawn(async move { FakeKeeper::serve(&fk2, ks.as_path()).await.unwrap() });
        wait_socket(&keeper_sock).await;
        let mut d =
            LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent")).with_dead_keep_ms(400);
        for id in ["a", "b", "c"] {
            if id != "c" {
                spawn_fake(&keeper_sock, &format!("ldmux-{id}")).await;
            }
            d.registry().upsert(running_record(id));
        }
        fk.set_snapshot("ldmux-a", "the last screen");
        fk.exit("ldmux-a", Some(3), None);
        fk.exit("ldmux-b", None, Some(9));

        let r = d.reconcile().await.unwrap();
        assert_eq!(r.dead.len(), 3, "{r:?}");
        let (a, b, c) = (
            d.registry().get("a").unwrap().clone(),
            d.registry().get("b").unwrap().clone(),
            d.registry().get("c").unwrap().clone(),
        );
        assert_eq!(
            (a.exit_code, a.exit_signal, a.ended_kind.as_deref()),
            (Some(3), None, Some("exited"))
        );
        assert_eq!(
            (b.exit_code, b.exit_signal, b.ended_kind.as_deref()),
            (None, Some(9), Some("exited"))
        );
        assert_eq!(
            (c.exit_code, c.ended_kind.as_deref()),
            (None, Some("keeper_lost"))
        );
        assert_eq!(
            c.ended_reason.as_deref(),
            Some("the keeper no longer has it")
        );

        // The pane is still there, and its last screen is readable.
        assert!(keeper_has(&keeper_sock, "ldmux-a").await);
        let screen = d
            .final_screen("ldmux-a")
            .await
            .expect("the final screen is kept");
        assert_eq!(screen.text, "the last screen");
        // Past the keep window it is released.
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        d.reconcile().await.unwrap();
        assert!(
            !keeper_has(&keeper_sock, "ldmux-a").await,
            "released after the keep window"
        );
        assert!(d.final_screen("ldmux-a").await.is_none());
    }

    /// Claims survive a daemon restart: held for their device for the restore
    /// grace, freed by the ordinary sweep after it, and a stopped daemon does not
    /// erase them on the way out.
    #[tokio::test]
    async fn claims_survive_a_restart_and_are_held_for_the_restore_grace() {
        let dir = scratch();
        let device = ldmux_protocol::rpc::DeviceInfo {
            id: "phone".into(),
            name: "phone".into(),
            kind: "phone".into(),
        };
        let mut d1 = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d1.registry().upsert(running_record("s1"));
        d1.claim("s1", &device, false);
        assert!(dir.join("claims.json").exists());
        let mode = std::fs::metadata(dir.join("claims.json")).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o600
        );
        d1.stop(); // a stopping daemon must not wipe the file
        drop(d1);

        let mut d2 = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d2.registry().upsert(running_record("s1"));
        d2.registry().upsert(running_record("s2"));
        d2.load_claims();
        assert!(d2.holds("s1", "phone"), "the claim came back");
        let nobody = std::collections::HashSet::new();
        // Unbacked for far longer than the ordinary grace, but inside the restore grace.
        let t = d2.mono_ms();
        d2.release_unbacked_claims(&nobody, t + CLAIM_GRACE_MS + 1);
        assert!(
            d2.release_unbacked_claims(&nobody, t + 2 * CLAIM_GRACE_MS + 2)
                .is_empty()
        );
        assert!(d2.holds("s1", "phone"), "held for the device to come back");
        // After the restore grace the sweep frees it.
        d2.release_unbacked_claims(&nobody, t + RESTORE_GRACE_MS + 1);
        assert_eq!(
            d2.release_unbacked_claims(&nobody, t + RESTORE_GRACE_MS + 1 + CLAIM_GRACE_MS),
            vec!["s1".to_string()]
        );
        assert!(!d2.holds("s1", "phone"));

        // A freed claim stays freed: the file was rewritten when d2 let it go.
        let mut d3 = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d3.registry().upsert(running_record("s1"));
        d3.load_claims();
        assert!(!d3.holds("s1", "phone"), "a freed claim is not resurrected");
    }

    /// A restore deadline belongs to a claim that is still there: when the claim
    /// goes (its record was pruned) the entry goes with it instead of living as long
    /// as the daemon.
    #[tokio::test]
    async fn a_restore_deadline_does_not_outlive_its_claim() {
        let dir = scratch();
        let device = ldmux_protocol::rpc::DeviceInfo {
            id: "phone".into(),
            name: "phone".into(),
            kind: "phone".into(),
        };
        let mut d1 = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d1.registry().upsert(running_record("s1"));
        d1.claim("s1", &device, false);
        d1.stop();
        drop(d1);
        let mut d2 = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d2.registry().upsert(running_record("s1"));
        d2.load_claims();
        assert_eq!(d2.restored.len(), 1);
        // The record goes (reconcile's `ownership.retain`), and the next sweep tidies.
        d2.ownership.retain(&HashSet::new());
        d2.release_unbacked_claims(&HashSet::new(), d2.mono_ms());
        assert!(d2.restored.is_empty(), "{:?}", d2.restored);
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let id = record.id.clone();
        // A running session cannot be forgotten.
        let err = d.forget_session(&id).await.unwrap_err();
        assert!(err.contains("running"));
        // Kill it, then forget it.
        d.kill_session(&id, "killed").await.unwrap();
        let removed = d.forget_session(&id).await.unwrap();
        assert!(removed);
        assert!(d.registry().get(&id).is_none());
        drop(fk);
    }

    /// Ending a shell is news even when the reconcile pass sees no *transition*.
    ///
    /// `registry.reconcile` reports a session in `dead` only when it **becomes**
    /// dead. `kill_session` writes `status = "dead"` itself, so by the next
    /// maintenance tick the record is already dead, nothing looks changed, and no
    /// list is sent — measured on the live daemon: a connected client received
    /// nothing for 8s after `ldmux kill` and went on showing a shell that was
    /// gone. The mutator now marks the state, and the tick obeys the mark.
    #[tokio::test]
    async fn ending_a_shell_is_news_even_without_a_reconcile_transition() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        // The create is itself news: a client must hear about a new shell.
        assert!(d.maintain().await.unwrap(), "creating a shell is news");
        assert!(!d.maintain().await.unwrap(), "an idle tick is not news");
        d.kill_session(&rec.id, "killed").await.unwrap();
        assert!(
            d.maintain().await.unwrap(),
            "the kill must be reported, even though the record is already dead \
             and `reconcile` therefore finds no transition"
        );
        assert!(!d.maintain().await.unwrap(), "and it is news only once");
    }

    /// The described list puts a running shell above one that is not, most
    /// recently active first.
    ///
    /// This is the fix for a launcher whose leading rows were ghosts: the
    /// registry orders by `updated_at` and ending a session *writes* it, so a
    /// shell that had just been killed sorted above every live one. Measured on
    /// the live daemon before the fix: after `kill`, rows 1 and 2 were both dead.
    #[tokio::test]
    async fn a_dead_shell_never_sorts_above_a_running_one() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let a = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let b = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let c = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        // Killed in an order that makes the *death* times the reverse of the
        // creation times, so an `updated_at` order would lead with the dead.
        // The sleeps are load-bearing: the death time and the activity fallback
        // are milliseconds, and two kills inside one millisecond tie, at which
        // point the id breaks the tie and the assertion below is a coin flip.
        d.kill_session(&a.id, "killed").await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        d.kill_session(&c.id, "killed").await.unwrap();
        let described = d.describe_sessions().await;
        let statuses: Vec<&str> = described.iter().map(|s| s.status.as_str()).collect();
        assert_eq!(
            statuses,
            vec!["running", "dead", "dead"],
            "running leads: {statuses:?}"
        );
        assert_eq!(described[0].id, b.id, "the only running shell is first");
        assert_eq!(described[1].id, c.id, "of the dead, the most recent leads");
        assert_eq!(described[2].id, a.id);
    }

    /// `lastActivityAt` is the pane's own clock, not the record's creation time.
    ///
    /// It was `Some(record.updated_at)`, which is only ever the creation or the
    /// death time, so a field documented as activity never moved while a shell
    /// was being used. The keeper stamps a session on input and output
    /// (its `List` carries the clock).
    #[tokio::test]
    async fn last_activity_is_the_pane_clock_not_the_creation_time() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let before = d
            .describe_sessions()
            .await
            .into_iter()
            .find(|s| s.id == rec.id)
            .unwrap()
            .last_activity_at
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        // Input to the pane is activity, and the fake keeper stamps its clock
        // exactly as the real one does (input and output).
        d.handle("send", serde_json::json!({ "id": rec.id, "text": "hi" }))
            .await
            .unwrap();
        let after = d
            .describe_sessions()
            .await
            .into_iter()
            .find(|s| s.id == rec.id)
            .unwrap()
            .last_activity_at
            .unwrap();
        assert!(
            after > before,
            "activity must advance on pane output: {before} -> {after}"
        );
        assert!(
            after > rec.created_at,
            "and it is not simply the creation time"
        );
    }

    /// Ending a shell drops its stream, so the client watching it is told.
    ///
    /// The subscriber is reset by `drop_stream`; without it an attached desk
    /// window kept its subscription to a pane that no longer existed. Measured
    /// live: after `ldmux kill`, the window and its three `ldmuxd attach`
    /// processes were all still alive.
    #[tokio::test]
    async fn a_killed_shell_drops_its_stream() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        d.stream_for(&rec.keeper_name);
        assert!(
            d.get_stream(&rec.keeper_name).is_some(),
            "the stream exists to be dropped"
        );
        d.kill_session(&rec.id, "killed").await.unwrap();
        assert!(
            d.get_stream(&rec.keeper_name).is_none(),
            "a killed shell must drop its stream, or its window never hears"
        );
    }

    /// And so does a shell that ended on its own — the `exit` path.
    ///
    /// The two paths are different code (`kill_session` vs the reconcile pass)
    /// and only the first dropped the stream. Measured after typing `exit`: the
    /// window stayed on screen with its attach processes alive while the registry
    /// already said `dead`.
    #[tokio::test]
    async fn a_shell_that_exits_drops_its_stream() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        d.stream_for(&rec.keeper_name);
        // The shell exits: the keeper marks the pane dead (its authoritative
        // death signal is `waitpid`, which is what `kill` models here).
        fk.kill(&rec.keeper_name);
        d.reconcile().await.unwrap();
        assert_eq!(d.registry().get(&rec.id).unwrap().status, "dead");
        assert!(
            d.get_stream(&rec.keeper_name).is_none(),
            "a shell that exited must drop its stream too, or its window sits on a dead pane"
        );
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));

        // `ping` returns the keeper socket.
        let r = d.handle("ping", serde_json::json!({})).await.unwrap();
        assert!(r.get("pong").and_then(|v| v.as_bool()).unwrap_or(false));
        assert_eq!(
            r.get("keeperSocket").and_then(|v| v.as_str()).unwrap_or(""),
            keeper_sock.to_string_lossy(),
        );

        // `new` creates a session.
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let r = d
            .handle("new", serde_json::json!({ "kind": "shell", "cwd": cwd }))
            .await
            .unwrap();
        let id: &str = r
            .get("session")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(!id.is_empty());

        // `list` includes the new session.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r
            .get("sessions")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            sessions
                .iter()
                .any(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
        );

        // `kill` kills the session.
        let r = d
            .handle("kill", serde_json::json!({ "id": id }))
            .await
            .unwrap();
        assert!(r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

        // `list` shows the session as dead.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r
            .get("sessions")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        let status = sessions
            .iter()
            .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
            .and_then(|s| s.get("status").and_then(|v| v.as_str()))
            .unwrap_or("");
        assert_eq!(status, "dead");

        // `forget` removes the dead session.
        let r = d
            .handle("forget", serde_json::json!({ "id": id }))
            .await
            .unwrap();
        assert!(r.get("removed").and_then(|v| v.as_bool()).unwrap_or(false));

        // `list` no longer includes the session.
        let r = d.handle("list", serde_json::json!({})).await.unwrap();
        let sessions: Vec<serde_json::Value> = r
            .get("sessions")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            !sessions
                .iter()
                .any(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
        );

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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let keeper_name = record.keeper_name.clone();

        // `stream_for` creates the stream.
        let stream = d.stream_for(&keeper_name);
        assert_eq!(stream.session_id(), keeper_name);
        assert_eq!(stream.subscriber_count(), 0);

        // `release_stream_if_idle` releases the stream (no subscribers).
        d.release_stream_if_idle(&keeper_name);
        assert!(!d.streams.contains_key(&keeper_name));

        // `stream_for` recreates the stream.
        let _stream = d.stream_for(&keeper_name);
        assert!(d.streams.contains_key(&keeper_name));

        // Kill the session, then `sweep_streams` drops the stream.
        d.kill_session(&record.id, "killed").await.unwrap();
        d.sweep_streams();
        assert!(!d.streams.contains_key(&keeper_name));

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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let id = record.id.clone();

        // `send` writes input to the keeper.
        let r = d
            .handle("send", serde_json::json!({ "id": id, "text": "hello" }))
            .await
            .unwrap();
        assert!(r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

        // `capture` returns the rendered screen (the fake keeper's snapshot_text
        // is empty until a test sets it, so we only assert the field exists).
        let r = d
            .handle("capture", serde_json::json!({ "id": id }))
            .await
            .unwrap();
        assert!(r.get("screen").is_some(), "the screen field should exist");

        // `snapshot` returns the rendered screen + state.
        let r = d
            .handle("snapshot", serde_json::json!({ "id": id }))
            .await
            .unwrap();
        assert!(r.get("cols").and_then(|v| v.as_u64()).is_some());
        assert!(r.get("rows").and_then(|v| v.as_u64()).is_some());

        drop(fk);
    }

    /// Stop removes the socket and the lock (the TS `stop removes the socket
    /// and the lock, and leaves the shells alone`).
    #[tokio::test]
    async fn stop_removes_the_socket_and_the_lock() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
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
        // The lock is released (the file stays: unlinking a flocked file races).
        // A sibling test's fork() can hold a copy of the descriptor until its exec
        // closes it (O_CLOEXEC), so the flock may read as held for a few
        // milliseconds under parallel load: poll rather than assert at once.
        let mut next = crate::daemon_lock::DaemonLock::new(&dir);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match next.acquire() {
                Ok(_) => break,
                Err(e) if std::time::Instant::now() >= deadline => {
                    panic!("the lock should be free after stop: {e:?}")
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
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
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;

        // Take a port with a raw TCP listener.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Start a daemon that wants the same port.
        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent")).with_remote(
            "127.0.0.1".to_string(),
            port,
            "test-token".to_string(),
        );
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

    /// A desk window is remembered in the registry, not only in memory: that is
    /// what lets a restart end a shell whose window closed while the daemon was
    /// down. The TS got this from the live shim's pid; the port has no shim, so
    /// the record carries it.
    #[tokio::test]
    async fn a_live_workspace_is_persisted_and_a_dead_one_is_cleared() {
        // Round trip: camelCase on the wire, back into the field.
        let r = SessionRecord {
            id: "live1".into(),
            keeper_name: "ldmux-live1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "t".into(),
            created_at: 1,
            updated_at: 1,
            created_by: "local".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: Some("1".into()),
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"liveWorkspace\":\"1\""), "{json}");
        assert_eq!(
            serde_json::from_str::<SessionRecord>(&json)
                .unwrap()
                .live_workspace
                .as_deref(),
            Some("1")
        );

        // The rebuild clears a window from a session that is not running, and
        // never leaves it in the live map.
        let dir = scratch();
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        d.registry().upsert(SessionRecord {
            id: "dead1".into(),
            keeper_name: "ldmux-dead1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "t".into(),
            created_at: 1,
            updated_at: 1,
            created_by: "local".into(),
            status: "dead".into(),
            exit_code: None,
            exited_at: None,
            live_workspace: Some("1".into()),
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        });
        let _ = d.rebuild_live_windows().await;
        assert!(d.registry().get("dead1").unwrap().live_workspace.is_none());
        assert!(!d.live_windows.contains_key("dead1"));

        // set_live_window records and clears both halves, persisting them.
        d.set_live_window("dead1", Some("2".into()), Some(4242));
        assert_eq!(
            d.registry().get("dead1").unwrap().live_workspace.as_deref(),
            Some("2")
        );
        assert_eq!(d.registry().get("dead1").unwrap().live_pid, Some(4242));
        d.set_live_window("dead1", None, None);
        assert!(d.registry().get("dead1").unwrap().live_workspace.is_none());
        assert_eq!(d.registry().get("dead1").unwrap().live_pid, None);
    }

    /// `ldmux shell` and `ldmux run` become a desk window in the terminal they
    /// were started from, so the daemon is told the window is the attach
    /// process (`spawn: false`, with its pid). Without that the shell had no
    /// window as far as the daemon knew: the phone showed the "no window"
    /// anomaly while the shell filled a window, and closing the terminal left
    /// the shell running with nothing left to end it (measured live
    /// 2026-10-05).
    #[tokio::test]
    async fn an_in_place_desk_window_is_the_process_it_was_given() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        // The window-close rule, without the real five seconds.
        d.live_window_grace_ms = 0;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();

        // A pid that is certainly alive: this test process.
        let me = std::process::id() as i32;
        let res = d
            .handle(
                "deskWindow",
                serde_json::json!({ "id": rec.id, "spawn": false, "pid": me }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], true, "{res}");
        // Recorded in both places, because the map is memory and the list is not.
        assert_eq!(d.registry().get(&rec.id).unwrap().live_pid, Some(me));
        assert_eq!(
            d.registry().get(&rec.id).unwrap().live_workspace.as_deref(),
            Some("")
        );

        // A window whose process is alive survives a reconcile pass.
        assert!(!d.reconcile_live().await);
        assert_eq!(d.registry().get(&rec.id).unwrap().status, "running");

        // A window whose process is gone ends its shell.
        let gone = {
            let mut c = std::process::Command::new("true").spawn().unwrap();
            let pid = c.id() as i32;
            let _ = c.wait();
            pid
        };
        d.live_windows.get_mut(&rec.id).unwrap().pid = Some(gone);
        assert!(d.reconcile_live().await);
        assert_eq!(d.registry().get(&rec.id).unwrap().status, "dead");
        assert!(d.registry().get(&rec.id).unwrap().live_workspace.is_none());
        // And it says why, which is the whole point of the record's reason: a
        // shell that ended because its window closed reads differently from one
        // whose shell exited.
        assert_eq!(
            d.registry().get(&rec.id).unwrap().ended_reason.as_deref(),
            Some("its desk window closed")
        );
        drop(fk);
    }

    /// An in-place desk window with no process is refused. There would be no way
    /// to tell whether it is still there, so it would be a window that never
    /// closes, and a shell that therefore never ends.
    #[tokio::test]
    async fn an_in_place_desk_window_without_a_process_is_refused() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();
        let res = d
            .handle(
                "deskWindow",
                serde_json::json!({ "id": rec.id, "spawn": false }),
            )
            .await
            .unwrap();
        assert_eq!(res["ok"], false, "{res}");
        assert!(!d.live_windows.contains_key(&rec.id));
        assert!(d.registry().get(&rec.id).unwrap().live_workspace.is_none());
        drop(fk);
    }

    /// A window whose workspace the window manager did not name is still a
    /// window. `deskWindow` records exactly that (`""`), and the session list
    /// filtered blanks out, so every `$mod+Return` window read "no window" on the
    /// phone while its window sat on the desk (measured 2026-10-05).
    #[tokio::test]
    async fn a_desk_window_with_an_unnamed_workspace_is_still_reported() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path())
                .await
                .unwrap();
        });
        wait_socket(&keeper_sock).await;
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let rec = d
            .create_session("shell", &cwd, None, "local")
            .await
            .unwrap();

        // What `deskWindow` records: a window on whatever workspace was focused.
        d.live_windows.insert(
            rec.id.clone(),
            LiveWindow {
                workspace: String::new(),
                since: 0,
                pid: None,
            },
        );
        d.set_live_window(&rec.id, Some(String::new()), None);
        let info = d
            .describe_sessions()
            .await
            .into_iter()
            .find(|s| s.id == rec.id)
            .unwrap();
        assert_eq!(
            info.live_workspace.as_deref(),
            Some(""),
            "an unnamed workspace is a window"
        );

        // The moment after a restart, before the live map is rebuilt, the record
        // alone must still answer: the window is not gone just because the map
        // has not been filled yet.
        d.live_windows.clear();
        let info = d
            .describe_sessions()
            .await
            .into_iter()
            .find(|s| s.id == rec.id)
            .unwrap();
        assert_eq!(
            info.live_workspace.as_deref(),
            Some(""),
            "the record is the fallback"
        );
    }

    /// `LDMUX_SOCK` is honoured: the daemon binds where the shared resolver says,
    /// not only under the state dir.
    #[test]
    fn the_control_socket_path_can_be_set_apart_from_the_state_dir() {
        let dir = scratch();
        let d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        assert_eq!(d.socket_path(), dir.join("ldmuxd.sock"));
        let elsewhere = dir.join("custom.sock");
        let d = d.with_socket_path(elsewhere.clone());
        assert_eq!(d.socket_path(), elsewhere);
    }

    /// The state dir is created 0700, a loose one of ours is tightened, and
    /// something that is not a directory is refused.
    #[test]
    fn the_state_dir_must_be_ours_and_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let fresh = root.join("a/b");
        ensure_private_state_dir(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_state_dir(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = root.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(ensure_private_state_dir(&file).is_err());
    }

    /// A background loop's pass that panics costs that pass: the next one runs, and
    /// the lock the panicking pass held is not left taken.
    #[tokio::test]
    async fn a_panicking_pass_does_not_end_the_loop_or_hold_its_lock() {
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(0u32));
        let l = lock.clone();
        let first = survive("test", async move {
            let _g = l.lock().await;
            panic!("a pass that fails while holding the daemon lock");
        })
        .await;
        assert!(first.is_none());
        let l = lock.clone();
        let second = survive("test", async move {
            *l.lock().await += 1;
            7
        })
        .await;
        assert_eq!(second, Some(7));
        assert_eq!(*lock.lock().await, 1);
    }

    #[test]
    fn process_alive_asks_the_kernel() {
        assert!(process_alive(std::process::id() as i32));
        assert!(!process_alive(0));
        assert!(!process_alive(999_999_999));
    }
}
