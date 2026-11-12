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

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ldmux_mesh::carrier::{Bound, Event};
use ldmux_mesh::constants::{RadioState, SendOutcome};

/// The longest this loop will sleep with a session attached. See the comment at the wait.
const MAX_IDLE_WAIT: Duration = Duration::from_millis(50);

/// How often a waiting command's pane is read.
///
/// The settle engine's own windows are hundreds of milliseconds, so a pass that slept longer would
/// answer late; a pass that slept shorter would read the keeper for nothing.
const SETTLE_TICK: Duration = Duration::from_millis(50);


/// How long opening the radio, or writing one record to it, may take.
///
/// The port is local, so seconds are already generous. Without a bound, a radio that accepted the
/// port and then stopped draining left the daemon waiting forever: no error, no backoff, no
/// reconnect, and a `mesh.status` that said "handshaking" for the life of the process.
/// How long an open may take, and it has to cover the whole of it.
///
/// `prepare` runs on a blocking task that cannot be cancelled, and it holds the port's `flock`
/// while it works: `APP_START` (5 s), `DEVICE_QUERY` (5 s), the one-off width write (5 s) and the
/// scope read (1.5 s) are 16.5 s worst case. A ten-second bound fired inside that window abandoned
/// the task with the lock still held, so the next attempt failed with "another process holds this
/// radio" — the wrong cause for the wrong symptom.
const CARRIER_TIMEOUT: Duration = Duration::from_secs(30);

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
use ldmux_protocol::mesh_term::{Decoder, Message};
use ldmux_session::endpoint::{
    Admitted, ArrivalInfo, EndpointConfig, Outbound, RadioEndpoint, Role,
};
use ldmux_session::handshake::PairingEntry;
use tokio::sync::Notify;

use crate::mesh_bridge::DaemonHost;
use crate::mesh_identity::{SessionFiles, SessionSnapshot};
use crate::mesh_rpc::{MeshRpc, Outcome, PEER_LINK};

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

    /// Where the hour's spent airtime is recorded, so a restart resumes the same hour.
    pub airtime_record: Option<std::path::PathBuf>,
    /// Where the **peer's radio key** is remembered, so a restarted daemon can ask the phone for a
    /// route instead of waiting for its next advert.
    pub peer_record: Option<std::path::PathBuf>,
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

                airtime_record: None,
                peer_record: None,
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
    /// [`crate::mesh_identity::adopt_legacy`].
    pub fn from_env(persist_dir: &Path, legacy_dir: &Path) -> Result<Option<Self>, String> {
        let env = std::env::var("LDMUX_MESH_PSK").ok();
        let moved = crate::mesh_identity::adopt_legacy(legacy_dir, persist_dir);
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
                crate::mesh_identity::write_private(&path, &psk)
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
    /// The flood scope this node's broadcasts are scoped to, with the key that proves them.
    ///
    /// **The third thing two radios can disagree about that produces no error.** Two ends on
    /// different scopes still hear each other's direct records — repeater region checks gate floods
    /// only — but neither one's discovery floods cross, so neither can learn a route. Reported here
    /// because the app compares itself against this line and an operator has nowhere else to look.
    pub flood_scope: Option<ldmux_mesh::ScopeInfo>,
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
    /// How long one full record takes on the air at this radio's tuple, when the carrier knows it.
    ///
    /// The session scales its retransmission timers by it, and the bridge paces a synced line's
    /// echo by it: a deferral is only worth making when it is shorter than the record it saves.
    pub airtime_ms: Option<u32>,
    /// How many repeaters the route to the peer uses, when one is known.
    pub peer_hops: Option<u8>,
    /// The radio's own reading of the air: noise floor, last packet's SNR and RSSI, and its
    /// transmit/receive airtime since boot.
    pub snr_quarter_db: Option<i8>,
    pub noise_floor_dbm: Option<i16>,
    pub radio_rssi: Option<i8>,
    /// Whether `radio_rssi` is the peer's last record rather than whatever the radio heard last.
    pub signal_from_peer: bool,
    pub radio_tx_secs: Option<u32>,
    pub radio_rx_secs: Option<u32>,
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
            flood_scope: None,
            tx_power_dbm: None,
            serial: Some(cfg.serial.clone()),
            uplink_record_max: None,
            records_left: None,
            airtime_ms: None,
            peer_hops: None,
            snr_quarter_db: None,
            noise_floor_dbm: None,
            signal_from_peer: false,
            radio_rssi: None,
            radio_tx_secs: None,
            radio_rx_secs: None,
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
            "peerHops": self.peer_hops,
            // The airtime of one record at this tuple, so the CLI can price the route: `2 × hops ×
            // airtime` is one exchange, and that is the figure that says whether typing is possible.
            "airtimeMs": self.airtime_ms,
            "snrDb": self.snr_quarter_db.map(|q| f32::from(q) / 4.0),
            "noiseFloorDbm": self.noise_floor_dbm,
            "radioRssi": self.radio_rssi,
            "signalFromPeer": self.signal_from_peer,
            "radioTxSecs": self.radio_tx_secs,
            "radioRxSecs": self.radio_rx_secs,
            "radioState": self.radio_state.map(|s| s.code()),
            // The tuple the host's own radio reported (or was retuned to). Absent before the
            // radio has been identified.
            "radioPreset": self
                .radio_preset
                .map(|(f, b, sf, cr)| vec![u64::from(f), u64::from(b), u64::from(sf), u64::from(cr)]),
            // The width the host's adverts carry: the other half of "the same tuple" when a
            // peer's advert path never resolves.
            "pathHashSize": self.path_hash_size,
            "floodScope": self.flood_scope.as_ref().map(|s| serde_json::json!({
                "name": s.name,
                "key": s.key_hex(),
            })),
            "txPowerDbm": self.tx_power_dbm,
            "rssi": self.rssi,
            "batteryPercent": self.battery_percent,
            "txAirtimeTenths": self.tx_airtime_tenths,
            "lastError": self.last_error,
            "reconnects": self.reconnects,
            "session": self.session.to_json(),
            "bridge": {
                "messagesSent": self.bridge.messages_sent,
                "inputs": self.bridge.inputs,
                "keys": self.bridge.keys,
                "resizes": self.bridge.resizes,
                "malformed": self.bridge.malformed,
                "hostUnanswered": self.bridge.host_unanswered,
                "releaseFailed": self.bridge.release_failed,
            },
        })
    }
}

