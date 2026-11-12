//! Building the two packets a session record rides, for **both** ends.
//!
//! A record goes out one of two ways, and the difference is the whole of `docs/RADIO-PLAN.md` §4:
//! a `RAW_CUSTOM` packet along a known path, or a `REQ` flood carrying the record inside the `LDMR`
//! wrapper. The builders are here rather than in `ldmux-companion` because the phone composes the
//! same packets over BLE and cannot link a host crate (`§8.1`, `IC-4`); `ldmux-companion::codec`
//! re-exports them, so the host's call sites read exactly as they did.
//!
//! Everything here is pure: no clock, no I/O, no policy. Which carriage to use is
//! `crate::route::carrier_for`'s decision, and the record is sealed before it reaches this module.

use crate::budget::{MAX_PATH_BYTES, SEND_PACKET_MAX};

/// Header byte for a raw custom packet sent DIRECT: `route = 2` (DIRECT), `payload_type = 15`
/// (RAW_CUSTOM), version 0.
pub const RAW_CUSTOM_DIRECT_HEADER: u8 = 0x3E;

/// Header byte for an unscoped flood whose payload is a route request: `route = 1` (FLOOD),
/// `payload_type = 0` (REQ), version 0.
///
/// **Why `REQ`, and why a flood from a companion at all.** A raw record cannot be flooded: the
/// firmware delivers `RAW_CUSTOM` only on a direct route (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM`
/// accepts only `isRouteDirect()`), and a repeater does not forward one either (the same case never
/// calls `routeRecvPacket`). Every other payload type *is* flood-routed, and `REQ` is the one
/// MeshCore itself uses for an on-demand route request.
pub const ROUTE_FLOOD_HEADER: u8 = 0x01;

/// Header byte for a scoped flood: `route = 0` (TRANSPORT_FLOOD), `payload_type = 0` (REQ).
pub const ROUTE_FLOOD_SCOPED_HEADER: u8 = 0x00;

/// The four bytes that mark a flooded session record.
pub const RECORD_FLOOD_MAGIC: &[u8; 4] = b"LDMR";

/// The path byte a flooded packet carries.
pub const PATH_FLOOD: u8 = 0xFF;

/// The largest packet a `CMD_SEND_RAW_PACKET` frame may carry: 174, the frame ceiling less the
/// command and priority bytes.
pub const PACKET_MAX: usize = SEND_PACKET_MAX;

/// The firmware's own path ceiling, in bytes (`MAX_PATH_SIZE`, `src/MeshCore.h:22`).
pub const MAX_PATH_SIZE: usize = MAX_PATH_BYTES;

