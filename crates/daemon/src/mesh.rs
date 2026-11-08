//! The daemon's mesh link.
//!
//! One carriage: with `LDMUX_MESH_SERIAL` set the daemon owns the host's MeshCore radio over USB
//! serial (`crate::mesh_serial`, on `crates/companion`), reads the tuple the radio reports, and
//! drives it. No gateway process, no loopback socket, no pairing token. A host without the
//! variable has no radio route, and that is a normal configuration, not an error.
//!
//! Everything above the carriage — the L3 session, the credit window, the retry ladder, the
//! bridge — is written once ([`Carrier`]).
//!
//! Configuration comes from the environment, because it is per-machine and belongs beside the
//! other deployment settings:
//!
//! ```text
//! LDMUX_MESH_SERIAL /dev/serial/by-id/...  own this radio (the only mesh setting that turns
//!                                          the link on)
//! LDMUX_MESH_PSK    <64 hex>               the pairing's pre-shared key, which turns on the L3
//!                                          session; persisted to mesh-psk (0600) in the persist
//!                                          directory, so a later start without the environment
//!                                          still speaks the same pairing
//! LDMUX_MESH_SELF   <64 hex>               optional: pin this host's radio public key instead of
//!                                          reading it from the radio at open
//! ```
//!
//! A malformed value disables the mesh link with a printed reason and leaves the daemon, the
//! shells and the tailnet listener running: one feature's typo must not stop the product.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event};
use ldmux_mesh::constants::{CREDIT_WINDOW, RadioState, SendOutcome};

/// The longest this loop will sleep with a session attached. See the comment at the wait.
const MAX_IDLE_WAIT: Duration = Duration::from_millis(50);

/// How long a record may wait for its `SEND_RESULT` before it stops counting against the credit
/// window.
///
/// The radio answers immediately after it queues a packet, so this is a link deadline and not an
/// airtime estimate. Ten seconds matches the phone's own raw-send timeout. Before this, a lost
/// `SEND_RESULT` held its slot for the life of the process: the window shrank by one per loss and
/// the bridge stopped sending entirely once it reached zero, with nothing but a restart to
/// recover it.
const AWAITING_DEADLINE: Duration = Duration::from_secs(10);

/// How long opening the radio, or writing one record to it, may take.
///
/// The port is local, so seconds are already generous. Without a bound, a radio that accepted the
/// port and then stopped draining left the daemon waiting forever: no error, no backoff, no
/// reconnect, and a `mesh.status` that said "handshaking" for the life of the process.
const CARRIER_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the carriage may carry nothing at all while a record is outstanding.
///
/// **A radio that stops answering is not a radio with nothing to say.** The steady-state receive
/// had no bound: opening and writing were wrapped in [`CARRIER_TIMEOUT`], but once the session was
/// up this loop would sit on a port that never spoke again, with the status reading `established`
/// and the phone's screen frozen — indefinitely, because nothing in this loop ever concluded
/// anything from silence. The ARQ's own ladder is the first line (it fails a record after five
/// attempts over ≈1 min); this is the backstop for a carriage that stops delivering
/// *anything*, including the answers the ladder is waiting for.
const CARRIER_SILENCE: Duration = Duration::from_secs(75);
use ldmux_mesh::error::MeshError;
use ldmux_session::arq::Limits;
use tokio::sync::Notify;

use crate::mesh_session::{INBOUND_MAX, SessionFiles, SessionHost, SessionSnapshot};

/// What turns the radio route on: the serial device the daemon owns.
///
/// One setting, because one process owns the radio. There is no address to dial and no token to
/// present: the daemon opens the port, identifies the radio with `SELF_INFO`, and drives it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MeshConfig {
    /// The device the daemon owns, e.g. a `/dev/serial/by-id/…` path. Never empty.
    pub serial: String,
    /// Where the "this radio's path width has been decided" record lives, in the state directory.
    ///
    /// Filled by the daemon rather than read from the environment, because it must sit beside the
    /// state it protects. `None` in a test that drives the radio directly.
    pub width_marker: Option<std::path::PathBuf>,
    /// Where the hour's spent airtime is recorded, so a restart resumes the same hour.
    pub airtime_record: Option<std::path::PathBuf>,
}

impl MeshConfig {
    /// Reads the configuration from a set of `(key, value)` pairs.
    ///
    /// Separated from [`Self::from_env`] so a test can exercise the rule without touching the
    /// process environment. `Ok(None)` is "no radio route on this host", which is the normal case
    /// for a daemon that is only reachable over the tailnet.
    pub fn from_pairs<'a, I>(pairs: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut serial: Option<String> = None;
        for (key, value) in pairs {
            if key == "LDMUX_MESH_SERIAL" {
                serial = Some(value.to_owned());
            }
        }
        match serial {
            None => Ok(None),
            Some(device) if device.trim().is_empty() => {
                Err("LDMUX_MESH_SERIAL is empty".to_owned())
            }
            Some(device) => Ok(Some(Self {
                serial: device.trim().to_owned(),
                // The daemon adds the state directory's records; a bare config has none.
                width_marker: None,
                airtime_record: None,
            })),
        }
    }

    /// The process environment's configuration.
    pub fn from_env() -> Result<Option<Self>, String> {
        let device = std::env::var("LDMUX_MESH_SERIAL").ok();
        let pairs: Vec<(&str, &str)> = device
            .as_deref()
            .map(|d| vec![("LDMUX_MESH_SERIAL", d)])
            .unwrap_or_default();
        Self::from_pairs(pairs)
    }

    /// This host's radio public key when the operator pins one.
    ///
    /// Optional: a daemon that owns its radio reads the key from `SELF_INFO` at open. The variable
    /// exists for a host that wants the key frozen — a radio swapped under a kept pairing is then
    /// a link that does not come up rather than a silent re-pin.
    #[must_use]
    pub fn self_key_from_env() -> Option<[u8; 32]> {
        let text = std::env::var("LDMUX_MESH_SELF").ok()?;
        parse_hex::<32>(&text, "LDMUX_MESH_SELF").ok()
    }
}

/// The L3 session's configuration: the pairing's PSK and the files beside the other state.
pub struct SessionSetup {
    files: Arc<SessionFiles>,
}

impl SessionSetup {
    /// `Ok(None)` when neither `LDMUX_MESH_PSK` nor the persisted `mesh-psk` exists.
    ///
    /// The environment is authoritative. When it is set, the secret is written to
    /// `mesh-psk` (0600) so a daemon restart without its environment keeps the same
    /// pairing. When it is absent, the file supplies the secret.
    ///
    /// **`persist_dir` is where those files go, and it must outlive a reboot.** The state
    /// dir is a tmpfs under systemd, so a session key kept there was re-minted on every boot
    /// and every paired phone's pinned host key stopped matching. `legacy_dir` is the old
    /// location, consulted once: anything still there is moved over by
    /// [`crate::mesh_session::adopt_legacy`].
    pub fn from_env(persist_dir: &Path, legacy_dir: &Path) -> Result<Option<Self>, String> {
        let env = std::env::var("LDMUX_MESH_PSK").ok();
        let moved = crate::mesh_session::adopt_legacy(legacy_dir, persist_dir);
        if !moved.is_empty() {
            eprintln!(
                "ldmuxd: moved {} from {} to {}. The state dir does not survive a reboot, so a mesh key kept there was re-minted on every boot",
                moved.join(", "),
                legacy_dir.display(),
                persist_dir.display()
            );
        }
        Self::from_value(persist_dir, env.as_deref())
    }

    fn from_value(state_dir: &Path, env: Option<&str>) -> Result<Option<Self>, String> {
        let path = state_dir.join("mesh-psk");
        let psk = match env {
            Some(text) => {
                let psk = parse_hex::<32>(text, "LDMUX_MESH_PSK")?;
                crate::mesh_session::write_private(&path, &psk)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                psk
            }
            None => match std::fs::read(&path) {
                Ok(bytes) if bytes.len() == 32 => {
                    let mut psk = [0u8; 32];
                    psk.copy_from_slice(&bytes);
                    psk
                }
                Ok(_) => return Err(format!("{} is not a 32-byte secret", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("{}: {e}", path.display())),
            },
        };
        Ok(Some(Self::new(state_dir, psk)?))
    }

    pub fn new(state_dir: &Path, psk: [u8; 32]) -> Result<Self, String> {
        Ok(Self {
            files: Arc::new(SessionFiles::load(state_dir, psk)?),
        })
    }

    /// The session's public key, hex: what a phone must be given to pair.
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        hex(&self.files.key.public)
    }

