//! The remote server: the phone-facing WebSocket endpoint (the tailnet address
//! + bearer-token auth).
//!
//! Port of `packages/daemon/src/remote.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`). The phone
//! connects over the tailnet address with the bearer token **at the HTTP
//! upgrade**; terminal output travels as binary OUTPUT frames (`frames.rs`);
//! control/handshake is JSON text.
//!
//! One connection = one attached session (open N connections to view N
//! sessions). On attach: subscribe first (queueing), snapshot, send CAPS +
//! SNAPSHOT, then flush — so no output is lost across the snapshot boundary.
//!
//! ## The message surface (the exact shapes the Kotlin client parses)
//!
//! - `caps` + `snapshot` (always a pair) answer `attach`, `resize`, `scrollback`.
//! - `sessions` answers `list`, `new`, `kill`, `close`.
//! - `created` (before `sessions`) answers `new`.
//! - `owned` answers a refused/taken claim; `live` answers make/close-live.
//! - `pong` answers `ping`; `error` carries a code + message.
//! - output is `FrameType::Output` (0x10) as a binary frame.
//!
//! ## Ownership (ADR-0010)
//!
//! `byDevice` maps a device id to its live connections; a claim is released
//! after a grace on disconnect (a reconnect within the grace still holds the
//! shell). A displaced device is told `owned` and cut off. Only the holder may
//! write (`input`/`key`/`mouseclick`).
//!
//! ## Flow control
//!
//! Output is coalesced into one frame per 8 ms (or 4 KiB), the input is capped
//! at 16 KiB, keys at 32, and a connection silent for 20 s is reaped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ldmux_protocol::frames::{encode_frame, t as ft};
use ldmux_protocol::messages::PROTOCOL_VERSION;
use ldmux_protocol::net::is_tailnet_or_loopback;
use ldmux_protocol::rpc::{DeviceInfo, SessionOwner, SessionRecord};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::ws::{Incoming, Limits, WsReader, WsWriter};

use crate::daemon::now_ms;
use crate::stream::{PaneStream, StreamSubscriber, relock};

/// The two halves of an upgraded connection, over any byte stream (the tailnet
/// TCP socket and the local Unix socket share one connection loop).
type Rd = Box<dyn AsyncRead + Send + Unpin>;
type Wr = Box<dyn AsyncWrite + Send + Unpin>;
type WsRead = WsReader<Rd>;
type WsWrite = WsWriter<Wr>;

/// Control JSON counted against its connection's backlog.
///
/// **The accounting is a type, not a convention.** `queued_json` used to be a bare
/// counter that one place incremented (`send_json_text`) and the writer
/// decremented, while any call site could also `tx.send(Out::Json(..))` directly.
/// One did (the attach-backlog fallback), the writer subtracted a message nobody
/// had added, the counter wrapped below zero, and the next `send_json_text`
/// panicked on `attempt to add with overflow` — taking the connection (and a desk
/// window with it: "link lost, reconnecting") down.
///
/// Now the only way to make an `Out::Json` is [`QueuedJson::reserve`], which adds
/// the length, and the only way it is ever un-counted is its `Drop`, which
/// subtracts it exactly once: after the writer sent it, when the channel refused
/// it, and when the connection ended with it still queued. A message cannot be
/// subtracted without having been added, and cannot be left counted.
mod queued {
    use std::ops::Deref;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) struct QueuedJson {
        text: String,
        counter: Arc<AtomicUsize>,
    }

    impl QueuedJson {
        /// Count `text` against `counter`, or `None` when that would put the
        /// connection past `max` (the peer is not reading at all). Saturating, so a
        /// counter that was somehow wrong degrades instead of panicking.
        pub(super) fn reserve(
            counter: &Arc<AtomicUsize>,
            text: String,
            max: usize,
        ) -> Option<QueuedJson> {
            let n = text.len();
            let before = counter
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                    Some(c.saturating_add(n))
                })
                .unwrap_or_else(|c| c);
            let queued = QueuedJson {
                text,
                counter: counter.clone(),
            };
            // Over the cap: dropping `queued` here gives the bytes back.
            (before.saturating_add(n) <= max).then_some(queued)
        }

        pub(super) fn as_str(&self) -> &str {
            &self.text
        }
    }

    impl Deref for QueuedJson {
        type Target = str;
        fn deref(&self) -> &str {
            &self.text
        }
    }

    impl Drop for QueuedJson {
        fn drop(&mut self) {
            let n = self.text.len();
            let _ = self
                .counter
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                    Some(c.saturating_sub(n))
                });
        }
    }
}
use queued::QueuedJson;

/// One outbound WebSocket message.
enum Wire<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// The daemon handle shared with the connection tasks.
type DaemonArc = Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>;

/// Messages that change what is in a shell, and so are only the holder's to
/// send (the TS `WRITES`).
/// `resize` is one of these, not a read: a pty's size is shared by every viewer, so a read-only
/// mirror that resized would reflow the shell out from under whoever holds it. It used to be
/// neither gated nor identity-checked, which let exactly that happen.
const WRITES: [&str; 4] = ["input", "key", "mouseclick", "resize"];

/// Messages that create, end or hand out shells; they need a `hello`.
const MUTATES: [&str; 5] = ["new", "kill", "close", "makelive", "closelive"];

/// Per-type caps (the TS `MAX_INPUT_BYTES` / `MAX_KEYS`).
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_KEYS: usize = 32;
/// Output queued between subscribe and snapshot is bounded before we resync.
const MAX_PENDING_BYTES: usize = 1024 * 1024;
/// Output coalescing window, and the size that flushes it early.
const OUTPUT_FLUSH_MS: u64 = 8;
const OUTPUT_FLUSH_BYTES: usize = 4 * 1024;
/// How much queued snapshot / scrollback JSON a connection may be carrying, on top
/// of [`WS_BACKLOG_BYTES`] of output, before output is refused too. A history page
/// is legitimately large (20 000 lines is a few MiB of JSON) and live output
/// correctly queues behind it, so the allowance covers one; what it bounds is a
/// link that is not draining *anything*.
const SNAPSHOT_ALLOWANCE_BYTES: usize = 4 * 1024 * 1024;
/// A scrollback page is served at most this often, and not while the previous
/// page (or any snapshot) is still more than [`WS_BACKLOG_BYTES`] from the wire.
const SCROLLBACK_MIN_INTERVAL_MS: u64 = 200;
/// How long an in-place resync, or a deferred scrollback page, waits **without
/// the unsent backlog moving** before the connection is declared stuck and closed.
///
/// Without movement, not elapsed: a slow link that is draining is not stuck. A
/// 24 MB backlog to a phone on a poor path is minutes of honest work, and a fixed
/// allowance turned "slow" into "disconnected" — the client then reconnects,
/// rebuilds the terminal and asks again. See [`Drained`].
const DRAIN_WAIT_MS: u64 = 10_000;

/// A wait for a socket to drain, with a deadline that only runs while nothing is
/// draining.
///
/// It exists because of the difference between *slow* and *stuck*. Every caller
/// here is waiting for the same thing ("the unsent backlog is small enough to send
/// more"), and the only fact that distinguishes a patient wait from a wedged one
/// is whether the backlog is falling.
struct Drained {
    last: usize,
    deadline: tokio::time::Instant,
}

impl Drained {
    fn new() -> Self {
        Self {
            last: usize::MAX,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(DRAIN_WAIT_MS),
        }
    }

    /// Whether the wait has run out: true only when the backlog has not fallen for
    /// [`DRAIN_WAIT_MS`]. Progress extends the deadline.
    fn stuck(&mut self, backlog: usize) -> bool {
        if backlog < self.last {
            self.last = backlog;
            self.deadline =
                tokio::time::Instant::now() + std::time::Duration::from_millis(DRAIN_WAIT_MS);
            return false;
        }
        tokio::time::Instant::now() > self.deadline
    }
}
/// Unsent output on one socket past this means the link, not the terminal, is
/// behind — answer `resync` instead of building a history the client cannot show.
const WS_BACKLOG_BYTES: usize = 64 * 1024;
/// Silence that means the other end has gone.
const WS_IDLE_MS: u64 = 20_000;

/// The idle window for a **presence** connection: one that asked for `buzz` and carries no stream.
///
/// A page pings every four seconds because its picture is live and a dead socket must be noticed at
/// once. The phone's presence connection — the one that exists so a finished turn can reach it while
/// no shell page is open — says something once a minute, so the page window would reap it nineteen
/// seconds in and the phone would reconnect for ever. Two and a half beats is the same tolerance a
/// page gets.
const PRESENCE_IDLE_MS: u64 = 150_000;
/// How long a closed socket's claims wait for the same device to come back.
///
/// **Not the unbacked-claim grace.** That one answers "how long may a claim
/// stand before the window behind it attaches" (a fraction of a second) and
/// [`crate::daemon::CLAIM_GRACE_MS`] is right for it. This one answers a
/// different question — how long may a *working* connection be gone before its
/// shell is put down — and the client's own reconnect floor is the number that
/// matters: after a network change the phone spends `NETWORK_PROBE_MS` (2.5 s)
/// deciding the old socket is dead and then `RETRY_BASE_MS` (400 ms, plus
/// jitter) before re-attaching. Sharing the 3 s claim grace made a dropped link
/// indistinguishable from a deliberate release at exactly the moment the phone
/// re-attached, and a desk window waiting on the shell took it in between: the
/// phone then reported "open on desktop" for a shell nobody had taken.
const RELEASE_GRACE_MS: u64 = crate::daemon::RECONNECT_GRACE_MS;
/// How often claims are checked for a connection behind them.
///
/// Both of these reach [`Shared`] as fields from the daemon, so a test (and a
/// deployment on a slow path) can shorten them without a rebuild; these are the
/// defaults. See [`crate::daemon::CLAIM_TICK_MS`].
/// How long one write to the socket may go **unaccepted** before the peer is
/// declared gone.
///
/// A write that makes no progress (a phone on a dead link: the radio is off, the
/// NAT forgot the flow, nothing ever ACKs) used to hold the connection loop in
/// `ws.send` for good, and the loop is also what reads the idle sweep's `Close`,
/// so the connection and its claim were never released.
///
/// It is a stall allowance, not a message allowance: the writer checks it per
/// 64 KiB slice ([`crate::ws::WsWriter::send_binary_stalling`]), so a 4 MiB page on
/// a link that moves 40 KiB/s survives, and the same page on a link that has taken
/// nothing for ten seconds does not. The single-deadline form this replaced killed
/// both, which is how a slow link became a reconnect loop.
const WS_SEND_TIMEOUT_MS: u64 = 10_000;
/// Queued control JSON (snapshots, session lists) past this on one connection
/// means the peer is not reading at all: close it rather than hold it in memory.
const MAX_QUEUED_JSON_BYTES: usize = 16 * 1024 * 1024;
/// Connections (pre-auth and upgraded) the server holds at once. A tailnet has
/// one user; this only bounds what a misbehaving peer can pin.
const MAX_CONNECTIONS: usize = 64;
/// Most scrollback lines one request may ask the keeper for.
const MAX_SCROLLBACK_LINES: u64 = 20_000;

/// The remote config: the hostname (the tailnet address), the port, and the
/// bearer token.
#[derive(Clone)]
pub struct RemoteConfig {
    /// The hostname to bind (the tailnet address, e.g. `100.127.119.54`).
    pub hostname: String,
    /// The port to bind (e.g. `7623`).
    pub port: u16,
    /// The bearer token (checked at the HTTP upgrade).
    pub token: String,
    /// Reap a connection that has said nothing for this long.
    ///
    /// A field rather than only the const so the keep-alive contract can be
    /// tested in milliseconds. It is a real contract: a client that does not
    /// speak on a schedule is closed, and the desktop attach client did not, so
    /// its window exited about 20 s after it opened.
    pub idle_ms: u64,
    /// How long one socket write may take before the peer is declared gone.
    pub send_timeout_ms: u64,
}

impl RemoteConfig {
    pub fn new(hostname: impl Into<String>, port: u16, token: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            port,
            token: token.into(),
            idle_ms: WS_IDLE_MS,
            send_timeout_ms: WS_SEND_TIMEOUT_MS,
        }
    }

    /// The send deadline for tests: a real value, shortened.
    pub fn with_send_timeout_ms(mut self, ms: u64) -> Self {
        self.send_timeout_ms = ms.max(1);
        self
    }

    /// The idle deadline for tests: a real value, shortened.
    pub fn with_idle_ms(mut self, idle_ms: u64) -> Self {
        self.idle_ms = idle_ms.max(1);
        self
    }
}

/// One outbound message for a connection's writer/coalescer.
enum Out {
    /// Pane output bytes (coalesced).
    Output(Vec<u8>),
    /// A JSON control message (flushes pending output first, so it cannot
    /// overtake output it should follow). Built only through
    /// [`ConnShared::send_json_text`], which is what counts it (see [`queued`]).
    Json(QueuedJson),
    /// The pane's picture on this connection is stale (output was dropped for a
    /// slow link, or the keeper lost some): a client that advertised
    /// `inplaceResync` gets a fresh snapshot on this socket; any other gets the
    /// legacy `error`/`resync`, and reconnects. A session that has **ended** travels the
    /// same way, with the message `session ended`; there is no separate ending message.
    Resync(String),
    /// Close the connection (the sweep, or `stop`).
    Close,
}

/// What a client said it understands, in `hello.features` (or `attach.features`).
/// A client that says nothing gets exactly the messages it always got.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Features {
    /// "inplaceResync": a stale picture is repaired with a fresh `caps`+`snapshot` on
    /// the same socket.
    inplace_resync: bool,
    /// "buzz": this client understands the mark pushes, so it is sent a `marks` table on hello and
    /// a `buzz` for each new mark. A client that says nothing receives neither, which is the
    /// additive rule the whole wire follows.
    buzz: bool,
}