/// Why a packet could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// A hash width outside 1..=3; 4 is reserved by the firmware.
    BadHashSize(u8),
    /// More hops than the six-bit path-length field can name.
    TooManyHops(u8),
    /// A path that does not divide into whole hashes: the hop count would truncate and the
    /// receiver would split the path in the wrong place.
    RaggedPath {
        /// The path's length in bytes.
        len: usize,
        /// The hash width it was built for.
        hash: u8,
    },
    /// A packet the firmware's frame cannot hold.
    TooLong {
        /// What the packet would have been.
        len: usize,
        /// The ceiling it broke.
        max: usize,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadHashSize(n) => write!(f, "path hash size {n} is not 1..=3"),
            Self::TooManyHops(n) => write!(f, "{n} hops exceeds the 6-bit path length"),
            Self::RaggedPath { len, hash } => {
                write!(f, "a {len}-byte path is not a whole number of {hash}-byte hashes")
            }
            Self::TooLong { len, max } => {
                write!(f, "a {len}-byte packet exceeds the {max}-byte limit for a raw send")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// The path byte for a direct packet: hash width in the top two bits, hop count in the low six.
///
/// `hash_size` is 1..=3: the two bits can name 4, and the firmware reserves that width for the
/// future (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`).
///
/// # Errors
///
/// [`BuildError::BadHashSize`] or [`BuildError::TooManyHops`].
pub fn path_byte(hash_size: u8, hops: u8) -> Result<u8, BuildError> {
    if !(1..=3).contains(&hash_size) {
        return Err(BuildError::BadHashSize(hash_size));
    }
    if hops > 0x3F {
        return Err(BuildError::TooManyHops(hops));
    }
    Ok(((hash_size - 1) << 6) | hops)
}

/// A packet's scope proof: `HMAC-SHA-256(key, payload_type || payload)` cut to two little-endian
/// bytes, with 0x0000 and 0xFFFF remapped (`TransportKey::calcTransportCode`,
/// `src/helpers/TransportKeyStore.cpp:4-18`).
///
/// **Only the payload and its type are hashed**, so the value survives every hop — the path grows,
/// the payload does not — and the *same* two bytes are what a repeater recomputes to decide whether
/// to forward. That is also why this must be computed over exactly the bytes that go on the air.
#[must_use]
pub fn transport_code(key: &[u8; 16], payload_type: u8, payload: &[u8]) -> u16 {
    use hmac::Mac as _;
    let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(key)
        .expect("HMAC accepts a key of any length");
    mac.update(&[payload_type]);
    mac.update(payload);
    let bytes = mac.finalize().into_bytes();
    let code = u16::from_le_bytes([bytes[0], bytes[1]]);
    match code {
        0 => 1,
        0xFFFF => 0xFFFE,
        other => other,
    }
}

/// Build a raw direct packet around `record`.
///
/// **Both ends of the path are checked here.** A path whose length is not a multiple of the hash
/// width cannot be encoded: the hop count truncates and the receiver walks `hops * hash` bytes, so
/// the record would be read from the wrong offset.
///
/// # Errors
///
/// [`BuildError`] for a hash width, a ragged path, a path past the firmware's ceiling, or a packet
/// that does not fit a send frame.
pub fn raw_direct(hash_size: u8, path: &[u8], record: &[u8]) -> Result<Vec<u8>, BuildError> {
    if !(1..=3).contains(&hash_size) {
        return Err(BuildError::BadHashSize(hash_size));
    }
    if !path.len().is_multiple_of(usize::from(hash_size)) {
        return Err(BuildError::RaggedPath {
            len: path.len(),
            hash: hash_size,
        });
    }
    // **A path longer than the firmware can hold is refused here.** 63 three-byte hops fit the
    // encoded length byte and are 189 bytes, which `isValidPathLen` rejects, so the packet would be
    // sent, dropped by every receiver, and reported as a link that carries nothing.
    if path.len() > MAX_PATH_SIZE {
        return Err(BuildError::TooLong {
            len: path.len(),
            max: MAX_PATH_SIZE,
        });
    }
    let total = 2 + path.len() + record.len();
    if total > PACKET_MAX {
        return Err(BuildError::TooLong {
            len: total,
            max: PACKET_MAX,
        });
    }
    #[allow(clippy::cast_possible_truncation)]
    let hops = (path.len() / usize::from(hash_size)) as u8;
    let encoded = path_byte(hash_size, hops)?;
    let mut out = Vec::with_capacity(total);
    out.push(RAW_CUSTOM_DIRECT_HEADER);
    out.push(encoded);
    out.extend_from_slice(path);
    out.extend_from_slice(record);
    Ok(out)
}

/// Build a flood carrying `record` inside the `LDMR` wrapper.
///
/// The wrapper is `[dest: 1][source: 1][magic: 4][nonce: 4][record]`: the first byte of each radio's
/// public key, so a receiver can tell an addressed record from a stranger's flood, and a **fresh
/// nonce on every transmission** so the firmware's duplicate table does not swallow a retransmission
/// (`SimpleMeshTables`, `Packet::calculatePacketHash`).
///
/// `scope` is the 16-byte transport key when the flood must be scoped, which is the *key* and not
/// the region's name: the name never travels.
///
/// # Errors
///
/// [`BuildError`] for a hash width or a packet that does not fit a send frame.
pub fn record_flood(
    hash_size: u8,
    peer_key: &[u8; 32],
    own_key: &[u8; 32],
    scope: Option<&[u8; 16]>,
    nonce: &[u8; 4],
    record: &[u8],
) -> Result<Vec<u8>, BuildError> {
    if !(1..=3).contains(&hash_size) {
        return Err(BuildError::BadHashSize(hash_size));
    }
    let mut payload = Vec::with_capacity(2 + RECORD_FLOOD_MAGIC.len() + 4 + record.len());
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(RECORD_FLOOD_MAGIC);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(record);
    let mut out = Vec::with_capacity(6 + payload.len());
    match scope {
        None => out.push(ROUTE_FLOOD_HEADER),
        Some(key) => {
            out.push(ROUTE_FLOOD_SCOPED_HEADER);
            let code = transport_code(key, 0, &payload);
            out.extend_from_slice(&code.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    out.push(path_byte(hash_size, 0)?);
    out.extend_from_slice(&payload);
    if out.len() > PACKET_MAX {
        return Err(BuildError::TooLong {
            len: out.len(),
            max: PACKET_MAX,
        });
    }
    Ok(out)
}

/// The complete companion frame that transmits `packet`: `CMD_SEND_RAW_PACKET`, a priority byte,
/// the packet, and the three-byte framing header.
///
/// **Both ends write exactly these bytes**, one to a USB port and one to a BLE characteristic, so
/// the rule for what a send frame is lives here rather than in each client (`docs/RADIO-PLAN.md`
/// §8.1). The priority byte is zero: MeshCore's own queue priority, and ldmux has never needed to
/// raise it.
///
/// # Errors
///
/// [`BuildError::TooLong`] when the packet is past what a send frame may carry.
pub fn send_raw_frame(packet: &[u8]) -> Result<Vec<u8>, BuildError> {
    if packet.len() > PACKET_MAX {
        return Err(BuildError::TooLong {
            len: packet.len(),
            max: PACKET_MAX,
        });
    }
    let mut payload = Vec::with_capacity(2 + packet.len());
    payload.push(0x41); // CMD_SEND_RAW_PACKET
    payload.push(0); // priority
    payload.extend_from_slice(packet);
    crate::companion::framing::encode(&payload).map_err(|e| {
        let crate::companion::framing::FrameError::TooLong(len) = e;
        BuildError::TooLong {
            len,
            max: crate::companion::framing::MAX_PAYLOAD,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::RECEIVE_MAX;

    #[test]
    fn a_direct_packet_names_the_path_it_will_follow() {
        let packet = raw_direct(2, &[0x11, 0x22, 0x33, 0x44], b"record").unwrap();
        assert_eq!(packet[0], RAW_CUSTOM_DIRECT_HEADER);
        // Two 2-byte hops.
        assert_eq!(packet[1], ((2 - 1) << 6) | 2);
        assert_eq!(&packet[2..6], &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(&packet[6..], b"record");
    }

    #[test]
    fn a_ragged_path_is_refused_rather_than_truncated() {
        assert_eq!(
            raw_direct(2, &[1, 2, 3], b"x"),
            Err(BuildError::RaggedPath { len: 3, hash: 2 })
        );
    }

    #[test]
    fn a_reserved_hash_width_is_refused() {
        assert_eq!(raw_direct(4, &[], b"x"), Err(BuildError::BadHashSize(4)));
    }

    #[test]
    fn a_record_past_the_send_ceiling_is_refused() {
        let record = vec![0u8; PACKET_MAX];
        assert!(matches!(
            raw_direct(1, &[], &record),
            Err(BuildError::TooLong { .. })
        ));
    }

    #[test]
    fn a_flood_wraps_the_record_and_names_the_two_radios() {
        let peer = [7u8; 32];
        let own = [9u8; 32];
        let packet = record_flood(1, &peer, &own, None, &[1, 2, 3, 4], b"record").unwrap();
        assert_eq!(packet[0], ROUTE_FLOOD_HEADER);
        assert_eq!(packet[1], 0, "a flood starts with an empty path");
        assert_eq!(&packet[2..4], &[7, 9]);
        assert_eq!(&packet[4..8], RECORD_FLOOD_MAGIC);
        assert_eq!(&packet[8..12], &[1, 2, 3, 4]);
        assert_eq!(&packet[12..], b"record");
    }

    #[test]
    fn a_scoped_flood_carries_the_code_and_costs_four_bytes_more() {
        let peer = [7u8; 32];
        let own = [9u8; 32];
        let key = [3u8; 16];
        let plain = record_flood(1, &peer, &own, None, &[0; 4], b"record").unwrap();
        let scoped = record_flood(1, &peer, &own, Some(&key), &[0; 4], b"record").unwrap();
        assert_eq!(scoped[0], ROUTE_FLOOD_SCOPED_HEADER);
        assert_eq!(scoped.len(), plain.len() + 4);
        // The code is an HMAC over the payload, so it must not be one of the two remapped values.
        let code = u16::from_le_bytes([scoped[1], scoped[2]]);
        assert_ne!(code, 0);
        assert_ne!(code, 0xFFFF);
    }

    #[test]
    fn the_scope_code_survives_a_growing_path() {
        // The path grows at every hop and the payload does not, which is why only the payload is
        // hashed: a repeater recomputes the same two bytes several hops away from the sender.
        let key = [5u8; 16];
        let payload = b"the payload";
        assert_eq!(
            transport_code(&key, 0, payload),
            transport_code(&key, 0, payload)
        );
        assert_ne!(transport_code(&key, 0, payload), transport_code(&key, 1, payload));
    }

    #[test]
    fn a_send_frame_carries_the_command_the_priority_and_the_packet() {
        let packet = raw_direct(1, &[], b"sealed").unwrap();
        let frame = send_raw_frame(&packet).unwrap();
        assert_eq!(frame[0], crate::companion::framing::SOF_HOST);
        assert_eq!(
            usize::from(u16::from_le_bytes([frame[1], frame[2]])),
            frame.len() - 3
        );
        assert_eq!(frame[3], 0x41);
        assert_eq!(frame[4], 0);
        assert_eq!(&frame[5..], packet.as_slice());
    }

    #[test]
    fn a_send_frame_longer_than_the_firmware_accepts_is_refused() {
        assert!(matches!(
            send_raw_frame(&[0u8; PACKET_MAX + 1]),
            Err(BuildError::TooLong { .. })
        ));
    }

    #[test]
    fn a_full_record_flood_stays_reported_at_the_receiver() {
        // The ceiling the flood budget promises: a 93-byte record at the longest legal path.
        let packet = record_flood(
            1,
            &[0; 32],
            &[0; 32],
            Some(&[1; 16]),
            &[0; 4],
            &[0u8; 93],
        )
        .unwrap();
        assert!(packet.len() <= RECEIVE_MAX, "{} bytes", packet.len());
    }
}