/// A retune asked for: frequency in kHz, bandwidth in Hz, spreading factor, coding rate.
///
/// The same four numbers the radio reports and the same four the command carries, so a request and
/// a report are comparable without conversion.
/// A retune request: the channel's four numbers, and optionally the path hash width in bytes.
///
/// The width is not part of the channel tuple: it travels **in each packet** (`getPathHashSize`
/// reads it from the path-length byte, and a forwarder appends with that width), so a two-byte
/// node and a one-byte repeater interoperate. MeshCore's public presets are one byte, which is
/// why ldmux follows the radio rather than writing a width, and the field here is what an operator
/// may still choose explicitly.
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
    wake: Arc<Notify>,
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
    /// Never fails: a link that is down is a state, and the daemon must start whether or not a
    /// radio is attached. Failures land in the snapshot. `host` is the daemon the service asks
    /// through; `session` is the pairing, or `None` for a radio with no session configured.
    pub fn start_with_session(
        cfg: MeshConfig,
        session: Option<SessionSetup>,
        host: DaemonHost,
    ) -> Self {
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
            wake: Arc::new(Notify::new()),
            preset_request: Arc::new(Mutex::new(None)),
        };
        tokio::spawn(supervise(cfg, session, runtime.clone(), host));
        runtime
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
async fn supervise(
    cfg: MeshConfig,
    mut setup: Option<SessionSetup>,
    runtime: MeshRuntime,
    host: DaemonHost,
) {
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
        match link(
            &cfg,
            &runtime,
            setup.as_mut(),
            Arc::clone(&bound),
            host.clone(),
        )
        .await
        {
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
            s.peer_hops = None;
            // A reading belongs to the radio that was open; a reopen has to measure again.
            s.snr_quarter_db = None;
            s.noise_floor_dbm = None;
            s.radio_rssi = None;
            s.radio_tx_secs = None;
            s.radio_rx_secs = None;
            // The scope belongs to the radio that was open; a reopen has to report it again before
            // `mesh status` claims one.
            s.flood_scope = None;
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
/// One carrier, and it is the one that ships: the daemon opens the serial device, reads the tuple
/// the radio reports, publishes it (the phone must match it), and hands the carrier to the loop.
async fn link(
    cfg: &MeshConfig,
    runtime: &MeshRuntime,
    setup: Option<&mut SessionSetup>,
    bound: Arc<AtomicBool>,
    host: DaemonHost,
) -> Result<(), MeshError> {
    set(&runtime.snapshot, |s| s.state = "handshaking");
    let opened = tokio::time::timeout(
        CARRIER_TIMEOUT,
        crate::mesh_serial::open_radio(
            &cfg.serial,
            cfg.airtime_record.clone(),
            cfg.peer_record.clone(),
        ),
    )
    .await
    .map_err(|_| MeshError::Timeout("the radio did not open"))??;
    let crate::mesh_serial::Opened {
        carrier,
        bound: promise,
        preset,
        scope,
        peer_key: _,
    } = opened;
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
        s.flood_scope = scope;
    });
    session(
        carrier,
        promise,
        runtime,
        setup,
        bound,
        host,
        preset,
    )
    .await
}