impl Features {
    fn from_json(v: &serde_json::Value) -> Option<Features> {
        let list = v.get("features")?.as_array()?;
        let has = |n: &str| list.iter().any(|x| x.as_str() == Some(n));
        Some(Features {
            inplace_resync: has("inplaceResync"),
            buzz: has("buzz"),
        })
    }
}

/// Which snapshot a connection is being sent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnapKind {
    /// The visible screen: an attach, a resize, or the repair of a stale picture. A
    /// client paints all three the same way, so they are one kind.
    Screen,
    /// A scrollback page (`history: true`).
    History,
}

/// The connection's mutable state, shared between the reader task and the
/// output callback (which runs on the stream's poll thread).
struct ConnInner {
    disposed: bool,
    /// Whether live output is flowing (false between subscribe and the snapshot).
    live: bool,
    /// Incremented per attach; a superseded attach must not commit.
    attach_gen: u64,
    session_id: Option<String>,
    record: Option<SessionRecord>,
    stream: Option<Arc<PaneStream>>,
    unsubscribe: Option<Box<dyn Fn() + Send + Sync>>,
    /// This connection's subscriber id on `stream` (so a snapshot can leave it
    /// out of the output tail it already contains).
    sub_id: Option<usize>,
    device: Option<DeviceInfo>,
    /// Output queued before `live` (bounded by `MAX_PENDING_BYTES`).
    pending: Vec<u8>,
    pending_bytes: usize,
    resyncing: bool,
    /// Monotonic: a wall-clock step must not reap a healthy connection.
    last_seen: std::time::Instant,
    features: Features,
    /// An in-place resync is waiting for the link to drain (one at a time).
    resync_task: bool,
    /// When the last scrollback page was served, and the newest request that is
    /// waiting its turn (only the newest matters: it is where the user is now).
    last_scrollback: Option<std::time::Instant>,
    deferred_scrollback: Option<u64>,
    scrollback_task: bool,
}

impl ConnInner {
    fn new() -> Self {
        Self {
            disposed: false,
            live: false,
            attach_gen: 0,
            session_id: None,
            record: None,
            stream: None,
            unsubscribe: None,
            sub_id: None,
            device: None,
            pending: Vec::new(),
            pending_bytes: 0,
            resyncing: false,
            last_seen: std::time::Instant::now(),
            features: Features::default(),
            resync_task: false,
            last_scrollback: None,
            deferred_scrollback: None,
            scrollback_task: false,
        }
    }
}

/// A live connection: the outbound channel, the shared state, and the count of
/// bytes produced but not yet written to the wire (the backlog probe).
struct ConnShared {
    tx: tokio::sync::mpsc::UnboundedSender<Out>,
    inner: Mutex<ConnInner>,
    outstanding: AtomicUsize,
    /// Bytes of control JSON queued and not yet written (the snapshot / list
    /// side of the backlog, which `outstanding` does not count). Only
    /// [`QueuedJson`] changes it.
    queued_json: Arc<AtomicUsize>,
}

impl ConnShared {
    fn with_inner(tx: tokio::sync::mpsc::UnboundedSender<Out>, inner: ConnInner) -> Self {
        Self {
            tx,
            inner: Mutex::new(inner),
            outstanding: AtomicUsize::new(0),
            queued_json: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn send_json(&self, value: &serde_json::Value) {
        self.send_json_text(value.to_string());
    }

    /// Queue one JSON text for the writer, bounded: a peer that has stopped
    /// reading is closed once [`MAX_QUEUED_JSON_BYTES`] are waiting for it. The
    /// one way a control message enters the channel.
    ///
    /// A message the channel refuses (the writer is gone) comes back inside the
    /// `SendError` and is dropped there, which un-counts it. One still queued when
    /// the connection ends is dropped with the channel, so nothing is left counted.
    fn send_json_text(&self, text: String) {
        match QueuedJson::reserve(&self.queued_json, text, MAX_QUEUED_JSON_BYTES) {
            Some(json) => {
                let _ = self.tx.send(Out::Json(json));
            }
            None => {
                let _ = self.tx.send(Out::Close);
            }
        }
    }

    /// Everything produced for this socket and not yet written: pane output and
    /// control JSON (snapshots, scrollback pages, lists) together.
    fn backlog_bytes(&self) -> usize {
        self.outstanding
            .load(Ordering::Relaxed)
            .saturating_add(self.queued_json.load(Ordering::Relaxed))
    }

    fn send_error(&self, code: &str, message: &str) {
        self.send_json(&serde_json::json!({
            "type": "error", "code": code, "message": message
        }));
    }
}

/// The server-shared state: the connections table + the daemon + the token.
pub(crate) struct Shared {
    conns: Mutex<HashMap<u64, Arc<ConnShared>>>,
    daemon: DaemonArc,
    token: String,
    idle_ms: u64,
    send_timeout_ms: u64,
    /// How long a closed socket's claims wait for the same device to come back
    /// ([`RELEASE_GRACE_MS`]).
    release_grace_ms: u64,
    /// How often the claim sweep runs ([`CLAIM_TICK_MS`]).
    claim_tick_ms: u64,
    stopped: AtomicBool,
    /// One permit per connection being served (see [`MAX_CONNECTIONS`]).
    slots: Arc<tokio::sync::Semaphore>,
    /// Connection ids, across every listener (the tailnet socket and the local
    /// one share the `conns` table, so their ids must not collide).
    next_conn: std::sync::atomic::AtomicU64,
    /// The claim/idle sweep, which walks `conns`.
    sweep: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Shared {
    /// The connection table and its sweep, for listeners to serve into. Built once
    /// per daemon so that every way in (the tailnet WebSocket, the local attach
    /// socket) is one population: a claim is held by a connection from either,
    /// and the sweep that frees unbacked claims has to see both.
    pub(crate) fn new(
        daemon: DaemonArc,
        token: String,
        idle_ms: u64,
        send_timeout_ms: u64,
        release_grace_ms: u64,
        claim_tick_ms: u64,
    ) -> Arc<Shared> {
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            daemon,
            token,
            idle_ms,
            send_timeout_ms,
            release_grace_ms,
            claim_tick_ms,
            stopped: AtomicBool::new(false),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            sweep: Mutex::new(None),
        });
        let sweep_shared = shared.clone();
        let sweep = tokio::spawn(async move {
            // Two cadences, because they answer to different things. Unbacked
            // claims are checked on their own tick — a window that is on screen
            // waiting for one is the reader, and `idle_ms / 4` is five seconds of
            // it. Reaping silent connections can stay as slow as the deadline it
            // is against, and the two share a task because both walk the same
            // connection list.
            let sweep_step = (sweep_shared.idle_ms / 4).max(25);
            let mut since_sweep = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(sweep_shared.claim_tick_ms))
                    .await;
                if sweep_shared.stopped.load(Ordering::Relaxed) {
                    break;
                }
                // Each pass on its own task (see `daemon::survive`): this loop is
                // what frees claims nobody holds and reaps silent connections.
                let pass = sweep_shared.clone();
                crate::daemon::survive(
                    "claim reconcile",
                    async move { reconcile_claims(&pass).await },
                )
                .await;
                since_sweep = since_sweep.saturating_add(sweep_shared.claim_tick_ms);
                if since_sweep >= sweep_step {
                    since_sweep = 0;
                    let pass = sweep_shared.clone();
                    crate::daemon::survive("idle sweep", async move { sweep_once(&pass).await })
                        .await;
                }
            }
        });
        *relock(&shared.sweep) = Some(sweep);
        shared
    }

    /// Stop serving: mark stopped, tell every connection to close, end the sweep.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        for c in relock(&self.conns).values() {
            let _ = c.tx.send(Out::Close);
        }
        if let Some(h) = relock(&self.sweep).take() {
            h.abort();
        }
    }
}

/// The remote server: the phone-facing WebSocket endpoint.
pub struct RemoteServer {
    cfg: RemoteConfig,
    /// The port actually bound, which is `cfg.port` unless that was 0.
    bound_port: u16,
    shared: Option<Arc<Shared>>,
    /// The daemon built `shared` (and stops it); otherwise this server did.
    shared_is_external: bool,
    accept: Option<tokio::task::JoinHandle<()>>,
}

impl RemoteServer {
    pub fn new(cfg: RemoteConfig) -> Self {
        Self {
            bound_port: cfg.port,
            cfg,
            shared: None,
            shared_is_external: false,
            accept: None,
        }
    }

    /// Serve into a connection table the daemon owns (shared with the local
    /// attach socket) instead of making one.
    pub(crate) fn with_shared(mut self, shared: Arc<Shared>) -> Self {
        self.shared = Some(shared);
        self.shared_is_external = true;
        self
    }

    /// Bind the WS server on the tailnet address and begin serving.
    pub async fn start(&mut self, daemon_arc: &DaemonArc) -> Result<(String, u16), String> {
        let addr = format!("{}:{}", self.cfg.hostname, self.cfg.port);
        // Refused here, at the one place that listens (the TS `start` check).
        if !is_tailnet_or_loopback(&self.cfg.hostname) {
            return Err(format!(
                "refusing to listen on {}: not a tailnet or loopback address",
                self.cfg.hostname
            ));
        }
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        // The port the kernel actually gave us. A configured port of 0 means
        // "any free one", so the configured number is not the answer to where
        // this is listening — and reporting 0 to a pairing client would send the
        // phone nowhere. It is also what lets a test ask for a port nobody else
        // can have, rather than picking a number and hoping.
        self.bound_port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(self.cfg.port);
        let shared = match &self.shared {
            Some(s) => s.clone(),
            None => Shared::new(
                daemon_arc.clone(),
                self.cfg.token.clone(),
                self.cfg.idle_ms,
                self.cfg.send_timeout_ms,
                RELEASE_GRACE_MS,
                crate::daemon::CLAIM_TICK_MS,
            ),
        };
        let accept_shared = shared.clone();
        let accept = tokio::spawn(async move {
            accept_loop(listener, accept_shared).await;
        });
        self.shared = Some(shared);
        self.accept = Some(accept);
        Ok((self.cfg.hostname.clone(), self.bound_port))
    }

    /// The port this server is listening on.
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    /// The remote's live state, for tests.
    pub fn is_listening(&self) -> bool {
        self.shared.is_some()
    }

    pub fn stop(&mut self) {
        // A table the daemon owns outlives this listener (the local attach socket
        // still serves into it); one this server made goes with it.
        if !self.shared_is_external
            && let Some(shared) = &self.shared
        {
            shared.stop();
        }
        if let Some(h) = self.accept.take() {
            h.abort();
        }
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The accept loop: auth at the upgrade, then one task per connection.
async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        if shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        let (mut stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => {
                if shared.stopped.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
        };
        // Before the token: a peer that is not on the tailnet is not a client,
        // however it reached this socket (the TS upgrade handler).
        if !is_tailnet_or_loopback(&peer.ip().to_string()) {
            write_http(&mut stream, 403, "forbidden\n").await;
            continue;
        }
        // A bound on what peers can hold at once, pre-auth included: past it the
        // newcomer is turned away, not queued.
        let Ok(permit) = shared.slots.clone().try_acquire_owned() else {
            eprintln!("ldmuxd: remote: {peer} refused: {MAX_CONNECTIONS} connections already open");
            write_http(&mut stream, 503, "too many connections\n").await;
            continue;
        };
        // Keystroke latency (Nagle) and dead-peer detection (keepalive): see
        // [`tune_socket`].
        tune_socket(&stream);
        let conn_id = shared.next_conn.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        // Everything past the cheap peer check runs on its own task. The head
        // read can block for its full deadline, and doing the pre-auth work
        // inline in the accept loop let one silent peer stall every client
        // behind it for that whole deadline.
        tokio::spawn(async move {
            serve_accepted(stream, peer, shared, conn_id).await;
            drop(permit);
        });
    }
}

/// Serve the local attach socket: the same WebSocket protocol, the same
/// connection loop, over a Unix socket the desk attach client uses so a window
/// needs neither the tailnet nor the token.
///
/// What stands in for the bearer token is the kernel: the socket is 0600 in the
/// private state directory *and* every connection's peer credentials
/// (`SO_PEERCRED`) must carry this daemon's uid, so a stray permission or a
/// handed-over descriptor still cannot let another user in.
pub(crate) fn serve_local(
    listener: tokio::net::UnixListener,
    shared: Arc<Shared>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let me = nix::unistd::getuid().as_raw();
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    if shared.stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    eprintln!("ldmuxd: local attach accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            match stream.peer_cred() {
                Ok(c) if c.uid() == me => {}
                other => {
                    eprintln!(
                        "ldmuxd: local attach: refused a peer that is not uid {me}: {other:?}"
                    );
                    continue;
                }
            }
            let Ok(permit) = shared.slots.clone().try_acquire_owned() else {
                eprintln!(
                    "ldmuxd: local attach refused: {MAX_CONNECTIONS} connections already open"
                );
                continue;
            };
            let conn_id = shared.next_conn.fetch_add(1, Ordering::Relaxed);
            let shared = shared.clone();
            tokio::spawn(async move {
                serve_local_conn(stream, shared, conn_id).await;
                drop(permit);
            });
        }
    })
}

async fn serve_local_conn(mut stream: tokio::net::UnixStream, shared: Arc<Shared>, conn_id: u64) {
    let Some((head, leftover)) =
        crate::ws::read_head(&mut stream, std::time::Duration::from_secs(5)).await
    else {
        return;
    };
    let Some((method, target, headers)) = parse_request_head(&head) else {
        return;
    };
    if !is_ws_upgrade(&headers) || path_of(&target) != "/ws" {
        return;
    }
    let Some(up) = parse_upgrade(&method, &headers) else {
        return;
    };
    serve_upgrade(stream, leftover, up, shared, conn_id).await;
}

