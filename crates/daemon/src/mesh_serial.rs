//! Opening the host's own radio, and what it reports about itself.
//!
//! The radio is a serial port and the companion protocol is synchronous, so the port lives on one
//! dedicated thread (`crate::mesh_carrier`) and the session loop talks to it over channels. This
//! module is the part before that: open the device, take the `flock`, identify the radio, read the
//! tuple, the path width, the scope and the peer's key, and hand the open companion to the carrier.
//!
//! **Nothing here decides a route.** The old carrier kept the peer's path, chose when a record
//! flooded, ran a flood ladder and answered route requests; all of that is `RadioEndpoint`'s now,
//! per attempt, before sealing (`docs/RADIO-PLAN.md` §1 `A1`, §10a). What is left is the open and
//! the facts the radio itself reports.

use std::time::Duration;

use ldmux_companion::{ByteLink, Companion, FloodScope, SerialLink, SessionError};
use ldmux_mesh::carrier::Bound;
use ldmux_mesh::constants::{RECORD_MAX, RECORD_RECEIVED_MAX};
use ldmux_mesh::error::MeshError;
use ldmux_mesh::messages::ScopeInfo;

use crate::mesh_carrier::{CarrierSettings, RadioCarrier, RadioSettings};

/// The largest record a flood may carry before any path is known.
///
/// A flood accumulates the path it crosses, so its record ceiling shrinks with distance. This is the
/// figure for a pathless flood at the public preset, and the endpoint revises it from the first
/// arrival (`Event::Limit`).
const LDMUX_FLOOD_RECORD_MAX: usize = 162;

/// What the radio is tuned to, as it reports it.
///
/// Published in the snapshot because two radios on different tuples cannot hear each other and the
/// phone could only ever compare against a number the host never gave it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RadioPreset {
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub sf: u8,
    pub cr: u8,
    /// Transmit power in dBm, as the radio reports it. Read from `SELF_INFO` byte 2, which is the
    /// *configured* power rather than the board's ceiling (byte 3).
    pub tx_power_dbm: i8,
    /// Bytes per path hash this radio puts on its own packets.
    ///
    /// Published with the tuple because it is the other half of what two radios have to agree about
    /// to find each other: the width the host's adverts carry. A phone on a different width still
    /// hears them (a repeater forwards whatever arrives), so this is a fact to read, not a
    /// constraint to match — but it is the first thing to check when a peer's advert path never
    /// resolves.
    pub hash_size: u8,
    /// This radio's own public key, from the same `SELF_INFO`.
    ///
    /// The daemon owns the radio, so it can read the key a phone must bind. Requiring the operator
    /// to type it into `LDMUX_MESH_SELF` was a step that could only be got wrong: a stale or
    /// mistyped key makes a pairing code that no phone can use.
    pub public_key: [u8; 32],
}

/// The flood scope this node's broadcasts are proved with, and where the choice came from.
struct ScopeChoice {
    scope: Option<FloodScope>,
    /// One word for the diagnostics line: `radio`, `env`, `off` or `none`.
    #[cfg_attr(not(test), allow(dead_code))]
    source: &'static str,
}

