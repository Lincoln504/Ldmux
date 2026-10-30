//! The daemon's MeshMore link.
//!
//! The daemon owns no radio. It dials the MeshMore gateway on loopback, speaks
//! the L2 contract (`ldmux-mesh`), and keeps a snapshot of what the link said.
//! Everything about frequencies, presets, paths and contacts stays on the other
//! side of that socket.
//!
//! Configuration comes from the environment, because it is per-machine and
//! belongs beside the other deployment settings:
//!
//! ```text
//! LDMUX_MESH_ADDR   127.0.0.1:41731        gateway address
//! LDMUX_MESH_TOKEN  1e96a66d...           pairing token, 32 hex chars
//! LDMUX_MESH_PEER   aa11bb22...           the peer's public key, 64 hex chars
//! ```
//!
//! A fourth, optional, `LDMUX_MESH_PSK` (64 hex characters, the pairing's pre-shared key)
//! turns on the L3 session ([`crate::mesh_session`]); without it, downlink records are
//! dropped with a visible `last_error`, as before.
//!
//! All three of the first group or none. A partial configuration is a startup error rather than a
//! silent no-op: a daemon that looks configured and is not is worse than one
//! that refuses to guess.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ldmux_mesh::client::MeshClient;
use ldmux_mesh::constants::{CAP_STATUS_PUSH, CREDIT_WINDOW, FrameType, RadioState, SendOutcome};
use ldmux_mesh::frame::MeshError;
use ldmux_mesh::messages;
use ldmux_session::arq::Limits;
use tokio::sync::Notify;

use crate::mesh_session::{INBOUND_MAX, SessionFiles, SessionHost, SessionSnapshot};

/// The three settings that turn the link on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MeshConfig {
    pub addr: String,
    pub token: [u8; 16],
    pub peer: [u8; 32],
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
        let mut token: Option<String> = None;
        let mut peer: Option<String> = None;
        for (key, value) in pairs {
            match key {
                "LDMUX_MESH_ADDR" => addr = Some(value.to_owned()),
                "LDMUX_MESH_TOKEN" => token = Some(value.to_owned()),
                "LDMUX_MESH_PEER" => peer = Some(value.to_owned()),
                _ => {}
            }
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
            ["LDMUX_MESH_ADDR", "LDMUX_MESH_TOKEN", "LDMUX_MESH_PEER"]
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
        if let Ok(text) = std::env::var("LDMUX_MESH_SELF") {
            if let Ok(key) = parse_hex::<32>(&text, "LDMUX_MESH_SELF") {
                return Some(key);
            }
        }
        let path = std::env::var("LDMUX_MESH_STATE").ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        parse_hex::<32>(json.get("radioKey")?.as_str()?, "radioKey").ok()
    }
}

/// The L3 session's configuration: the pairing's PSK and the files beside the other state.
pub struct SessionSetup {
    files: SessionFiles,
}

impl SessionSetup {
    /// `Ok(None)` when `LDMUX_MESH_PSK` is not set.
    pub fn from_env(state_dir: &Path) -> Result<Option<Self>, String> {
        let Ok(text) = std::env::var("LDMUX_MESH_PSK") else {
            return Ok(None);
        };
        let psk = parse_hex::<32>(&text, "LDMUX_MESH_PSK")?;
        Ok(Some(Self::new(state_dir, psk)?))
    }

    pub fn new(state_dir: &Path, psk: [u8; 32]) -> Result<Self, String> {
        Ok(Self {
            files: SessionFiles::load(state_dir, psk)?,
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
        }
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
    pub addr: String,
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
}

impl MeshSnapshot {
    fn new(cfg: &MeshConfig) -> Self {
        Self {
            state: "connecting",
            addr: cfg.addr.clone(),
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
        }
    }

    /// The JSON shape `mesh.status` answers with.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "addr": self.addr,
            "peer": self.peer,
            "uplinkRecordMax": self.uplink_record_max,
            "recordsLeft": self.records_left,
            "radioState": self.radio_state.map(|s| s.code()),
            "rssi": self.rssi,
            "batteryPercent": self.battery_percent,
            "txAirtimeTenths": self.tx_airtime_tenths,
            "lastError": self.last_error,
            "reconnects": self.reconnects,
            "session": self.session.to_json(),
        })
    }
}

