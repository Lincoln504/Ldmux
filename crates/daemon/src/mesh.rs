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
//! LDMUX_MESH_PEER   615f431f...           the peer's public key, 64 hex chars
//! ```
//!
//! All three or none. A partial configuration is a startup error rather than a
//! silent no-op: a daemon that looks configured and is not is worse than one
//! that refuses to guess.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ldmux_mesh::client::MeshClient;
use ldmux_mesh::constants::{CAP_STATUS_PUSH, FrameType, RadioState, SendOutcome};
use ldmux_mesh::frame::MeshError;
use ldmux_mesh::messages;

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
        let pairs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Self::from_pairs(pairs)
    }
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
        })
    }
}

/// The running link, shared with the RPC handler.
#[derive(Clone)]
pub struct MeshRuntime {
    snapshot: Arc<Mutex<MeshSnapshot>>,
}

static GLOBAL: OnceLock<MeshRuntime> = OnceLock::new();

impl MeshRuntime {
    /// Connects and keeps the connection up in the background.
    ///
    /// Never fails: a link that is down is a state, and the daemon must start
    /// whether or not a radio is attached. Failures land in the snapshot.
    pub fn start(cfg: MeshConfig) -> Self {
        let snapshot = Arc::new(Mutex::new(MeshSnapshot::new(&cfg)));
        let runtime = Self {
            snapshot: Arc::clone(&snapshot),
        };
        tokio::spawn(supervise(cfg, snapshot));
        runtime
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
        })
    }
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
async fn supervise(cfg: MeshConfig, snapshot: Arc<Mutex<MeshSnapshot>>) {
    let mut backoff = Duration::from_secs(1);
    let mut reconnects: u32 = 0;
    loop {
        set(&snapshot, |s| {
            s.state = "connecting";
            s.last_error = None;
        });
        match session(&cfg, &snapshot).await {
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

/// One connected session, from handshake to close.
async fn session(cfg: &MeshConfig, snapshot: &Arc<Mutex<MeshSnapshot>>) -> Result<(), MeshError> {
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

    loop {
        let frame = client.recv().await?;
        match frame.kind {
            FrameType::Status => {
                let status = messages::read_status(&frame)?;
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
                // The L3 session that consumes records does not exist yet, so a
                // downlink record is deliberately dropped here rather than
                // queued forever. Recorded as an error so it is visible.
                set(snapshot, |s| {
                    s.last_error =
                        Some("a downlink record arrived; no session consumes them yet".to_owned());
                });
            }
            FrameType::SendResult => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "00112233445566778899aabbccddeeff";
    const PEER: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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
}