/// Socket options for an accepted client: `TCP_NODELAY` (a keystroke is a few
/// bytes and must not wait for Nagle), and keepalive probing so the kernel
/// notices a peer that vanished without a FIN (the phone's radio went off).
/// Best effort: a failure is logged and the connection is served anyway, since
/// the idle sweep and the send deadline still bound a dead peer.
fn tune_socket(stream: &TcpStream) {
    use std::os::fd::AsRawFd;
    if let Err(e) = stream.set_nodelay(true) {
        eprintln!("ldmuxd: remote: set TCP_NODELAY: {e}");
    }
    let fd = stream.as_raw_fd();
    let set = |level: i32, name: i32, value: i32| -> bool {
        // SAFETY: `fd` is a live socket owned by `stream` for the duration of the
        // call, and the option value is a plain `c_int` of the size passed.
        unsafe {
            nix::libc::setsockopt(
                fd,
                level,
                name,
                &value as *const i32 as *const nix::libc::c_void,
                std::mem::size_of::<i32>() as nix::libc::socklen_t,
            ) == 0
        }
    };
    let ok = set(nix::libc::SOL_SOCKET, nix::libc::SO_KEEPALIVE, 1)
        // Probe after 15 s idle, every 5 s, give up after 3 misses (~30 s).
        && set(nix::libc::IPPROTO_TCP, nix::libc::TCP_KEEPIDLE, 15)
        && set(nix::libc::IPPROTO_TCP, nix::libc::TCP_KEEPINTVL, 5)
        && set(nix::libc::IPPROTO_TCP, nix::libc::TCP_KEEPCNT, 3);
    if !ok {
        eprintln!(
            "ldmuxd: remote: TCP keepalive could not be set: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// The pre-auth and upgrade half of a connection, on its own task.
///
/// Returns without ceremony when the peer is not a client: an unanswered head,
/// a plain GET, a wrong path, a bad token, or a failed upgrade.
async fn serve_accepted(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    shared: Arc<Shared>,
    conn_id: u64,
) {
    // Read the request head so a plain HTTP GET is answered rather than left to
    // the upgrade to fail — the phone's `probe()` reads the status to tell a
    // refused token (401) from a machine that is off. The bytes are consumed:
    // the upgrade below is ours, not a library's (see `crate::ws`).
    let Some((head, leftover)) =
        crate::ws::read_head(&mut stream, std::time::Duration::from_secs(5)).await
    else {
        return;
    };
    let Some((_method, target, headers)) = parse_request_head(&head) else {
        write_http(&mut stream, 400, "bad request\n").await;
        return;
    };
    let path = path_of(&target);
    let authed = authorized(&headers, &shared.token);
    if !is_ws_upgrade(&headers) {
        // The TS `handleRequest` statuses.
        let (status, body) = plain_response(&path, authed);
        eprintln!("ldmuxd: remote: {peer} {_method} {path} -> {status}");
        write_http(&mut stream, status, body).await;
        return;
    }
    if path != "/ws" {
        eprintln!("ldmuxd: remote: {peer} ws {path} -> 404");
        write_http(&mut stream, 404, "ldmux remote: the endpoint is /ws\n").await;
        return;
    }
    if !authed {
        eprintln!("ldmuxd: remote: {peer} ws /ws -> 401");
        write_http(&mut stream, 401, "unauthorized\n").await;
        return;
    }
    let Some(up) = parse_upgrade(&_method, &headers) else {
        eprintln!("ldmuxd: remote: upgrade from {peer} is not a valid WebSocket handshake");
        write_http(&mut stream, 400, "bad websocket handshake\n").await;
        return;
    };
    eprintln!(
        "ldmuxd: remote: connection from {peer}{}",
        if up.deflate.is_some() {
            " (permessage-deflate)"
        } else {
            ""
        }
    );
    serve_upgrade(stream, leftover, up, shared, conn_id).await;
}

/// Answer the handshake on any stream and run the connection on it.
///
/// The inbound limits: the TypeScript server was `new WebSocketServer({
/// maxPayload: 64 * 1024 })`, which closed the socket on anything larger; the
/// port lost it and inherited tungstenite's 64 MiB default. 64 KiB is far above
/// every client message (`input` is capped at 16 KiB by the handler, `key` at 32
/// names), and it bounds a compressed message *after* inflation too. The limit is
/// on reads only, so server→client snapshots are unaffected however large.
pub(crate) async fn serve_upgrade<S>(
    mut stream: S,
    leftover: Vec<u8>,
    up: crate::ws::Upgrade,
    shared: Arc<Shared>,
    conn_id: u64,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    if stream
        .write_all(crate::ws::upgrade_response(&up).as_bytes())
        .await
        .is_err()
        || stream.flush().await.is_err()
    {
        return;
    }
    let params = up.deflate.map(|(p, _)| p);
    let (rd, wr) = tokio::io::split(stream);
    let rd = WsReader::new(
        Box::new(rd) as Rd,
        leftover,
        Limits {
            max_frame: 64 * 1024,
            max_message: 64 * 1024,
        },
        params,
    );
    let wr = WsWriter::new(Box::new(wr) as Wr, params);
    handle_connection(rd, wr, shared, conn_id).await;
}

/// The handshake fields a WebSocket upgrade must carry, and the extension offer
/// (`None` when the request is not a well-formed RFC 6455 handshake).
pub(crate) fn parse_upgrade(
    method: &str,
    headers: &[(String, String)],
) -> Option<crate::ws::Upgrade> {
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    if get("sec-websocket-version")? != "13" {
        return None;
    }
    let key = get("sec-websocket-key")?.trim().to_string();
    if key.is_empty() {
        return None;
    }
    let offers: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "sec-websocket-extensions")
        .map(|(_, v)| v.as_str())
        .collect();
    Some(crate::ws::Upgrade {
        key,
        deflate: crate::ws::negotiate(&offers),
    })
}

/// The per-connection loop: read messages, enqueue responses, coalesce output.
async fn handle_connection(rd: WsRead, wr: WsWrite, shared: Arc<Shared>, conn_id: u64) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
    let conn = Arc::new(ConnShared::with_inner(tx, ConnInner::new()));
    relock(&shared.conns).insert(conn_id, conn.clone());

    // The loop runs on its own task so a panic in a handler ends only that
    // connection, and the cleanup below still runs (the claim is released, the
    // subscription dropped) instead of both dying with the unwound task.
    let looped = tokio::spawn(connection_loop(rd, wr, shared.clone(), conn.clone(), rx));
    if let Err(e) = looped.await {
        eprintln!("ldmuxd: remote: connection {conn_id} task ended abnormally: {e}");
    }

    // The connection ended. Clean up: unsubscribe, release the stream, and
    // release the device's claims after the grace.
    disconnect(conn, shared).await;
}

/// The connection loop proper: read messages, write responses and output.
async fn connection_loop(
    mut rd: WsRead,
    mut ws: WsWrite,
    shared: Arc<Shared>,
    conn: Arc<ConnShared>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Out>,
) {
    let send_limit = std::time::Duration::from_millis(shared.send_timeout_ms);
    // The connection loop owns the socket; outbound goes through `tx` so the
    // coalescer and every control message share one ordering.
    let mut pending: Vec<u8> = Vec::new();
    let mut last_flush = tokio::time::Instant::now();
    // Every write is bounded by `send_limit`; a write that does not finish means
    // the peer is gone, and the connection ends (so its claim can be released)
    // instead of waiting here for an ACK that will never come.
    'conn: loop {
        // Arm the coalescing timer only when there is output to flush.
        let mut timer: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            if pending.is_empty() {
                Box::pin(std::future::pending())
            } else {
                Box::pin(tokio::time::sleep(std::time::Duration::from_millis(
                    OUTPUT_FLUSH_MS,
                )))
            };
        tokio::select! {
            maybe = rd.next() => {
                match maybe {
                    Ok(None) => break,
                    Err(e) => {
                        // A protocol violation is owed its close code; a dead
                        // transport is owed nothing.
                        if e.io.is_none() {
                            let _ = tokio::time::timeout(send_limit, ws.send_close(e.code)).await;
                        }
                        break;
                    }
                    Ok(Some(msg)) => {
                        // Flush buffered output before acting on the message, so a
                        // reply cannot overtake the output it should follow.
                        if !flush_output(&mut ws, &mut pending, &conn, send_limit, &mut last_flush).await {
                            break 'conn;
                        }
                        match msg {
                            Incoming::Text(text) => {
                                touch(&conn);
                                handle_message(&text, &conn, &shared).await;
                            }
                            Incoming::Binary(b) => {
                                touch(&conn);
                                handle_message(&String::from_utf8_lossy(&b), &conn, &shared).await;
                            }
                            Incoming::Ping(p) => {
                                let sent = tokio::time::timeout(send_limit, ws.send_pong(&p)).await;
                                if !matches!(sent, Ok(Ok(()))) {
                                    break 'conn;
                                }
                            }
                            Incoming::Pong => {}
                            Incoming::Close => {
                                let _ = tokio::time::timeout(send_limit, ws.send_close(1000)).await;
                                break 'conn;
                            }
                        }
                    }
                }
            }
            out = rx.recv() => {
                match out {
                    None => break,
                    Some(Out::Close) => break,
                    Some(Out::Resync(reason)) => {
                        if relock(&conn.inner).features.inplace_resync {
                            tokio::spawn(resync_in_place(conn.clone()));
                        } else {
                            conn.send_error("resync", &reason);
                        }
                    }
                    Some(Out::Output(b)) => {
                        // Leading edge: output that arrives after a quiet spell goes
                        // out now (a keystroke's echo must not wait for the batch
                        // timer); what follows within the window is coalesced.
                        let quiet = pending.is_empty()
                            && last_flush.elapsed() >= std::time::Duration::from_millis(OUTPUT_FLUSH_MS);
                        pending.extend_from_slice(&b);
                        if (quiet || pending.len() >= OUTPUT_FLUSH_BYTES)
                            && !flush_output(&mut ws, &mut pending, &conn, send_limit, &mut last_flush).await
                        {
                            break 'conn;
                        }
                    }
                    Some(Out::Json(json)) => {
                        let sent = flush_output(&mut ws, &mut pending, &conn, send_limit, &mut last_flush).await
                            && send_bounded(&mut ws, Wire::Text(json.as_str()), send_limit).await;
                        // Counted until it has left (or failed to leave) the daemon.
                        drop(json);
                        if !sent {
                            break 'conn;
                        }
                    }
                }
            }
            _ = &mut timer => {
                if !flush_output(&mut ws, &mut pending, &conn, send_limit, &mut last_flush).await {
                    break 'conn;
                }
            }
        }
    }
}

/// Give `n` bytes back to a connection's `outstanding` count, saturating at zero.
///
/// The count is a backlog probe read on every output chunk, and `backlog_bytes` adds
/// it to `queued_json`: a count that wrapped below zero would be a panic there (the
/// failure that took a connection down for `queued_json`), so a mis-accounting here
/// degrades to "no backlog" instead.
fn release_outstanding(outstanding: &AtomicUsize, n: usize) {
    let _ = outstanding.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
        Some(c.saturating_sub(n))
    });
}

/// Send every buffered output frame, then release those bytes from the
/// connection's `outstanding` count (they have left the daemon).
///
/// Returns `false` when the peer did not take the write within `limit` (or the
/// socket is broken): the connection is over.
async fn flush_output(
    ws: &mut WsWrite,
    pending: &mut Vec<u8>,
    conn: &ConnShared,
    limit: std::time::Duration,
    last_flush: &mut tokio::time::Instant,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    *last_flush = tokio::time::Instant::now();
    let data = std::mem::take(pending);
    let n = data.len();
    let ok = match encode_frame(ft::OUTPUT, &data, 0) {
        Ok(frame) => send_bounded(ws, Wire::Binary(&frame), limit).await,
        Err(_) => {
            send_bounded(
                ws,
                Wire::Text(
                    &serde_json::json!({"type":"error","code":"resync","message":"output frame too large"})
                        .to_string(),
                ),
                limit,
            )
            .await
        }
    };
    // Only now are the bytes off the daemon: a client that is not reading holds
    // this await open, and `outstanding` must keep counting them meanwhile.
    release_outstanding(&conn.outstanding, n);
    ok
}

/// One socket write (compression included). `false` is a stall or a socket error.
///
/// `limit` is **how long one slice may go unaccepted**, not how long the message may
/// take: the writer checks it per 64 KiB. A single deadline for the whole message
/// killed a link that was draining a large snapshot slowly, which turned "slow" into
/// "disconnected" and made the reconnect re-request the same page — a livelock, since
/// the retry could never be faster than the first attempt. A peer that has taken
/// nothing for `limit` is still cut off, so a dead link costs no more than before.
async fn send_bounded(ws: &mut WsWrite, msg: Wire<'_>, limit: std::time::Duration) -> bool {
    let sent = match msg {
        Wire::Text(t) => ws.send_text_stalling(t, limit).await,
        Wire::Binary(b) => ws.send_binary_stalling(b, limit).await,
    };
    matches!(sent, Ok(()))
}

/// Mark the connection seen (any inbound message counts).
fn touch(conn: &ConnShared) {
    relock(&conn.inner).last_seen = std::time::Instant::now();
}

/// Hand pane output to the connection's writer, or — when the link is behind —
/// refuse it and ask the client to resync (the TS `queueOutput` backlog rule).
///
/// `outstanding` is the bytes queued for this socket and not yet written. Past
/// `WS_BACKLOG_BYTES` the client is not keeping up, and building a history it
/// cannot display only grows memory: the queue is unbounded, so without this a
/// phone on a bad link made the daemon buffer a busy shell's whole output. It is
/// said once per backlog (`resyncing`), because a client that is already
/// re-attaching must not be told again on every write; a fresh `attach` clears
/// the flag and its snapshot is the recovery.
fn queue_output(conn: &ConnShared, inner: &mut ConnInner, bytes: &[u8]) {
    if inner.resyncing {
        return;
    }
    let outstanding = conn.outstanding.load(Ordering::Relaxed);
    if outstanding > WS_BACKLOG_BYTES
        || conn.backlog_bytes() > WS_BACKLOG_BYTES + SNAPSHOT_ALLOWANCE_BYTES
    {
        inner.resyncing = true;
        let _ = conn
            .tx
            .send(Out::Resync("client is behind the link".into()));
        return;
    }
    conn.outstanding
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
            Some(c.saturating_add(bytes.len()))
        })
        .ok();
    if conn.tx.send(Out::Output(bytes.to_vec())).is_err() {
        // The writer is gone: nothing will ever drain this.
        release_outstanding(&conn.outstanding, bytes.len());
    }
}