/// The link's channel for each shell, allocated by the loop.
///
/// **A channel is an internal slot, not a wire fact** (`AR-11`). A version-15 message names its
/// shell, and this map is what turns that name into the index `MeshRpc` keys its per-shell state by
/// — the screen identity, the pending command, the attachment. A phone that re-attaches to a shell
/// it already holds keeps its slot, so its screen identity survives and the answer is a diff rather
/// than a repaint.
#[derive(Default)]
struct Channels {
    by_id: std::collections::HashMap<String, u8>,
    next: u8,
}

impl Channels {
    /// The slot for a shell, allocating one the first time it is seen.
    fn slot(&mut self) -> u8 {
        let slot = self.next;
        self.next = self.next.wrapping_add(1);
        slot
    }

    fn get(&self, id: &str) -> Option<u8> {
        self.by_id.get(id).copied()
    }

    fn bind(&mut self, id: &str, slot: u8) {
        self.by_id.insert(id.to_owned(), slot);
    }
}

/// One connected session, from handshake to close.
///
/// **The loop is a poll, a send and a delivery.** `RadioEndpoint` owns the handshake, the replay
/// window, the transaction layer, the retries, the acks and the route table; `MeshRpc` owns the
/// at-most-once ring, the chunk assemblies and the settle engine; `DaemonHost` owns the keeper and
/// the claim table. Nothing here decides a retry, a timeout or a byte layout — a second copy of any
/// of those is how the old stack came to have two answers to every question
/// (`docs/RADIO-PLAN.md` §10a).
async fn session<C, H>(mut carrier: C, promise: Bound, runtime: &MeshRuntime, setup: Option<&mut SessionSetup>, bound: Arc<AtomicBool>, mut host: H,
    preset: crate::mesh_serial::RadioPreset,
) -> Result<(), MeshError>
where
    C: ldmux_session::endpoint::CarriageSink,
    H: crate::mesh_rpc::MeshHost,
{
    let snapshot = &runtime.snapshot;
    bound.store(true, Ordering::SeqCst);
    set(snapshot, |s| {
        s.state = "ready";
        s.uplink_record_max = Some(promise.uplink_record_max);
        s.airtime_ms = promise.airtime_ms.map(|a| a.round() as u32);
        s.radio_state = Some(promise.radio_state);
        s.last_error = None;
    });

    let Some(setup) = setup else {
        // No session configured: the radio is open and idle. Its readings still reach the snapshot,
        // and nothing is ever dispatched, because there is no key to open a record with.
        return idle(carrier, runtime).await;
    };

    let pairing = setup.files.current_pairing();
    let mut persist = setup.files.persister();
    let peer_static = pairing.pinned.unwrap_or([0u8; 32]);
    let config = EndpointConfig::new(Role::Host, peer_static, pairing.psk)
        .with_airtime_ms(promise.airtime_ms.unwrap_or(602.6))
        .with_path_width(preset.hash_size)
        .with_sf(preset.sf)
        // The endpoint only needs to know *whether* floods are scoped, for the record budget; the
        // scope key itself is the carrier's, and it is already in `CarrierSettings`.
        .with_scope(preset.hash_size.checked_mul(0).map(|_| 0u16))
        .with_pairings(vec![pairing.clone()]);
    let mut endpoint = RadioEndpoint::host(config, setup.files.clone_key());
    let mut rpc = MeshRpc::new();
    let mut channels = Channels::default();
    let started = runtime_start();
    let mut last_carrier = Instant::now();
    let mut stats = crate::mesh_bridge::BridgeStats::default();

    loop {
        if runtime.reset_pin.swap(false, Ordering::SeqCst) {
            if let Err(e) = setup.files.clear_pin() {
                set(snapshot, |s| {
                    s.last_error = Some(format!("could not clear the pinned key: {e}"));
                });
            }
            set(snapshot, |s| s.session.peer_key = None);
        }
        // **A retune is the radio's own, so the link task does it.** The firmware validates the
        // tuple and either retunes (and saves it) or refuses, and the snapshot says which.
        if let Some(want) = runtime.take_preset() {
            apply_retune(&mut carrier, snapshot, want).await;
        }

        // **What this end owes the air, decided once and handed over.** The endpoint stages a
        // bootstrap initiation, a queued record whose instant has come, and new transaction work, in
        // that order; a call with nothing due does no work.
        let now_ms = elapsed_ms(started);
        let report = endpoint.poll(now_ms, &mut carrier).await?;
        for out in &report {
            if let Outbound::TimedOut { tx_id } = out {
                set(snapshot, |s| {
                    s.last_error = Some(format!("message {tx_id} ran out of attempts"));
                });
            }
        }

        // **Deliver what arrived.** One message is one command: the transaction layer reassembled
        // it, so a partial command never reaches a pty.
        for bytes in endpoint.take_inbound() {
            dispatch(
                &bytes,
                &mut rpc,
                &mut channels,
                &mut host,
                &mut endpoint,
                &mut stats,
                snapshot,
                now_ms,
            )
            .await;
        }

        // **Answer every command whose pane has settled.** Driven from the same pass as everything
        // else, so a settled command's answer leaves on the next pass rather than on a timer of its
        // own. A shell with nothing waiting costs one map lookup.
        let busy: Vec<u8> = channels
            .by_id
            .values()
            .copied()
            .filter(|slot| rpc.busy(*slot))
            .collect();
        for slot in busy {
            let Some(id) = rpc.attached_to(slot).map(str::to_owned) else {
                rpc.forget_shell(slot);
                continue;
            };
            if let Some(messages) = rpc.settle(slot, &id, &mut host, Instant::now()).await {
                submit(&mut endpoint, &messages, now_ms, &mut stats, snapshot);
            }
        }

        // **A pin is persisted the moment the handshake produces one.** The endpoint computes it and
        // cannot write the file; a host that accepted a phone and never pinned it would let anyone
        // holding the PSK re-negotiate the pairing.
        if let Some((key, timestamp)) = endpoint.take_pin() {
            let entry = PairingEntry {
                psk: pairing.psk,
                pinned: Some(key),
                last_timestamp_ms: timestamp,
            };
            if let Err(e) = persist(&entry) {
                set(snapshot, |s| {
                    s.last_error = Some(format!("could not record the pairing: {e}"));
                });
            }
            set(snapshot, |s| {
                s.session.peer_key = Some(hex(&key));
                s.session.state = "established";
            });
        }
        set(snapshot, |s| {
            s.session.state = if endpoint.established() {
                "established"
            } else {
                "waiting"
            };
            s.session.pending = endpoint.queued();
            s.bridge = stats;
        });

        // **Silence with something outstanding is the end of this connection.** The endpoint's own
        // ladder fails a message after its attempts; this is the backstop for a carriage that stops
        // delivering *anything*, including the answers the ladder is waiting for.
        if endpoint.queued() > 0 && last_carrier.elapsed() >= CARRIER_SILENCE {
            return Err(MeshError::Timeout("the carriage stopped answering"));
        }

        // **Wait for the next thing, not for a tick.** An idle radio waits for a notification, a
        // deadline the endpoint named, or the settle engine's own quiet window.
        let wait = wait_for(&endpoint, &rpc, &channels, started);
        tokio::select! {
            biased;
            event = carrier.recv() => {
                last_carrier = Instant::now();
                on_event(event?, &mut endpoint, snapshot, started);
            }
            () = tokio::time::sleep(wait) => {}
            () = runtime.wake.notified() => {}
        }
    }
}

