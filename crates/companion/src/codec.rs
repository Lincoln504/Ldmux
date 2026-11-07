//! The two things this crate must build and read.
//!
//! **Identity** — `SELF_INFO` (0x05) says what the radio is tuned to; `DEVICE_INFO`
//! (0x0D) says what the firmware can do. Both are parsed leniently: a short frame
//! from older firmware degrades to the fields it actually carries instead of
//! raising, because a client that throws on an unknown tail cannot serve two
//! firmware generations.
//!
//! **Packets** — a raw send is the one place this crate builds MeshCore's own
//! wire format, and it is copied from the phone rather than invented:
//! `AndroidRadioLink.rawPacketFor` writes `[header][path byte][path][record]` with
//! `header = 0x3E` (route DIRECT, payload RAW_CUSTOM). The radio then transmits
//! those bytes as given, which is why the header constants live here and not in
//! the caller.

/// Response and push codes this crate interprets.
pub mod codes {
    pub const OK: u8 = 0x00;
    pub const ERROR: u8 = 0x01;
    pub const SELF_INFO: u8 = 0x05;
    pub const CONTACT_START: u8 = 0x02;
    pub const DEVICE_INFO: u8 = 0x0D;
    pub const NO_MORE_MESSAGES: u8 = 0x0A;
    pub const CHANNEL_MSG_RECV: u8 = 0x08;
    pub const CHANNEL_MSG_RECV_V3: u8 = 0x11;
    pub const LOG_RX_DATA: u8 = 0x88;
    /// The reply to [`commands::GET_ADVERT_PATH`]: `received u32`, `path byte`, path bytes.
    pub const ADVERT_PATH: u8 = 0x16;
}

/// Command codes this crate sends.
pub mod commands {
    pub const APP_START: u8 = 0x01;
    /// Tune the radio: `freq u32 LE` (kHz), `bw u32 LE` (Hz), `sf u8`, `cr u8`. The firmware
    /// validates every one of them and answers `OK` or `ERR_CODE_ILLEGAL_ARG`
    /// (`MyMesh.cpp:1386-1420`), then saves the prefs and retunes the driver.
    ///
    /// The host owns a radio of its own, and until this existed nothing in ldmux could tell it
    /// what to tune to: the user had to configure it with a different tool while the phone
    /// configured its own from the app. There is no reason for that asymmetry.
    pub const SET_RADIO_PARAMS: u8 = 0x0B;
    pub const SYNC_NEXT_MESSAGE: u8 = 0x0A;
    pub const DEVICE_QUERY: u8 = 0x16;
    /// Send a complete packet we built: `priority u8`, then the packet bytes.
    ///
    /// This is `CMD_SEND_RAW_PACKET`, the command that takes a whole `[header][path][payload]`
    /// frame (`examples/companion_radio/MyMesh.cpp`, `tryParsePacket`), so the route type and the
    /// path encoding are ours to write correctly. Its older sibling `CMD_SEND_RAW_DATA` (0x19)
    /// takes a path-length byte that the firmware reads both as a byte count and as an encoded
    /// value, which agrees only at a one-byte hash.
    pub const SEND_RAW_PACKET: u8 = 0x41;
    /// Emit this node's advert: `mode u8`, where 1 floods it and 0 sends it zero-hop
    /// (`MyMesh.cpp:1257-1272`).
    ///
    /// **This is how the peer learns a path to us.** A flooded advert is forwarded by repeaters and
    /// carries the accumulated path, so the receiving companion stores it as a contact with an
    /// out-path — a route that survives the radio being out of direct range. Nothing else in
    /// MeshCore tells a peer how to reach a node it has never heard.
    pub const SEND_SELF_ADVERT: u8 = 0x07;
    /// Read the path a recently heard advert took: `reserved u8`, then the originator's **full
    /// 32-byte** public key (`MyMesh.cpp:1851`, guard `len >= PUB_KEY_SIZE + 2`). The cache is keyed
    /// by the first seven bytes of it, but the request must carry all 32.
    ///
    /// **This is the only route a companion can learn on stock firmware.** A flooded request
    /// (`CMD_SEND_PATH_DISCOVERY_REQ`) is answered with a path return only when the destination
    /// produces a reply, which needs a contact entry and a telemetry permission that defaults to
    /// deny; the advert-path cache needs neither.
    pub const GET_ADVERT_PATH: u8 = 0x2A;
    /// Set the width of the path hashes **this radio's own packets** carry:
    /// `reserved u8`, then the mode (`MyMesh.cpp:1466-1473`). Mode 0 is one byte (the firmware's
    /// factory default), 1 is two, 2 is three; 3 or more is `ERR_CODE_ILLEGAL_ARG`.
    ///
    /// **It does not change what the radio forwards.** A repeater appends at the width the packet
    /// arrives with, whatever its own mode says, so this decides adverts only — and ldmux's adverts
    /// are the only way a peer learns a route here.
    pub const SET_PATH_HASH_MODE: u8 = 0x3D;
}