impl ScopeChoice {
    /// What the radio reports, an explicit pin winning over it, and an explicit `off`.
    fn resolve<L: ByteLink>(companion: &mut Companion<L>, log: bool) -> Self {
        let pinned = std::env::var("LDMUX_MESH_SCOPE")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if let Some(pin) = pinned {
            if pin.eq_ignore_ascii_case("off") {
                if log {
                    eprintln!("ldmuxd: scope off (LDMUX_MESH_SCOPE=off): every flood is unscoped");
                }
                return Self {
                    scope: None,
                    source: "off",
                };
            }
            // **A pin is checked before it is used.** Region names are lower-case letters, digits
            // and hyphens (the firmware's `is_name_char` rejects space, `*`, `,` and `:` as well),
            // the hash covers the exact bytes, and a name nobody has is not a degraded scope — it is
            // a scope every repeater drops. Refusing a bad pin and staying unscoped is the safe
            // direction, and it is said out loud rather than silently ignored.
            //
            // **A `$`-prefixed name is refused here by the character rule**, which is the right
            // answer for a different reason too: the firmware treats a leading `$` as a *stored key*
            // and never hashes it (`RegionMap.cpp:173-188`), so a derived key would name a region no
            // repeater carries and only floods would fail — silently.
            let bare = pin.trim().trim_start_matches('#');
            let valid = !bare.is_empty()
                && bare.len() <= 30
                && bare
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
            if !valid {
                eprintln!(
                    "ldmuxd: scope '{pin}' is not a region name (lower-case letters, digits and \
                     '-', up to 30); staying unscoped"
                );
                return Self {
                    scope: None,
                    source: "none",
                };
            }
            let scope = FloodScope::from_name(bare);
            if log {
                eprintln!("ldmuxd: scope {} pinned by LDMUX_MESH_SCOPE", scope.name);
            }
            return Self {
                scope: Some(scope),
                source: "env",
            };
        }
        match companion.get_default_flood_scope() {
            Ok(Some(scope)) => {
                if log {
                    eprintln!("ldmuxd: scope {} from the radio", scope.name);
                }
                Self {
                    scope: Some(scope),
                    source: "radio",
                }
            }
            // A radio with none is the firmware's companion default, and unscoped floods are what
            // most meshes forward. It is not an error, and it is not a place to guess.
            Ok(None) | Err(_) => Self {
                scope: None,
                source: "none",
            },
        }
    }

    /// The scope for a builder, or `None` for an unscoped flood.
    #[cfg_attr(not(test), allow(dead_code))]
    fn as_ref(&self) -> Option<&FloodScope> {
        self.scope.as_ref()
    }

    /// What the snapshot shows: the name and the key, or nothing.
    fn info(&self) -> Option<ScopeInfo> {
        self.scope.as_ref().map(|s| ScopeInfo {
            name: s.name.clone(),
            key: s.key,
        })
    }
}

/// The largest record a pathless flood may carry.
fn flood_record_limit() -> u8 {
    u8::try_from(LDMUX_FLOOD_RECORD_MAX.min(RECORD_RECEIVED_MAX))
        .unwrap_or(1)
        .max(1)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The peer's radio key as the state directory holds it, or `None` when absent or malformed.
///
/// Held as 64 hex characters, the same shape as the width record, because a person may have to read
/// it out of a state directory on a host whose phone is not with them.
fn load_peer(path: &std::path::Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

fn session_error(e: SessionError) -> MeshError {
    MeshError::Io(std::io::Error::other(e.to_string()))
}

/// What opening the radio produced, for the caller that publishes it.
pub struct Opened {
    pub carrier: RadioCarrier,
    pub bound: Bound,
    pub preset: RadioPreset,
    /// The flood scope this node's broadcasts carry, when it has one.
    pub scope: Option<ScopeInfo>,
    /// The peer's radio key, when the state directory remembers one.
    pub peer_key: Option<[u8; 32]>,
}

/// Open the device, identify the radio, and start serving it.
///
/// # Errors
///
/// [`MeshError`] when the port cannot be opened or locked, or the radio does not answer `APP_START`
/// or `DEVICE_QUERY`.
pub async fn open_radio(
    device: &str,
    window_record: Option<std::path::PathBuf>,
    peer_record: Option<std::path::PathBuf>,
) -> Result<Opened, MeshError> {
    let path = device.to_owned();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare(&path, window_record, peer_record)
    })
    .await
    .map_err(|e| MeshError::Io(std::io::Error::other(e.to_string())))??;
    let (companion, _bound, settings) = prepared;
    let preset = settings.preset;
    let scope = settings.scope.info();
    let peer_key = settings.peer_key;
    let carrier_settings = CarrierSettings {
        hash_size: settings.hash_size,
        own_key: preset.public_key,
        // **A flood names its destination by one byte of the peer's key.** Until the peer's key is
        // known the byte is zero, and the first flood the peer sends teaches it
        // (`classify_push` → `Command::SetPeerHash`).
        peer_key: peer_key.unwrap_or([0u8; 32]),
        scope: settings.scope.scope.clone(),
        airtime_ms: Some(settings.airtime_ms),
    };
    let radio = RadioSettings {
        repeat_enabled: settings.repeat_enabled,
        freq_mhz: preset.freq_mhz,
        bw_khz: preset.bw_khz,
        sf: preset.sf,
        cr: preset.cr,
        tx_power_dbm: preset.tx_power_dbm,
        airtime_ms: settings.airtime_ms,
        window_record: settings.window_record,
        peer_record: settings.peer_record,
    };
    let (carrier, bound) = RadioCarrier::spawn(companion, carrier_settings, radio);
    Ok(Opened {
        carrier,
        bound,
        preset,
        scope,
        peer_key,
    })
}

