//! The daemon's mesh link.
//!
//! Two carriages, one loop. With `LDMUX_MESH_SERIAL` set the daemon owns the radio
//! itself over USB serial (`crate::mesh_serial`, on `crates/companion`): no
//! gateway process, no socket, no token. Without it the daemon dials a MeshMore
//! gateway on loopback and speaks the L2 contract, exactly as before. Both arrive
//! at [`Carrier`], and everything above it — the L3 session, the budget, the retry
//! ladder — is written once.
//!
//! The daemon still learns nothing about presets, paths or contacts when it uses
//! the gateway; when it owns the radio it reads the preset the radio reports, and
//! nothing else.
//!
//! Configuration comes from the environment, because it is per-machine and
//! belongs beside the other deployment settings:
//!
//! ```text
//! LDMUX_MESH_SERIAL /dev/serial/by-id/...  own the radio (no gateway needed)
//! LDMUX_MESH_ADDR   127.0.0.1:41731        gateway address
//! LDMUX_MESH_TOKEN  1e96a66d...           pairing token, 32 hex chars
//! LDMUX_MESH_PEER   aa11bb22...           the peer's public key, 64 hex chars
//! ```
//!
//! A fourth, optional, `LDMUX_MESH_PSK` (64 hex characters, the pairing's pre-shared key)
//! turns on the L3 session ([`crate::mesh_session`]). It is persisted to `mesh-psk` (0600) in the
//! state directory, so a later start without the environment still speaks the same pairing;
//! without either source, downlink records are dropped with a visible `last_error`.
//!
//! All three of the first group or none. A partial configuration is a startup error rather than a
//! silent no-op: a daemon that looks configured and is not is worse than one
//! that refuses to guess.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event, GatewayCarrier};
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

/// How long a budget of zero may block new records before one is sent to test it.
///
/// `STATUS` arrives with every send result, so a live link keeps its budget current. The gap is
/// the idle case: a budget that reached zero just before the hour rolled over stays zero, nothing
/// is sent, no send result arrives, and so no fresh `STATUS` ever comes — the link would stay
/// silent until something reconnected. A refusal costs no airtime (the radio's governor refuses
/// before transmitting), and it brings a `STATUS` with the real number, so one probe a minute is
/// the cheapest way out of that dead end.
const BUDGET_PROBE: Duration = Duration::from_secs(60);

/// How long the loopback socket may take to connect, handshake, or accept one frame.
///
/// The gateway is on the same machine, so seconds are already generous. Before this, a gateway
/// that accepted the socket and then went quiet — a wedged process, a serial port that stopped
/// draining — left the daemon waiting in `recv` forever: no error, no backoff, no reconnect, and a
/// `mesh.status` that said "handshaking" for the life of the process.
const GATEWAY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the carriage may carry nothing at all while a record is outstanding.
///
/// **A gateway that stops answering is not a gateway that has nothing to say.** The steady-state
/// receive had no bound: connect, bind and send were each wrapped in [`GATEWAY_TIMEOUT`], but once
/// the session was up this loop would sit on a socket that never spoke again, with the status
/// reading `established` and the phone's screen frozen — indefinitely, because nothing in this
/// loop ever concluded anything from silence. The ARQ's own ladder is the first line (it fails a
/// record after five attempts over about a minute); this is the backstop for a carriage that stops
/// delivering *anything*, including the answers the ladder is waiting for.
const CARRIER_SILENCE: Duration = Duration::from_secs(75);
use ldmux_mesh::frame::MeshError;
use ldmux_session::arq::Limits;
use tokio::sync::Notify;

use crate::mesh_session::{INBOUND_MAX, SessionFiles, SessionHost, SessionSnapshot};

/// The three settings that turn the link on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MeshConfig {
    pub addr: String,
    pub token: [u8; 16],
    pub peer: [u8; 32],
    /// A serial device the daemon drives itself, instead of dialling a gateway.
    ///
    /// This is what removes the gateway process from the deployment: with a device
    /// set, the daemon owns the radio through `crates/companion`. When it is set the
    /// three gateway settings are not needed and are not used — a device is chosen
    /// deliberately, never by a partial configuration.
    pub serial: Option<String>,
}