    /// What a phone is given to use the mesh route: this daemon's session public key and the
    /// pairing's PSK. Secret material: it leaves this process only as the pairing link.
    #[must_use]
    pub fn pairing_keys(&self) -> ldmux_protocol::pairing::MeshKeys {
        ldmux_protocol::pairing::MeshKeys {
            host_key: self.files.key.public,
            psk: self.files.pairing.psk,
            host_radio: host_radio_from_env(),
            // Filled in by the pairing answer, which reads the snapshot; the keys themselves do not
            // know what the radio is tuned to.
            host_preset: None,
        }
    }

    /// The pairing file, shared so a new grant can clear the pin on disk and on the live host.
    fn files(&self) -> Arc<SessionFiles> {
        Arc::clone(&self.files)
    }
}

/// `LDMUX_MESH_SELF`: this host's own radio public key (64 hex), if the operator says
/// so. The daemon does not own the radio and cannot learn it; it is only passed along so a phone
/// can bind it. A malformed value is ignored with no key rather than a wrong one.
fn host_radio_from_env() -> Option<[u8; 32]> {
    MeshConfig::self_key_from_env()
}

fn parse_hex<const N: usize>(text: &str, what: &str) -> Result<[u8; N], String> {
    let text = text.trim();
    if text.len() != N * 2 {
        return Err(format!("{what} must be {} hex characters", N * 2));
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = text.get(i * 2..i * 2 + 2).ok_or("short hex")?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| format!("{what} is not hex"))?;
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the link currently knows. Cheap to clone, and JSON-serialisable as-is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MeshSnapshot {
    /// `connecting`, `handshaking`, `ready`, or `down`.
    pub state: &'static str,
    /// The host radio's own public key, read from `SELF_INFO` when the radio was identified.
    pub own_radio_key: Option<[u8; 32]>,
    /// The tuple the host's own radio reported: `(frequency in kHz, bandwidth in Hz, SF, CR)`.
    /// `None` before the radio has been identified.
    pub radio_preset: Option<(u32, u32, u8, u8)>,
    /// Bytes per path hash on this radio's own adverts, once it has been identified.
    pub path_hash_size: Option<u8>,
    /// The host radio's transmit power in dBm, as it reports it (`SELF_INFO` byte 2).
    ///
    /// Published for the same reason as the tuple and the width: the phone shows its own power, so
    /// a host that never says its own leaves an asymmetry whose only cure is guessing.
    pub tx_power_dbm: Option<i8>,
    /// The serial device the daemon drives, `None` when this host has no radio route.
    pub serial: Option<String>,
    /// The largest record this peer's path allows, reported by the carrier at open.
    pub uplink_record_max: Option<u8>,
    /// Records of the current size left in this hour's airtime budget, from a status report.
    /// `None` until the radio has reported one.
    pub records_left: Option<u8>,
    pub radio_state: Option<RadioState>,
    /// `-128` is the contract's "no reading".
    pub rssi: i8,
    /// `0xFF` is the contract's "not reported".
    pub battery_percent: u8,
    /// The radio's own transmit airtime this hour, in 0.1 s units.
    pub tx_airtime_tenths: u16,
    /// The last failure, if the link is down or recovering.
    pub last_error: Option<String>,
    /// How many times the supervisor has reconnected.
    pub reconnects: u32,
    /// The L3 session on top of the link.
    pub session: SessionSnapshot,
    /// The shell bridge's counters: what the phone asked for and what the host could not
    /// answer. Published by the bridge task ([`MeshRuntime::publish_bridge`]); zero until it
    /// runs, which is the honest value when no session is configured.
    pub bridge: crate::mesh_bridge::BridgeStats,
}

impl MeshSnapshot {
    fn new(cfg: &MeshConfig) -> Self {
        Self {
            state: "connecting",
            own_radio_key: None,
            radio_preset: None,
            path_hash_size: None,
            tx_power_dbm: None,
            serial: Some(cfg.serial.clone()),
            uplink_record_max: None,
            records_left: None,
            radio_state: None,
            rssi: ldmux_mesh::UNKNOWN_RSSI,
            battery_percent: ldmux_mesh::UNKNOWN_BATTERY,
            tx_airtime_tenths: 0,
            last_error: None,
            reconnects: 0,
            session: SessionSnapshot::off(),
            bridge: crate::mesh_bridge::BridgeStats::default(),
        }
    }

    /// The JSON shape `mesh.status` answers with.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "serial": self.serial,
            "uplinkRecordMax": self.uplink_record_max,
            "recordsLeft": self.records_left,
            "radioState": self.radio_state.map(|s| s.code()),
            // The tuple the host's own radio reported (or was retuned to). Absent before the
            // radio has been identified.
            "radioPreset": self
                .radio_preset
                .map(|(f, b, sf, cr)| vec![u64::from(f), u64::from(b), u64::from(sf), u64::from(cr)]),
            // The width the host's adverts carry: the other half of "the same tuple" when a
            // peer's advert path never resolves.
            "pathHashSize": self.path_hash_size,
            "txPowerDbm": self.tx_power_dbm,
            "rssi": self.rssi,
            "batteryPercent": self.battery_percent,
            "txAirtimeTenths": self.tx_airtime_tenths,
            "lastError": self.last_error,
            "reconnects": self.reconnects,
            "session": self.session.to_json(),
            "bridge": {
                "updates": self.bridge.updates,
                "messagesSent": self.bridge.messages_sent,
                "recordsSent": self.bridge.records_sent,
                "inputs": self.bridge.inputs,
                "keys": self.bridge.keys,
                "resizes": self.bridge.resizes,
                "resizesUnchanged": self.bridge.resizes_unchanged,
                "malformed": self.bridge.malformed,
                "refusedSends": self.bridge.refused_sends,
                "hostUnanswered": self.bridge.host_unanswered,
                "releaseFailed": self.bridge.release_failed,
            },
        })
    }
}

/// Bytes crossing between the daemon and the session task.
#[derive(Default)]
struct Exchange {
    outbound: VecDeque<(u8, Vec<u8>)>,
    inbound: VecDeque<(u8, Vec<u8>)>,
    /// Records dropped on the way to the bridge because the queue was full. The session layer
    /// has already acknowledged them, so this counter is the only evidence the bytes existed.
    dropped_inbound: u64,
}

/// A retune asked for: frequency in kHz, bandwidth in Hz, spreading factor, coding rate.
///
/// The same four numbers the radio reports and the same four the command carries, so a request and
/// a report are comparable without conversion.
/// A retune request: the channel's four numbers, and optionally the path hash width in bytes.
///
/// The width is not part of the channel tuple as the firmware sees it, but it is part of *the
/// channel a network uses*: MeshCore's public presets are one byte, so a repeater's hash is one
/// byte, and a node that names its neighbours in two bytes cannot read a path a public repeater
/// built. Hence one request that can carry both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retune {
    pub freq_khz: u32,
    pub bw_hz: u32,
    pub sf: u8,
    pub cr: u8,
    /// `None` leaves the width alone.
    pub path_bytes: Option<u8>,
    /// `None` leaves the power alone. One signed byte on the wire, window -9..max.
    pub power_dbm: Option<i8>,
}