/// What `prepare` learned, in the shape the carrier wants.
struct Settings {
    /// Bytes per path hash, from the firmware's reported path-hash mode.
    hash_size: u8,
    /// Time on air for a full record, from the preset the radio reports.
    airtime_ms: f64,
    /// What this radio is actually tuned to.
    preset: RadioPreset,
    /// Where the **peer's radio key** is remembered, so a restarted daemon can still address a
    /// flood to the peer instead of waiting for its next advert.
    peer_record: Option<std::path::PathBuf>,
    /// The peer's radio key as the record held it at open.
    peer_key: Option<[u8; 32]>,
    /// The radio's own client-repeat flag, as the firmware reported it.
    ///
    /// Carried into every preset write because `CMD_SET_RADIO_PARAMS`'s sixth byte is *persisted* as
    /// `setRepeatEn(repeat != 0)`: a frame that stops at the coding rate clears an operator's repeat
    /// setting.
    repeat_enabled: Option<bool>,
    /// Where the hour's spent airtime is recorded, so a restart resumes the same hour.
    window_record: Option<std::path::PathBuf>,
    /// The flood scope this node's broadcasts carry.
    scope: ScopeChoice,
}

/// Open the port and identify the radio. Runs off the async runtime: it blocks.
fn prepare(
    device: &str,
    window_record: Option<std::path::PathBuf>,
    peer_record: Option<std::path::PathBuf>,
) -> Result<(Companion<SerialLink>, Bound, Settings), MeshError> {
    let link = SerialLink::open(device).map_err(MeshError::Io)?;
    link.lock_exclusive().map_err(|e| {
        MeshError::Io(std::io::Error::other(format!(
            "{device}: another process holds this radio ({e})"
        )))
    })?;
    let mut companion = Companion::new(link);
    let self_info = companion.app_start("ldmux").map_err(session_error)?;
    // **The radio's clock is what stamps an advert.** A companion has no network and starts its
    // clock at zero at boot, so `CMD_GET_ADVERT_PATH` would report route ages against the epoch and
    // every "is this path still true" question would be unanswerable. One command, best effort.
    let _ = companion.set_time(u32::try_from(unix_now()).unwrap_or(u32::MAX));
    // **The host advertises too, and that is how the phone learns a route to it.** A record reaches
    // past one hop only along a path: the firmware never hands a flooded plaintext record to the
    // application (`crates/mesh/src/companion/push.rs`, AR-10), so each end needs the other's chain
    // in its radio's advert cache. Without this the phone's `GET_ADVERT_PATH` is answered
    // `ERR_CODE_NOT_FOUND` for ever and every record it sends is a one-hop broadcast.
    //
    // Best effort and once per link: an advert is a broadcast with no acknowledgement, and a radio
    // that refuses it (a full queue) costs a route rather than the link.
    let _ = companion.send_self_advert(true);
    // The version this sends is what the firmware latches to decide whether it emits its V3 frames
    // (`MyMesh.cpp:1028-1031`); `crates/companion` sends 3.
    let device_info = companion.device_query().map_err(session_error)?;
    // **The radio's width is the radio's, and ldmux does not write it.** We are optimising for the
    // public MeshCore mesh, where one byte is the convention every client and repeater shares. A
    // node that quietly widened itself costs one extra byte per hop in every record it sends, for a
    // property only its neighbours see, and it stops matching the hashes the rest of the mesh stores
    // for it. Read what the firmware reports, publish it, and follow it.
    let reported_mode = device_info.path_hash_mode;
    let hash_size = reported_mode.map_or(1, |m| m.saturating_add(1).clamp(1, 3));
    eprintln!("ldmuxd: host radio path hashes: {hash_size} byte(s)");
    let airtime_ms = self_info.time_on_air_ms(RECORD_MAX);
    let repeat_enabled = device_info.repeat_enabled;
    let bound = Bound {
        // No route yet, so no path bytes are spent: the receiver's cap is the whole answer until
        // `Event::Limit` revises it as soon as a route is learned.
        uplink_record_max: flood_record_limit(),
        radio_state: ldmux_mesh::constants::RadioState::Connected,
        // No gateway is in the path: this process owns the radio, and the firmware is the meter.
        budget: ldmux_mesh::carrier::Budget::RadioGoverned,
        airtime_ms: Some(airtime_ms),
    };
    let preset = RadioPreset {
        freq_mhz: self_info.radio_freq_mhz,
        bw_khz: self_info.radio_bw_khz,
        sf: self_info.radio_sf,
        cr: self_info.radio_cr,
        tx_power_dbm: i8::try_from(self_info.tx_power).unwrap_or(0),
        public_key: self_info.public_key,
        hash_size,
    };
    let peer_key = peer_record
        .as_deref()
        .and_then(load_peer)
        .filter(|key| *key != self_info.public_key);
    // **What the radio itself says about flood scoping**, read before anything is transmitted: the
    // operator's own MeshCore app sets it there (`region default`), and a regional firmware build
    // can ship one. Reading it is the difference between following a decision someone made and
    // inventing one.
    let scope = ScopeChoice::resolve(&mut companion, true);
    Ok((
        companion,
        bound,
        Settings {
            hash_size,
            airtime_ms,
            preset,
            peer_record,
            peer_key,
            repeat_enabled,
            window_record,
            scope,
        },
    ))
}