/// Milliseconds since the loop started, which is the clock every deadline is measured on.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// How long the loop may sleep before it must look again.
///
/// The earliest of: the endpoint's own next deadline, and the settle engine's quiet window while a
/// command waits. A loop with nothing to do sleeps for [`MAX_IDLE_WAIT`] and is woken by the carrier
/// or by a retune.
fn wait_for(
    endpoint: &RadioEndpoint,
    rpc: &MeshRpc,
    channels: &Channels,
    started: Instant,
) -> Duration {
    let now = elapsed_ms(started);
    let mut wait = endpoint
        .next_deadline_ms()
        .map_or(MAX_IDLE_WAIT, |d| Duration::from_millis(d.saturating_sub(now)))
        .min(MAX_IDLE_WAIT);
    if channels.by_id.values().any(|slot| rpc.busy(*slot)) {
        // A waiting command is read once per pass; the settle engine's own windows are tens of
        // milliseconds, so a pass that slept longer would answer late.
        wait = wait.min(SETTLE_TICK);
    }
    wait
}

/// Apply a retune the operator asked for, and publish what the radio said.
async fn apply_retune<C: ldmux_session::endpoint::CarriageSink>(
    carrier: &mut C,
    snapshot: &Arc<Mutex<MeshSnapshot>>,
    want: Retune,
) {
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
            // **The channel is the tuple *and* the width.** A retune that only moved the frequency
            // left a node on the public channel naming its neighbours in two bytes, which is a path
            // no public repeater builds. Reported apart from the tuple because the two can fail
            // independently.
            if let Some(bytes) = path_bytes {
                match carrier.set_path_width(bytes).await {
                    Ok(()) => set(snapshot, |s| s.path_hash_size = Some(bytes)),
                    Err(e) => set(snapshot, |s| s.last_error = Some(e.to_string())),
                }
            }
            // **Power is the third register**, separate from the tuple and the width, and global to
            // the radio: whichever app writes it last wins.
            if let Some(dbm) = power_dbm {
                match carrier.set_tx_power(dbm).await {
                    Ok(()) => set(snapshot, |s| s.tx_power_dbm = Some(dbm)),
                    Err(e) => set(snapshot, |s| s.last_error = Some(e.to_string())),
                }
            }
        }
        Err(e) => set(snapshot, |s| s.last_error = Some(e.to_string())),
    }
}