/// Output that arrives between subscribe and the snapshot is held, bounded. Past
/// [`MAX_PENDING_BYTES`] it is dropped: the snapshot this attach is about to send
/// repaints everything dropped here, so only a client that cannot take a repaint on
/// the same socket (no `inplaceResync`: the desk window, until it said so) is told
/// to start over.
///
/// A function of its own because it is a place that once wrote to the channel
/// directly. The notice goes through [`ConnShared::send_json`] like every other
/// control message, which does not touch `inner` (the caller is holding it).
fn hold_until_live(conn: &ConnShared, inner: &mut ConnInner, bytes: &[u8]) {
    inner.pending.extend_from_slice(bytes);
    inner.pending_bytes = inner.pending_bytes.saturating_add(bytes.len());
    if inner.pending_bytes > MAX_PENDING_BYTES {
        inner.pending.clear();
        inner.pending_bytes = 0;
        if !inner.features.inplace_resync {
            conn.send_json(&serde_json::json!({
                "type":"error","code":"resync",
                "message":"output backlog exceeded while attaching"
            }));
        }
    }
}

/// Handle one control message (the TS `onMessage` dispatch).
async fn handle_message(text: &str, conn: &Arc<ConnShared>, shared: &Arc<Shared>) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => {
            conn.send_error("bad_json", "invalid JSON");
            return;
        }
    };
    let mtype = v
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    // Writing to a shell is the holder's (the TS `WRITES` / `mayWrite`).
    //
    // A connection that attached without a `hello` is a read-only observer (the
    // desktop mirror, `ldmuxd attach --readonly`): it holds nothing and so can
    // type nothing. The check used to be skipped for it, which made "no identity"
    // the way around ADR-0010. Not attached at all falls through to each
    // handler's own `not_attached`.
    if WRITES.contains(&mtype.as_str()) {
        let (device, session) = {
            let inner = relock(&conn.inner);
            (inner.device.clone(), inner.session_id.clone())
        };
        if let Some(session) = session {
            let Some(device) = device else {
                conn.send_error("hello_required", "say hello before writing to a shell");
                return;
            };
            let holds = {
                let mut d = shared.daemon.lock().await;
                let holds = d.holds(&session, &device.id);
                // **Typing is what makes a claim count as in use.** A claim is otherwise a lock
                // held for as long as a window is open, and nine idle desk windows refused every
                // phone attach until the operator pressed `take it back` on each. `resize` is a
                // write but not a sign of presence: every viewer resizes on reconnect, including
                // the one the phone just displaced.
                if holds && mtype != "resize" {
                    d.touch_claim(&session, &device.id);
                }
                holds
            };
            if !holds {
                conn.send_error("not_holder", "this device does not hold that shell");
                return;
            }
        }
    }

    // Changing the set of shells is the same kind of act and needs an identity
    // too, and a shell another device holds is not ours to end or hand to the
    // desk without a takeover (ADR-0010: no forced detach without consent).
    if MUTATES.contains(&mtype.as_str()) {
        let Some(device) = device_of(conn) else {
            conn.send_error("hello_required", "say hello before changing shells");
            return;
        };
        if matches!(mtype.as_str(), "kill" | "makelive") {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("");
            if held_by_another(shared, id, &device.id).await {
                conn.send_error("not_holder", "another device holds that shell");
                return;
            }
        }
    }

    match mtype.as_str() {
        "ping" => {
            let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
            conn.send_json(&serde_json::json!({"type":"pong","id":id}));
        }
        "hello" => {
            if let Some(f) = Features::from_json(&v) {
                relock(&conn.inner).features = f;
            }
            // Identity, not authentication (ADR-0010).
            let dev = v.get("device");
            if let Some(dev) = dev {
                let id = dev.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if !id.is_empty() {
                    let id = id.chars().take(64).collect::<String>();
                    let name = dev
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("device")
                        .chars()
                        .take(32)
                        .collect::<String>();
                    let kind = if dev.get("kind").and_then(|x| x.as_str()) == Some("desktop") {
                        "desktop"
                    } else {
                        "phone"
                    };
                    let previous = relock(&conn.inner).device.replace(DeviceInfo {
                        id: id.clone(),
                        name: if name.is_empty() {
                            "device".into()
                        } else {
                            name
                        },
                        kind: kind.into(),
                    });
                    // A second `hello` under a different id means this socket is
                    // no longer the device it was. If no other socket still is,
                    // that identity's claims have no holder and nothing else
                    // would free them (they would block everyone else until the
                    // daemon restarts).
                    if let Some(prev) = previous.filter(|p| p.id != id) {
                        let still_held = relock(&shared.conns).values().any(|c| {
                            relock(&c.inner)
                                .device
                                .as_ref()
                                .map(|d| d.id == prev.id)
                                .unwrap_or(false)
                        });
                        if !still_held {
                            let freed = !shared
                                .daemon
                                .lock()
                                .await
                                .release_device(&prev.id)
                                .is_empty();
                            if freed {
                                broadcast_sessions(shared).await;
                            }
                        }
                    }
                }
            }
            // **The table a device missed.** A phone that has just identified itself is handed the
            // marks that are true now, which is how a buzz made while it was away reaches it. Sent
            // only to a client that asked for buzzes, so an old client sees nothing new.
            if relock(&conn.inner).features.buzz {
                send_marks(conn, shared).await;
            }
        }
        "list" => {
            let sessions = {
                let mut d = shared.daemon.lock().await;
                d.describe_sessions().await
            };
            conn.send_json(&sessions_message(&sessions));
        }
        "desktop" => {
            let inv = {
                let mut d = shared.daemon.lock().await;
                d.desktop_inventory().await
            };
            conn.send_json(&serde_json::json!({"type":"desktop","inventory":inv}));
        }
        "new" => {
            // A session is a shell. A client that still sends `kind` is not refused; the
            // field is simply not read.
            let cwd = v.get("cwd").and_then(|x| x.as_str());
            let title = v.get("title").and_then(|x| x.as_str());
            // Who opened it: what the client says, else who the client is.
            //
            // This was the literal `"phone"` for **every** remote client, so a
            // desktop that connected over the wire had its shells recorded as the
            // phone's — and `createdBy` is not decoration: the launcher shows its
            // per-shell menu (close, send to desktop) only for a shell it opened,
            // and `seizing_closes_the_desk_window` is true only for `"phone"`. A
            // desktop client's shells were therefore offered to the phone as its
            // own. The connection already said what it was (`hello`), so the
            // record now follows the connection: a client that sends `createdBy`
            // still wins, and everything else falls back to its device kind.
            let origin = v
                .get("createdBy")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    relock(&conn.inner)
                        .device
                        .as_ref()
                        .map(|d| d.kind.clone())
                        .filter(|k| !k.is_empty())
                })
                .unwrap_or_else(|| "local".to_string());
            let record = {
                let mut d = shared.daemon.lock().await;
                let fallback = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                d.create_session(cwd.unwrap_or(&fallback), title, &origin)
                    .await
            };
            match record {
                Ok(record) => {
                    // Before the list, so a client that opens what it asked for
                    // has the id by the time the list arrives.
                    conn.send_json(&serde_json::json!({
                        "type": "created", "sessionId": record.id, "cwd": record.cwd
                    }));
                    // The list is news for every client, not just the one that
                    // asked: a shell opened from the desk CLI has to appear on the
                    // phone, and a phone that opened one has to appear on the
                    // other phone. This used to go to the asking connection
                    // alone, so everyone else waited for the next maintenance tick
                    // (2 s) — and only then if some mutator had remembered to set
                    // `sessions_dirty`.
                    broadcast_sessions(shared).await;
                }
                Err(e) => conn.send_error("handler_error", &e),
            }
        }
        "makelive" => {
            let id = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let workspace = v.get("workspace").and_then(|x| x.as_str()).unwrap_or("");
            let result = {
                let mut d = shared.daemon.lock().await;
                d.handle(
                    "makeLive",
                    serde_json::json!({"id": id, "workspace": workspace}),
                )
                .await
            };
            let (ok, detail) = live_result(result);
            conn.send_json(&serde_json::json!({
                "type": "live", "sessionId": id, "ok": ok, "detail": detail
            }));
            if ok {
                broadcast_sessions(shared).await;
            }
        }
        "closelive" => {
            let id = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let result = {
                let mut d = shared.daemon.lock().await;
                d.handle("closeLive", serde_json::json!({"id": id})).await
            };
            let (ok, detail) = live_result(result);
            conn.send_json(&serde_json::json!({
                "type": "live", "sessionId": id, "ok": ok, "detail": detail
            }));
            broadcast_sessions(shared).await;
        }
        "kill" => {
            let id = v.get("sessionId").and_then(|x| x.as_str()).unwrap_or("");
            let reason = ended_by("killed", device_of(conn).as_ref());
            let killed = {
                let mut d = shared.daemon.lock().await;
                d.kill_session(id, &reason).await
            };
            if let Err(e) = killed {
                conn.send_error("kill_failed", &e);
            }
            broadcast_sessions(shared).await;
        }
        "close" => {
            let id = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let device = relock(&conn.inner).device.clone();
            // A shell opened at the desk belongs to the desk: closing it from a
            // phone is putting it down, not destroying it (ADR-0010).
            let record = {
                let d = shared.daemon.lock().await;
                d.get_session(&id)
            };
            let phone_owned_closed = device
                .as_ref()
                .map(|dev| dev.kind == "phone")
                .unwrap_or(false)
                && record
                    .as_ref()
                    .map(|r| r.created_by != "phone")
                    .unwrap_or(false);
            if phone_owned_closed {
                if let Some(dev) = &device {
                    let mut d = shared.daemon.lock().await;
                    d.release_claim(&id, &dev.id);
                }
                stop_viewing(conn, shared).await;
                broadcast_sessions(shared).await;
                return;
            }
            // Ending it is the holder's call; putting it down is handled above.
            if let Some(dev) = &device
                && held_by_another(shared, &id, &dev.id).await
            {
                conn.send_error("not_holder", "another device holds that shell");
                return;
            }
            let reason = ended_by("closed", device_of(conn).as_ref());
            let closed = {
                let mut d = shared.daemon.lock().await;
                d.close_session(&id, &reason).await
            };
            let ended = match closed {
                Ok((ended, _)) => ended,
                Err(e) => {
                    // Not "ended": that would release a claim and detach the
                    // viewer for a shell that is still there.
                    conn.send_error("close_failed", &e);
                    broadcast_sessions(shared).await;
                    return;
                }
            };
            if let Some(dev) = &device
                && !ended
            {
                let mut d = shared.daemon.lock().await;
                d.release_claim(&id, &dev.id);
            }
            stop_viewing(conn, shared).await;
            broadcast_sessions(shared).await;
        }
        // The device is leaving this shell on purpose: a page was closed, or the app went
        // away. A socket that closes and a socket that dies look identical on the wire, so
        // only the client can say which happened, and everyone else waits out
        // `RECONNECT_GRACE_MS` when it cannot. Releasing here is what lets the desk window
        // that was waiting on this shell take it back at once instead of fifteen seconds
        // later.
        //
        // `sessionId` omitted means every claim this device holds. A release from a device
        // that does not hold the shell is a no-op, not an error: a phone that was already
        // displaced still says goodbye to what it thought it had.
        "release" => {
            let device = relock(&conn.inner).device.clone();
            let id = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(device) = device {
                let freed = {
                    let mut d = shared.daemon.lock().await;
                    if id.is_empty() {
                        !d.release_device(&device.id).is_empty()
                    } else {
                        d.release_claim(&id, &device.id)
                    }
                };
                if freed {
                    broadcast_sessions(shared).await;
                }
            }
        }
        "attach" => {
            let session_id = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let takeover = v.get("takeover").and_then(|x| x.as_bool()).unwrap_or(false);
            let cols = v.get("cols").and_then(|x| x.as_u64());
            let rows = v.get("rows").and_then(|x| x.as_u64());
            if let Some(f) = Features::from_json(&v) {
                relock(&conn.inner).features = f;
            }
            attach(conn, shared, session_id, takeover, cols, rows).await;
        }
        "resize" => {
            let (cols, rows, gen_id) = {
                let inner = relock(&conn.inner);
                (
                    v.get("cols").and_then(|x| x.as_u64()),
                    v.get("rows").and_then(|x| x.as_u64()),
                    inner.attach_gen,
                )
            };
            let (Some(cols), Some(rows)) = (cols, rows) else {
                conn.send_error("bad_size", "invalid size");
                return;
            };
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before resizing");
                return;
            };
            if on_stream(&stream, move |s| s.resize(cols, rows))
                .await
                .unwrap_or_else(|| Err("resize task failed".to_string()))
                .is_err()
            {
                conn.send_error("resize_failed", "resize failed");
                return;
            }
            // The pane geometry changed: answer with a fresh snapshot.
            let sid = relock(&conn.inner).session_id.clone().unwrap_or_default();
            let sub = relock(&conn.inner).sub_id;
            if let Err(e) =
                snapshot_and_go_live(conn, &stream, sub, None, gen_id, &sid, SnapKind::Screen).await
            {
                conn.send_error("resize_failed", &e);
            }
        }
        "mouseclick" => {
            let col = v.get("col").and_then(|x| x.as_u64());
            let row = v.get("row").and_then(|x| x.as_u64());
            let (Some(col), Some(row)) = (col, row) else {
                conn.send_error("bad_cell", "invalid cell");
                return;
            };
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before clicking");
                return;
            };
            match on_stream(&stream, move |s| s.snapshot_full())
                .await
                .unwrap_or_else(|| Err("snapshot task failed".to_string()))
            {
                Ok(state) => {
                    if !(state.mouse && state.mouse_sgr) {
                        conn.send_error("no_mouse", "pane has not enabled SGR mouse reporting");
                        return;
                    }
                    if crate::desktop::is_shell_foreground(&state.foreground) {
                        conn.send_error(
                            "shell_foreground",
                            &format!(
                                "pane is at a shell prompt ({}), not a mouse-aware program",
                                state.foreground
                            ),
                        );
                        return;
                    }
                    if col >= state.cols || row >= state.rows {
                        conn.send_error("bad_cell", "cell outside the pane");
                        return;
                    }
                    match on_stream(&stream, move |s| s.mouse_click(col, row)).await {
                        Some(Ok(_)) => conn.send_json(&serde_json::json!({
                            "type": "mousedelivered", "col": col, "row": row
                        })),
                        Some(Err(e)) => conn.send_error("input_failed", &e),
                        None => conn.send_error("input_failed", "click task failed"),
                    }
                }
                Err(e) => conn.send_error("handler_error", &e),
            }
        }
        "input" => {
            let text = v
                .get("data")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if text.len() > MAX_INPUT_BYTES {
                conn.send_error(
                    "input_too_large",
                    &format!("input exceeds {MAX_INPUT_BYTES} bytes"),
                );
                return;
            }
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before typing");
                return;
            };
            match on_stream(&stream, move |s| s.write(&text)).await {
                Some(Ok(_)) => {}
                Some(Err(e)) => conn.send_error("input_failed", &e),
                None => conn.send_error("input_failed", "input task failed"),
            }
        }
        "key" => {
            let keys: Vec<String> = v
                .get("keys")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|k| k.as_str())
                        .filter(|k| !k.is_empty())
                        .take(MAX_KEYS)
                        .map(|k| k.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let stream = active_stream(conn);
            let Some(stream) = stream else {
                conn.send_error("not_attached", "attach before sending keys");
                return;
            };
            // A name the table refuses is reported, not swallowed: the client
            // logs the code, and nothing reaches the shell.
            let sent = on_stream(&stream, move |s| {
                let refs: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
                s.send_key(&refs)
            })
            .await;
            if let Some(Err(e)) = sent {
                conn.send_error("bad_key", &e);
            }
        }
        "scrollback" => {
            // The keeper is asked for exactly this many rows of history: clamp it
            // to what a client can page through, not whatever a message says.
            let lines = v
                .get("lines")
                .and_then(|x| x.as_u64())
                .unwrap_or(0)
                .min(MAX_SCROLLBACK_LINES);
            if active_stream(conn).is_none() {
                conn.send_error("not_attached", "attach before requesting scrollback");
                return;
            }
            // A page is a big message. One that arrives while the last is still
            // on its way is held (only the newest, which is where the user is now)
            // until the link has drained, rather than stacked behind it.
            let now = std::time::Instant::now();
            let serve_now = {
                let mut inner = relock(&conn.inner);
                let paced = inner.last_scrollback.is_none_or(|t| {
                    now.duration_since(t).as_millis() as u64 >= SCROLLBACK_MIN_INTERVAL_MS
                });
                if paced && !inner.scrollback_task && conn.backlog_bytes() <= WS_BACKLOG_BYTES {
                    inner.last_scrollback = Some(now);
                    true
                } else {
                    inner.deferred_scrollback = Some(lines);
                    if !inner.scrollback_task {
                        inner.scrollback_task = true;
                        tokio::spawn(deferred_scrollback(conn.clone()));
                    }
                    false
                }
            };
            if serve_now {
                serve_scrollback(conn, lines).await;
            }
        }
        other => {
            conn.send_error("bad_message", &format!("unknown type: {other}"));
        }
    }
}