/// Bytes crossing between the daemon and the session task.
#[derive(Default)]
struct Exchange {
    outbound: VecDeque<(u8, Vec<u8>)>,
    inbound: VecDeque<(u8, Vec<u8>)>,
}

/// The running link, shared with the RPC handler.
#[derive(Clone)]
pub struct MeshRuntime {
    /// The keys a phone needs to pair for the mesh route; `None` when no session is configured.
    pairing_keys: Option<ldmux_protocol::pairing::MeshKeys>,
    snapshot: Arc<Mutex<MeshSnapshot>>,
    exchange: Arc<Mutex<Exchange>>,
    wake: Arc<Notify>,
}

static GLOBAL: OnceLock<MeshRuntime> = OnceLock::new();

impl MeshRuntime {
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
            snapshot: Arc::clone(&snapshot),
            exchange: Arc::new(Mutex::new(Exchange::default())),
            wake: Arc::new(Notify::new()),
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
        drop(ex);
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

    /// Makes this runtime the one the RPC handler answers from.
    pub fn install(self) {
        let _ = GLOBAL.set(self);
    }

    /// The installed runtime, if the link is configured.
    pub fn global() -> Option<&'static MeshRuntime> {
        GLOBAL.get()
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
        })
    }
}

/// The answer to `mesh.pairing`: the keys for a mesh pairing link, hex. An error says what is
/// missing; the secret is never in an error or a log line.
pub fn pairing_json() -> Result<serde_json::Value, String> {
    let keys = MeshRuntime::global()
        .and_then(MeshRuntime::pairing_keys)
        .ok_or_else(|| {
            "no mesh session is configured (set LDMUX_MESH_PSK, with the link variables)".to_owned()
        })?;
    Ok(serde_json::json!({
        "hostKey": hex(&keys.host_key),
        "psk": hex(&keys.psk),
        "hostRadio": keys.host_radio.map(|r| hex(&r)),
    }))
}