/// One carrier event, applied to the endpoint and the snapshot.
fn on_event(
    event: Event,
    endpoint: &mut RadioEndpoint,
    snapshot: &Arc<Mutex<MeshSnapshot>>,
    started: Instant,
) {
    match event {
        Event::Signal(reading) => {
            // **What the radio measured**, kept in the snapshot so `mesh status` prints a reading
            // instead of "no reading".
            set(snapshot, |s| {
                s.snr_quarter_db = Some(reading.snr_quarter_db);
                s.noise_floor_dbm = Some(reading.noise_floor_dbm);
                s.radio_rssi = reading.rssi_dbm;
                s.signal_from_peer = reading.from_peer;
                s.radio_tx_secs = Some(reading.tx_air_secs);
                s.radio_rx_secs = Some(reading.rx_air_secs);
            });
        }
        Event::Limit { record_max, hops } => {
            set(snapshot, |s| {
                s.uplink_record_max = Some(record_max);
                s.peer_hops = Some(hops);
            });
        }
        Event::Scope(scope) => set(snapshot, |s| s.flood_scope = scope),
        Event::Status(status) => {
            // **The carrier is the accountant, and its figure replaces ours.** It counts what it
            // actually transmitted — retransmissions and acks included — and that is the 1 % the
            // band allows. `None` is "this band has no duty cycle", which is not "zero left".
            endpoint.set_records_left(status.ops_left_this_hour.unwrap_or(u8::MAX));
            set(snapshot, |s| {
                s.radio_state = Some(status.radio_state);
                s.rssi = status.rssi;
                s.battery_percent = status.battery_percent;
                s.tx_airtime_tenths = status.tx_airtime_tenths_this_hour;
                s.records_left = status.ops_left_this_hour;
            });
        }
        Event::Recv {
            record,
            path,
            flood,
            snr_db,
        } => {
            let hops = u8::try_from(path.len()).unwrap_or(u8::MAX);
            let info = if flood {
                ArrivalInfo::flood(path.clone(), 1, snr_db)
            } else if path.is_empty() {
                ArrivalInfo::direct(snr_db)
            } else {
                ArrivalInfo::in_transit(hops, snr_db)
            };
            let now = elapsed_ms(started);
            match endpoint.on_record(&record, &info, now, now_unix_ms()) {
                Admitted::Answered { .. } => {}
                Admitted::Ignored(_) | Admitted::Message { .. } | Admitted::Part { .. } => {}
                Admitted::Ack | Admitted::AnswerAccepted { .. } | Admitted::HandshakeRequested => {}
            }
        }
        Event::SendResult(result) => {
            if matches!(
                result.outcome,
                SendOutcome::ErrBusy | SendOutcome::ErrNotFound | SendOutcome::ErrTooLong
            ) {
                set(snapshot, |s| {
                    s.last_error = Some(format!(
                        "the radio refused record {} ({:?})",
                        result.seq, result.outcome
                    ));
                });
            }
        }
    }
}

/// Decode one delivered message and let the service answer it.
#[allow(clippy::too_many_arguments)]
async fn dispatch<H: crate::mesh_rpc::MeshHost>(
    bytes: &[u8],
    rpc: &mut MeshRpc,
    channels: &mut Channels,
    host: &mut H,
    endpoint: &mut RadioEndpoint,
    stats: &mut crate::mesh_bridge::BridgeStats,
    snapshot: &Arc<Mutex<MeshSnapshot>>,
    now_ms: u64,
) -> Vec<Message> {
    let mut answers = Vec::new();
    let mut decoder = Decoder::new();
    let messages = match decoder.push(bytes) {
        Ok(list) => list,
        Err(_) => {
            stats.malformed = stats.malformed.saturating_add(1);
            return answers;
        }
    };
    for decoded in messages {
        let Ok(message) = decoded else {
            stats.malformed = stats.malformed.saturating_add(1);
            continue;
        };
        // **Which slot this message belongs to.** `Attach` and `Takeover` name a selector rather
        // than a shell, so they take a fresh slot and the service says which shell it resolved to;
        // every other message names its shell, and an unknown one is a phone that is not attached.
        let slot = match &message {
            Message::Attach { .. } | Message::Takeover { .. } => None,
            Message::Send { shell, .. } | Message::Fetch { shell, .. } => channels.get(shell),
            // **`WantHistory` and `Detach` name no shell.** The version-15 message set has no field
            // for it (`crates/session/src/client.rs::close` says why: adding one would be a wire
            // change), so they apply to the shell this link attached most recently — which is
            // unambiguous for the one-page case the radio route ships with.
            Message::WantHistory { .. } | Message::Detach => channels.by_id.values().copied().max(),
            _ => None,
        };
        let slot = match slot {
            Some(slot) => slot,
            None => match &message {
                Message::Attach { .. } | Message::Takeover { .. } => channels.slot(),
                // A command for a shell this link never attached: the phone is told so rather than
                // left waiting for a screen that belongs to no pane.
                Message::Send {
                    op_id,
                    have_snapshot_id,
                    ..
                } => {
                    let refusal = MeshRpc::refusal(
                        *op_id,
                        *have_snapshot_id,
                        ldmux_protocol::mesh_term::applied::NO_SHELL,
                    );
                    submit(endpoint, std::slice::from_ref(&refusal), now_ms, stats, snapshot);
                    answers.push(refusal);
                    continue;
                }
                _ => continue,
            },
        };
        let outcome = rpc
            .on_message(slot, PEER_LINK, &message, host, Instant::now())
            .await;
        // An attach that resolved binds the shell to its slot, so the next `Send` finds it.
        if matches!(message, Message::Attach { .. } | Message::Takeover { .. })
            && let Some(id) = rpc.attached_to(slot).map(str::to_owned)
        {
            channels.bind(&id, slot);
        }
        match outcome {
            Ok(Outcome::Pending) => {}
            Ok(Outcome::Answer(messages)) => {
                submit(endpoint, &messages, now_ms, stats, snapshot);
                answers.extend(messages);
            }
            Err(status) => {
                if let Message::Send {
                    op_id,
                    have_snapshot_id,
                    ..
                } = &message
                {
                    let refusal = MeshRpc::refusal(*op_id, *have_snapshot_id, status);
                    submit(endpoint, std::slice::from_ref(&refusal), now_ms, stats, snapshot);
                    answers.push(refusal);
                }
            }
        }
    }
    answers
}