impl MeshConfig {
    /// Reads the configuration from a set of `(key, value)` pairs.
    ///
    /// Separated from [`Self::from_env`] so a test can exercise the "all three
    /// or none" rule without touching the process environment.
    pub fn from_pairs<'a, I>(pairs: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut addr: Option<String> = None;
        let mut serial: Option<String> = None;
        let mut token: Option<String> = None;
        let mut peer: Option<String> = None;
        for (key, value) in pairs {
            match key {
                "LDMUX_MESH_ADDR" => addr = Some(value.to_owned()),
                "LDMUX_MESH_TOKEN" => token = Some(value.to_owned()),
                "LDMUX_MESH_PEER" => peer = Some(value.to_owned()),
                "LDMUX_MESH_SERIAL" => serial = Some(value.to_owned()),
                _ => {}
            }
        }
        // A serial device is a complete configuration on its own: the daemon owns the
        // radio, so there is no address to dial, no token to present and no peer to
        // bind. The gateway fields are filled with values that are never read, and
        // `serial` is what the supervisor dispatches on.
        if let Some(device) = serial {
            if device.trim().is_empty() {
                return Err("LDMUX_MESH_SERIAL is empty".to_owned());
            }
            return Ok(Some(Self {
                addr: addr.unwrap_or_default(),
                token: token
                    .map(|t| parse_hex::<16>(&t, "LDMUX_MESH_TOKEN"))
                    .transpose()?
                    .unwrap_or([0u8; 16]),
                peer: peer
                    .map(|p| parse_hex::<32>(&p, "LDMUX_MESH_PEER"))
                    .transpose()?
                    .unwrap_or([0u8; 32]),
                serial: Some(device.trim().to_owned()),
            }));
        }
        match (addr, token, peer) {
            (None, None, None) => Ok(None),
            (Some(addr), Some(token), Some(peer)) => {
                let token = parse_hex::<16>(&token, "LDMUX_MESH_TOKEN")?;
                let peer = parse_hex::<32>(&peer, "LDMUX_MESH_PEER")?;
                if addr.trim().is_empty() {
                    return Err("LDMUX_MESH_ADDR is empty".to_owned());
                }
                Ok(Some(Self {
                    addr: addr.trim().to_owned(),
                    token,
                    peer,
                    serial: None,
                }))
            }
            _ => Err(
                "set all of LDMUX_MESH_ADDR, LDMUX_MESH_TOKEN and LDMUX_MESH_PEER, or none"
                    .to_owned(),
            ),
        }
    }

    /// The process environment's configuration.
    ///
    /// `LDMUX_MESH_STATE` names the host gateway's state file, which already holds the
    /// loopback address, the pairing token and both radio public keys. Explicit variables
    /// win key by key, so an operator can move the port without rewriting anything, and the
    /// file is read only when a value is missing.
    pub fn from_env() -> Result<Option<Self>, String> {
        let vars: Vec<(String, String)> =
            ["LDMUX_MESH_ADDR", "LDMUX_MESH_TOKEN", "LDMUX_MESH_PEER", "LDMUX_MESH_SERIAL"]
                .iter()
                .filter_map(|key| {
                    std::env::var(key)
                        .ok()
                        .map(|value| ((*key).to_owned(), value))
                })
                .collect();
        let explicit: Vec<(&str, &str)> =
            vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        if explicit.len() == 3 {
            return Self::from_pairs(explicit);
        }
        let Some(path) = std::env::var("LDMUX_MESH_STATE").ok().filter(|p| !p.trim().is_empty())
        else {
            return Self::from_pairs(explicit);
        };
        let file = Self::from_state_file(std::path::Path::new(&path))?;
        let mut merged: Vec<(String, String)> = Vec::new();
        if let Some(f) = &file {
            merged.push(("LDMUX_MESH_ADDR".to_owned(), f.addr.clone()));
            merged.push((
                "LDMUX_MESH_TOKEN".to_owned(),
                f.token.iter().map(|b| format!("{b:02x}")).collect(),
            ));
            merged.push((
                "LDMUX_MESH_PEER".to_owned(),
                f.peer.iter().map(|b| format!("{b:02x}")).collect(),
            ));
        }
        for (key, value) in &vars {
            if let Some(slot) = merged.iter_mut().find(|(k, _)| k == key) {
                slot.1 = value.clone();
            } else {
                merged.push((key.clone(), value.clone()));
            }
        }
        let pairs: Vec<(&str, &str)> =
            merged.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Self::from_pairs(pairs)
    }

    /// Configuration from a host gateway's state file (`meshmore_gateway --state`).
    ///
    /// The gateway knows its own address, its token and the radio public keys; the daemon
    /// must not learn anything else about the radio, and this file carries nothing else.
    /// `Ok(None)` when the file exists but names no gateway yet.
    pub fn from_state_file(path: &std::path::Path) -> Result<Option<Self>, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("LDMUX_MESH_STATE {} is unreadable: {e}", path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("LDMUX_MESH_STATE {} is not JSON: {e}", path.display()))?;
        let get = |key: &str| json.get(key).and_then(serde_json::Value::as_str);
        let (Some(addr), Some(token), Some(peer)) =
            (get("gatewayAddr"), get("pairingToken"), get("peerKey"))
        else {
            return Ok(None);
        };
        Self::from_pairs([
            ("LDMUX_MESH_ADDR", addr),
            ("LDMUX_MESH_TOKEN", token),
            ("LDMUX_MESH_PEER", peer),
        ])
    }

    /// This radio's public key from the same state file, when `LDMUX_MESH_SELF` is unset.
    #[must_use]
    pub fn self_key_from_env() -> Option<[u8; 32]> {
        if let Ok(text) = std::env::var("LDMUX_MESH_SELF")
            && let Ok(key) = parse_hex::<32>(&text, "LDMUX_MESH_SELF")
        {
            return Some(key);
        }
        let path = std::env::var("LDMUX_MESH_STATE").ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        parse_hex::<32>(json.get("radioKey")?.as_str()?, "radioKey").ok()
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

    /// The gateway's public key, hex: what a phone must be given to pair.
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
    /// `None` for a gateway, which does not say, and before the radio has been identified.
    pub radio_preset: Option<(u32, u32, u8, u8)>,
    pub addr: String,
    /// The serial device the daemon drives itself, when it owns the radio.
    pub serial: Option<String>,
    /// The peer's public key, hex.
    pub peer: String,
    /// The largest record this peer's path allows, from `BIND_OK`.
    pub uplink_record_max: Option<u8>,
    /// Records of the current size left in this hour's airtime budget, from
    /// `STATUS`. `None` until the gateway sends the sixth byte.
    pub records_left: Option<u8>,
    pub radio_state: Option<RadioState>,
    /// `-128` is the contract's "no reading".
    pub rssi: i8,
    /// `0xFF` is the contract's "not reported".
    pub battery_percent: u8,
    /// The gateway's own transmit airtime this hour, in 0.1 s units.
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
            addr: cfg.addr.clone(),
            serial: cfg.serial.clone(),
            peer: hex(&cfg.peer),
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
            "addr": self.addr,
            "serial": self.serial,
            "peer": self.peer,
            "uplinkRecordMax": self.uplink_record_max,
            "recordsLeft": self.records_left,
            "radioState": self.radio_state.map(|s| s.code()),
            // The tuple the host's own radio reported (or was retuned to). Absent for a gateway,
            // which does not say, and before the radio has been identified.
            "radioPreset": self
                .radio_preset
                .map(|(f, b, sf, cr)| vec![u64::from(f), u64::from(b), u64::from(sf), u64::from(cr)]),
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
pub type Retune = (u32, u32, u8, u8);

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
    pub fn request_preset(&self, freq_khz: u32, bw_hz: u32, sf: u8, cr: u8) -> bool {
        match self.preset_request.lock() {
            Ok(mut slot) => {
                *slot = Some((freq_khz, bw_hz, sf, cr));
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
            "addr": serde_json::Value::Null,
            "peer": serde_json::Value::Null,
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
    Ok(serde_json::json!({
        "hostKey": hex(&keys.host_key),
        "psk": hex(&keys.psk),
        "hostRadio": host_radio.map(|r| hex(&r)),
        "hostPreset": host_preset.map(|(f, b, sf, cr)| vec![
            u64::from(f), u64::from(b), u64::from(sf), u64::from(cr)
        ]),
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
        // Whether this attempt ever completed a handshake. Only a handshake says the gateway is
        // healthy, and only then does the backoff go back to its floor: before this, one outage
        // left every later reconnect waiting the full 30 s cap for the life of the process.
        let bound = Arc::new(AtomicBool::new(false));
        match link(&cfg, &runtime, setup.as_mut(), Arc::clone(&bound)).await {
            Ok(()) => {
                set(&snapshot, |s| {
                    s.state = "down";
                    s.last_error = Some("the gateway closed the connection".to_owned());
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
        // Capped: a radio that is off for an hour should not be dialled every
        // second, and a gateway that comes back is noticed on the next 1 s attempt because the
        // reset above ran for this connection.
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

/// Connect whatever carries this link and run the session on it.
///
/// A serial device means the daemon owns the radio (`crate::mesh_serial`); without
/// one it dials the gateway exactly as before. Everything below this function is
/// the same for both, which is the point of the [`Carrier`] seam.
async fn link(
    cfg: &MeshConfig,
    runtime: &MeshRuntime,
    setup: Option<&mut SessionSetup>,
    bound: Arc<AtomicBool>,
) -> Result<(), MeshError> {
    match cfg.serial.clone() {
        Some(device) => {
            set(&runtime.snapshot, |s| s.state = "handshaking");
            let opened = tokio::time::timeout(
                GATEWAY_TIMEOUT,
                crate::mesh_serial::SerialCarrier::open(&device),
            )
            .await
            .map_err(|_| MeshError::Timeout("serial open"))??;
            let (carrier, promise, preset) = opened;
            // **The host's own tuple, said out loud.** Read from the radio at open and published
            // here, because two radios on different tuples cannot hear each other and the phone
            // could only ever compare against a number the host never gave it.
            set(&runtime.snapshot, |s| {
                s.own_radio_key = Some(preset.public_key);
                s.radio_preset = Some((
                    (preset.freq_mhz * 1000.0).round() as u32,
                    (preset.bw_khz * 1000.0).round() as u32,
                    preset.sf,
                    preset.cr,
                ));
            });
            session(carrier, promise, runtime, setup, bound).await
        }
        None => {
            set(&runtime.snapshot, |s| s.state = "handshaking");
            let mut carrier = tokio::time::timeout(
                GATEWAY_TIMEOUT,
                GatewayCarrier::open(&cfg.addr),
            )
            .await
            .map_err(|_| MeshError::Timeout("gateway connect"))??;
            let promise = tokio::time::timeout(
                GATEWAY_TIMEOUT,
                carrier.bind(&cfg.token, &cfg.peer),
            )
            .await
            .map_err(|_| MeshError::Timeout("gateway handshake"))??;
            session(carrier, promise, runtime, setup, bound).await
        }
    }
}

/// How many records this poll may hand to the carrier.
///
/// The rule differs by who owns the meter, and getting it wrong is not a detail: with
/// `Budget::Reported` an unknown budget is zero (a gateway that has said nothing has allowed
/// nothing), and applying that same rule with no gateway at all left a serial host able to send
/// one probe record a minute and answer nothing - a paired phone retried `attach` for ever while
/// `ldmux mesh status` read `budget 0 records left this hour` (measured 2026-11-05).
fn poll_budget(source: Budget, reported: Option<u8>, probe: bool) -> u8 {
    match source {
        // The radio's firmware runs the transmitter's duty cycle and refuses what it will not
        // send, so there is no second meter to apply here.
        Budget::RadioGoverned => u8::MAX,
        Budget::Reported => {
            if probe {
                1
            } else {
                reported.unwrap_or(0)
            }
        }
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
        )
    });
    // Same clock as every other probe in the process. A bare `Instant::now()` here was a second
    // origin, so `drained … ms` and `reply queued at …` differed by a constant that read as
    // latency — 76 748 ms on one run. Anything two probes are compared against comes from here.
    let started = runtime_start();
    let mut budget: Option<u8> = None;
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
    let mut last_status_at = Instant::now();

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
        if let Some((freq_khz, bw_hz, sf, cr)) = runtime.take_preset() {
            match carrier.set_preset(freq_khz, bw_hz, sf, cr).await {
                Ok(()) => {
                    set(snapshot, |s| {
                        s.radio_preset = Some((freq_khz, bw_hz, sf, cr));
                        s.last_error = None;
                    });
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
                        last_status_at = Instant::now();
                        if status.ops_left_this_hour.is_some() {
                            budget = status.ops_left_this_hour;
                        }
                        set(snapshot, |s| {
                            s.radio_state = Some(status.radio_state);
                            s.rssi = status.rssi;
                            s.battery_percent = status.battery_percent;
                            s.tx_airtime_tenths = status.tx_airtime_tenths_this_hour;
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

        // The budget is the gateway's own number. Unknown means zero: a gateway that has not
        // said how much airtime is left has not allowed anything.
        //
        // A zero that has blocked us for a minute is tested, because a zero is a number from the
        // last `STATUS` and the hour may have rolled over while the link was idle. See
        // [`BUDGET_PROBE`]: the probe is refused without airtime if the budget really is spent.
        let used = u8::try_from(awaiting.len()).unwrap_or(u8::MAX);
        let probe = budget == Some(0)
            && h.wants_poll()
            && now.saturating_duration_since(last_status_at) >= BUDGET_PROBE;
        // **A radio we own is not gated here.** Its firmware runs the transmitter's duty cycle and
        // refuses a record it will not send; applying the gateway's "unknown means zero" rule on
        // top of that left a serial host able to send one probe a minute and answer nothing, which
        // is exactly what a paired phone saw as an endless `attach` (measured 2026-11-05, with
        // `ldmux mesh status` reporting `budget 0 records left this hour`).
        let records_left = poll_budget(promise.budget, budget, probe);
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
                "ldmuxd: mesh timing: queued {} -> polled {} at {now_ms} ms (awaiting {used}, budget {budget:?}, l2_slots {})",
                queued_n,
                polled.len(),
                CREDIT_WINDOW.saturating_sub(used),
            );
        }
        let sent_now = polled.len();
        let pending_now = h.snapshot().pending;
        for record in polled {
            let t_write = std::time::Instant::now();
            tokio::time::timeout(GATEWAY_TIMEOUT, carrier.send_record(seq, &record.bytes))
                .await
                .map_err(|_| MeshError::Timeout("gateway write"))??;
            let took = t_write.elapsed().as_millis();
            if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") && took > 50 {
                eprintln!("ldmuxd: mesh timing: socket write took {took} ms");
            }
            awaiting.push_back((seq, Instant::now(), record.seq));
            seq = seq.wrapping_add(1);
            budget = budget.map(|b| b.saturating_sub(1));
            set(snapshot, |s| s.records_left = budget);
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

    const TOKEN: &str = "00112233445566778899aabbccddeeff";
    const PEER: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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
            addr: String::new(),
            token: [0; 16],
            peer: [0; 32],
            serial: Some("/dev/null".to_owned()),
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
    fn a_serial_device_is_a_complete_configuration_on_its_own() {
        // The point of the serial carrier: no address to dial, no token, no peer to
        // bind. This is what lets a host own its radio with nothing else running.
        let cfg = MeshConfig::from_pairs([("LDMUX_MESH_SERIAL", "/dev/ttyACM0")])
            .expect("a device alone is enough")
            .expect("a device is a configuration");
        assert_eq!(cfg.serial.as_deref(), Some("/dev/ttyACM0"));
        assert!(cfg.addr.is_empty(), "no gateway address was given");
        assert_eq!(cfg.token, [0u8; 16], "no token was given");
    }

    #[test]
    fn an_empty_serial_device_is_refused_rather_than_ignored() {
        let e = MeshConfig::from_pairs([("LDMUX_MESH_SERIAL", "  ")]).unwrap_err();
        assert!(e.contains("LDMUX_MESH_SERIAL"), "{e}");
    }

    #[test]
    fn a_serial_device_wins_over_the_gateway_settings() {
        let cfg = MeshConfig::from_pairs([
            ("LDMUX_MESH_SERIAL", "/dev/ttyUSB0"),
            ("LDMUX_MESH_ADDR", "127.0.0.1:41731"),
            ("LDMUX_MESH_TOKEN", "00112233445566778899aabbccddeeff"),
            ("LDMUX_MESH_PEER", PEER),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cfg.serial.as_deref(), Some("/dev/ttyUSB0"));
        // The gateway fields are still parsed, so nothing is silently half-read.
        assert_eq!(cfg.token[0], 0x00);
        assert_eq!(cfg.peer[31], 0x1f);
    }

    #[test]
    fn all_three_parse() {
        let cfg = MeshConfig::from_pairs(vec![
            ("LDMUX_MESH_ADDR", "127.0.0.1:41731"),
            ("LDMUX_MESH_TOKEN", TOKEN),
            ("LDMUX_MESH_PEER", PEER),
        ])
        .expect("valid")
        .expect("some");
        assert_eq!(cfg.addr, "127.0.0.1:41731");
        assert_eq!(cfg.token[0], 0x00);
        assert_eq!(cfg.token[15], 0xff);
        assert_eq!(cfg.peer[31], 0x1f);
    }

    #[test]
    fn a_partial_configuration_is_an_error() {
        let err = MeshConfig::from_pairs(vec![
            ("LDMUX_MESH_ADDR", "127.0.0.1:41731"),
            ("LDMUX_MESH_TOKEN", TOKEN),
        ])
        .expect_err("partial");
        assert!(err.contains("or none"), "{err}");
    }

    #[test]
    fn a_bad_token_is_refused() {
        assert!(
            MeshConfig::from_pairs(vec![
                ("LDMUX_MESH_ADDR", "127.0.0.1:41731"),
                ("LDMUX_MESH_TOKEN", "abcd"),
                ("LDMUX_MESH_PEER", PEER),
            ])
            .is_err()
        );
        assert!(
            MeshConfig::from_pairs(vec![
                ("LDMUX_MESH_ADDR", "127.0.0.1:41731"),
                ("LDMUX_MESH_TOKEN", &"z".repeat(32)),
                ("LDMUX_MESH_PEER", PEER),
            ])
            .is_err()
        );
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
            addr: "127.0.0.1:41731".to_owned(),
            token: [0; 16],
            peer: [0xAB; 32],
            serial: None,
        };
        let snapshot = MeshSnapshot::new(&cfg);
        let json = snapshot.to_json();
        for key in [
            "state",
            "addr",
            "serial",
            "peer",
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
        // Not status: whether this host knows its own radio decides whether a pairing code
        // it prints can be used at all, so `mesh status` answers it beside the link.
        assert_eq!(json["hostRadio"], serde_json::Value::Null);
        assert_eq!(json["peer"], "ab".repeat(32));
    }

    fn state_file(dir: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(dir);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        let file = path.join("gateway.json");
        std::fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn a_gateway_state_file_supplies_address_token_and_peer() {
        // The host gateway resolves the peer radio by name and publishes both keys, so the
        // daemon never needs a 64-hex copy. The file carries nothing about radio parameters.
        let file = state_file(
            "ldmux-mesh-state-ok",
            &format!(
                r#"{{"gatewayAddr":"127.0.0.1:41731","pairingToken":"{TOKEN}","peerKey":"{PEER}","radioKey":"{PEER}"}}"#
            ),
        );
        let cfg = MeshConfig::from_state_file(&file).unwrap().expect("configured");
        assert_eq!(cfg.addr, "127.0.0.1:41731");
        assert_eq!(cfg.token[15], 0xff);
        assert_eq!(cfg.peer[31], 0x1f);
    }

    #[test]
    fn a_state_file_without_a_resolved_peer_configures_nothing() {
        // `--peer-name` unresolved writes a null peerKey. A daemon that guessed a peer would
        // send records into the wrong path, so absence is "not configured", not "any peer".
        let file = state_file(
            "ldmux-mesh-state-nopeer",
            &format!(r#"{{"gatewayAddr":"127.0.0.1:41731","pairingToken":"{TOKEN}","peerKey":null}}"#),
        );
        assert!(MeshConfig::from_state_file(&file).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_state_file_names_itself() {
        let path = std::env::temp_dir().join("ldmux-mesh-state-missing.json");
        let _ = std::fs::remove_file(&path);
        let err = MeshConfig::from_state_file(&path).unwrap_err();
        assert!(err.contains("LDMUX_MESH_STATE"), "{err}");
        assert!(err.contains("unreadable"), "{err}");
    }


    /// The serial carrier has no gateway, so "unknown means zero" must not apply to it.
    ///
    /// This is the exact shape of the reported failure: a paired phone retrying `attach` for ever
    /// against a host whose status read `budget 0 records left this hour`, because the budget was
    /// `None` (no gateway to report one) and `None` was treated as spent.
    #[test]
    fn a_radio_we_own_is_gated_by_its_firmware_not_by_a_missing_gateway_number() {
        assert_eq!(poll_budget(Budget::RadioGoverned, None, false), u8::MAX);
        assert_eq!(poll_budget(Budget::RadioGoverned, Some(0), false), u8::MAX);
        assert_eq!(poll_budget(Budget::RadioGoverned, Some(7), false), u8::MAX);
    }

    /// A gateway's budget is its own number, and silence is not permission.
    #[test]
    fn a_gateway_that_has_said_nothing_has_allowed_nothing() {
        assert_eq!(poll_budget(Budget::Reported, None, false), 0);
        assert_eq!(poll_budget(Budget::Reported, Some(0), false), 0);
        assert_eq!(poll_budget(Budget::Reported, Some(9), false), 9);
    }

    /// The probe exists so a spent hour does not need a manual restart, and it is one record.
    #[test]
    fn a_spent_gateway_budget_still_allows_the_probe() {
        assert_eq!(poll_budget(Budget::Reported, Some(0), true), 1);
        assert_eq!(poll_budget(Budget::Reported, None, true), 1);
    }
}