/// Answer one `scrollback` request (`lines == 0` is the return-to-bottom repaint).
async fn serve_scrollback(conn: &Arc<ConnShared>, lines: u64) {
    let wants_history = lines > 0;
    let (gen_id, sid, sub) = {
        let inner = relock(&conn.inner);
        (
            inner.attach_gen,
            inner.session_id.clone().unwrap_or_default(),
            inner.sub_id,
        )
    };
    let Some(stream) = active_stream(conn) else {
        conn.send_error("not_attached", "attach before requesting scrollback");
        return;
    };
    let hist = if wants_history { Some(lines) } else { None };
    let kind = if wants_history {
        SnapKind::History
    } else {
        SnapKind::Screen
    };
    if let Err(e) = snapshot_and_go_live(conn, &stream, sub, hist, gen_id, &sid, kind).await {
        conn.send_error("scrollback_failed", &e);
    }
}

/// Wait for the link to drain and the pacing interval to pass, then serve the
/// newest scrollback request that arrived meanwhile. A link that never drains
/// ends the connection (the send deadline would have anyway).
async fn deferred_scrollback(conn: Arc<ConnShared>) {
    let mut drained = Drained::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let backlog = conn.backlog_bytes();
        let ready = {
            let mut inner = relock(&conn.inner);
            if inner.disposed {
                inner.scrollback_task = false;
                return;
            }
            let paced = inner
                .last_scrollback
                .is_none_or(|t| t.elapsed().as_millis() as u64 >= SCROLLBACK_MIN_INTERVAL_MS);
            if paced && conn.backlog_bytes() <= WS_BACKLOG_BYTES {
                inner.last_scrollback = Some(std::time::Instant::now());
                inner.scrollback_task = false;
                Some(inner.deferred_scrollback.take())
            } else {
                None
            }
        };
        match ready {
            Some(Some(lines)) => return serve_scrollback(&conn, lines).await,
            Some(None) => return,
            None if drained.stuck(backlog) => {
                relock(&conn.inner).scrollback_task = false;
                let _ = conn.tx.send(Out::Close);
                return;
            }
            None => {}
        }
    }
}