/// The answer to `mesh.status`: the snapshot, or the disabled shape.
#[must_use]
pub fn status_json() -> serde_json::Value {
    match MeshRuntime::global() {
        Some(runtime) => runtime.snapshot().to_json(),
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
        match session(&cfg, &runtime, setup.as_mut()).await {
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
        tokio::time::sleep(backoff).await;
        // Capped: a radio that is off for an hour should not be dialled every
        // second, and a gateway that comes back should be noticed quickly.
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

/// One connected session, from handshake to close.
async fn session(
    cfg: &MeshConfig,
    runtime: &MeshRuntime,
    setup: Option<&mut SessionSetup>,
) -> Result<(), MeshError> {
    let snapshot = &runtime.snapshot;
    let mut client = MeshClient::connect(&cfg.addr).await?;
    set(snapshot, |s| s.state = "handshaking");
    let handshake = client
        .handshake(&cfg.token, &cfg.peer, CAP_STATUS_PUSH)
        .await?;
    set(snapshot, |s| {
        s.state = "ready";
        s.uplink_record_max = Some(handshake.bind.uplink_record_max);
        s.radio_state = Some(handshake.hello.radio_state);
        s.last_error = None;
    });

    // The L3 host lives as long as this L2 connection: a reconnect means a new handshake
    // (ADR-0020 section 3), so nothing here outlives the socket.
    let mut host: Option<SessionHost> = setup.as_ref().map(|s| {
        let key = crate::mesh_session::SessionFiles::clone_key(&s.files);
        SessionHost::new(
            key,
            s.files.current_pairing(),
            usize::from(handshake.bind.uplink_record_max),
            s.files.persister(),
        )
    });
    let started = Instant::now();
    let mut budget: Option<u8> = None;
    let mut awaiting: u8 = 0;
    let mut seq: u8 = 0;

    loop {
        let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let wait = host
            .as_ref()
            .and_then(SessionHost::next_deadline)
            .map_or(Duration::from_secs(3600), |d| {
                Duration::from_millis(d.saturating_sub(now_ms))
            });
        tokio::select! {
            frame = client.recv() => {
                let frame = frame?;
                match frame.kind {
                    FrameType::Status => {
                        let status = messages::read_status(&frame)?;
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
                    FrameType::Recv => {
                        let record = messages::read_recv(&frame)?;
                        match host.as_mut() {
                            Some(h) => {
                                let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                                h.on_record(now, now_unix_ms(), &record.0);
                            }
                            None => set(snapshot, |s| {
                                s.last_error = Some(
                                    "a downlink record arrived; no session is configured (LDMUX_MESH_PSK)"
                                        .to_owned(),
                                );
                            }),
                        }
                    }
                    FrameType::SendResult => {
                        awaiting = awaiting.saturating_sub(1);
                        let result = messages::read_send_result(&frame)?;
                        if result.outcome == SendOutcome::ErrBusy {
                            set(snapshot, |s| {
                                s.last_error = Some("the radio refused a record (busy)".to_owned());
                            });
                        }
                    }
                    FrameType::Close => {
                        let reason = messages::read_close(&frame)?;
                        return Err(MeshError::Refused("close", reason.code()));
                    }
                    _ => {}
                }
            }
            () = tokio::time::sleep(wait) => {}
            () = runtime.wake.notified() => {}
        }

        let Some(h) = host.as_mut() else { continue };
        let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        // Application bytes queued by the daemon go into the ARQ; a refusal is shown, not hidden.
        let queued: Vec<(u8, Vec<u8>)> = runtime
            .exchange
            .lock()
            .map(|mut ex| ex.outbound.drain(..).collect())
            .unwrap_or_default();
        let queued_n = queued.len();
        for (shell, bytes) in queued {
            if let Err(e) = h.send(shell, &bytes) {
                set(snapshot, |s| {
                    s.last_error = Some(format!("session send refused: {e}"))
                });
            }
        }

        // The budget is the gateway's own number. Unknown means zero: a gateway that has not
        // said how much airtime is left has not allowed anything.
        let limits = Limits {
            records_left: budget.unwrap_or(0),
            l2_slots: CREDIT_WINDOW.saturating_sub(awaiting),
        };
        let polled = h.poll(now_ms, limits);
        if std::env::var_os("LDMUX_MESH_TIMING").is_some_and(|v| v == "1") && queued_n > 0 {
            eprintln!(
                "ldmuxd: mesh timing: queued {} -> polled {} at {now_ms} ms (awaiting {awaiting}, budget {budget:?}, l2_slots {})",
                queued_n,
                polled.len(),
                CREDIT_WINDOW.saturating_sub(awaiting),
            );
        }
        for record in polled {
            client.send_record(seq, &record).await?;
            seq = seq.wrapping_add(1);
            awaiting = awaiting.saturating_add(1);
            budget = budget.map(|b| b.saturating_sub(1));
            set(snapshot, |s| s.records_left = budget);
        }

        let view = h.snapshot();
        if let Ok(mut ex) = runtime.exchange.lock() {
            while let Some(item) = h.inbound.pop_front() {
                if ex.inbound.len() >= INBOUND_MAX {
                    ex.inbound.pop_front();
                }
                ex.inbound.push_back(item);
            }
        }
        set(snapshot, |s| s.session = view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "00112233445566778899aabbccddeeff";
    const PEER: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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
    }

    #[test]
    fn a_snapshot_serialises_every_field_the_cli_prints() {
        let cfg = MeshConfig {
            addr: "127.0.0.1:41731".to_owned(),
            token: [0; 16],
            peer: [0xAB; 32],
        };
        let snapshot = MeshSnapshot::new(&cfg);
        let json = snapshot.to_json();
        for key in [
            "state",
            "addr",
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

}