/// The running link, shared with the RPC handler.
#[derive(Clone)]
pub struct MeshRuntime {
    /// The keys a phone needs to pair for the mesh route; `None` when no session is configured.
    pairing_keys: Option<ldmux_protocol::pairing::MeshKeys>,
    pairing_files: Option<Arc<SessionFiles>>,
    reset_pin: Arc<AtomicBool>,
    snapshot: Arc<Mutex<MeshSnapshot>>,
    exchange: Arc<Mutex<Exchange>>,
    wake: Arc<Notify>,
    /// Resolves when a downlink record has been queued for the bridge.
    ///
    /// Separate from [`Self::wake`] because the two directions have different waiters: `wake`
    /// tells the session loop that the daemon queued an uplink record, and this tells the bridge
    /// that the session loop queued a downlink one. Sharing one handle would make each waiter
    /// wake for the other's traffic — and would let a permit meant for one be consumed by the
    /// other, which is a lost wakeup in the direction that did not get it.
    pub(crate) inbound_wake: Arc<Notify>,
    /// A retune asked for, waiting for the link task to apply it.
    ///
    /// The radio belongs to the link task, which owns the carrier, so every other caller can only
    /// leave a request. It is applied on the loop's next pass and the snapshot then says whether
    /// the firmware took it.
    preset_request: Arc<Mutex<Option<Retune>>>,
}

static GLOBAL: OnceLock<MeshRuntime> = OnceLock::new();

impl MeshRuntime {
    /// Ask the link to tune the host's own radio. `false` when no session is running.
    pub fn request_preset(&self, want: Retune) -> bool {
        match self.preset_request.lock() {
            Ok(mut slot) => {
                *slot = Some(want);
                self.wake.notify_one();
                true
            }
            Err(_) => false,
        }
    }

    /// The pending retune, if any. Taken once, by the link task.
    fn take_preset(&self) -> Option<Retune> {
        self.preset_request.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Connects and keeps the connection up in the background.
    ///
    /// Never fails: a link that is down is a state, and the daemon must start
    /// whether or not a radio is attached. Failures land in the snapshot.
    pub fn start(cfg: MeshConfig) -> Self {
        Self::start_with_session(cfg, None)
    }

    /// As [`Self::start`], with the L3 session if `session` is given.
    pub fn start_with_session(cfg: MeshConfig, session: Option<SessionSetup>) -> Self {
        let mut first = MeshSnapshot::new(&cfg);
        if let Some(setup) = &session {
            first.session = SessionSnapshot {
                state: "waiting",
                public_key: setup.public_key_hex(),
                peer_key: setup.files.pairing.pinned.map(|k| hex(&k)),
                ..SessionSnapshot::default()
            };
        }
        let snapshot = Arc::new(Mutex::new(first));
        let runtime = Self {
            pairing_keys: session.as_ref().map(SessionSetup::pairing_keys),
            pairing_files: session.as_ref().map(SessionSetup::files),
            reset_pin: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::clone(&snapshot),
            exchange: Arc::new(Mutex::new(Exchange::default())),
            wake: Arc::new(Notify::new()),
            inbound_wake: Arc::new(Notify::new()),
            preset_request: Arc::new(Mutex::new(None)),
        };
        tokio::spawn(supervise(cfg, session, runtime.clone()));
        runtime
    }

    /// Queue bytes for `shell` toward the phone. Fails, visibly, unless a session is
    /// established; bytes are never held across a gap in the session.
    pub fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        if self.snapshot().session.state != "established" {
            return Err("no session is established".to_owned());
        }
        let mut ex = self.exchange.lock().map_err(|_| "poisoned".to_owned())?;
        ex.outbound.push_back((shell, bytes.to_vec()));
        let depth = ex.outbound.len();
        drop(ex);
        if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") {
            eprintln!(
                "ldmuxd: mesh timing: pushed to outbound, depth {depth} at {:?}",
                runtime_stamp()
            );
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Application bytes the phone has sent, in order, `(shell, bytes)`.
    #[must_use]
    pub fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
        self.exchange
            .lock()
            .map(|mut ex| ex.inbound.drain(..).collect())
            .unwrap_or_default()
    }

    /// The keys for a mesh pairing link, or `None` when no session is configured.
    #[must_use]
    pub fn pairing_keys(&self) -> Option<&ldmux_protocol::pairing::MeshKeys> {
        self.pairing_keys.as_ref()
    }

    /// A new pairing grant clears the pinned phone key, on disk and, on the next loop turn, in
    /// the live host. The old phone re-pins itself on its next handshake.
    pub fn clear_pin(&self) -> Result<(), String> {
        let files = self
            .pairing_files
            .as_ref()
            .ok_or_else(|| "no mesh session is configured".to_owned())?;
        files.clear_pin().map_err(|e| e.to_string())?;
        self.reset_pin.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Makes this runtime the one the RPC handler answers from.
    pub fn install(self) {
        let _ = GLOBAL.set(self);
    }

    /// The installed runtime, if the link is configured.
    pub fn global() -> Option<&'static MeshRuntime> {
        GLOBAL.get()
    }

    /// Record what the shell bridge has done, for `mesh status`.
    ///
    /// The bridge is its own task with its own counters, so without this the number that says
    /// "the host could not answer" was invisible from outside - which is the number that tells a
    /// blank launcher apart from a host with no shells.
    pub fn publish_bridge(&self, stats: crate::mesh_bridge::BridgeStats) {
        set(&self.snapshot, |s| s.bridge = stats);
    }