/// Repair a stale picture on the same socket: wait until the link has drained
/// (so the snapshot is not queued behind what it replaces, and a slow link is
/// not handed a new burst every time it falls behind), then send a fresh
/// `caps`+`snapshot` pair. The client paints a snapshot the same way whether it
/// arrived for an attach or a repair; the `resync` marker this used to carry had no
/// reader on either client.
async fn resync_in_place(conn: Arc<ConnShared>) {
    {
        let mut inner = relock(&conn.inner);
        if inner.resync_task {
            return;
        }
        inner.resync_task = true;
    }
    let done = |conn: &Arc<ConnShared>| relock(&conn.inner).resync_task = false;
    let mut drained = Drained::new();
    while conn.backlog_bytes() > WS_BACKLOG_BYTES / 2 {
        if relock(&conn.inner).disposed {
            return done(&conn);
        }
        if drained.stuck(conn.backlog_bytes()) {
            let _ = conn.tx.send(Out::Close);
            return done(&conn);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (stream, gen_id, sid, sub) = {
        let inner = relock(&conn.inner);
        (
            inner.stream.clone(),
            inner.attach_gen,
            inner.session_id.clone(),
            inner.sub_id,
        )
    };
    if let (Some(stream), Some(sid)) = (stream, sid)
        && let Err(e) =
            snapshot_and_go_live(&conn, &stream, sub, None, gen_id, &sid, SnapKind::Screen).await
    {
        conn.send_error("resync_failed", &e);
        // `resyncing` is still set, and while it is `queue_output` drops every byte
        // of live output: left like this the client is told once and then shows a
        // frozen picture over a link that still answers pings. Ending the
        // connection is the recovery every client already has (it reconnects and
        // attaches, which clears the flag with a fresh snapshot).
        let _ = conn.tx.send(Out::Close);
    }
    done(&conn);
}

/// The `attach` handler: claim, subscribe, snapshot, CAPS + SNAPSHOT, flush.
async fn attach(
    conn: &Arc<ConnShared>,
    shared: &Arc<Shared>,
    session_id: String,
    takeover: bool,
    cols: Option<u64>,
    rows: Option<u64>,
) {
    // Resolve the record + claim under the daemon lock; remember any displaced
    // device to notify after the lock is released.
    let (record, displaced, stream, device) = {
        let mut d = shared.daemon.lock().await;
        let Some(record) = d.get_session(&session_id) else {
            drop(d);
            conn.send_error(
                "unknown_session",
                &format!("no terminal-backed session: {session_id}"),
            );
            return;
        };
        if record.keeper_name.is_empty() {
            drop(d);
            conn.send_error(
                "unknown_session",
                &format!("no terminal-backed session: {session_id}"),
            );
            return;
        }
        if record.status != "running" {
            // A session that has ended is refused, naming the ending and its reason. The
            // watcher of a session learns that it ended from `error`/`resync` (`session
            // ended`), which every client already handles — there is no separate ending
            // message to understand.
            drop(d);
            conn.send_error(
                "session_dead",
                &format!("session is {}: {session_id}", record.status),
            );
            return;
        }
        let device = relock(&conn.inner).device.clone();
        let mut displaced = None;
        if let Some(device) = &device {
            match d.claim(&session_id, device, takeover) {
                crate::ownership::ClaimResult::Acquired { previous, .. } => {
                    // This claim has a connection behind it, so it is the
                    // device's to keep while it reconnects
                    // (`RECONNECT_GRACE_MS`), not a window's unbacked claim.
                    d.mark_backed(&session_id, &device.id);
                    // The mark for this shell is cleared by the claim itself, so both routes get
                    // one rule instead of two.
                    displaced = previous;
                }
                crate::ownership::ClaimResult::HeldBy { device: holder } => {
                    drop(d);
                    conn.send_json(&serde_json::json!({
                        "type": "owned",
                        "sessionId": session_id,
                        "owner": SessionOwner {
                            id: holder.id, name: holder.name, kind: holder.kind,
                            since: now_ms(),
                        }
                    }));
                    return;
                }
            }
        }
        let keeper_name = record.keeper_name.clone();
        let stream = d.stream_for(&keeper_name).clone();
        (record, displaced, stream, device)
    };

    // A connection that attaches somewhere else has stopped watching where it
    // was, so its claim on that shell goes with it. Otherwise a client that moves
    // between shells over one socket (nothing in the protocol forbids it — the
    // phone happens to use one socket per shell) holds its last one for good:
    // nobody can take it without a takeover, and a desk window it displaced sits
    // on "Press r to take it back" for a shell no device is actually looking at.
    //
    // Per *device*, not per connection: a device with two live sockets on the
    // same shell keeps it, because one of them is still watching.
    {
        let previous = relock(&conn.inner).session_id.clone();
        if let (Some(device), Some(previous)) = (&device, previous)
            && previous != session_id
        {
            let still_watched = {
                let conns: Vec<Arc<ConnShared>> = {
                    let map = relock(&shared.conns);
                    map.values().cloned().collect()
                };
                conns.iter().any(|c| {
                    // Not this one: it is the connection that is moving, and its
                    // `session_id` is only updated below.
                    if Arc::ptr_eq(c, conn) {
                        return false;
                    }
                    let inner = relock(&c.inner);
                    inner.session_id.as_deref() == Some(previous.as_str())
                        && inner
                            .device
                            .as_ref()
                            .map(|d| d.id == device.id)
                            .unwrap_or(false)
                })
            };
            if !still_watched {
                let freed = {
                    let mut d = shared.daemon.lock().await;
                    d.release_claim(&previous, &device.id)
                };
                if freed {
                    broadcast_sessions(shared).await;
                }
            }
        }
    }

    // Bump the generation, clear the old subscription.
    let (gen_id, old_unsub) = {
        let mut inner = relock(&conn.inner);
        inner.attach_gen += 1;
        let gen_id = inner.attach_gen;
        // Taken out and called below, **after** the guard is gone: unsubscribing
        // takes the stream's subscriber lock, and the stream's thread holds that
        // lock while it calls back into this connection's lock.
        let old_unsub = inner.unsubscribe.take();
        inner.stream = None;
        inner.session_id = Some(session_id.clone());
        inner.record = Some(record.clone());
        inner.live = false;
        inner.resyncing = false;
        inner.pending.clear();
        inner.pending_bytes = 0;
        (gen_id, old_unsub)
    };
    if let Some(unsub) = old_unsub {
        unsub();
    }

    // Subscribe (queueing output until `live`).
    let inner_for_cb = conn.clone();
    let cb_gen = gen_id;
    let sub = StreamSubscriber {
        on_output: Box::new(move |bytes: &[u8]| {
            let mut inner = relock(&inner_for_cb.inner);
            if inner.attach_gen != cb_gen {
                return;
            }
            if inner.live {
                queue_output(&inner_for_cb, &mut inner, bytes);
            } else {
                hold_until_live(&inner_for_cb, &mut inner, bytes);
            }
        }),
        on_reset: Box::new({
            let conn2 = conn.clone();
            move |reason: &str| {
                // Not while attaching: the snapshot that is about to be sent
                // repaints from scratch, which is all a resync asks for.
                let mut inner = relock(&conn2.inner);
                if !(inner.attach_gen == cb_gen && inner.live) {
                    return;
                }
                match reason {
                    // The keeper dropped output: the picture is stale. Output is
                    // held back until the repaint (once, however often it is said).
                    crate::stream::RESET_LOST => {
                        if !inner.features.inplace_resync {
                            let _ = conn2.tx.send(Out::Resync(reason.to_string()));
                        } else if !inner.resyncing {
                            inner.resyncing = true;
                            let _ = conn2.tx.send(Out::Resync(reason.to_string()));
                        }
                    }
                    other => {
                        drop(inner);
                        conn2.send_error("resync", other);
                    }
                }
            }
        }),
    };
    let (sub_id, unsubscribe) = stream.subscribe_owned(sub);
    let leftover = {
        let mut inner = relock(&conn.inner);
        if inner.disposed || inner.attach_gen != gen_id {
            // The connection ended (or attached elsewhere) while we were
            // subscribing: nothing else will ever call this unsubscribe, so it
            // is called here, after the guard is gone.
            Some(unsubscribe)
        } else {
            inner.unsubscribe = Some(unsubscribe);
            inner.sub_id = Some(sub_id);
            inner.stream = Some(stream.clone());
            None
        }
    };
    if let Some(unsubscribe) = leftover {
        unsubscribe();
        shared
            .daemon
            .lock()
            .await
            .release_stream_if_idle(&record.keeper_name);
        return;
    }
    // The stream was handed out under the daemon lock and subscribed to after it
    // was released. In between, another connection of the same shell can have
    // closed (and `release_stream_if_idle` dropped) it, because at that moment
    // nobody was subscribed — and a subscriber on a closed stream is sent a
    // snapshot and then nothing, for good, with the link looking healthy. Once this
    // connection is subscribed the stream cannot be dropped as idle, so one check
    // here closes the window. The client's answer to `resync` is a fresh attach,
    // which gets a live stream.
    if stream.is_closed() {
        stop_viewing(conn, shared).await;
        conn.send_error("resync", "the stream ended while attaching");
        // The claim stays: this device took the shell and its re-attach is the
        // same device, which is `Acquired`; one that never comes back is an unbacked
        // claim and is freed after the grace. The device it displaced is still owed
        // its notice.
        if let Some(prev) = displaced {
            notify_displaced(shared, &prev.id, &session_id).await;
        }
        return;
    }

    // Resize before the snapshot so the screen we send already matches the grid
    // the client is about to paint.
    if let (Some(c), Some(r)) = (cols, rows) {
        let _ = on_stream(&stream, move |s| s.resize(c, r)).await;
    }
    // The snapshot, cut from live output at one byte boundary. The fence runs
    // with the stream's poll held off: whatever was queued while attaching is
    // already in the snapshot (dropped), the snapshot goes out first, and
    // everything after it is new output.
    let fenced = snapshot_and_go_live(
        conn,
        &stream,
        Some(sub_id),
        None,
        gen_id,
        &session_id,
        SnapKind::Screen,
    )
    .await;

    // A second attach (or a disconnect) may have superseded us while we awaited.
    if relock(&conn.inner).attach_gen != gen_id {
        return;
    }

    if let Err(e) = fenced {
        // Tear the half-made attach down and give the claim back: left alone,
        // this connection holds a shell it is not looking at and the next attach
        // to it (by this very device) is the one refused — the Stale State
        // lockout ADR-0010 exists to prevent.
        stop_viewing(conn, shared).await;
        let freed = match &device {
            Some(dev) => shared
                .daemon
                .lock()
                .await
                .release_claim(&session_id, &dev.id),
            None => false,
        };
        conn.send_error("attach_failed", &e);
        if freed {
            broadcast_sessions(shared).await;
        }
        return;
    }

    // Tell a displaced device (and cut it off).
    if let Some(prev) = displaced {
        notify_displaced(shared, &prev.id, &session_id).await;
    }
}

/// Take a snapshot for this connection and, at the fence, send it and switch the
/// connection to live output. The superseded check happens under the fence too,
/// so a stale attach sends nothing. `history` is `Some(n)` for a scrollback page.
async fn snapshot_and_go_live(
    conn: &Arc<ConnShared>,
    stream: &Arc<PaneStream>,
    requester: Option<usize>,
    history: Option<u64>,
    gen_id: u64,
    session_id: &str,
    kind: SnapKind,
) -> Result<(), String> {
    let conn2 = conn.clone();
    let sid = session_id.to_string();
    on_stream(stream, move |s| {
        s.snapshot_fenced(requester, history, |snap, epoch| {
            {
                let mut inner = relock(&conn2.inner);
                if inner.disposed || inner.attach_gen != gen_id {
                    return;
                }
                inner.pending.clear();
                inner.pending_bytes = 0;
                inner.live = true;
                // Whatever was dropped is in this snapshot.
                inner.resyncing = false;
            }
            send_state(&conn2, &sid, snap, epoch, kind);
        })
    })
    .await
    .unwrap_or_else(|| Err("snapshot task failed".to_string()))
    .map(|_| ())
}

/// Notify every connection of `device_id` that it no longer holds `session_id`,
/// and cut it off (the TS `notifyDisplaced`).
async fn notify_displaced(shared: &Arc<Shared>, device_id: &str, session_id: &str) {
    // Name the device that has it now, and leave a device alone if it has the
    // shell back already.
    //
    // `owner: null` used to be the whole message, and the phone turned null into
    // the words "another device" (`TerminalController.kt`): a made-up device, in
    // a notice that is supposed to say who to ask. The daemon knows the holder
    // here; the claim is the same one the takeover just wrote.
    //
    // The re-check is for the interleaving two takeovers allow: each attach
    // calls this *after* it has released the daemon lock and gone live, so a
    // device that was displaced and has since taken the shell back must not be
    // cut off by the older takeover's late notice.
    let (still_held, holder) = {
        let d = shared.daemon.lock().await;
        let held = d.holds(session_id, device_id);
        let holder = d.ownership().owner(session_id).map(|c| SessionOwner {
            id: c.device.id.clone(),
            name: c.device.name.clone(),
            kind: c.device.kind.clone(),
            since: c.since.max(0) as u64,
        });
        (held, holder)
    };
    if still_held {
        return;
    }
    let conns: Vec<Arc<ConnShared>> = {
        let map = relock(&shared.conns);
        map.values().cloned().collect()
    };
    for c in conns {
        let (is_holder, _sid) = {
            let inner = relock(&c.inner);
            (
                inner
                    .device
                    .as_ref()
                    .map(|d| d.id == device_id)
                    .unwrap_or(false),
                inner.session_id.clone(),
            )
        };
        if !is_holder {
            continue;
        }
        let viewing = { relock(&c.inner).session_id.as_deref() == Some(session_id) };
        if !viewing {
            continue;
        }
        c.send_json(&serde_json::json!({
            "type": "owned",
            "sessionId": session_id,
            "owner": holder,
        }));
        // Cut it off: unsubscribe and forget the stream.
        let (keeper_name, unsub) = {
            let mut inner = relock(&c.inner);
            let unsub = inner.unsubscribe.take();
            inner.stream = None;
            inner.session_id = None;
            inner.live = false;
            (inner.record.as_ref().map(|r| r.keeper_name.clone()), unsub)
        };
        if let Some(unsub) = unsub {
            unsub();
        }
        if let Some(keeper_name) = keeper_name {
            let mut d = shared.daemon.lock().await;
            d.release_stream_if_idle(&keeper_name);
        }
    }
}

/// Send the session list to one connection.
/// Push the session list to every connected client.
/// Push the marks made since the last tick to every client that asked for them.
///
/// Separate from [`broadcast_sessions`] because a mark is not a session fact (it is not in the
/// session list at all) and because draining the queue is only correct for the connections that can
/// read it: one client without the feature must not consume the others' news.
pub(crate) async fn broadcast_buzzes(shared: &Arc<Shared>) {
    let marks = {
        let mut d = shared.daemon.lock().await;
        d.prune_marks();
        d.take_buzzes()
    };
    if marks.is_empty() {
        return;
    }
    let conns: Vec<Arc<ConnShared>> = {
        let map = relock(&shared.conns);
        map.values().cloned().collect()
    };
    for c in conns {
        if !relock(&c.inner).features.buzz {
            continue;
        }
        for mark in &marks {
            c.send_json(&serde_json::json!({"type":"buzz", "mark": mark.to_json()}));
        }
    }
}

/// The whole mark table, sent to a client that has just said `hello` and asked for buzzes.
///
/// This is the delivery model in one function: a phone that was away for an hour is handed what is
/// true now rather than a replayed stream, and the app draws it beside the rows.
async fn send_marks(conn: &Arc<ConnShared>, shared: &Arc<Shared>) {
    let marks = {
        let d = shared.daemon.lock().await;
        d.mark_snapshot()
    };
    conn.send_json(&serde_json::json!({
        "type": "marks",
        "marks": marks.iter().map(crate::buzz::Mark::to_json).collect::<Vec<_>>(),
    }));
}

pub(crate) async fn broadcast_sessions(shared: &Arc<Shared>) {
    let sessions = {
        let mut d = shared.daemon.lock().await;
        d.describe_sessions().await
    };
    let msg = sessions_message(&sessions).to_string();
    let conns: Vec<Arc<ConnShared>> = {
        let map = relock(&shared.conns);
        map.values().cloned().collect()
    };
    for c in conns {
        c.send_json_text(msg.clone());
    }
}

fn sessions_message(sessions: &[ldmux_protocol::rpc::SessionInfo]) -> serde_json::Value {
    serde_json::json!({"type":"sessions","sessions": sessions})
}

/// The `caps` + `snapshot` pair (the TS `sendState`).
fn send_state(
    conn: &Arc<ConnShared>,
    session_id: &str,
    snap: &ldmux_protocol::keeper::SnapshotResult,
    epoch: u64,
    kind: SnapKind,
) {
    conn.send_json(&serde_json::json!({
        "type": "caps",
        "protocolVersion": PROTOCOL_VERSION,
        "sessionId": session_id,
        "cols": snap.cols,
        "rows": snap.rows,
        "snapshotEpoch": epoch,
        "capabilities": {"scrollback": true, "shiftKeys": true},
    }));
    let mut snapshot = serde_json::json!({
        "type": "snapshot",
        "sessionId": session_id,
        "cols": snap.cols,
        "rows": snap.rows,
        "screen": snap.text,
        "cursorX": snap.cursor_x,
        "cursorY": snap.cursor_y,
        "alternateOn": snap.alternate_on,
        "bracketedPaste": snap.bracketed_paste,
        "mouse": snap.mouse,
        "mouseSgr": snap.mouse_sgr,
        "appCursor": snap.app_cursor,
        "historySize": snap.history_size,
        "snapshotEpoch": epoch,
    });
    match kind {
        SnapKind::History => snapshot["history"] = serde_json::Value::Bool(true),
        SnapKind::Screen => {}
    }
    conn.send_json(&snapshot);
}

/// Run a `PaneStream` call on the blocking pool. The stream's keeper connection
/// is a **blocking** socket with no read timeout, so calling it directly from an
/// async task would let a slow or wedged keeper stall the executor (and, under
/// the daemon lock, every other RPC).
async fn on_stream<T, F>(stream: &Arc<PaneStream>, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<PaneStream>) -> T + Send + 'static,
{
    let s = Arc::clone(stream);
    tokio::task::spawn_blocking(move || f(s)).await.ok()
}

/// The connection's active stream (cloned).
fn active_stream(conn: &Arc<ConnShared>) -> Option<Arc<PaneStream>> {
    relock(&conn.inner).stream.clone()
}

/// Stop viewing the currently-attached session (a `close`/`kill` path).
async fn stop_viewing(conn: &Arc<ConnShared>, shared: &Arc<Shared>) {
    let (keeper_name, unsub) = {
        let mut inner = relock(&conn.inner);
        let unsub = inner.unsubscribe.take();
        inner.stream = None;
        inner.session_id = None;
        inner.live = false;
        (inner.record.take().map(|r| r.keeper_name), unsub)
    };
    if let Some(unsub) = unsub {
        unsub();
    }
    if let Some(keeper_name) = keeper_name {
        let mut d = shared.daemon.lock().await;
        d.release_stream_if_idle(&keeper_name);
    }
}

/// The connection ended: unsubscribe, release the stream, release claims.
async fn disconnect(conn: Arc<ConnShared>, shared: Arc<Shared>) {
    let (keeper_name, device, session, unsub) = {
        let mut inner = relock(&conn.inner);
        inner.disposed = true;
        let unsub = inner.unsubscribe.take();
        (
            inner.record.take().map(|r| r.keeper_name),
            inner.device.clone(),
            inner.session_id.clone(),
            unsub,
        )
    };
    if let Some(unsub) = unsub {
        unsub();
    }
    relock(&shared.conns).remove_by_ptr(&conn);
    if let Some(keeper_name) = keeper_name {
        let mut d = shared.daemon.lock().await;
        d.release_stream_if_idle(&keeper_name);
    }
    if let Some(device) = device {
        release_after_grace(&shared, device.id, session).await;
    }
}

/// Release a closed socket's claims after the grace (a reconnect within it
/// still holds; one that does not lets go).
async fn release_after_grace(shared: &Arc<Shared>, device_id: String, session_id: Option<String>) {
    let shared = shared.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(shared.release_grace_ms)).await;
        let (device_has_conns, still_viewing) = {
            let map = relock(&shared.conns);
            let has = map.values().any(|c| {
                relock(&c.inner)
                    .device
                    .as_ref()
                    .map(|d| d.id == device_id)
                    .unwrap_or(false)
            });
            let viewing = map.values().any(|c| {
                let inner = relock(&c.inner);
                inner
                    .device
                    .as_ref()
                    .map(|d| d.id == device_id)
                    .unwrap_or(false)
                    && session_id.is_some()
                    && inner.session_id == session_id
            });
            (has, viewing)
        };
        let mut freed = false;
        if !device_has_conns {
            let mut d = shared.daemon.lock().await;
            freed = !d.release_device(&device_id).is_empty();
        } else if let Some(sid) = &session_id
            && !still_viewing
        {
            let mut d = shared.daemon.lock().await;
            freed = d.release_claim(sid, &device_id);
        }
        if freed {
            broadcast_sessions(&shared).await;
        }
    });
}