/// The protocol version this client claims. The firmware latches it from
/// `DEVICE_QUERY` and gates its V3 message frames on `>= 3`
/// (`MyMesh.cpp:1028-1031`, `:435`, `:548`).
pub const APP_TARGET_VERSION: u8 = 3;

/// Header byte for a raw custom packet sent DIRECT: `route = 2`,
/// `payload_type = 15` (RAW_CUSTOM), version 0. Identical to the phone's
/// `RAW_CUSTOM_DIRECT_HEADER`.
pub const RAW_CUSTOM_DIRECT_HEADER: u8 = 0x3E;

/// The path byte a flooded packet carries.
pub const PATH_FLOOD: u8 = 0xFF;

/// Build the path byte for a direct packet: hash width in the top two bits, hop
/// count in the low six. `hash_size` is 1..=3: the two bits can name 4, and the firmware reserves
/// that width for the future (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`).
pub fn path_byte(hash_size: u8, hops: u8) -> Result<u8, CodecError> {
    // 4 is reserved for the future (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`).
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    if hops > 0x3F {
        return Err(CodecError::TooManyHops(hops));
    }
    Ok(((hash_size - 1) << 6) | hops)
}

/// The largest packet a `CMD_SEND_RAW_PACKET` frame may carry.
///
/// The firmware's frame ceiling is 176 bytes (`MAX_FRAME_SIZE`, `BaseSerialInterface.h:5`), and a raw
/// send is `[0x41][priority][header][path_len][path][record]` — the command and the priority are two
/// of those bytes, so the packet itself gets 174. A longer frame is **truncated** by the serial
/// reader (`checkRecvFrame`, `ArduinoSerialInterface.cpp:56-59`), which turns an oversized record
/// into a parse failure rather than a send, so the limit has to be exact here.
pub const PACKET_MAX: usize = 174;

/// The firmware's own path ceiling, in bytes (`MAX_PATH_SIZE`, `src/MeshCore.h:22`).
///
/// `Packet::isValidPathLen` refuses anything longer (`src/Packet.cpp:13-18`), so a packet built
/// with a longer path is dropped by every receiver while this end believes it was sent. The hop
/// count alone cannot catch it: 63 one-byte hops fit the encoded byte, and 63 three-byte hops are
/// 189 bytes.
pub const MAX_PATH_SIZE: usize = 64;