    #[must_use]
    pub fn snapshot(&self) -> MeshSnapshot {
        self.snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// `mesh.status` for a daemon with no link configured.
    #[must_use]
    pub fn disabled_json() -> serde_json::Value {
        serde_json::json!({
            "state": "disabled",
            "serial": serde_json::Value::Null,
            "uplinkRecordMax": serde_json::Value::Null,
            "recordsLeft": serde_json::Value::Null,
            "radioState": serde_json::Value::Null,
            "rssi": ldmux_mesh::UNKNOWN_RSSI,
            "batteryPercent": ldmux_mesh::UNKNOWN_BATTERY,
            "txAirtimeTenths": 0,
            "lastError": serde_json::Value::Null,
            "reconnects": 0,
            "session": SessionSnapshot::off().to_json(),
            "hostRadio": serde_json::Value::Null,
        })
    }
}

/// The answer to `mesh.pairing`: the keys for a mesh pairing link, hex. An error says what is
/// missing; the secret is never in an error or a log line.
pub fn pairing_json() -> Result<serde_json::Value, String> {
    let runtime = MeshRuntime::global().ok_or_else(|| {
        "no mesh session is configured (set LDMUX_MESH_PSK once, with the link variables)"
            .to_owned()
    })?;
    let keys = runtime.pairing_keys().ok_or_else(|| {
        "no mesh session is configured (set LDMUX_MESH_PSK once, with the link variables)"
            .to_owned()
    })?;
    runtime.clear_pin()?;
    // **The radio's own key, when the daemon drives the radio.** `LDMUX_MESH_SELF` existed because
    // the daemon could not learn it; now it reads it from `SELF_INFO` at open, so a code is
    // complete without a second environment variable that could be stale or mistyped. The
    // variable still wins when set, so an operator who pinned a key keeps that pin.
    let host_radio = keys.host_radio.or_else(|| runtime.snapshot().own_radio_key);
    // **The tuple, so the phone can match it.** Two radios on different tuples cannot hear each
    // other, and the phone had no way to learn the host's: a mismatch was silent and looked like
    // being out of range.
    let host_preset = runtime.snapshot().radio_preset;
    // **The width rides with the tuple.** A phone that matches the frequency and keeps its own path
    // width matches half a channel: on MeshCore's public presets a repeater's hash is one byte, so
    // a two-byte reader cannot follow the route a public repeater built — and the mismatch only
    // shows up once a repeater is in the way. Sent as a fifth element, so a phone that predates it
    // reads four and carries on.
    let host_path = runtime.snapshot().path_hash_size;
    Ok(serde_json::json!({
        "hostKey": hex(&keys.host_key),
        "psk": hex(&keys.psk),
        "hostRadio": host_radio.map(|r| hex(&r)),
        "hostPreset": host_preset.map(|(f, b, sf, cr)| {
            let mut v = vec![u64::from(f), u64::from(b), u64::from(sf), u64::from(cr)];
            if let Some(width) = host_path {
                v.push(u64::from(width));
            }
            v
        }),
    }))
}

/// The answer to `mesh.status`: the snapshot, or the disabled shape.
#[must_use]
pub fn status_json() -> serde_json::Value {
    match MeshRuntime::global() {
        Some(runtime) => {
            let mut v = runtime.snapshot().to_json();
            // This host's own radio key is not radio state: it is what decides whether a
            // pairing code can be used at all. A code without it sends the phone to
            // `NeedsPeer`, and re-scanning the same code cannot fix that because the key is
            // simply not in it. It belongs in the status the operator reads before printing.
            if let Some(keys) = runtime.pairing_keys() {
                v["hostRadio"] = match keys.host_radio.or_else(|| runtime.snapshot().own_radio_key) {
                    Some(key) => serde_json::Value::String(hex(&key)),
                    None => serde_json::Value::Null,
                };
            }
            v
        }
        None => MeshRuntime::disabled_json(),
    }
}

/// One connection's lifetime, then a reconnect.
async fn supervise(cfg: MeshConfig, mut setup: Option<SessionSetup>, runtime: MeshRuntime) {
    let snapshot = Arc::clone(&runtime.snapshot);
    let mut backoff = Duration::from_secs(1);
    let mut reconnects: u32 = 0;
    loop {
        set(&snapshot, |s| {
            s.state = "connecting";
            s.last_error = None;
        });
        // Whether this attempt ever completed the open. Only that says the radio is healthy, and
        // only then does the backoff go back to its floor: before this, one outage
        // left every later reconnect waiting the full 30 s cap for the life of the process.
        let bound = Arc::new(AtomicBool::new(false));
        match link(&cfg, &runtime, setup.as_mut(), Arc::clone(&bound)).await {
            Ok(()) => {
                set(&snapshot, |s| {
                    s.state = "down";
                    s.last_error = Some("the link closed".to_owned());
                });
            }
            Err(e) => {
                set(&snapshot, |s| {
                    s.state = "down";
                    s.last_error = Some(e.to_string());
                });
            }
        }
        reconnects = reconnects.saturating_add(1);
        set(&snapshot, |s| {
            s.reconnects = reconnects;
            s.records_left = None;
            s.uplink_record_max = None;
            if s.session.state != "off" {
                s.session.state = "waiting";
            }
        });
        if bound.load(Ordering::SeqCst) {
            backoff = Duration::from_secs(1);
        }
        tokio::time::sleep(backoff).await;
        // Capped: a radio that is off for an hour should not be reopened every second, and one
        // that comes back is noticed on the next 1 s attempt because the reset above ran for this
        // connection.
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn set(snapshot: &Arc<Mutex<MeshSnapshot>>, f: impl FnOnce(&mut MeshSnapshot)) {
    if let Ok(mut guard) = snapshot.lock() {
        f(&mut guard);
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Open the radio and run the session on it.
///
/// One carrier, and it is the one that ships: the daemon opens the serial device, reads the tuple
/// the radio reports, publishes it (the phone must match it), and hands the carrier to the loop.
async fn link(
    cfg: &MeshConfig,
    runtime: &MeshRuntime,
    setup: Option<&mut SessionSetup>,
    bound: Arc<AtomicBool>,
) -> Result<(), MeshError> {
    set(&runtime.snapshot, |s| s.state = "handshaking");
    let opened = tokio::time::timeout(
        CARRIER_TIMEOUT,
        crate::mesh_serial::SerialCarrier::open(
            &cfg.serial,
            cfg.width_marker.clone(),
            cfg.airtime_record.clone(),
        ),
    )
    .await
    .map_err(|_| MeshError::Timeout("the radio did not open"))??;
    let (carrier, promise, preset) = opened;
    // **The host's own tuple, said out loud.** Read from the radio at open and published here,
    // because two radios on different tuples cannot hear each other and the phone could only ever
    // compare against a number the host never gave it.
    set(&runtime.snapshot, |s| {
        s.own_radio_key = Some(preset.public_key);
        s.radio_preset = Some((
            (preset.freq_mhz * 1000.0).round() as u32,
            (preset.bw_khz * 1000.0).round() as u32,
            preset.sf,
            preset.cr,
        ));
        s.path_hash_size = Some(preset.hash_size);
        s.tx_power_dbm = Some(preset.tx_power_dbm);
    });
    session(carrier, promise, runtime, setup, bound).await
}

/// How many records this poll may hand to the carrier.
///
/// **The carrier's own countdown is the meter, and this loop applies it.** The radio's firmware
/// does run a transmitter budget — MeshCore's `Dispatcher` charges measured airtime against
/// `tx_budget_ms` and delays the next send when it is spent — but its factor is 1.0, which is a
/// 50 % duty cycle (`meshcore/src/Dispatcher.cpp`: `getAirtimeBudgetFactor()` returns 1.0,
/// `duty_cycle = 1/(1+factor)`, read 2026-11-06). That is not the 1 % a European band allows, so
/// the firmware's limiter cannot be the whole answer and the serial carrier publishes the 1 %
/// countdown it computes from the tuple the radio reports. This returns that figure when the
/// carrier has published one; `u8::MAX` only before the first status arrives.
///
/// The ARQ charges `records_left` to **new** records only: retransmissions and pure acks pass on
/// top of it (`crates/session/src/arq.rs`, `Arq::poll`), so a spent budget stalls new output and
/// never the exchange that keeps the link alive.
fn poll_budget(source: Budget, published: Option<u8>) -> u8 {
    match source {
        Budget::RadioGoverned => published.unwrap_or(u8::MAX),
    }
}

/// One connected session, from handshake to close.
///
/// Generic over the carriage: the L3 session, the credit window, the budget and the
/// retry ladder are written once, and a carrier only has to move records.
async fn session<C: Carrier>(
    mut carrier: C,
    promise: Bound,
    runtime: &MeshRuntime,
    setup: Option<&mut SessionSetup>,
    bound: Arc<AtomicBool>,
) -> Result<(), MeshError> {
    let snapshot = &runtime.snapshot;
    bound.store(true, Ordering::SeqCst);
    set(snapshot, |s| {
        s.state = "ready";
        s.uplink_record_max = Some(promise.uplink_record_max);
        s.radio_state = Some(promise.radio_state);
        s.last_error = None;
    });

    // The L3 host lives as long as this L2 connection: a reconnect means a new handshake
    // (ADR-0020 section 3), so nothing here outlives the socket.
    let mut host: Option<SessionHost> = setup.as_ref().map(|s| {
        let key = crate::mesh_session::SessionFiles::clone_key(&s.files);
        SessionHost::new(
            key,
            s.files.current_pairing(),
            usize::from(promise.uplink_record_max),
            s.files.persister(),
            promise.airtime_ms,
        )
    });
    // Same clock as every other probe in the process. A bare `Instant::now()` here was a second
    // origin, so `drained … ms` and `reply queued at …` differed by a constant that read as
    // latency — 76 748 ms on one run. Anything two probes are compared against comes from here.
    let started = runtime_start();
    // When this carriage last delivered anything, in either direction.
    let mut last_carrier = Instant::now();
    // One entry per record written and not yet answered, keyed by its L2 sequence number.
    // `SEND_RESULT` echoes that number, so an answer can be matched to its record: a late answer
    // for an expired slot must not decrement a live one. Bounded by `CREDIT_WINDOW`.
    //
    // The third field is the ARQ slot the record belongs to. The wire carries a one-byte sequence
    // that wraps, so it cannot name an ARQ slot by itself; keeping the mapping here is what lets a
    // refusal be handed back to the right slot. A queued handshake reply has no ARQ slot (`None`).
    let mut awaiting: VecDeque<(u8, Instant, Option<u16>)> = VecDeque::new();
    let mut seq: u8 = 0;
    // The hour's remaining 1 % airtime, in full-size records, as the carrier last published it
    // and as this loop has spent it since. **Decremented here per new data record**, because the
    // ARQ's limit is per poll: a count that was not decremented would allow that many records
    // every pass, which is not a budget at all.
    let mut hour_left: Option<u8> = None;

    loop {
        if runtime.reset_pin.swap(false, Ordering::SeqCst)
            && let Some(h) = host.as_mut()
        {
            if let Err(e) = h.clear_pin() {
                set(snapshot, |s| {
                    s.last_error = Some(format!("could not clear the pinned key: {e}"));
                });
            }
            set(snapshot, |s| s.session.peer_key = None);
        }
        // **Anything queued for the phone leaves now.** Waiting on the wake notification alone
        // left a record sitting in the outbound queue for as long as whichever deadline the ARQ
        // was waiting on: measured on the device as eleven seconds between the bridge queueing a
        // screen update and the loop handing it to the radio. A non-empty queue is its own wake
        // reason, and checking it costs one mutex per iteration.
        let has_outbound = runtime
            .exchange
            .lock()
            .map(|ex| !ex.outbound.is_empty())
            .unwrap_or(false);
        let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // **Never sleep longer than a tick.** The wake notification is not the only way work
        // arrives here — the bridge hands records over from its own task — and a missed
        // notification used to park a queued screen update until whichever deadline the ARQ was
        // waiting on: measured on the device as ten seconds between the bridge queueing an echo
        // and the loop handing it to the radio. A 50 ms ceiling costs twenty wakeups a second and
        // removes the whole class of "it waits for something it did not need to".
        let wait = if has_outbound {
            // Nothing to wait for: the outbound queue is its own wake reason, so fall through
            // to the body on the next poll of this select.
            Duration::ZERO
        } else {
            host.as_ref()
                .and_then(SessionHost::next_deadline)
                .map_or(MAX_IDLE_WAIT, |d| Duration::from_millis(d.saturating_sub(now_ms)))
                .min(MAX_IDLE_WAIT)
        };
        // **A retune is the radio's own, so the link task does it.** The firmware validates the
        // tuple and either retunes (and saves it) or refuses, and the snapshot says which: the
        // host owning its radio is new, and until this existed the host radio could only be
        // configured with a different tool while the phone configured its own from the app.
        if let Some(want) = runtime.take_preset() {
            let Retune {
                freq_khz,
                bw_hz,
                sf,
                cr,
                path_bytes,
                power_dbm,
            } = want;
            match carrier.set_preset(freq_khz, bw_hz, sf, cr).await {
                Ok(()) => {
                    set(snapshot, |s| {
                        s.radio_preset = Some((freq_khz, bw_hz, sf, cr));
                        s.last_error = None;
                    });
                    // **The channel is the tuple *and* the width.** A retune that only moved the
                    // frequency left a node on the public channel naming its neighbours in two
                    // bytes, which is a path no public repeater builds. Reported apart from the
                    // tuple because the two can fail independently.
                    if let Some(bytes) = path_bytes {
                        match carrier.set_path_width(bytes).await {
                            Ok(()) => {
                                set(snapshot, |s| s.path_hash_size = Some(bytes));
                            }
                            Err(e) => {
                                set(snapshot, |s| s.last_error = Some(e.to_string()));
                            }
                        }
                    }
                    // **Power is the third register**, separate from the tuple and the width, and
                    // global to the radio: whichever app writes it last wins.
                    if let Some(dbm) = power_dbm {
                        match carrier.set_tx_power(dbm).await {
                            Ok(()) => {
                                set(snapshot, |s| s.tx_power_dbm = Some(dbm));
                            }
                            Err(e) => {
                                set(snapshot, |s| s.last_error = Some(e.to_string()));
                            }
                        }
                    }
                }
                Err(e) => {
                    set(snapshot, |s| s.last_error = Some(e.to_string()));
                }
            }
        }
        // **The receive side is polled on every pass, never skipped.** This used to skip
        // `carrier.recv()` whenever the outbound queue was non-empty. Under a steady screen
        // producer that could skip it on consecutive passes, so `SEND_RESULT`s and inbound
        // records were delayed, every credit slot stayed full and `last_carrier` went stale —
        // and at 75 s the link reported "the carriage stopped answering" when the host had
        // simply stopped listening. `biased` keeps the ordering the queue rule wanted: an
        // event that is ready is handled before the zero-length sleep falls through to send.
        tokio::select! {
            biased;
            event = carrier.recv() => {
                last_carrier = Instant::now();
                match event? {
                    Event::Status(status) => {
                        // **Upward only, and that is the whole fix for a double charge.** The loop
                        // reserves against the countdown when it hands records to the carrier
                        // (below), and the carrier reports what it actually transmitted afterwards.
                        // Taking every report was a second decrement of the same records: 255 in an
                        // hour became 128, then zero, then a stall that never recovered because at
                        // zero nothing was polled and nothing transmitted (measured 2026-11-06:
                        // `held back 3959 times`, a phone waiting on a healthy session).
                        //
                        // Taking a *higher* report is how an hour rollover comes back, and it is the
                        // only thing that can reopen a spent budget.
                        if let Some(left) = status.ops_left_this_hour {
                            hour_left = Some(match hour_left {
                                Some(current) if left <= current => current,
                                _ => left,
                            });
                        }
                        set(snapshot, |s| {
                            s.radio_state = Some(status.radio_state);
                            s.rssi = status.rssi;
                            s.battery_percent = status.battery_percent;
                            s.tx_airtime_tenths = status.tx_airtime_tenths_this_hour;
                            // What is left of the hour's 1 % airtime, computed by the carrier
                            // from the tuple the radio reports. Enforced on the next poll
                            // (`poll_budget`), charged to new records only.
                            if status.ops_left_this_hour.is_some() {
                                s.records_left = status.ops_left_this_hour;
                            }
                        });
                    }
                    Event::Recv(record) => {
                        match host.as_mut() {
                            Some(h) => {
                                let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                                h.on_record(now, now_unix_ms(), &record);
                            }
                            None => set(snapshot, |s| {
                                s.last_error = Some(
                                    "a downlink record arrived; no session is configured (LDMUX_MESH_PSK)"
                                        .to_owned(),
                                );
                            }),
                        }
                    }
                    Event::SendResult(result) => {
                        // Match the answer to its record. An answer whose slot already expired
                        // (or a duplicate answer) removes nothing: the record it names is gone.
                        // The wire sequence names the *record we wrote*; the ARQ sequence names
                        // the slot that produced it. The two differ (the wire byte wraps, and a
                        // handshake reply has no slot at all), so the mapping is looked up here and
                        // dropped with the entry.
                        let arq_seq = awaiting
                            .iter()
                            .position(|(s, _, _)| *s == result.seq)
                            .and_then(|i| awaiting.remove(i))
                            .and_then(|(_, _, arq)| arq);
                        match result.outcome {
                            SendOutcome::ErrBusy => set(snapshot, |s| {
                                s.last_error = Some(format!(
                                    "the radio refused record {} (busy); its retry was given back",
                                    result.seq
                                ));
                            }),
                            SendOutcome::ErrNotFound | SendOutcome::ErrTooLong => set(snapshot, |s| {
                                s.last_error = Some(format!(
                                    "the radio refused record {} ({:?})",
                                    result.seq, result.outcome
                                ));
                            }),
                            _ => {}
                        }
                        // A refusal never reached the air, so it must not count against the retry
                        // ladder: five refusals used to end the session over a budget that refills.
                        if matches!(
                            result.outcome,
                            SendOutcome::ErrBusy | SendOutcome::ErrNotFound | SendOutcome::ErrTooLong
                        ) && let Some(arq_seq) = arq_seq
                            && let Some(h) = host.as_mut()
                        {
                            h.on_send_refused(
                                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                                arq_seq,
                            );
                        }
                    }
                }
            }
            () = tokio::time::sleep(wait) => {}
            () = runtime.wake.notified() => {}
        }

        let Some(h) = host.as_mut() else { continue };
        let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // **Silence with something outstanding is the end of this connection.** Nothing else
        // concludes anything from it: see [`CARRIER_SILENCE`]. Returning here is what lets
        // `supervise` build a new one, which is the only cure for a dead carriage.
        if h.snapshot().pending > 0 && last_carrier.elapsed() >= CARRIER_SILENCE {
            return Err(MeshError::Timeout("the carriage stopped answering"));
        }

        // **Credit that was never answered expires.** A `SEND_RESULT` that never arrives used to
        // hold its slot forever, so a handful of lost answers closed the window permanently.
        let now = Instant::now();
        let stale = awaiting
            .iter()
            .take_while(|(_, at, _)| now.saturating_duration_since(*at) >= AWAITING_DEADLINE)
            .count();
        if stale > 0 {
            for _ in 0..stale {
                awaiting.pop_front();
            }
            set(snapshot, |s| {
                s.last_error = Some(format!(
                    "{stale} record(s) never got a send result; their credit slots were released"
                ));
            });
        }

        // Application bytes queued by the daemon go into the ARQ; a refusal is shown, not hidden.
        let queued: Vec<(u8, Vec<u8>)> = runtime
            .exchange
            .lock()
            .map(|mut ex| ex.outbound.drain(..).collect())
            .unwrap_or_default();
        let queued_n = queued.len();
        if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") && !queued.is_empty() {
            eprintln!(
                "ldmuxd: mesh timing: drained {} at {} ms",
                queued.len(),
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
            );
        }
        for (shell, bytes) in queued {
            if let Err(e) = h.send(shell, &bytes) {
                set(snapshot, |s| {
                    s.last_error = Some(format!("session send refused: {e}"))
                });
            }
        }

        // **The carrier's countdown, applied here.** See [`poll_budget`]: the firmware's own
        // budget is a 50 % limiter, so the 1 % the operator is held to comes from the carrier's
        // computation and this is where it takes effect.
        let used = u8::try_from(awaiting.len()).unwrap_or(u8::MAX);
        let records_left = poll_budget(promise.budget, hour_left);
        let limits = Limits {
            records_left,
            l2_slots: CREDIT_WINDOW.saturating_sub(used),
        };
        let t_poll = std::time::Instant::now();
        let polled = h.poll_with_seq(now_ms, limits);
        if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1")
            && t_poll.elapsed().as_millis() > 50
        {
            eprintln!("ldmuxd: mesh timing: poll took {} ms", t_poll.elapsed().as_millis());
        }
        if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") && queued_n > 0 {
            eprintln!(
                "ldmuxd: mesh timing: queued {} -> polled {} at {now_ms} ms (awaiting {used}, l2_slots {})",
                queued_n,
                polled.len(),
                CREDIT_WINDOW.saturating_sub(used),
            );
        }
        let sent_now = polled.len();
        // **One accountant: the carrier.** It counts what it actually transmitted and publishes the
        // remainder as `Status.ops_left_this_hour`; this loop only reads it.
        //
        // It used to decrement here as well, per new record — and the carrier's figure already
        // accounts for the records it sent, so every record was charged twice. At 500 kHz that is
        // 255 records an hour becoming 128, and then a stall the link never left: with
        // `records_left` at zero nothing was polled, so the carrier sent nothing, so it published no
        // new `Status`, so the countdown could not recover. Measured on this bench, 2026-11-06:
        // `records 0 left this hour` after 17.8 s of air, `held back 3959 times`, and a phone sitting
        // on "the host has not described a terminal yet" with a healthy session.
        //
        // Retransmissions and acks are not charged by the carrier either: a retransmission is
        // traffic already paid for, and an ack is the exchange that keeps the link alive. The ARQ
        // lets both past `records_left`.
        //
        // **A reservation, not a second account.** It keeps the loop from handing the carrier more
        // inside one tick than the countdown allows, before the carrier's own report has arrived;
        // the rules above make the two agree instead of adding up.
        let charged = polled.iter().filter(|r| r.new_data).count();
        if let Some(left) = hour_left {
            hour_left = Some(left.saturating_sub(u8::try_from(charged).unwrap_or(u8::MAX)));
            set(snapshot, |s| s.records_left = hour_left);
        }
        let pending_now = h.snapshot().pending;
        for record in polled {
            let t_write = std::time::Instant::now();
            tokio::time::timeout(CARRIER_TIMEOUT, carrier.send_record(seq, &record.bytes))
                .await
                .map_err(|_| MeshError::Timeout("the write to the radio"))??;
            let took = t_write.elapsed().as_millis();
            if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") && took > 50 {
                eprintln!("ldmuxd: mesh timing: socket write took {took} ms");
            }
            awaiting.push_back((seq, Instant::now(), record.seq));
            seq = seq.wrapping_add(1);
        }
        // **Drain what is sendable before sleeping.** The loop used to send one poll's worth and
        // then wait for the ARQ's next deadline, so a screen update of six records could take
        // eleven seconds to leave while the link sat idle in between. If the poll returned records
        // and the window still has room, go round again now; a poll that returns nothing falls
        // through to the normal wait, so this cannot spin.
        if sent_now > 0 && awaiting.len() < usize::from(CREDIT_WINDOW) && pending_now > 0 {
            continue;
        }

        let view = h.snapshot();
        let mut queued_inbound = false;
        if let Ok(mut ex) = runtime.exchange.lock() {
            while let Some(item) = h.inbound.pop_front() {
                if ex.inbound.len() >= INBOUND_MAX {
                    ex.inbound.pop_front();
                    ex.dropped_inbound = ex.dropped_inbound.saturating_add(1);
                    // Say it, at powers of two so a flood of drops is one line per doubling.
                    if ex.dropped_inbound.is_power_of_two() {
                        eprintln!(
                            "ldmuxd: mesh bridge queue full, dropped the oldest record \
                             ({} dropped so far)",
                            ex.dropped_inbound
                        );
                    }
                }
                ex.inbound.push_back(item);
                queued_inbound = true;
            }
        }
        if queued_inbound {
            // The bridge waits on this instead of the timer. A permit is stored when nobody is
            // waiting, so a record queued while the bridge is busy is still noticed at once.
            runtime.inbound_wake.notify_one();
        }
        set(snapshot, |s| s.session = view);
    }
}

/// Milliseconds since the first call in this process, for cross-task timing lines.
///
/// **One clock for every probe.** This and `mesh_bridge::stamp` used to be two separate
/// `OnceLock<Instant>`s, each started by its own first call, so a line from the bridge and a line
/// from this loop differed by a constant offset. That offset was read as latency: a constant
/// 11 550 ms "round trip" that was really 0 ms. Any probe that can be compared with another must
/// come from here.
pub(crate) fn runtime_stamp() -> u128 {
    runtime_start().elapsed().as_millis()
}

/// The process's single timing origin.
///
/// Callers that need elapsed milliseconds keep this instant rather than taking their own, so no
/// two probes can drift apart by a constant offset.
pub(crate) fn runtime_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A carriage that counts how often the loop asks it for the next event.
    struct PollCountingCarrier {
        polls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Carrier for PollCountingCarrier {
        async fn send_record(&mut self, _seq: u8, _record: &[u8]) -> Result<(), MeshError> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<Event, MeshError> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            // Never resolves: the loop must not need an event to fall through and send.
            std::future::pending().await
        }
    }

    fn test_runtime() -> MeshRuntime {
        let cfg = MeshConfig {
            serial: "/dev/null".to_owned(),
            width_marker: None,
            airtime_record: None,
        };
        MeshRuntime {
            pairing_keys: None,
            pairing_files: None,
            reset_pin: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(Mutex::new(MeshSnapshot::new(&cfg))),
            exchange: Arc::new(Mutex::new(Exchange::default())),
            wake: Arc::new(Notify::new()),
            inbound_wake: Arc::new(Notify::new()),
            preset_request: Arc::new(Mutex::new(None)),
        }
    }

    /// **The receive side is polled even while a record waits to go out.** The loop used to
    /// skip `carrier.recv()` whenever the outbound queue was non-empty, so under a steady
    /// producer a `SEND_RESULT` or an inbound record could wait indefinitely and the link then
    /// reported "the carriage stopped answering" about a carriage that was fine.
    #[tokio::test]
    async fn a_queued_record_does_not_stop_the_carriage_being_polled() {
        let runtime = test_runtime();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let carrier = PollCountingCarrier {
            polls: Arc::clone(&polls),
        };
        // No `SessionSetup`: nothing is sent, so the queue below stays non-empty for the whole
        // test — exactly the state in which the receive side used to be skipped.
        let task = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                let _ = session(carrier, Bound {
                    uplink_record_max: 172,
                    radio_state: RadioState::Connected,
                    budget: Budget::RadioGoverned,
                    airtime_ms: None,
                }, &runtime, None, Arc::new(AtomicBool::new(false)))
                .await;
            }
        });
        runtime
            .exchange
            .lock()
            .unwrap()
            .outbound
            .push_back((0, vec![1, 2, 3]));
        // Wait for the poll count to move rather than sleeping a fixed time.
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the loop never polled the carriage");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        task.abort();
    }

    /// A carriage that is not a radio: what the daemon writes is handed to the test, and what
    /// the test seals is handed back as an inbound record.
    ///
    /// This is the seam the deleted gateway-based integration tests used to go through. The
    /// transport they drove is gone (there is no loopback gateway), but the L3 path over *some*
    /// carrier is the part that matters, and this keeps it covered without a device.
    struct AirCarrier {
        to_phone: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        events: tokio::sync::mpsc::UnboundedReceiver<Event>,
        pending: std::collections::VecDeque<Event>,
        /// What the carrier last told the loop it had left, and what it counts down from.
        ///
        /// **The real radio publishes this after every send**, which is what makes it the one
        /// accountant: the loop reads the remainder instead of decrementing a copy that would be
        /// charged twice. `u8::MAX` here means "this carriage does not meter", so the tests that are
        /// about the protocol rather than the budget are not bounded by an imaginary hour.
        left: u8,
    }

    impl Carrier for AirCarrier {
        async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
            if self.to_phone.send(record.to_vec()).is_err() {
                return Err(MeshError::Closed);
            }
            // A healthy radio takes the record; the loop needs the answer to retire the slot.
            self.pending.push_back(Event::SendResult(ldmux_mesh::messages::SendResult {
                seq,
                outcome: SendOutcome::SentDirect,
            }));
            // ...and says what is left of the hour, exactly as `mesh_serial` does after every send.
            if self.left != u8::MAX {
                self.left = self.left.saturating_sub(1);
                self.pending.push_back(Event::Status(ldmux_mesh::messages::Status {
                    radio_state: RadioState::Connected,
                    rssi: ldmux_mesh::UNKNOWN_RSSI,
                    battery_percent: ldmux_mesh::UNKNOWN_BATTERY,
                    tx_airtime_tenths_this_hour: 0,
                    ops_left_this_hour: Some(self.left),
                }));
            }
            Ok(())
        }

        async fn recv(&mut self) -> Result<Event, MeshError> {
            if let Some(e) = self.pending.pop_front() {
                return Ok(e);
            }
            self.events.recv().await.ok_or(MeshError::Closed)
        }
    }

    const E2E_PSK: [u8; 32] = [0x33; 32];

    fn hex_key(hex_text: &str) -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex_text[i * 2..i * 2 + 2], 16).unwrap();
        }
        k
    }

    fn e2e_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ldmux-l3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// **A spent hour stops new output and keeps the link.** The carrier publishes a countdown;
    /// the loop must hand the ARQ at most that many *new* records, while retransmissions and acks
    /// still pass (the ARQ does not charge them). Three shells' worth of output is queued against a
    /// budget of one, so exactly one new record may leave.
    #[tokio::test]
    async fn a_published_budget_bounds_new_records() {
        let dir = e2e_dir("budget");
        let setup = SessionSetup::new(&dir, E2E_PSK).unwrap();
        let host_public = hex_key(&setup.public_key_hex());
        let runtime = test_runtime();
        // The carrier reports the hour's last record, which is the only way the loop learns its
        // budget: a value written into the snapshot by hand does not reach `hour_left`.
        let (to_phone_tx, mut to_phone) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let carrier = AirCarrier {
            to_phone: to_phone_tx,
            events: event_rx,
            pending: std::collections::VecDeque::new(),
            // The first status below is what meters this test; until then, unmetered.
            left: u8::MAX,
        };
        let session_task = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                let mut setup = setup;
                let _ = session(
                    carrier,
                    Bound {
                        uplink_record_max: 172,
                        radio_state: RadioState::Connected,
                        budget: Budget::RadioGoverned,
                        airtime_ms: None,
                    },
                    &runtime,
                    Some(&mut setup),
                    Arc::new(AtomicBool::new(false)),
                )
                .await;
            }
        });

        // The budget report, as the serial carrier sends one after opening the radio.
        event_tx
            .send(Event::Status(ldmux_mesh::messages::Status {
                radio_state: RadioState::Connected,
                rssi: ldmux_mesh::UNKNOWN_RSSI,
                battery_percent: ldmux_mesh::UNKNOWN_BATTERY,
                tx_airtime_tenths_this_hour: 0,
                ops_left_this_hour: Some(1),
            }))
            .unwrap();

        // Handshake, so the session is established and the budget path is live.
        let phone_keys = ldmux_session::handshake::Keypair::generate().unwrap();
        let wall = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let (init, m1) =
            ldmux_session::handshake::Initiator::start(&phone_keys, &host_public, &E2E_PSK, wall)
                .unwrap();
        event_tx.send(Event::Recv(m1)).unwrap();
        let m2 = tokio::time::timeout(Duration::from_secs(5), to_phone.recv())
            .await
            .expect("message 2")
            .expect("open");
        let mut phone = ldmux_session::Session::new(init.finish(&m2).unwrap(), 172, 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.snapshot().session.state != "established" {
            assert!(Instant::now() < deadline, "never established");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Drain anything the handshake left on the wire.
        while to_phone.try_recv().is_ok() {}

        // Three shells want to send; the budget says one record of new traffic.
        for shell in 0..3u8 {
            runtime.send(shell, b"payload").unwrap();
        }
        let settle = Instant::now() + Duration::from_secs(3);
        let mut left = Vec::new();
        while Instant::now() < settle {
            while let Ok(rec) = to_phone.try_recv() {
                left.push(rec);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The record is sealed, so count what the *phone* can decode as data rather than by shape.
        let mut data = 0usize;
        let mut now = 4_000u64;
        for rec in &left {
            if let Ok(ldmux_session::Inbound::Data(d)) = phone.receive(now, rec) {
                data += d.len();
            }
            now += 4_000;
        }
        assert!(
            data <= 1,
            "the budget was one record and {data} arrived: {}",
            left.len()
        );

        // **The hour rolls over and the link comes back.** A fresh countdown is the only thing that
        // can reopen a spent budget, and it must not require a reconnect: the carrier republishes
        // `Status` on a timer for exactly this, because a status published only after a
        // transmission could never arrive while nothing was being transmitted. Measured on this
        // bench 2026-11-06: `records 0 left this hour` with the loop `held back 3959 times`, and a
        // phone sitting on "the host has not described a terminal yet" over a healthy session.
        event_tx
            .send(Event::Status(ldmux_mesh::messages::Status {
                radio_state: RadioState::Connected,
                rssi: ldmux_mesh::UNKNOWN_RSSI,
                battery_percent: ldmux_mesh::UNKNOWN_BATTERY,
                tx_airtime_tenths_this_hour: 0,
                ops_left_this_hour: Some(4),
            }))
            .unwrap();
        let settle = Instant::now() + Duration::from_secs(3);
        let mut after = Vec::new();
        while Instant::now() < settle {
            while let Ok(rec) = to_phone.try_recv() {
                after.push(rec);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut reopened = 0usize;
        for rec in &after {
            if let Ok(ldmux_session::Inbound::Data(d)) = phone.receive(now, rec) {
                reopened += d.len();
            }
            now += 4_000;
        }
        assert!(
            reopened > 0,
            "a refreshed countdown must let the queued output leave: {} records arrived",
            after.len()
        );

        session_task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The whole L3 path over a carrier**: handshake, then one record each way.
    ///
    /// This is what the gateway-based integration tests covered, ported to the seam that
    /// exists now. It fails if the handshake is never answered, if a sealed record does not
    /// reach the runtime's inbound queue, or if a record queued for the phone never leaves.
    #[tokio::test]
    async fn a_handshake_and_a_record_each_way_cross_the_carrier() {
        let dir = e2e_dir("e2e");
        let setup = SessionSetup::new(&dir, E2E_PSK).unwrap();
        let host_public = hex_key(&setup.public_key_hex());
        let runtime = test_runtime();

        let (to_phone_tx, mut to_phone) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let carrier = AirCarrier {
            to_phone: to_phone_tx,
            events: event_rx,
            pending: std::collections::VecDeque::new(),
            // The first status below is what meters this test; until then, unmetered.
            left: u8::MAX,
        };
        let session_task = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                let mut setup = setup;
                let _ = session(
                    carrier,
                    Bound {
                        uplink_record_max: 172,
                        radio_state: RadioState::Connected,
                        budget: Budget::RadioGoverned,
                        airtime_ms: None,
                    },
                    &runtime,
                    Some(&mut setup),
                    Arc::new(AtomicBool::new(false)),
                )
                .await;
            }
        });

        // Message 1 in, message 2 out.
        let phone_keys = ldmux_session::handshake::Keypair::generate().unwrap();
        // The host refuses a handshake whose timestamp is outside its clock window, so the
        // phone's timestamp has to be the real one, exactly as a phone's would be.
        let wall_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let (init, m1) = ldmux_session::handshake::Initiator::start(
            &phone_keys,
            &host_public,
            &E2E_PSK,
            wall_ms,
        )
        .unwrap();
        event_tx.send(Event::Recv(m1)).unwrap();
        let m2 = tokio::time::timeout(Duration::from_secs(5), to_phone.recv())
            .await
            .expect("the daemon answers message 1")
            .expect("the carrier is open");
        let mut phone = ldmux_session::Session::new(init.finish(&m2).unwrap(), 172, 0).unwrap();
        let established = Instant::now() + Duration::from_secs(5);
        while runtime.snapshot().session.state != "established" {
            assert!(Instant::now() < established, "the session never established");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Phone -> daemon.
        phone.send(7, b"hello host").unwrap();
        let mut now = 4_000u64;
        let limits = ldmux_session::arq::Limits {
            records_left: 255,
            l2_slots: 8,
        };
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.is_empty() {
            assert!(Instant::now() < deadline, "the daemon never received the record");
            for rec in phone.poll(now, limits).unwrap() {
                event_tx.send(Event::Recv(rec)).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            for (shell, bytes) in runtime.take_inbound() {
                assert_eq!(shell, 7);
                got.extend(bytes);
            }
            now += 4_000;
        }
        assert_eq!(got, b"hello host");

        // Daemon -> phone.
        runtime.send(7, b"hello phone").unwrap();
        let mut reply = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while reply.is_empty() {
            assert!(Instant::now() < deadline, "the daemon never sent the record");
            let rec = tokio::time::timeout(Duration::from_millis(200), to_phone.recv())
                .await
                .ok()
                .flatten();
            if let Some(rec) = rec
                && let Ok(ldmux_session::Inbound::Data(d)) = phone.receive(now, &rec)
            {
                reply.extend(d.into_iter().flat_map(|x| x.payload));
            }
            now += 4_000;
        }
        assert_eq!(reply, b"hello phone");

        session_task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pairing_keys_are_this_daemons_public_key_and_the_configured_secret() {
        let dir = std::env::temp_dir().join(format!("ldmux-pairing-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let psk = [0x5au8; 32];
        let setup = SessionSetup::new(&dir, psk).unwrap();
        let keys = setup.pairing_keys();
        assert_eq!(keys.psk, psk);
        assert_eq!(hex(&keys.host_key), setup.public_key_hex());
        // A second load reads the same key from disk, so a re-printed QR does not change it.
        let again = SessionSetup::new(&dir, psk).unwrap();
        assert_eq!(again.pairing_keys().host_key, keys.host_key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_psk_is_persisted_and_survives_without_the_environment() {
        let dir = std::env::temp_dir().join(format!("ldmux-mesh-psk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = "ab".repeat(32);
        let setup = SessionSetup::from_value(&dir, Some(&text))
            .unwrap()
            .unwrap();
        assert_eq!(setup.pairing_keys().psk, [0xab; 32]);
        let again = SessionSetup::from_value(&dir, None).unwrap().unwrap();
        assert_eq!(again.pairing_keys().psk, [0xab; 32]);
        assert_eq!(
            std::fs::metadata(dir.join("mesh-psk")).unwrap().len(),
            32,
            "the secret is stored as 32 bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mesh_pairing_with_no_session_is_an_error_that_names_the_variable() {
        // No runtime is installed in a unit test, so this is the no-session answer.
        let e = pairing_json().unwrap_err();
        assert!(e.contains("LDMUX_MESH_PSK"), "{e}");
    }

    #[test]
    fn no_variables_means_disabled() {
        assert_eq!(MeshConfig::from_pairs(Vec::new()), Ok(None));
    }

    #[test]
    fn a_serial_device_is_the_whole_configuration() {
        // One setting, because one process owns the radio: no address to dial, no token to
        // present, no peer to bind.
        let cfg = MeshConfig::from_pairs([("LDMUX_MESH_SERIAL", "/dev/ttyACM0")])
            .expect("a device is the configuration")
            .expect("some");
        assert_eq!(cfg.serial, "/dev/ttyACM0");
    }

    #[test]
    fn an_empty_serial_device_is_refused_rather_than_ignored() {
        let e = MeshConfig::from_pairs([("LDMUX_MESH_SERIAL", "  ")]).unwrap_err();
        assert!(e.contains("LDMUX_MESH_SERIAL"), "{e}");
    }

    #[test]
    fn the_disabled_shape_says_disabled_and_null_not_zero() {
        let json = MeshRuntime::disabled_json();
        assert_eq!(json["state"], "disabled");
        assert!(json["uplinkRecordMax"].is_null());
        assert!(json["recordsLeft"].is_null());
        // The sentinels, not a plausible reading.
        assert_eq!(json["rssi"], -128);
        assert_eq!(json["batteryPercent"], 255);
        assert!(json.get("hostRadio").is_some(), "the shape is the same when disabled");
    }

    #[test]
    fn a_snapshot_serialises_every_field_the_cli_prints() {
        let cfg = MeshConfig {
            serial: "/dev/ttyACM0".to_owned(),
            width_marker: None,
            airtime_record: None,
        };
        let snapshot = MeshSnapshot::new(&cfg);
        let json = snapshot.to_json();
        for key in [
            "state",
            "serial",
            "uplinkRecordMax",
            "recordsLeft",
            "radioState",
            "rssi",
            "batteryPercent",
            "txAirtimeTenths",
            "lastError",
            "reconnects",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        assert_eq!(json["serial"], "/dev/ttyACM0");
        // Not status: whether this host knows its own radio decides whether a pairing code
        // it prints can be used at all, so `mesh status` answers it beside the link.
        assert_eq!(json["hostRadio"], serde_json::Value::Null);
    }

    /// **The carrier's countdown is the meter, and a missing one is not zero.**
    ///
    /// Before the first status arrives there is no countdown, and the loop must not read that as
    /// "nothing may be sent": the reported failure this guards is a paired phone retrying `attach`
    /// for ever against a host whose status read `budget 0 records left this hour` because a missing
    /// budget had been taken as a spent one. Once the carrier publishes a countdown, that is what
    /// bounds new records — the firmware's own factor is 1.0, a 50 % limiter, which cannot stand in
    /// for the 1 % a European band allows.
    #[test]
    fn the_carrier_publishes_the_meter_and_a_missing_one_is_not_zero() {
        assert_eq!(poll_budget(Budget::RadioGoverned, None), u8::MAX);
        assert_eq!(poll_budget(Budget::RadioGoverned, Some(0)), 0);
        assert_eq!(poll_budget(Budget::RadioGoverned, Some(17)), 17);
        assert_eq!(poll_budget(Budget::RadioGoverned, Some(255)), 255);
    }
}