/// Free claims with no connection behind them (see
/// [`crate::daemon::LdmuxDaemon::release_unbacked_claims`]), and tell every
/// client, because who holds a shell is on the wire.
///
/// It runs on the sweep's own tick rather than the maintenance pass: it is
/// about connections, and the sweep already walks them.
async fn reconcile_claims(shared: &Arc<Shared>) {
    let attached: std::collections::HashSet<(String, String)> = {
        let map = relock(&shared.conns);
        map.values()
            .filter_map(|c| {
                let inner = relock(&c.inner);
                let device = inner.device.as_ref()?.id.clone();
                let session = inner.session_id.clone()?;
                Some((session, device))
            })
            .collect()
    };
    let freed = {
        let mut d = shared.daemon.lock().await;
        let now = d.mono_ms();
        d.release_unbacked_claims(&attached, now)
    };
    if !freed.is_empty() {
        broadcast_sessions(shared).await;
    }
    displace_viewers(shared).await;
}

/// Reap connections silent for the idle deadline (the TS `sweep`).
async fn sweep_once(shared: &Arc<Shared>) {
    let now = std::time::Instant::now();
    let conns: Vec<Arc<ConnShared>> = {
        let map = relock(&shared.conns);
        map.values().cloned().collect()
    };
    for c in conns {
        let (idle, disposed) = {
            let inner = relock(&c.inner);
            (
                now.saturating_duration_since(inner.last_seen),
                inner.disposed,
            )
        };
        if disposed {
            continue;
        }
        // **A presence connection is not held to a page's cadence.** It asked for `buzz` and no
        // stream, so it is the phone's background link: one beat a minute, and an idle window that
        // fits it. Everything else keeps the four-second rule its picture needs.
        let window = if relock(&c.inner).features.buzz {
            PRESENCE_IDLE_MS
        } else {
            shared.idle_ms
        };
        if idle.as_millis() as u64 > window {
            let _ = c.tx.send(Out::Close);
        }
    }
}

/// **Tell every connection watching a shell it no longer holds, whoever claimed it.**
///
/// The claim handlers in this file tell the displaced window themselves, but a claim can arrive
/// without touching them: the mesh bridge claims through the daemon's `reclaim` verb for a phone
/// over the radio, and the CLI does the same. Those claims left the desk window painting live,
/// resizing the pty under the phone's grid and showing no notice, so the shell looked taken with
/// no way back (reported 2026-11-05). This runs on the claim tick, beside the check that frees
/// unbacked claims — the two answer the same question, "who holds this now".
async fn displace_viewers(shared: &Arc<Shared>) {
    let held: Vec<(String, SessionOwner)> = {
        let d = shared.daemon.lock().await;
        d.ownership()
            .export()
            .into_iter()
            .map(|(id, claim)| {
                (
                    id,
                    SessionOwner {
                        id: claim.device.id.clone(),
                        name: claim.device.name.clone(),
                        kind: claim.device.kind.clone(),
                        since: claim.since.max(0) as u64,
                    },
                )
            })
            .collect()
    };
    if held.is_empty() {
        return;
    }
    let conns: Vec<Arc<ConnShared>> = {
        let map = relock(&shared.conns);
        map.values().cloned().collect()
    };
    for c in conns {
        let (viewing, device, disposed) = {
            let inner = relock(&c.inner);
            (
                inner.session_id.clone(),
                inner.device.as_ref().map(|d| d.id.clone()),
                inner.disposed,
            )
        };
        if disposed {
            continue;
        }
        let Some(session_id) = viewing else { continue };
        let Some((_, holder)) = held.iter().find(|(id, _)| *id == session_id) else {
            continue;
        };
        if device.as_deref() == Some(holder.id.as_str()) {
            continue;
        }
        c.send_json(&serde_json::json!({
            "type": "owned",
            "sessionId": session_id,
            "owner": holder,
        }));
        // Cut it off, exactly as the claim path does: a displaced window that kept its stream kept
        // painting the phone's screen at the phone's size.
        let (keeper_name, unsub) = {
            let mut inner = relock(&c.inner);
            let unsub = inner.unsubscribe.take();
            inner.stream = None;
            inner.session_id = None;
            inner.live = false;
            (inner.record.as_ref().map(|r| r.keeper_name.clone()), unsub)
        };
        if let Some(unsub) = unsub {
            unsub();
        }
        if let Some(keeper_name) = keeper_name {
            let mut d = shared.daemon.lock().await;
            d.release_stream_if_idle(&keeper_name);
        }
    }
}

/// The `live` result fields (ok + detail) out of a `handle` JSON value.
fn live_result(result: Result<serde_json::Value, String>) -> (bool, Option<String>) {
    match result {
        Ok(v) => (
            v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
            v.get("detail")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        ),
        Err(e) => (false, Some(e)),
    }
}

/// Whether some device other than `device_id` holds `session_id`.
async fn held_by_another(shared: &Arc<Shared>, session_id: &str, device_id: &str) -> bool {
    let d = shared.daemon.lock().await;
    d.ownership()
        .owner(session_id)
        .is_some_and(|c| c.device.id != device_id)
}

/// How an ending reads when the device behind it is known: `killed from motorola
/// edge 2024`. A connection that announced no device (a bare socket, the CLI over
/// RPC) leaves the bare verb — a reason that guessed at a device would be worse
/// than one that does not name it.
fn ended_by(verb: &str, device: Option<&ldmux_protocol::rpc::DeviceInfo>) -> String {
    match device {
        Some(dev) if !dev.name.is_empty() => format!("{verb} from {}", dev.name),
        _ => verb.to_string(),
    }
}

/// The device behind a connection, for [`ended_by`], without holding the lock.
fn device_of(conn: &ConnShared) -> Option<ldmux_protocol::rpc::DeviceInfo> {
    relock(&conn.inner).device.clone()
}

// ── HTTP head parsing / responses ───────────────────────────────────────────

/// A parsed request head: `(method, path, headers)`.
type ParsedHead = (String, String, Vec<(String, String)>);

/// Parse the request line + headers from a head. Header names are lowercased.
fn parse_request_head(head: &str) -> Option<ParsedHead> {
    let mut lines = head.split("\r\n");
    let request = lines.next()?;
    let mut parts = request.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Some((method, target, headers))
}

/// The path of a request target (query removed).
fn path_of(target: &str) -> String {
    target.split('?').next().unwrap_or("/").to_string()
}