/// Build a raw direct packet around `record`.
///
/// **Both ends of the path are checked here.** A path whose length is not a multiple of the hash
/// width cannot be encoded: the hop count truncates and the receiver walks `hops * hash` bytes,
/// landing inside the path, so every record fails its tag and the link looks like a radio fault
/// rather than a caller's mistake. A packet over [`PACKET_MAX`] would be truncated by the firmware's
/// serial reader, and refusing it here names the fault at its source.
pub fn build_raw_direct(hash_size: u8, path: &[u8], record: &[u8]) -> Result<Vec<u8>, CodecError> {
    // 4 is reserved for the future and `Packet::isValidPathLen` refuses it (`src/Packet.cpp:13-18`),
    // so a path built at that width would be dropped by every receiver.
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    if !path.len().is_multiple_of(usize::from(hash_size)) {
        return Err(CodecError::RaggedPath {
            len: path.len(),
            hash: hash_size,
        });
    }
    // **A path longer than the firmware can hold is refused here.** 63 three-byte hops fit the
    // encoded length byte and are 189 bytes, which `isValidPathLen` rejects, so the packet would
    // be sent, dropped by every receiver, and reported as a link that carries nothing.
    if path.len() > MAX_PATH_SIZE {
        return Err(CodecError::TooLong {
            len: path.len(),
            max: MAX_PATH_SIZE,
        });
    }
    let total = 2 + path.len() + record.len();
    if total > PACKET_MAX {
        return Err(CodecError::TooLong { len: total, max: PACKET_MAX });
    }
    let encoded = path_byte(hash_size, (path.len() / hash_size as usize) as u8)?;
    let mut out = Vec::with_capacity(2 + path.len() + record.len());
    out.push(RAW_CUSTOM_DIRECT_HEADER);
    out.push(encoded);
    out.extend_from_slice(path);
    out.extend_from_slice(record);
    Ok(out)
}

/// A route read out of the radio's advert-path cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertPath {
    /// The advert's own timestamp, as the firmware stored it.
    pub received_at: u32,
    /// Bytes per hop the path was heard with (1..=3; 4 is reserved and never parsed).
    pub hash_size: u8,
    /// Hops, in travel order towards us.
    pub hops: u8,
    /// The path bytes, `hops * hash_size` of them.
    pub bytes: Vec<u8>,
}

/// Parse a `RESP_CODE_ADVERT_PATH` frame, code included.
///
/// The length byte is the firmware's own encoding (`((hash_size - 1) << 6) | hops`), not a count of
/// bytes, so a frame whose byte count disagrees with it is refused rather than half-read.
#[must_use]
pub fn parse_advert_path(frame: &[u8]) -> Option<AdvertPath> {
    if frame.first() != Some(&codes::ADVERT_PATH) || frame.len() < 6 {
        return None;
    }
    let received_at = u32::from_le_bytes(frame.get(1..5)?.try_into().ok()?);
    let path_byte = *frame.get(5)?;
    let hash_size = (path_byte >> 6) + 1;
    // The two width bits can name 4, and the firmware reserves that width for the future
    // (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`), so a frame claiming it is malformed.
    if hash_size == 4 {
        return None;
    }
    let hops = path_byte & 0x3F;
    let want = usize::from(hops) * usize::from(hash_size);
    let bytes = frame.get(6..6 + want)?.to_vec();
    Some(AdvertPath {
        received_at,
        hash_size,
        hops,
        bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    BadHashSize(u8),
    TooManyHops(u8),
    /// A path that does not divide into whole hashes: the hop count would truncate and the
    /// receiver would split the path in the wrong place.
    RaggedPath { len: usize, hash: u8 },
    /// A frame the firmware's `readPath` buffer cannot hold.
    TooLong { len: usize, max: usize },
    /// A reply that is not the one the parse expected: the firmware answered, but with
    /// another command's frame. A framing fault, not a path one.
    WrongResponse { want: u8, got: u8 },
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::BadHashSize(n) => write!(f, "path hash size {n} is not 1..=3"),
            CodecError::TooManyHops(n) => write!(f, "{n} hops exceeds the 6-bit path length"),
            CodecError::RaggedPath { len, hash } => {
                write!(f, "a {len}-byte path is not a whole number of {hash}-byte hashes")
            }
            CodecError::TooLong { len, max } => {
                write!(f, "a {len}-byte packet exceeds the {max}-byte limit for a raw send")
            }
            CodecError::WrongResponse { want, got } => {
                write!(f, "expected response {want:#04x}, got {got:#04x}")
            }
        }
    }
}

impl std::error::Error for CodecError {}