/// How long a write to the radio may take before the caller is told it is still blocked.
///
/// Kept here because it is a property of the port, not of the session: the carrier's own send bound
/// is the same figure.
pub const PORT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_companion::MemoryLink;

    /// A scope comes from the radio or an explicit pin, and never from a guess: a name nobody has is
    /// a scope every repeater drops, so an invalid pin leaves the node unscoped rather than scoped
    /// wrongly.
    #[test]
    fn a_scope_comes_from_the_radio_or_a_pin_and_never_from_a_guess() {
        let mut companion = Companion::new(MemoryLink::new());
        // SAFETY: these keys are read only by `ScopeChoice::resolve`, and the tests that write them
        // take turns by owning the keys for their whole body.
        unsafe { std::env::remove_var("LDMUX_MESH_SCOPE") };
        let none = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(none.source, "none", "a radio that says nothing is unscoped");
        assert!(none.info().is_none());

        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_SCOPE", "off") };
        let off = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(off.source, "off");
        assert!(off.as_ref().is_none(), "off means every flood is unscoped");

        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_SCOPE", "#us-co") };
        let pinned = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(pinned.source, "env");
        let scope = pinned.as_ref().expect("a pin is a scope");
        assert_eq!(scope.name, "us-co", "the leading # is not part of the name");
        assert_eq!(
            scope.key,
            ldmux_companion::FloodScope::from_name("us-co").key,
            "a pin derives the public key the firmware would"
        );

        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_SCOPE", "not a region") };
        let bad = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(bad.source, "none", "an invalid pin is refused, not used");
        assert!(bad.as_ref().is_none());

        // SAFETY: as above.
        unsafe { std::env::remove_var("LDMUX_MESH_SCOPE") };
    }

    /// The peer's radio key survives a restart, so a host that comes back can address a flood to the
    /// phone instead of waiting for its next advert.
    #[test]
    fn the_peers_radio_key_is_remembered_across_a_restart() {
        let dir = std::env::temp_dir().join(format!("ldmux-peer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mesh-peer");
        assert!(load_peer(&path).is_none(), "nothing recorded yet");
        let key = [0x5Au8; 32];
        std::fs::write(&path, crate::mesh_carrier::hex_key(&key)).unwrap();
        assert_eq!(load_peer(&path), Some(key), "what the last contact learned");
        std::fs::write(&path, "not a key").unwrap();
        assert!(load_peer(&path).is_none(), "a malformed record is no key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pathless flood is sized for the path it will accumulate, not for the empty-path maximum: a
    /// record built to the direct figure does not fit its carrier and is refused at the first hop.
    #[test]
    fn a_pathless_flood_is_sized_below_the_direct_maximum() {
        assert_eq!(flood_record_limit(), 162);
        assert!(
            flood_record_limit() < u8::try_from(RECORD_MAX).unwrap(),
            "a flood that assumed an empty path would be refused at the first repeater"
        );
    }

}