/// Hand a reply to the transaction layer, one message at a time.
fn submit(
    endpoint: &mut RadioEndpoint,
    messages: &[Message],
    now_ms: u64,
    stats: &mut crate::mesh_bridge::BridgeStats,
    snapshot: &Arc<Mutex<MeshSnapshot>>,
) {
    for message in messages {
        let Ok(bytes) = message.encode() else {
            stats.malformed = stats.malformed.saturating_add(1);
            continue;
        };
        match endpoint.submit(&bytes, now_ms) {
            Ok(_) => stats.messages_sent = stats.messages_sent.saturating_add(1),
            Err(e) => set(snapshot, |s| {
                s.last_error = Some(format!("the answer could not be queued: {e}"));
            }),
        }
    }
}

/// Serve a radio with no session configured: publish its readings, dispatch nothing.
async fn idle<C: ldmux_session::endpoint::CarriageSink>(
    mut carrier: C,
    runtime: &MeshRuntime,
) -> Result<(), MeshError> {
    let snapshot = &runtime.snapshot;
    loop {
        match carrier.recv().await? {
            Event::Status(status) => set(snapshot, |s| {
                s.radio_state = Some(status.radio_state);
                s.rssi = status.rssi;
                s.battery_percent = status.battery_percent;
                s.tx_airtime_tenths = status.tx_airtime_tenths_this_hour;
                s.records_left = status.ops_left_this_hour;
            }),
            Event::Signal(reading) => set(snapshot, |s| {
                s.snr_quarter_db = Some(reading.snr_quarter_db);
                s.noise_floor_dbm = Some(reading.noise_floor_dbm);
                s.radio_rssi = reading.rssi_dbm;
                s.radio_tx_secs = Some(reading.tx_air_secs);
                s.radio_rx_secs = Some(reading.rx_air_secs);
            }),
            Event::Scope(scope) => set(snapshot, |s| s.flood_scope = scope),
            Event::Limit { record_max, hops } => set(snapshot, |s| {
                s.uplink_record_max = Some(record_max);
                s.peer_hops = Some(hops);
            }),
            Event::Recv { .. } => set(snapshot, |s| {
                s.last_error = Some(
                    "a downlink record arrived; no session is configured (LDMUX_MESH_PSK)".to_owned(),
                );
            }),
            Event::SendResult(_) => {}
        }
    }
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
    use ldmux_protocol::mesh_term::{Message, VERSION, closed};

    /// A host that answers from memory, so the dispatch path can be driven without a daemon.
    #[derive(Default)]
    struct FakeHost {
        shells: Vec<String>,
        typed: Vec<(String, Vec<u8>)>,
        keyed: Vec<(String, Vec<String>)>,
        resized: Vec<(String, u8, u8)>,
        screen: Option<ldmux_protocol::mesh_term::Screen>,
    }

    impl crate::mesh_rpc::MeshHost for FakeHost {
        async fn screen(
            &mut self,
            _id: &str,
        ) -> Result<ldmux_protocol::mesh_term::Screen, String> {
            self.screen
                .clone()
                .ok_or_else(|| "no screen".to_owned())
        }
        async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
            self.typed.push((id.to_owned(), bytes.to_vec()));
            Ok(())
        }
        async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String> {
            self.keyed.push((id.to_owned(), names.to_vec()));
            Ok(())
        }
        async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String> {
            self.resized.push((id.to_owned(), cols, rows));
            Ok(())
        }
        async fn listing(
            &mut self,
        ) -> Option<Vec<ldmux_protocol::mesh_term::SessionSummary>> {
            Some(
                self.shells
                    .iter()
                    .map(|id| ldmux_protocol::mesh_term::SessionSummary {
                        id: id.clone(),
                        ..Default::default()
                    })
                    .collect(),
            )
        }
        async fn claim(
            &mut self,
            _id: &str,
            _takeover: bool,
        ) -> Result<crate::mesh_rpc::ClaimOutcome, String> {
            Ok(crate::mesh_rpc::ClaimOutcome::Claimed)
        }
        async fn release(&mut self, _id: &str) -> bool {
            true
        }
    }

    fn endpoint() -> RadioEndpoint {
        let config = EndpointConfig::new(Role::Host, [0x11; 32], [0x22; 32]);
        RadioEndpoint::host(
            config,
            ldmux_session::handshake::Keypair::from_parts([0x33; 32], [0x44; 32]),
        )
    }

    fn runtime() -> MeshRuntime {
        let cfg = MeshConfig {
            serial: "/dev/null".to_owned(),
            airtime_record: None,
            peer_record: None,
        };
        MeshRuntime {
            pairing_keys: None,
            pairing_files: None,
            reset_pin: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(Mutex::new(MeshSnapshot::new(&cfg))),
            wake: Arc::new(Notify::new()),
            preset_request: Arc::new(Mutex::new(None)),
        }
    }

    /// **A version-14 claim gets one `Closed { VERSION }` and nothing else.** A build that speaks
    /// the old frame loop cannot read a version-15 screen, and half-serving it is worse than saying
    /// so: the phone would draw a screen it cannot update.
    #[tokio::test]
    async fn a_version_14_attach_is_closed_with_the_version_reason() {
        let mut rpc = MeshRpc::new();
        let mut channels = Channels::default();
        let mut host = FakeHost {
            shells: vec!["s1".to_owned()],
            ..FakeHost::default()
        };
        let mut endpoint = endpoint();
        let mut stats = crate::mesh_bridge::BridgeStats::default();
        let runtime = runtime();
        let attach = Message::Attach {
            version: VERSION - 1,
            selector: "s1".to_owned(),
            cols: 80,
            rows: 24,
        };
        // **The dispatch's answer, not the carrier's queue.** An endpoint with no session refuses
        // `submit`, so asserting on `take_inbound` measured the carrier and not the decision this
        // test is named for (measured 2026-11-12: it read 0 answers and called that a missing
        // refusal).
        let answers = dispatch(
            &attach.encode().unwrap(),
            &mut rpc,
            &mut channels,
            &mut host,
            &mut endpoint,
            &mut stats,
            &runtime.snapshot,
            0,
        )
        .await;
        assert_eq!(answers.len(), 1, "one message, not a screen and a refusal");
        assert!(
            matches!(
                answers.first(),
                Some(Message::Closed { reason }) if *reason == closed::VERSION
            ),
            "a version-14 claim was not told why it was refused: {answers:?}"
        );
        assert!(
            host.typed.is_empty() && host.resized.is_empty(),
            "a refused claim touched the pane"
        );
    }

    /// **A command for a shell this link never attached is refused, not dropped.** The phone would
    /// otherwise wait for a screen that belongs to no pane.
    #[tokio::test]
    async fn a_send_with_no_attachment_is_refused_with_no_shell() {
        let mut rpc = MeshRpc::new();
        let mut channels = Channels::default();
        let mut host = FakeHost::default();
        let mut endpoint = endpoint();
        let mut stats = crate::mesh_bridge::BridgeStats::default();
        let runtime = runtime();
        let send = Message::Send {
            op_id: 7,
            chunk_idx: 0,
            chunk_count: 1,
            have_snapshot_id: 0,
            cols: 0,
            rows: 0,
            flags: 0,
            shell: "gone".to_owned(),
            items: vec![ldmux_protocol::mesh_term::SendItem::Text(b"ls".to_vec())],
        };
        // The dispatch's own answer, for the reason the version-14 test gives: an endpoint with no
        // session refuses `submit`, so the queue is empty whatever the decision was.
        let answers = dispatch(
            &send.encode().unwrap(),
            &mut rpc,
            &mut channels,
            &mut host,
            &mut endpoint,
            &mut stats,
            &runtime.snapshot,
            0,
        )
        .await;
        assert_eq!(answers.len(), 1, "the refusal is the whole answer");
        assert!(
            matches!(
                answers.first(),
                Some(Message::Snapshot { applied_status, .. })
                    if *applied_status == ldmux_protocol::mesh_term::applied::NO_SHELL
            ),
            "a command for an unattached shell was not refused: {answers:?}"
        );
        assert!(host.typed.is_empty(), "nothing was written to a pty");
    }

    /// **An attach binds the shell to a slot, and the next command finds it.** This is the whole of
    /// what replaced the page byte: the message names its shell, and the loop's map turns that name
    /// into the index the service keys its per-shell state by.
    #[tokio::test]
    async fn an_attach_binds_the_shell_and_a_send_reaches_it() {
        let mut rpc = MeshRpc::new();
        let mut channels = Channels::default();
        let mut host = FakeHost {
            shells: vec!["s1".to_owned()],
            screen: Some(ldmux_protocol::mesh_term::Screen::from_keeper(
                "hello", 80, 24, (0, 0), false, false, 0,
            )),
            ..FakeHost::default()
        };
        let mut endpoint = endpoint();
        let mut stats = crate::mesh_bridge::BridgeStats::default();
        let runtime = runtime();
        let attach = Message::Attach {
            version: VERSION,
            selector: "s1".to_owned(),
            cols: 80,
            rows: 24,
        };
        dispatch(
            &attach.encode().unwrap(),
            &mut rpc,
            &mut channels,
            &mut host,
            &mut endpoint,
            &mut stats,
            &runtime.snapshot,
            0,
        )
        .await;
        assert_eq!(
            channels.get("s1"),
            Some(0),
            "the attach did not bind the shell to a slot"
        );
        let send = Message::Send {
            op_id: 9,
            chunk_idx: 0,
            chunk_count: 1,
            have_snapshot_id: 0,
            cols: 80,
            rows: 24,
            flags: 0,
            shell: "s1".to_owned(),
            items: vec![ldmux_protocol::mesh_term::SendItem::Text(b"ls".to_vec())],
        };
        dispatch(
            &send.encode().unwrap(),
            &mut rpc,
            &mut channels,
            &mut host,
            &mut endpoint,
            &mut stats,
            &runtime.snapshot,
            0,
        )
        .await;
        assert_eq!(
            host.typed,
            vec![("s1".to_owned(), b"ls".to_vec())],
            "the command did not reach the shell it named"
        );
    }



    /// **Two shells are two slots, and a slot is decided by the message, not by a record.**
    ///
    /// Gate `B11` in `docs/RADIO-PLAN.md` §9. The old transport carried the page in a record
    /// header, so a command could only ever mean "the shell this record came from"; a version-15
    /// message names its shell, and this is the test that says the map turns that name into a slot
    /// and that two shells keep two independent screens. Without it, a second shell would be served
    /// by the first one's attachment — the failure `AR-11` predicted and this pins.
    #[tokio::test]
    async fn two_shells_are_two_slots_and_each_command_reaches_the_shell_it_names() {
        let mut rpc = MeshRpc::new();
        let mut channels = Channels::default();
        let mut host = FakeHost {
            shells: vec!["s1".to_owned(), "s2".to_owned()],
            screen: Some(ldmux_protocol::mesh_term::Screen::from_keeper(
                "hello", 80, 24, (0, 0), false, false, 0,
            )),
            ..FakeHost::default()
        };
        let mut endpoint = endpoint();
        let mut stats = crate::mesh_bridge::BridgeStats::default();
        let runtime = runtime();

        for shell in ["s1", "s2"] {
            let attach = Message::Attach {
                version: VERSION,
                selector: shell.to_owned(),
                cols: 80,
                rows: 24,
            };
            dispatch(
                &attach.encode().unwrap(),
                &mut rpc,
                &mut channels,
                &mut host,
                &mut endpoint,
                &mut stats,
                &runtime.snapshot,
                0,
            )
            .await;
        }
        assert_eq!(channels.get("s1"), Some(0));
        assert_eq!(channels.get("s2"), Some(1), "the second shell took a second slot");

        // One command for each, named. The second must not land on the first: two commands would
        // still be two writes, so the assertion is on *which* shell each one reached.
        for (op_id, shell) in [(1u32, "s1"), (2u32, "s2")] {
            let send = Message::Send {
                op_id,
                chunk_idx: 0,
                chunk_count: 1,
                have_snapshot_id: 0,
                cols: 80,
                rows: 24,
                flags: 0,
                shell: shell.to_owned(),
                items: vec![ldmux_protocol::mesh_term::SendItem::Text(
                    shell.as_bytes().to_vec(),
                )],
            };
            dispatch(
                &send.encode().unwrap(),
                &mut rpc,
                &mut channels,
                &mut host,
                &mut endpoint,
                &mut stats,
                &runtime.snapshot,
                0,
            )
            .await;
        }
        assert_eq!(
            host.typed,
            vec![
                ("s1".to_owned(), b"s1".to_vec()),
                ("s2".to_owned(), b"s2".to_vec())
            ],
            "a command reached the wrong shell: {channel:?}",
            channel = channels.by_id
        );
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
            airtime_record: None,
            peer_record: None,
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
}