/// What the radio says it is and how it is tuned (`RESP_SELF_INFO`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SelfInfo {
    pub adv_type: u8,
    pub tx_power: u8,
    pub max_tx_power: u8,
    pub public_key: [u8; 32],
    pub adv_lat: f64,
    pub adv_lon: f64,
    pub adv_loc_policy: u8,
    pub radio_freq_mhz: f64,
    pub radio_bw_khz: f64,
    pub radio_sf: u8,
    pub radio_cr: u8,
    pub name: String,
}

/// Time on air for a payload, in milliseconds, from a tuple.
///
/// The firmware's own formula, with the 32-symbol preamble used below SF8. **One copy**, because
/// two callers need it: the radio reporting its own tuple, and a retune computing what the new
/// tuple will cost before the radio has been asked. A budget computed from a constant rather than
/// from this is a budget for a radio nobody has.
pub fn time_on_air_ms(bw_khz: f64, sf: u8, cr: u8, payload_len: usize) -> f64 {
    let bw_hz = bw_khz * 1000.0;
    if bw_hz <= 0.0 || sf == 0 {
        return 0.0;
    }
    let sf_f = f64::from(sf);
    let cr_f = f64::from(cr).max(1.0);
    // Header is explicit in MeshCore; CRC on. Preamble 32 symbols below SF8.
    let preamble = if sf < 8 { 8.0 } else { 16.0 };
    let symbol_ms = (2f64.powf(sf_f)) / bw_hz * 1000.0;
    let numerator = 8.0 * payload_len as f64 - 4.0 * sf_f + 28.0 + 16.0;
    let denominator = 4.0 * (sf_f - 2.0 * 0.0);
    let payload_symbols = 8.0 + (numerator / denominator).ceil().max(0.0) * cr_f;
    (preamble + 4.25 + payload_symbols) * symbol_ms
}

impl SelfInfo {
    /// Time on air for a payload under the tuple this radio reports.
    pub fn time_on_air_ms(&self, payload_len: usize) -> f64 {
        time_on_air_ms(self.radio_bw_khz, self.radio_sf, self.radio_cr, payload_len)
    }
}

/// What the firmware says it can do (`RESP_DEVICE_INFO`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceInfo {
    pub protocol_version: u8,
    pub max_contacts: Option<u16>,
    pub max_channels: Option<u8>,
    pub ble_pin: Option<u32>,
    pub firmware_build: String,
    pub manufacturer: String,
    pub firmware_version: String,
    pub repeat_enabled: Option<bool>,
    pub path_hash_mode: Option<u8>,
}

/// Parse `RESP_SELF_INFO`, stopping at whatever the frame length allows.
pub fn parse_self_info(payload: &[u8]) -> Result<SelfInfo, CodecError> {
    match payload.first() {
        Some(&codes::SELF_INFO) => {}
        Some(&got) => return Err(CodecError::WrongResponse { want: codes::SELF_INFO, got }),
        // An empty frame is a truncated reply, and the fields below are parsed as far as the
        // length allows; there is nothing to parse here.
        None => return Err(CodecError::WrongResponse { want: codes::SELF_INFO, got: 0 }),
    }
    let mut info = SelfInfo::default();
    let mut rest = &payload[1..];
    macro_rules! take {
        ($n:expr) => {{
            if rest.len() < $n {
                return Ok(info);
            }
            let (head, tail) = rest.split_at($n);
            rest = tail;
            head
        }};
    }
    // The same bytes as a fixed-size array: `take!` has already proved the length, so this cannot
    // fail, and it does not need an `unwrap` to say so. A truncated SELF_INFO from older firmware
    // ends the parse at the first missing field, which is what the `return Ok(info)` above is for.
    macro_rules! take_array {
        ($n:expr) => {{
            let head = take!($n);
            let mut out = [0u8; $n];
            out.copy_from_slice(head);
            out
        }};
    }
    info.adv_type = take!(1)[0];
    info.tx_power = take!(1)[0];
    info.max_tx_power = take!(1)[0];
    info.public_key = take_array!(32);
    info.adv_lat = i32::from_le_bytes(take_array!(4)) as f64 / 1e6;
    info.adv_lon = i32::from_le_bytes(take_array!(4)) as f64 / 1e6;
    let _multi_acks = take!(1)[0];
    info.adv_loc_policy = take!(1)[0];
    let _telemetry = take!(1)[0];
    let _manual = take!(1)[0];
    info.radio_freq_mhz = u32::from_le_bytes(take_array!(4)) as f64 / 1000.0;
    info.radio_bw_khz = u32::from_le_bytes(take_array!(4)) as f64 / 1000.0;
    info.radio_sf = take!(1)[0];
    info.radio_cr = take!(1)[0];
    info.name = String::from_utf8_lossy(rest).trim_end_matches('\0').to_string();
    Ok(info)
}