/// Whether the request is a WebSocket upgrade.
fn is_ws_upgrade(headers: &[(String, String)]) -> bool {
    let connection = headers
        .iter()
        .filter(|(k, _)| k == "connection")
        .map(|(_, v)| v.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(",");
    let upgrade = headers
        .iter()
        .find(|(k, _)| k == "upgrade")
        .map(|(_, v)| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    connection.split(',').any(|p| p.trim() == "upgrade") && upgrade
}

/// Whether the request carries the bearer token in its `Authorization` header.
///
/// **The header is the only place a token is read.** `?token=` used to be accepted too, for a
/// client that predates the header. No shipped client sends it (the phone and the desk attach
/// client both use the header), and a bearer credential in a URL reaches places a header does
/// not: proxy logs, crash reports, shell history. The probe and the upgrade read the same
/// header, so refusing the query costs nothing a user can observe.
fn authorized(headers: &[(String, String)], token: &str) -> bool {
    let provided = headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| {
            // **The auth scheme is case-insensitive** (RFC 7235 §2.1), so `bearer <token>` is a
            // legal header and used to be answered 401 by a client that spelled it that way.
            let (scheme, rest) = v.trim().split_once(' ').unwrap_or((v.trim(), ""));
            if scheme.eq_ignore_ascii_case("bearer") {
                rest.trim().to_string()
            } else {
                String::new()
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    crate::auth::token_matches(token, &provided)
}

/// The plain-HTTP status + body for a non-upgrade request (the TS `handleRequest`).
fn plain_response(path: &str, authed: bool) -> (u16, &'static str) {
    if path == "/ws" {
        if !authed {
            (401, "unauthorized\n")
        } else {
            (400, "upgrade required\n")
        }
    } else {
        (404, "ldmux remote: the endpoint is /ws\n")
    }
}

/// Write a minimal HTTP response and close the connection.
async fn write_http(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let text = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// Remove a connection from the table by pointer identity (the reader task
/// still holds the `Arc`, so `Arc::ptr_eq` is the key).
trait RemoveByPtr {
    fn remove_by_ptr(&mut self, needle: &Arc<ConnShared>);
}
impl RemoveByPtr for HashMap<u64, Arc<ConnShared>> {
    fn remove_by_ptr(&mut self, needle: &Arc<ConnShared>) {
        let key = self
            .iter()
            .find(|(_, v)| Arc::ptr_eq(v, needle))
            .map(|(k, _)| *k);
        if let Some(k) = key {
            self.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_head() {
        let head = "GET /ws?token=abc HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        let (m, t, h) = parse_request_head(head).unwrap();
        assert_eq!(m, "GET");
        assert_eq!(path_of(&t), "/ws");
        assert!(is_ws_upgrade(&h));
        assert!(authorized(&h, "t"));
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive() {
        let (_, _target, headers) = parse_request_head(
            "GET /ws HTTP/1.1\r\nHost: x\r\nAuthorization: bearer abc\r\n\r\n",
        )
        .unwrap();
        assert!(
            authorized(&headers, "abc"),
            "RFC 7235 makes the scheme case-insensitive"
        );
        let (_, _target, headers) = parse_request_head(
            "GET /ws HTTP/1.1\r\nHost: x\r\nAuthorization: BEARER abc\r\n\r\n",
        )
        .unwrap();
        assert!(authorized(&headers, "abc"));
    }

    #[test]
    fn a_token_in_the_query_string_is_not_a_credential() {
        // Removing this path is the point: a URL travels further than a header (proxy logs,
        // crash reports, history), and nothing shipped still sends one.
        let (_, _target, headers) = parse_request_head(
            "GET /ws?token=abc HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        )
        .unwrap();
        assert!(!authorized(&headers, "abc"));
        // The header still works, with or without a query string beside it.
        let (_, _target, headers) = parse_request_head(
            "GET /ws?token=abc HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer abc\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        )
        .unwrap();
        assert!(authorized(&headers, "abc"));
        assert!(!authorized(&headers, "zzz"));
    }

    #[test]
    fn plain_get_answers_401_without_a_token() {
        assert_eq!(plain_response("/ws", false), (401, "unauthorized\n"));
        assert_eq!(plain_response("/ws", true).0, 400);
        assert_eq!(plain_response("/", true).0, 404);
    }

    #[test]
    fn a_non_upgrade_request_is_not_an_upgrade() {
        let (_, _, headers) = parse_request_head("GET /ws HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert!(!is_ws_upgrade(&headers));
    }

    /// A connection whose writer never drains (a phone on a dead link).
    fn undrained_conn() -> (Arc<ConnShared>, tokio::sync::mpsc::UnboundedReceiver<Out>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
        let conn = Arc::new(ConnShared::with_inner(
            tx,
            ConnInner {
                live: true,
                attach_gen: 1,
                ..ConnInner::new()
            },
        ));
        (conn, rx)
    }

    /// A write from a connection that is not attached says so.
    ///
    /// All three writes go to the pane, and only `mouseclick` answered: `key` and
    /// `input` did nothing at all, on the wire, in either direction. A tap test on
    /// the phone looked like "the app sends nothing" because of it — the app's key
    /// batches were dropped by a daemon that said nothing back.
    #[tokio::test]
    async fn a_write_without_an_attach_is_refused_loudly() {
        let (conn, mut rx) = undrained_conn();
        let dir = std::env::temp_dir().join(format!("ldmux-remote-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let daemon = Arc::new(tokio::sync::Mutex::new(crate::daemon::LdmuxDaemon::new(
            dir.clone(),
            std::path::PathBuf::from("/nonexistent"),
        )));
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            daemon,
            token: "t".into(),
            idle_ms: 20_000,
            send_timeout_ms: WS_SEND_TIMEOUT_MS,
            release_grace_ms: RELEASE_GRACE_MS,
            claim_tick_ms: crate::daemon::CLAIM_TICK_MS,
            stopped: AtomicBool::new(false),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            sweep: Mutex::new(None),
        });
        for msg in [
            r#"{"type":"key","keys":["Enter"]}"#,
            r#"{"type":"input","data":"ls\n"}"#,
            r#"{"type":"mouseclick","col":1,"row":1}"#,
        ] {
            handle_message(msg, &conn, &shared).await;
            let out = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("no answer to {msg}"));
            let Out::Json(text) = out else {
                panic!("expected json for {msg}")
            };
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "error", "{msg} -> {v}");
            assert_eq!(v["code"], "not_attached", "{msg} -> {v}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The backlog rule: a client that is not reading costs the daemon a bounded
    /// amount, is told to resync exactly once, and is not buffered for after that.
    /// (`outstanding` was never incremented, so this guard did nothing and the
    /// writer's queue grew with the shell's whole output.)
    #[test]
    fn a_client_that_does_not_read_is_resynced_once_and_not_buffered() {
        let (conn, mut rx) = undrained_conn();
        let chunk = vec![b'x'; 4096];
        for _ in 0..10_000 {
            let mut inner = conn.inner.lock().unwrap();
            queue_output(&conn, &mut inner, &chunk);
        }
        let mut output = 0usize;
        let mut resyncs = 0usize;
        while let Ok(m) = rx.try_recv() {
            match m {
                Out::Output(b) => output += b.len(),
                Out::Resync(_) => resyncs += 1,
                Out::Json(_) => panic!("only a resync is expected"),
                Out::Close => panic!("no close"),
            }
        }
        assert_eq!(resyncs, 1, "told once per backlog");
        assert!(
            output <= WS_BACKLOG_BYTES + 4096,
            "queued {output} bytes against a {WS_BACKLOG_BYTES} byte bound"
        );
        assert_eq!(conn.outstanding.load(Ordering::Relaxed), output);

        // A fresh attach clears the flag; once the writer has caught up output flows again.
        conn.outstanding.store(0, Ordering::Relaxed);
        conn.inner.lock().unwrap().resyncing = false;
        let mut inner = conn.inner.lock().unwrap();
        queue_output(&conn, &mut inner, b"hi");
        drop(inner);
        assert!(matches!(rx.try_recv(), Ok(Out::Output(b)) if b == b"hi"));
    }

    #[test]
    fn the_caps_and_snapshot_pair_has_the_client_fields() {
        // Build a fake connection with a channel, then assert the two shapes.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
        let conn = Arc::new(ConnShared::with_inner(tx, ConnInner::new()));
        let snap = ldmux_protocol::keeper::SnapshotResult {
            total_rows: 0,
            session_id: "s".into(),
            cols: 80,
            rows: 24,
            history_lines: 0,
            text: "screen".into(),
            cursor_x: 1,
            cursor_y: 2,
            alternate_on: false,
            mouse: true,
            mouse_sgr: true,
            app_cursor: false,
            bracketed_paste: true,
            history_size: 0,
            dead: false,
            foreground: "bash".into(),
            cwd: "/".into(),
            tail: Vec::new(),
        };
        send_state(&conn, "s1", &snap, 7, SnapKind::Screen);
        let a = rx.try_recv().unwrap();
        let b = rx.try_recv().unwrap();
        let (Out::Json(a), Out::Json(b)) = (a, b) else {
            panic!("expected json");
        };
        let a: serde_json::Value = serde_json::from_str(&a).unwrap();
        let b: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_eq!(a["type"], "caps");
        assert_eq!(a["protocolVersion"], 1);
        assert_eq!(a["snapshotEpoch"], 7);
        assert_eq!(b["type"], "snapshot");
        assert_eq!(b["screen"], "screen");
        assert_eq!(b["mouseSgr"], true);
        assert!(b.get("history").is_none());
    }

    /// A reason names the device when the connection announced one, and does not
    /// invent one when it did not. The device is how "why did my shell die"
    /// answers "because the phone closed it" rather than "closed".
    #[test]
    fn a_reason_comes_from_the_device_that_ended_it() {
        let phone = ldmux_protocol::rpc::DeviceInfo {
            id: "d1".into(),
            name: "motorola edge 2024".into(),
            kind: "phone".into(),
        };
        assert_eq!(
            ended_by("killed", Some(&phone)),
            "killed from motorola edge 2024"
        );
        assert_eq!(
            ended_by("closed", Some(&phone)),
            "closed from motorola edge 2024"
        );
        // A connection with no device (a bare socket, the CLI over RPC), and one
        // that announced itself without a name, both leave the bare verb.
        assert_eq!(ended_by("killed", None), "killed");
        let unnamed = ldmux_protocol::rpc::DeviceInfo {
            id: "d2".into(),
            name: String::new(),
            kind: "phone".into(),
        };
        assert_eq!(ended_by("killed", Some(&unnamed)), "killed");
    }

    /// A server state over a scratch daemon, for the handler tests.
    fn test_shared(tag: &str, send_timeout_ms: u64) -> (Arc<Shared>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ldmux-remote-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let daemon = Arc::new(tokio::sync::Mutex::new(crate::daemon::LdmuxDaemon::new(
            dir.clone(),
            std::path::PathBuf::from("/nonexistent"),
        )));
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            daemon,
            token: "t".into(),
            idle_ms: 20_000,
            send_timeout_ms,
            release_grace_ms: RELEASE_GRACE_MS,
            claim_tick_ms: crate::daemon::CLAIM_TICK_MS,
            stopped: AtomicBool::new(false),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            sweep: Mutex::new(None),
        });
        (shared, dir)
    }

    fn error_code(out: Out) -> String {
        let Out::Json(text) = out else {
            panic!("expected json")
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "error", "{v}");
        v["code"].as_str().unwrap().to_string()
    }

    fn hello_as(conn: &Arc<ConnShared>, id: &str) {
        relock(&conn.inner).device = Some(DeviceInfo {
            id: id.into(),
            name: id.into(),
            kind: "phone".into(),
        });
    }

    /// ADR-0010: a connection with no identity holds nothing. Writing from an
    /// attached connection that never said `hello` used to skip the holder check
    /// altogether, and `kill`/`close`/`new`/`makelive`/`closelive` had no check.
    #[tokio::test]
    async fn no_hello_means_no_writes_and_no_shell_changes() {
        let (conn, mut rx) = undrained_conn();
        let (shared, dir) = test_shared("nohello", WS_SEND_TIMEOUT_MS);
        // Attached as a read-only observer would be.
        relock(&conn.inner).session_id = Some("s1".into());
        for msg in [
            r#"{"type":"input","data":"rm -rf x\n"}"#,
            r#"{"type":"key","keys":["Enter"]}"#,
            r#"{"type":"mouseclick","col":1,"row":1}"#,
            r#"{"type":"new","kind":"shell"}"#,
            r#"{"type":"kill","sessionId":"s1"}"#,
            r#"{"type":"close","sessionId":"s1"}"#,
            r#"{"type":"makelive","sessionId":"s1"}"#,
            r#"{"type":"closelive","sessionId":"s1"}"#,
        ] {
            handle_message(msg, &conn, &shared).await;
            let out = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("no answer to {msg}"));
            assert_eq!(error_code(out), "hello_required", "{msg}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A device that does not hold a shell cannot end it or hand it to the desk;
    /// the holder can, and so can anyone when nobody holds it.
    #[tokio::test]
    async fn a_shell_another_device_holds_cannot_be_killed_or_closed() {
        let (conn, mut rx) = undrained_conn();
        let (shared, dir) = test_shared("holder", WS_SEND_TIMEOUT_MS);
        {
            let mut d = shared.daemon.lock().await;
            let now = crate::daemon::now_ms();
            d.registry().upsert(SessionRecord {
                id: "s1".into(),
                keeper_name: String::new(),
                kind: "shell".into(),
                cwd: "/".into(),
                title: "t".into(),
                created_at: now,
                updated_at: now,
                created_by: "phone".into(),
                status: "running".into(),
                exit_code: None,
                exited_at: None,
                live_workspace: None,
                had_window: false,
                live_pid: None,
                ended_reason: None,
                exit_signal: None,
                ended_kind: None,
            });
            let owner = DeviceInfo {
                id: "desk".into(),
                name: "desk".into(),
                kind: "desktop".into(),
            };
            d.claim("s1", &owner, false);
        }
        hello_as(&conn, "phone-1");
        for msg in [
            r#"{"type":"kill","sessionId":"s1"}"#,
            r#"{"type":"close","sessionId":"s1"}"#,
            r#"{"type":"makelive","sessionId":"s1"}"#,
        ] {
            handle_message(msg, &conn, &shared).await;
            let out = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("no answer to {msg}"));
            assert_eq!(error_code(out), "not_holder", "{msg}");
        }
        assert_eq!(
            shared.daemon.lock().await.get_session("s1").unwrap().status,
            "running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression: the attach-backlog notice used to go straight into the
    /// channel, so the writer subtracted a length nobody had added, `queued_json`
    /// wrapped below zero, and the next control message panicked on
    /// `attempt to add with overflow` (the desk window's "link lost, reconnecting").
    /// The notice is counted now, and the writer's subtraction balances it.
    #[test]
    fn the_attach_backlog_notice_is_counted_and_a_following_message_does_not_panic() {
        let (conn, mut rx) = undrained_conn();
        let chunk = vec![b'x'; 256 * 1024];
        {
            let mut inner = conn.inner.lock().unwrap();
            inner.live = false;
            // A client that said nothing about `inplaceResync` (the desk window).
            for _ in 0..(MAX_PENDING_BYTES / chunk.len() + 1) {
                hold_until_live(&conn, &mut inner, &chunk);
            }
            assert_eq!(inner.pending_bytes, 0, "the held output is dropped");
        }
        let Ok(Out::Json(notice)) = rx.try_recv() else {
            panic!("the legacy client is told to resync");
        };
        assert!(notice.contains("\"resync\""));
        assert_eq!(
            conn.queued_json.load(Ordering::Relaxed),
            notice.len(),
            "the notice is counted while it is queued"
        );
        // The writer has sent it.
        drop(notice);
        assert_eq!(conn.queued_json.load(Ordering::Relaxed), 0);
        // The next control message, which is where the old code panicked.
        conn.send_json(&serde_json::json!({"type": "sessions", "sessions": []}));
        assert!(matches!(rx.try_recv(), Ok(Out::Json(_))));
    }

    /// A client that can take a repaint on the same socket is not told anything:
    /// the snapshot is the recovery.
    #[test]
    fn an_in_place_client_is_not_sent_a_resync_notice_for_the_attach_backlog() {
        let (conn, mut rx) = undrained_conn();
        let chunk = vec![b'x'; 256 * 1024];
        let mut inner = conn.inner.lock().unwrap();
        inner.live = false;
        inner.features.inplace_resync = true;
        for _ in 0..(MAX_PENDING_BYTES / chunk.len() + 1) {
            hold_until_live(&conn, &mut inner, &chunk);
        }
        drop(inner);
        assert!(rx.try_recv().is_err(), "nothing queued");
        assert_eq!(conn.queued_json.load(Ordering::Relaxed), 0);
    }

    /// A message queued and never written (the connection ended first) must not
    /// leave the counter high, and one sent into a channel with no reader is not
    /// counted at all.
    #[test]
    fn control_json_that_is_never_written_is_not_left_counted() {
        let (conn, rx) = undrained_conn();
        for _ in 0..5 {
            conn.send_json(&serde_json::json!({"type": "pong"}));
        }
        assert!(conn.queued_json.load(Ordering::Relaxed) > 0);
        // The connection ends: the loop drops the receiver with messages in it.
        drop(rx);
        assert_eq!(conn.queued_json.load(Ordering::Relaxed), 0);
        // And a send after that is refused by the channel and un-counted.
        conn.send_json(&serde_json::json!({"type": "pong"}));
        assert_eq!(conn.queued_json.load(Ordering::Relaxed), 0);
    }

    /// Whatever else goes wrong, the counter does not wrap: a message dropped
    /// after the counter was reset (a mis-accounting) bottoms out at zero and the
    /// next send still works.
    #[test]
    fn the_json_counter_saturates_instead_of_wrapping() {
        let (conn, mut rx) = undrained_conn();
        conn.send_json(&serde_json::json!({"type": "pong"}));
        let queued = rx.try_recv().unwrap();
        conn.queued_json.store(0, Ordering::Relaxed);
        drop(queued);
        assert_eq!(conn.queued_json.load(Ordering::Relaxed), 0);
        // A counter at the top of the range does not overflow the next send.
        conn.queued_json.store(usize::MAX, Ordering::Relaxed);
        conn.send_json(&serde_json::json!({"type": "pong"}));
        assert!(
            matches!(rx.try_recv(), Ok(Out::Close)),
            "past the cap closes, it does not panic"
        );
    }

    /// The output half of the backlog count has the same property: a release larger
    /// than what was counted bottoms out at zero, and the sum the probe reads cannot
    /// overflow even when both halves are at the top of the range.
    #[test]
    fn the_output_backlog_count_saturates_instead_of_wrapping() {
        let (conn, _rx) = undrained_conn();
        release_outstanding(&conn.outstanding, 5);
        assert_eq!(conn.outstanding.load(Ordering::Relaxed), 0);
        conn.outstanding.store(usize::MAX, Ordering::Relaxed);
        conn.queued_json.store(usize::MAX, Ordering::Relaxed);
        assert_eq!(conn.backlog_bytes(), usize::MAX);
    }

    /// A control message queue is bounded: a peer that has stopped reading is
    /// closed once the cap is passed, not buffered without limit.
    #[test]
    fn queued_control_json_is_bounded() {
        let (conn, mut rx) = undrained_conn();
        let big = "x".repeat(1024 * 1024);
        for _ in 0..(MAX_QUEUED_JSON_BYTES / big.len() + 4) {
            conn.send_json_text(big.clone());
        }
        let mut queued = 0usize;
        let mut closes = 0usize;
        while let Ok(m) = rx.try_recv() {
            match m {
                Out::Json(j) => queued += j.len(),
                Out::Close => closes += 1,
                Out::Output(_) | Out::Resync(_) => panic!("no output"),
            }
        }
        assert!(queued <= MAX_QUEUED_JSON_BYTES, "queued {queued}");
        assert!(closes >= 1, "the connection must be told to close");
    }

    /// A peer that stops reading wedged the connection loop in `ws.send`, which
    /// is also where the sweep's `Close` is read, so the connection (and its
    /// claim) were never released. With a send deadline it ends.
    #[tokio::test]
    async fn a_peer_that_stops_reading_does_not_wedge_the_connection() {
        let (shared, dir) = test_shared("deadpeer", 300);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_shared = shared.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            tune_socket(&stream);
            let (head, rest) = crate::ws::read_head(&mut stream, std::time::Duration::from_secs(5))
                .await
                .unwrap();
            let (m, _, h) = parse_request_head(&head).unwrap();
            let up = parse_upgrade(&m, &h).unwrap();
            serve_upgrade(stream, rest, up, server_shared, 1).await;
        });
        // The client completes the handshake and then never reads again.
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (_client_ws, _) = tokio_tungstenite::client_async("ws://127.0.0.1/ws", tcp)
            .await
            .unwrap();
        let conn = {
            let mut found = None;
            for _ in 0..100 {
                if let Some(c) = relock(&shared.conns).values().next().cloned() {
                    found = Some(c);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            found.expect("the connection registered")
        };
        // More than both kernel buffers can hold, straight into the writer. Pane
        // output, not control JSON: that is capped (and closes the connection) at
        // `MAX_QUEUED_JSON_BYTES`, which would end it for a different reason.
        let chunk = vec![b'y'; 256 * 1024];
        for _ in 0..192 {
            let _ = conn.tx.send(Out::Output(chunk.clone()));
        }
        let mut gone = false;
        for _ in 0..100 {
            if relock(&shared.conns).is_empty() {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            gone,
            "the connection must end when the peer stops taking writes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