/// Parse `RESP_DEVICE_INFO`. Only the protocol version is guaranteed; every later
/// field is version-gated, which is how one client serves several firmware
/// generations.
pub fn parse_device_info(payload: &[u8]) -> Option<DeviceInfo> {
    if payload.first() != Some(&codes::DEVICE_INFO) || payload.len() < 2 {
        return None;
    }
    let mut info = DeviceInfo { protocol_version: payload[1], ..Default::default() };
    let mut rest = &payload[2..];
    if info.protocol_version >= 3 && rest.len() >= 1 + 1 + 4 + 12 + 40 + 20 {
        info.max_contacts = Some(rest[0] as u16 * 2); // firmware stores half
        info.max_channels = Some(rest[1]);
        info.ble_pin = Some(u32::from_le_bytes([rest[2], rest[3], rest[4], rest[5]]));
        info.firmware_build = text(&rest[6..18]);
        info.manufacturer = text(&rest[18..58]);
        info.firmware_version = text(&rest[58..78]);
        rest = &rest[78..];
    }
    if info.protocol_version >= 9 && !rest.is_empty() {
        info.repeat_enabled = Some(rest[0] != 0);
        rest = &rest[1..];
    }
    if info.protocol_version >= 10 && !rest.is_empty() {
        info.path_hash_mode = Some(rest[0]);
    }
    Some(info)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end_matches('\0').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_info_frame(name: &str) -> Vec<u8> {
        let mut v = vec![codes::SELF_INFO, 1, 22, 22];
        v.extend((0..32).map(|i| i as u8));
        v.extend((-45_683_412i32).to_le_bytes());
        v.extend((-111_094_072i32).to_le_bytes());
        v.extend([0u8, 1, 0, 0]);
        v.extend(914_250u32.to_le_bytes());
        v.extend(500_000u32.to_le_bytes());
        v.extend([8u8, 5]);
        v.extend(name.as_bytes());
        v
    }

    #[test]
    fn self_info_round_trips() {
        let info = parse_self_info(&self_info_frame("HELTEC-BASE")).unwrap();
        assert_eq!(info.name, "HELTEC-BASE");
        assert_eq!(info.radio_freq_mhz, 914.25);
        assert_eq!(info.radio_bw_khz, 500.0);
        assert_eq!(info.radio_sf, 8);
        assert_eq!(info.radio_cr, 5);
        assert_eq!(info.tx_power, 22);
        assert!((info.adv_lat + 45.683412).abs() < 1e-9);
    }

    #[test]
    fn a_truncated_self_info_degrades_instead_of_panicking() {
        let full = self_info_frame("X");
        for cut in 1..full.len() {
            let info = parse_self_info(&full[..cut]).unwrap();
            assert!(info.name.is_empty() || !info.name.is_empty());
        }
    }

    #[test]
    fn the_wrong_code_is_not_self_info() {
        // A framing fault, named as one: it used to be reported as `TooManyHops`, a path error,
        // which sent a reader looking at the route when the answer was simply another command's.
        match parse_self_info(&[codes::DEVICE_INFO, 3]) {
            Err(CodecError::WrongResponse { want, got }) => {
                assert_eq!((want, got), (codes::SELF_INFO, codes::DEVICE_INFO));
            }
            other => panic!("expected a framing error, got {other:?}"),
        }
        assert!(parse_self_info(&[]).is_err());
    }

    #[test]
    fn device_info_reads_the_version_gated_tail() {
        let mut v = vec![codes::DEVICE_INFO, 13, 175 /* 350/2 */, 40];
        v.extend(123_456u32.to_le_bytes());
        v.extend(b"14 Aug 2026".iter().copied().chain(std::iter::repeat(0)).take(12));
        v.extend(b"Heltec V4.3 OLED".iter().copied().chain(std::iter::repeat(0)).take(40));
        v.extend(b"v1.17.1".iter().copied().chain(std::iter::repeat(0)).take(20));
        v.push(0); // repeat
        v.push(2); // path hash mode
        let info = parse_device_info(&v).unwrap();
        assert_eq!(info.protocol_version, 13);
        assert_eq!(info.max_contacts, Some(350));
        assert_eq!(info.max_channels, Some(40));
        assert_eq!(info.firmware_version, "v1.17.1");
        assert_eq!(info.manufacturer, "Heltec V4.3 OLED");
        assert_eq!(info.repeat_enabled, Some(false));
        assert_eq!(info.path_hash_mode, Some(2));
    }

    #[test]
    fn an_old_device_info_has_no_tail() {
        let info = parse_device_info(&[codes::DEVICE_INFO, 2]).unwrap();
        assert_eq!(info.protocol_version, 2);
        assert_eq!(info.max_contacts, None);
    }

    #[test]
    fn path_byte_encodes_hash_width_in_the_top_two_bits() {
        assert_eq!(path_byte(1, 0).unwrap(), 0x00);
        assert_eq!(path_byte(2, 3).unwrap(), 0x43);
        assert_eq!(path_byte(3, 63).unwrap(), 0b1000_0000 | 63);
        assert!(path_byte(0, 0).is_err());
        assert!(path_byte(5, 0).is_err());
        // 4 is reserved for the future and `Packet::isValidPathLen` refuses it, so a path built at
        // that width would be dropped by every receiver.
        assert!(path_byte(4, 1).is_err());
        assert!(path_byte(1, 64).is_err());
    }

    #[test]
    fn a_raw_direct_packet_matches_the_phones_layout() {
        let path = [0xAA, 0xBB];
        let record = [1u8, 2, 3];
        let packet = build_raw_direct(2, &path, &record).unwrap();
        assert_eq!(packet[0], 0x3E, "direct raw-custom header");
        assert_eq!(packet[1], 0x41, "one hop of two-byte hashes");
        assert_eq!(&packet[2..4], &path);
        assert_eq!(&packet[4..], &record);
    }

    #[test]
    fn a_path_longer_than_the_firmware_holds_is_refused() {
        // 22 hops at three bytes is 66 bytes: the encoded length byte has room for it, the
        // firmware's 64-byte path buffer does not (`MAX_PATH_SIZE`, `src/MeshCore.h:22`), and a
        // packet carrying it is dropped by every receiver while this end believes it was sent.
        assert_eq!(MAX_PATH_SIZE, 64);
        let long = vec![0u8; 3 * 22];
        assert!(matches!(
            build_raw_direct(3, &long, &[0u8; 8]),
            Err(CodecError::TooLong { max, .. }) if max == MAX_PATH_SIZE
        ));
        // 21 hops at three bytes is 63, inside the ceiling, and still encodable.
        let fits = vec![0u8; 3 * 21];
        assert!(build_raw_direct(3, &fits, &[0u8; 8]).is_ok());
    }

    #[test]
    fn airtime_grows_with_payload_and_with_spreading_factor() {
        let mut slow = parse_self_info(&self_info_frame("x")).unwrap();
        slow.radio_bw_khz = 500.0;
        slow.radio_sf = 8;
        let sf8 = slow.time_on_air_ms(172);
        slow.radio_sf = 9;
        let sf9 = slow.time_on_air_ms(172);
        assert!(sf9 > sf8, "one spreading factor step doubles the symbol time");
        assert!(slow.time_on_air_ms(50) < sf9 * 2.0);
        assert!(sf8 > 10.0 && sf8 < 500.0, "a 172-byte frame is tens to hundreds of ms");
    }
}
