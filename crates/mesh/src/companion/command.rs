//! The companion's **command** side: one opcode at a time, and the identity reports it answers.
//!
//! `push` says what the radio volunteers; this says what it is asked. Both ends ask the same
//! questions — `crates/companion` over a USB port and the phone over BLE — so the opcodes, the
//! layouts and the answer table live here rather than being written twice
//! (`docs/RADIO-PLAN.md` §8.1). `ldmux-companion::codec` re-exports every item below, so the host's
//! callers are unchanged.
//!
//! **The encoder produces the payload, not the frame.** A serial write wraps it (`0x3C`, length,
//! payload) and a BLE write does not: the characteristic carries the payload alone, which is why
//! `MeshcoreFrameCodec` reads byte 0 as the opcode. Putting the framing in here would have a phone
//! write three bytes the radio reads as a command.
//!
//! Every layout is read from the firmware's own dispatch (`handleCmdFrame`,
//! `examples/companion_radio/MyMesh.cpp`), and each one degrades leniently: an older firmware that
//! stops short ends the parse where its frame ends rather than failing, because one client has to
//! serve several firmware generations.
pub mod codes {
    pub const OK: u8 = 0x00;
    /// `RESP_CODE_SENT` — the answer to a raw send on firmware that confirms sends.
    ///
    /// Ten bytes on the v1.18 protocol: the route flag, the tag, and **the firmware's own suggested
    /// timeout** for that send. A client that only knows `OK` sees this as a framing fault, so the
    /// same command works on one firmware and fails on the next with no configuration change.
    pub const SENT: u8 = 0x06;
    pub const ERROR: u8 = 0x01;
    pub const SELF_INFO: u8 = 0x05;
    /// The reply to [`commands::GET_BATT_AND_STORAGE`]: `battery_mv u16`, `used_kb u32`,
    /// `total_kb u32` (`MyMesh.cpp:1518-1529`). Eleven bytes.
    pub const BATT_AND_STORAGE: u8 = 0x0C;
    pub const CONTACT_START: u8 = 0x02;
    pub const DEVICE_INFO: u8 = 0x0D;
    pub const NO_MORE_MESSAGES: u8 = 0x0A;
    pub const CHANNEL_MSG_RECV: u8 = 0x08;
    pub const CHANNEL_MSG_RECV_V3: u8 = 0x11;
    pub const LOG_RX_DATA: u8 = 0x88;
    /// The reply to [`commands::GET_ADVERT_PATH`]: `received u32`, `path byte`, path bytes.
    pub const ADVERT_PATH: u8 = 0x16;
    /// The reply to [`commands::GET_DEFAULT_FLOOD_SCOPE`]: `name 31`, `key 16`, or the code alone
    /// when the radio has no default scope (`MyMesh.cpp:1964-1972`).
    pub const DEFAULT_FLOOD_SCOPE: u8 = 0x1C;
    /// `RESP_CODE_DISABLED`: the firmware is in another state and cannot serve the command now
    /// (`MyMesh.cpp:86`). A retry a moment later is the right response, not a reparse.
    pub const DISABLED: u8 = 0x0F;
    /// The reply to [`commands::GET_STATS`]: `type u8`, then that type's fields.
    pub const STATS: u8 = 0x18;
    /// The stats sub-types, sent as the request's and the reply's second byte.
    pub const STATS_TYPE_CORE: u8 = 0;
    pub const STATS_TYPE_RADIO: u8 = 1;
    /// The firmware's "no reading" RSSI. A radio that has heard nothing reports this rather than a
    /// plausible number, so a link is never credited with a reading it does not have.
    pub const UNKNOWN_RSSI: i8 = i8::MIN;
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
    /// Set transmit power: **one signed byte of dBm** (`MyMesh.cpp:1386`), window -9..max.
    ///
    /// Separate from the tuple because it is a separate register, and global to the radio like the
    /// tuple: whichever app writes it last is what the radio transmits at.
    pub const SET_RADIO_TX_POWER: u8 = 0x0C;
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
    /// Set the radio's clock: `unix seconds u32` little-endian.
    ///
    /// **The radio's clock is what stamps an advert**, and ldmux reads those stamps back as route
    /// ages (`CMD_GET_ADVERT_PATH` returns `recv_timestamp`). A companion has no network, so its
    /// clock starts at zero at boot: without this every advert looks as old as the epoch, and the
    /// "is this route still true" question has no answer. The firmware accepts a time only if it is
    /// not in the past (`MyMesh.cpp:1247-1256`), which a wall clock always is.
    pub const SET_DEVICE_TIME: u8 = 0x06;

    /// `CMD_GET_STATS`, whose second byte selects what to ask for (firmware v8+).
    ///
    /// **The radio's own counters are the only ground truth about the air.** `STATS_RADIO` reports
    /// the noise floor, the RSSI and SNR of the last packet *it heard*, and its own transmit and
    /// receive airtime — so a link can be judged on what the radio measured rather than on what the
    /// application hoped, and the daemon's own meter has something to be checked against.
    pub const GET_STATS: u8 = 0x38;
    /// Read the radio's **default flood scope**: no argument, and the answer is
    /// [`codes::DEFAULT_FLOOD_SCOPE`] with the name and the 16-byte key, or the code alone when the
    /// radio has none (`MyMesh.cpp:1964-1972`).
    ///
    /// **The radio is the source of truth for the scope**, because the operator's own MeshCore app
    /// sets it there (`region default`, the app's Experimental Settings from v1.43.0) and a regional
    /// firmware build can ship one (`DEFAULT_FLOOD_SCOPE_NAME`). ldmux reads it rather than asking
    /// for the same fact twice in two places.
    pub const GET_DEFAULT_FLOOD_SCOPE: u8 = 0x40;
    /// Write the radio's default flood scope: `name 31` then `key 16`; a frame of the code alone
    /// clears it (`MyMesh.cpp:1947-1963`). Used only when ldmux *learned* a scope from the air and
    /// the radio had none, so the operator's device and this daemon agree.
    pub const SET_DEFAULT_FLOOD_SCOPE: u8 = 0x3F;
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
    /// Read the radio's battery and its contact store: no argument, answered by
    /// [`codes::BATT_AND_STORAGE`] (`MyMesh.cpp:1518-1529`).
    ///
    /// **It is how a phone shows a battery.** The status the app draws used to carry a voltage only
    /// when the operator's own MeshCore app had read it; asking is one local command, and the answer
    /// is what the radio measured rather than what the app guessed.
    pub const GET_BATT_AND_STORAGE: u8 = 0x14;
}

/// The protocol version this client claims. The firmware latches it from `DEVICE_QUERY` and gates
/// its V3 message frames on `>= 3` (`MyMesh.cpp:1028-1031`, `:435`, `:548`).
pub const APP_TARGET_VERSION: u8 = 3;

use crate::companion::fifo::{CommandSpec, Entry, UNIVERSAL_ANSWERS};

/// How long a command may wait for its answer, by the kind of work the firmware does to produce it.
///
/// A command whose handler writes its answer from the dispatch loop needs a round trip; one that
/// saves preferences first is a flash write on both boards. `TRANSMIT` is a send, whose answer comes
/// after the packet is queued rather than after it is transmitted.
const PROMPT_MS: u32 = 1_000;
const FLASH_MS: u32 = 3_000;
const TRANSMIT_MS: u32 = 2_000;

/// The phone's answer contract, as [`CommandSpec`] wants it: which codes complete which command.
///
/// **The host's table is not this one.** The two ends ask different questions — a phone reads its
/// battery and configures its own tuple, a host reads adverts and route scopes — and a table that
/// claimed both would settle a command neither end sent. `crates/companion/src/window.rs` is the
/// host's; this is the phone's, and the client engine supplies it (`docs/RADIO-PLAN.md` §8.1,
/// `AR-8`).
///
/// Every command without an entry here is settled by the universal codes alone, which is correct:
/// `SET_RADIO_PARAMS`, `SET_RADIO_TX_POWER` and `SET_PATH_HASH_MODE` each end in `writeOKFrame()`
/// or `writeErrFrame(...)` (`MyMesh.cpp:1386-1420`, `:1462-1500`, `:1506-1518`).
pub const PHONE_COMMANDS: CommandSpec = CommandSpec {
    universal: UNIVERSAL_ANSWERS,
    entries: &[
        // `APP_START` (`0x01`) → `RESP_CODE_SELF_INFO` (`MyMesh.cpp:1076-1114`).
        Entry::typed(commands::APP_START, &[codes::SELF_INFO], PROMPT_MS),
        // `DEVICE_QUERY` (`0x16`) → `RESP_CODE_DEVICE_INFO` (`:1056-1075`).
        Entry::typed(commands::DEVICE_QUERY, &[codes::DEVICE_INFO], PROMPT_MS),
        // `GET_BATT_AND_STORAGE` (`0x14`) → `RESP_CODE_BATT_AND_STORAGE` (`:1518-1529`).
        Entry::typed(
            commands::GET_BATT_AND_STORAGE,
            &[codes::BATT_AND_STORAGE],
            PROMPT_MS,
        ),
        // `GET_STATS` (`0x38`) → `RESP_CODE_STATS` (`:1916-1990`).
        Entry::typed(commands::GET_STATS, &[codes::STATS], PROMPT_MS),
        // A raw send is answered `OK` on firmware that does not confirm, `RESP_CODE_SENT` on
        // firmware that does, and `ERR_CODE_ILLEGAL_ARG`/`ERR_CODE_TABLE_FULL` when it cannot
        // (`:2047-2060`). The errors are universal; `SENT` is the one code that has to be named.
        Entry::typed(commands::SEND_RAW_PACKET, &[codes::SENT], TRANSMIT_MS),
        // `SEND_SELF_ADVERT` (`0x07`) → `OK`. *The phone asks too now*: an advert is how the mesh's
        // own path cache learns where this radio is, and a learned path is the only way a record
        // travels past one hop, because the firmware never delivers a flooded plaintext record
        // (`crates/mesh/src/companion/push.rs`, AR-10).
        Entry::typed(commands::SEND_SELF_ADVERT, &[codes::OK], PROMPT_MS),
        // `GET_ADVERT_PATH` (`0x2A`) → `RESP_CODE_ADVERT_PATH` (`:1895-1915`), or `ERR_CODE_NOT_FOUND`
        // when the radio has not heard that node recently — no route, which is not a fault.
        Entry::typed(commands::GET_ADVERT_PATH, &[codes::ADVERT_PATH], PROMPT_MS),
        // The tuple, the power and the path width are flash writes.
        Entry::terminal(commands::SET_RADIO_PARAMS, FLASH_MS),
        Entry::terminal(commands::SET_RADIO_TX_POWER, FLASH_MS),
        Entry::terminal(commands::SET_PATH_HASH_MODE, FLASH_MS),
    ],
};

/// A route the radio's own advert cache remembers: `received_at`, the path's width, and the chain.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdvertPath {
    /// The advert's own timestamp, as the firmware stored it.
    pub received_at: u32,
    /// Bytes per hop the path was heard with (1..=3; 4 is reserved and never parsed).
    pub hash_size: u8,
    /// The chain in travel order **towards this radio**, so a sender reverses it.
    pub bytes: Vec<u8>,
}

impl AdvertPath {
    /// The chain reversed hop group by hop group, which is the path a packet must carry to reach
    /// the node that advertised.
    ///
    /// **Reversed by hop, not by byte.** A flood appends each hop's hash at the end as it travels
    /// (`Mesh.cpp:344-357`) while a direct packet pops from the front (`removeSelfFromPath`), so the
    /// outbound path is the arrival chain with its groups in the opposite order. Reversing bytes
    /// instead produces a path no receiver recognises.
    #[must_use]
    pub fn outbound(&self) -> Vec<u8> {
        let width = usize::from(self.hash_size.clamp(1, 3));
        let mut out = Vec::with_capacity(self.bytes.len());
        for hop in self.bytes.chunks(width).rev() {
            out.extend_from_slice(hop);
        }
        out
    }
}

/// Parse a `RESP_CODE_ADVERT_PATH` frame, code included (`MyMesh.cpp:1906-1912`).
///
/// The length byte is the firmware's own encoding (`((hash_size - 1) << 6) | hops`), not a count of
/// bytes, so a frame whose byte count disagrees with it is refused rather than half-read.
#[must_use]
pub fn parse_advert_path(frame: &[u8]) -> Option<AdvertPath> {
    if frame.first() != Some(&codes::ADVERT_PATH) {
        return None;
    }
    let received_at = u32::from_le_bytes(frame.get(1..5)?.try_into().ok()?);
    let path_byte = *frame.get(5)?;
    let hash_size = (path_byte >> 6) + 1;
    if hash_size == 4 {
        return None;
    }
    let hops = path_byte & 0x3F;
    let want = usize::from(hops) * usize::from(hash_size);
    let bytes = frame.get(6..6 + want)?.to_vec();
    Some(AdvertPath {
        received_at,
        hash_size,
        bytes,
    })
}

/// Why an answer could not be read as the report it was supposed to be.
///
/// Separate from the host's `CodecError`, which also carries the packet-builder's faults: this is
/// only about reading a command's answer, and a caller that only parses needs no vocabulary for a
/// path that is not a whole number of hashes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandError {
    /// The frame's first byte is not the answer this command is settled by.
    WrongAnswer {
        /// The code the command asked for.
        want: u8,
        /// The code that arrived.
        got: u8,
    },
    /// The frame is shorter than the layout it claims.
    Truncated,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongAnswer { want, got } => {
                write!(f, "expected answer {want:#04x}, got {got:#04x}")
            }
            Self::Truncated => write!(f, "the radio's answer is shorter than its layout"),
        }
    }
}

impl std::error::Error for CommandError {}

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
///
/// [`crate::airtime::airtime_ms`] is the same arithmetic with the tuple as a struct; the two are
/// pinned against each other by `crates/companion/tests/airtime_agreement.rs`, which is what makes
/// keeping this signature safe rather than a second opinion.
#[must_use]
pub fn time_on_air_ms(bw_khz: f64, sf: u8, cr: u8, payload_len: usize) -> f64 {
    let bw_hz = bw_khz * 1000.0;
    if bw_hz <= 0.0 || sf == 0 {
        return 0.0;
    }
    let sf_f = f64::from(sf);
    // The firmware's coding rate is the denominator index RadioLib reads it as: 5 is 4/5, and the
    // symbol multiplier is that number, which is the datasheet's `CR + 4` with CR counted from 1.
    let cr_f = f64::from(cr).clamp(5.0, 8.0);
    let symbol_ms = (2f64.powf(sf_f)) / bw_hz * 1000.0;
    // **32 preamble symbols at SF8 and below**, 16 above: `preambleLengthForSF` in the
    // firmware (`src/helpers/radiolib/RadioLibWrappers.h:56`). It was 8 here, which understated a
    // record by about 50 ms at 62.5 kHz — an 8 % error in the hour's allowance, and the phone's own
    // arithmetic already used 32, so the two ends disagreed about what an hour buys.
    let preamble = if sf <= 8 { 32.0 } else { 16.0 };
    // Low-data-rate optimization is automatic in RadioLib: it is on once a symbol is 16 ms or
    // longer (`SX126x::setModulationParams`, `SX126x_commands.cpp:220-228`), and it halves the
    // denominator in the payload-symbol count.
    let de = if symbol_ms >= 16.0 { 1.0 } else { 0.0 };
    // Explicit header, CRC on, 20 header symbols folded into the +28 (the firmware's own
    // `sfCoeff2 + N_symbol_header`, `SX126x.cpp:755-759`).
    let numerator = 8.0 * payload_len as f64 - 4.0 * sf_f + 28.0 + 16.0;
    let denominator = 4.0 * (sf_f - 2.0 * de);
    let payload_symbols = 8.0 + (numerator / denominator).ceil().max(0.0) * cr_f;
    (preamble + 4.25 + payload_symbols) * symbol_ms
}

impl SelfInfo {
    /// Time on air for a payload under the tuple this radio reports.
    #[must_use]
    pub fn time_on_air_ms(&self, payload_len: usize) -> f64 {
        time_on_air_ms(self.radio_bw_khz, self.radio_sf, self.radio_cr, payload_len)
    }
}

/// What the firmware says it can do (`RESP_DEVICE_INFO`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceInfo {
    /// The **firmware's** version code (`FIRMWARE_VER_CODE`), which is the gate for the trailing
    /// fields below.
    ///
    /// **Not the app protocol version.** That one is [`APP_TARGET_VERSION`], which this client sends
    /// in `DEVICE_QUERY` and the firmware latches to choose its frame shapes (`MyMesh.cpp:1028-1031`).
    /// The two axes were both called `protocol_version`, which made the next reader check the wrong
    /// one against the wrong feature gate.
    pub firmware_version_code: u8,
    pub max_contacts: Option<u16>,
    pub max_channels: Option<u8>,
    pub ble_pin: Option<u32>,
    pub firmware_build: String,
    pub manufacturer: String,
    pub firmware_version: String,
    pub repeat_enabled: Option<bool>,
    pub path_hash_mode: Option<u8>,
}

/// What the radio measured on the air, and what it spent there (`RESP_STATS`, `STATS_TYPE_RADIO`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadioStats {
    /// The receiver's noise floor, in dBm. A negative number, and the floor a link must clear.
    pub noise_floor_dbm: i16,
    /// RSSI of the last packet the radio heard, in dBm. [`codes::UNKNOWN_RSSI`] when none yet.
    pub last_rssi: i8,
    /// SNR of that packet in **quarters of a dB**, which is the firmware's own scale: an integer
    /// keeps the reading comparable, and a quarter of a dB is finer than the measurement is.
    pub last_snr_quarter_db: i8,
    /// The radio's own transmit airtime since boot, in seconds.
    pub tx_air_secs: u32,
    /// The radio's own receive airtime since boot, in seconds.
    pub rx_air_secs: u32,
}

impl RadioStats {
    /// Whether the radio has heard anything since it booted.
    #[must_use]
    pub fn has_signal(&self) -> bool {
        self.last_rssi != codes::UNKNOWN_RSSI
    }

    /// The SNR in dB, for display.
    #[must_use]
    pub fn last_snr_db(&self) -> f32 {
        f32::from(self.last_snr_quarter_db) / 4.0
    }
}

/// The radio's battery and its contact store (`RESP_CODE_BATT_AND_STORAGE`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryAndStorage {
    /// The board's battery voltage in millivolts.
    pub millivolts: u16,
    /// Contact-store pages used, in kilobytes.
    pub storage_used_kb: u32,
    /// Contact-store size, in kilobytes.
    pub storage_total_kb: u32,
}

/// Parse `RESP_CODE_SELF_INFO`, stopping at whatever the frame length allows.
///
/// # Errors
///
/// [`CommandError::WrongAnswer`] when the frame is not a `SELF_INFO`, which is the only case that is
/// an error: a short frame from older firmware is a `SelfInfo` with its known fields filled in, not a
/// fault.
pub fn parse_self_info(payload: &[u8]) -> Result<SelfInfo, CommandError> {
    match payload.first() {
        Some(&codes::SELF_INFO) => {}
        Some(&got) => {
            return Err(CommandError::WrongAnswer {
                want: codes::SELF_INFO,
                got,
            });
        }
        // An empty frame cannot be a reply: say so with the code it was supposed to carry.
        None => {
            return Err(CommandError::WrongAnswer {
                want: codes::SELF_INFO,
                got: 0,
            });
        }
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
    // fail, and it does not need an `unwrap` to say so.
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
    info.adv_lat = f64::from(i32::from_le_bytes(take_array!(4))) / 1e6;
    info.adv_lon = f64::from(i32::from_le_bytes(take_array!(4))) / 1e6;
    let _multi_acks = take!(1)[0];
    info.adv_loc_policy = take!(1)[0];
    let _telemetry = take!(1)[0];
    let _manual = take!(1)[0];
    info.radio_freq_mhz = f64::from(u32::from_le_bytes(take_array!(4))) / 1000.0;
    info.radio_bw_khz = f64::from(u32::from_le_bytes(take_array!(4))) / 1000.0;
    info.radio_sf = take!(1)[0];
    info.radio_cr = take!(1)[0];
    info.name = String::from_utf8_lossy(rest).trim_end_matches('\0').to_string();
    Ok(info)
}

/// Parse `RESP_DEVICE_INFO`. Only the firmware version is guaranteed; every later field is
/// version-gated, which is how one client serves several firmware generations.
#[must_use]
pub fn parse_device_info(payload: &[u8]) -> Option<DeviceInfo> {
    if payload.first() != Some(&codes::DEVICE_INFO) || payload.len() < 2 {
        return None;
    }
    let mut info = DeviceInfo {
        firmware_version_code: payload[1],
        ..Default::default()
    };
    let mut rest = &payload[2..];
    if info.firmware_version_code >= 3 && rest.len() >= 1 + 1 + 4 + 12 + 40 + 20 {
        info.max_contacts = Some(u16::from(rest[0]) * 2); // firmware stores half
        info.max_channels = Some(rest[1]);
        info.ble_pin = Some(u32::from_le_bytes([rest[2], rest[3], rest[4], rest[5]]));
        info.firmware_build = text(&rest[6..18]);
        info.manufacturer = text(&rest[18..58]);
        info.firmware_version = text(&rest[58..78]);
        rest = &rest[78..];
    }
    if info.firmware_version_code >= 9 && !rest.is_empty() {
        info.repeat_enabled = Some(rest[0] != 0);
        rest = &rest[1..];
    }
    if info.firmware_version_code >= 10 && !rest.is_empty() {
        info.path_hash_mode = Some(rest[0]);
    }
    Some(info)
}

/// Parse `RESP_STATS` for `STATS_TYPE_RADIO`.
///
/// # Errors
///
/// [`CommandError::WrongAnswer`] when the frame is not a radio-stats answer (a core-stats answer
/// says so by its type byte), or [`CommandError::Truncated`] when it stops inside the layout.
pub fn parse_radio_stats(payload: &[u8]) -> Result<RadioStats, CommandError> {
    match payload.first() {
        Some(&codes::STATS) => {}
        Some(&got) => {
            return Err(CommandError::WrongAnswer {
                want: codes::STATS,
                got,
            });
        }
        None => return Err(CommandError::Truncated),
    }
    if payload.get(1) != Some(&codes::STATS_TYPE_RADIO) {
        return Err(CommandError::WrongAnswer {
            want: codes::STATS_TYPE_RADIO,
            got: payload.get(1).copied().unwrap_or(0),
        });
    }
    let noise = payload.get(2..4).ok_or(CommandError::Truncated)?;
    let rssi = payload.get(4).copied().ok_or(CommandError::Truncated)?;
    let snr = payload.get(5).copied().ok_or(CommandError::Truncated)?;
    let tx = payload.get(6..10).ok_or(CommandError::Truncated)?;
    let rx = payload.get(10..14).ok_or(CommandError::Truncated)?;
    Ok(RadioStats {
        noise_floor_dbm: i16::from_le_bytes([noise[0], noise[1]]),
        last_rssi: rssi as i8,
        last_snr_quarter_db: snr as i8,
        tx_air_secs: u32::from_le_bytes([tx[0], tx[1], tx[2], tx[3]]),
        rx_air_secs: u32::from_le_bytes([rx[0], rx[1], rx[2], rx[3]]),
    })
}

/// Parse `RESP_CODE_BATT_AND_STORAGE`.
///
/// # Errors
///
/// [`CommandError::WrongAnswer`] for another code, [`CommandError::Truncated`] for a frame that stops
/// inside the eleven-byte layout.
pub fn parse_battery_and_storage(payload: &[u8]) -> Result<BatteryAndStorage, CommandError> {
    match payload.first() {
        Some(&codes::BATT_AND_STORAGE) => {}
        Some(&got) => {
            return Err(CommandError::WrongAnswer {
                want: codes::BATT_AND_STORAGE,
                got,
            });
        }
        None => return Err(CommandError::Truncated),
    }
    let mv = payload.get(1..3).ok_or(CommandError::Truncated)?;
    let used = payload.get(3..7).ok_or(CommandError::Truncated)?;
    let total = payload.get(7..11).ok_or(CommandError::Truncated)?;
    Ok(BatteryAndStorage {
        millivolts: u16::from_le_bytes([mv[0], mv[1]]),
        storage_used_kb: u32::from_le_bytes([used[0], used[1], used[2], used[3]]),
        storage_total_kb: u32::from_le_bytes([total[0], total[1], total[2], total[3]]),
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

/// The longest app name `APP_START` carries. The firmware null-terminates the frame in place
/// (`MyMesh.cpp:1078`), so a name that leaves the frame room to grow is the name that is read.
pub const APP_NAME_MAX: usize = 31;

/// `CMD_APP_START`: say hello, and be answered with [`SelfInfo`].
///
/// **The frame is `0x01`, seven reserved bytes, then the app name, and the name is not
/// decoration.** The firmware handles this command only under `len >= 8`
/// (`examples/companion_radio/MyMesh.cpp:1076-1080`: `cmd_frame[0] == CMD_APP_START && len >= 8`,
/// with the name read from `cmd_frame[8]`), so a bare `0x01` is **silently ignored**: the radio
/// accepts the write and answers nothing, `SELF_INFO` never arrives, and the link looks like a
/// radio that is present and mute. Measured on the phone 2026-11-12: `ble send 1B` and `ble send
/// ok 1B` in a loop, zero notifications, and the settings screen reading "not read" for ever.
///
/// The reserved bytes stay zero because the firmware latches the protocol version from
/// [`device_query`], not from this frame.
#[must_use]
pub fn app_start(app_name: &str) -> Vec<u8> {
    let name = &app_name.as_bytes()[..app_name.len().min(APP_NAME_MAX)];
    let mut out = Vec::with_capacity(8 + name.len());
    out.push(commands::APP_START);
    out.extend_from_slice(&[0u8; 7]);
    out.extend_from_slice(name);
    out
}

/// `CMD_SEND_SELF_ADVERT`: emit this radio's advert, flooded so repeaters forward it.
///
/// The firmware answers `OK` for "queued", never for "heard": an advert is a broadcast with no
/// acknowledgement, which is why the path it teaches appears in the cache a moment later.
#[must_use]
pub fn self_advert(flood: bool) -> Vec<u8> {
    vec![commands::SEND_SELF_ADVERT, u8::from(flood)]
}

/// `CMD_GET_ADVERT_PATH`: read the route a recently heard advert took.
///
/// The request carries the **whole** 32-byte key even though the firmware's cache is keyed by the
/// first seven bytes: its guard is `len >= PUB_KEY_SIZE + 2` and it then compares seven bytes
/// (`MyMesh.cpp:1895-1902`), so a short frame is answered `ERR_CODE_UNSUPPORTED_CMD` and no route
/// is ever learned.
#[must_use]
pub fn get_advert_path(pub_key: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 32);
    out.push(commands::GET_ADVERT_PATH);
    out.push(0); // reserved
    out.extend_from_slice(pub_key);
    out
}

/// `CMD_DEVICE_QUERY`: ask what the firmware can do, latching [`APP_TARGET_VERSION`].
#[must_use]
pub fn device_query() -> Vec<u8> {
    vec![commands::DEVICE_QUERY, APP_TARGET_VERSION]
}

/// `CMD_GET_STATS` for `STATS_TYPE_RADIO`: the noise floor, the last reading and the airtime.
#[must_use]
pub fn get_radio_stats() -> Vec<u8> {
    vec![commands::GET_STATS, codes::STATS_TYPE_RADIO]
}

/// `CMD_GET_BATT_AND_STORAGE`: the board's voltage and the contact store's fill.
#[must_use]
pub fn get_battery_and_storage() -> Vec<u8> {
    vec![commands::GET_BATT_AND_STORAGE]
}

/// `CMD_SET_RADIO_PARAMS`: `freq kHz u32`, `bw Hz u32`, `sf u8`, `cr u8`.
#[must_use]
pub fn set_radio_params(freq_khz: u32, bw_hz: u32, sf: u8, cr: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.push(commands::SET_RADIO_PARAMS);
    out.extend_from_slice(&freq_khz.to_le_bytes());
    out.extend_from_slice(&bw_hz.to_le_bytes());
    out.push(sf);
    out.push(cr);
    out
}

/// `CMD_SET_RADIO_TX_POWER`: one signed byte of dBm.
#[must_use]
pub fn set_radio_tx_power(dbm: i8) -> Vec<u8> {
    vec![commands::SET_RADIO_TX_POWER, dbm as u8]
}

/// `CMD_SET_PATH_HASH_MODE`: `reserved u8 (0)`, then the mode, which is the width minus one.
#[must_use]
pub fn set_path_hash_mode(mode: u8) -> Vec<u8> {
    vec![commands::SET_PATH_HASH_MODE, 0, mode]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phone's table names every command it sends, so a typed answer settles its own command
    /// rather than every command.
    #[test]
    fn every_command_the_phone_sends_has_an_answer_contract() {
        for (command, answer) in [
            (commands::APP_START, codes::SELF_INFO),
            (commands::DEVICE_QUERY, codes::DEVICE_INFO),
            (commands::GET_BATT_AND_STORAGE, codes::BATT_AND_STORAGE),
            (commands::GET_STATS, codes::STATS),
            (commands::SEND_RAW_PACKET, codes::SENT),
        ] {
            assert!(
                PHONE_COMMANDS.accepts(command, answer),
                "command {command:#04x} is not settled by its own answer {answer:#04x}"
            );
            // And a reply for a *different* typed command settles nothing.
            assert!(
                !PHONE_COMMANDS.accepts(command, codes::BATT_AND_STORAGE)
                    || answer == codes::BATT_AND_STORAGE,
                "command {command:#04x} is settled by someone else's answer"
            );
        }
    }

    /// **The phone's table is still not the host's, and the difference has moved.**
    ///
    /// It used to exclude the advert path with the reasoning that "a phone never asks for one".
    /// That reasoning is false: a path learned from an advert is the only way a phone's record
    /// travels past one hop *directly*, and a flood is a fallback that costs every repeater in range
    /// a transmission. The flood-scope commands stay excluded, because a phone pins its scope through
    /// the settings screen and never reads one back.
    #[test]
    fn the_phones_table_claims_the_advert_path_and_not_the_flood_scope() {
        assert!(
            PHONE_COMMANDS
                .entry(crate::companion::command::commands::GET_ADVERT_PATH)
                .is_some(),
            "the path a phone learns from an advert is how it reaches past one hop"
        );
        assert!(
            PHONE_COMMANDS
                .entry(crate::companion::command::commands::GET_DEFAULT_FLOOD_SCOPE)
                .is_none(),
            "a phone never reads a flood scope back"
        );
    }

    #[test]
    fn an_advert_path_reverses_into_the_path_a_packet_carries() {
        // **Reversed by hop, not by byte.** A flood appends each hop's hash as it travels, so the
        // chain towards us is the reverse of the chain towards the peer. One-byte hops make the two
        // indistinguishable, which is why the three-byte case is the one that matters.
        let frame = |width: u8, hops: &[u8]| {
            let mut f = vec![codes::ADVERT_PATH];
            f.extend_from_slice(&0u32.to_le_bytes());
            f.push(((width - 1) << 6) | u8::try_from(hops.len() / usize::from(width)).unwrap());
            f.extend_from_slice(hops);
            f
        };
        let one = parse_advert_path(&frame(1, &[0xAA, 0xBB, 0xCC])).expect("one-byte hops");
        assert_eq!(one.outbound(), vec![0xCC, 0xBB, 0xAA]);
        let two = parse_advert_path(&frame(2, &[0x11, 0x12, 0x21, 0x22])).expect("two-byte hops");
        assert_eq!(two.outbound(), vec![0x21, 0x22, 0x11, 0x12], "groups swapped, bytes kept");
        let three =
            parse_advert_path(&frame(3, &[1, 2, 3, 4, 5, 6])).expect("three-byte hops");
        assert_eq!(three.outbound(), vec![4, 5, 6, 1, 2, 3]);
        // A width the firmware reserves is malformed, not a path.
        let mut bad = frame(1, &[0xAA]);
        bad[5] = 0xC0;
        assert_eq!(parse_advert_path(&bad), None);
    }

    #[test]
    fn the_advert_commands_carry_what_the_firmware_reads() {
        assert_eq!(self_advert(true), vec![commands::SEND_SELF_ADVERT, 1]);
        assert_eq!(self_advert(false), vec![commands::SEND_SELF_ADVERT, 0]);
        let key = [7u8; 32];
        let frame = get_advert_path(&key);
        assert_eq!(frame.len(), 34, "the firmware's guard is len >= PUB_KEY_SIZE + 2");
        assert_eq!(&frame[2..], &key, "the whole key, not the seven-byte cache prefix");
    }

    #[test]
    fn a_battery_answer_reads_as_its_three_facts() {
        let mut frame = vec![codes::BATT_AND_STORAGE];
        frame.extend_from_slice(&4_050u16.to_le_bytes());
        frame.extend_from_slice(&12u32.to_le_bytes());
        frame.extend_from_slice(&256u32.to_le_bytes());
        let parsed = parse_battery_and_storage(&frame).expect("eleven bytes is the layout");
        assert_eq!(parsed.millivolts, 4_050);
        assert_eq!(parsed.storage_used_kb, 12);
        assert_eq!(parsed.storage_total_kb, 256);
    }

    #[test]
    fn a_short_battery_answer_is_refused_rather_than_guessed() {
        assert_eq!(
            parse_battery_and_storage(&[codes::BATT_AND_STORAGE, 1, 2]),
            Err(CommandError::Truncated)
        );
        assert_eq!(
            parse_battery_and_storage(&[codes::SELF_INFO]),
            Err(CommandError::WrongAnswer {
                want: codes::BATT_AND_STORAGE,
                got: codes::SELF_INFO
            })
        );
    }

    #[test]
    fn the_parameters_are_the_bytes_the_firmware_reads() {
        assert_eq!(
            set_radio_params(910_525, 62_500, 7, 5),
            vec![commands::SET_RADIO_PARAMS, 0xBD, 0xE4, 0x0D, 0x00, 0x24, 0xF4, 0x00, 0x00, 7, 5]
        );
        // The power is signed, and the firmware reads it as one byte window `-9..=max`.
        assert_eq!(set_radio_tx_power(-9), vec![commands::SET_RADIO_TX_POWER, 0xF7]);
        // The width is a mode, so one byte of width is mode zero.
        assert_eq!(set_path_hash_mode(0), vec![commands::SET_PATH_HASH_MODE, 0, 0]);
    }

    #[test]
    fn the_queries_are_one_command_and_its_argument() {
        // **The name is load-bearing**, not decoration: the firmware gates the whole command on
        // `len >= 8` (`MyMesh.cpp:1076`), so a bare `0x01` is a command the radio ignores while
        // accepting the write.
        assert_eq!(app_start("ldmux"), b"\x01\0\0\0\0\0\0\0ldmux".to_vec());
        assert!(app_start("").len() >= 8, "the firmware needs eight bytes before it answers");
        assert_eq!(app_start(&"n".repeat(64)).len(), 8 + APP_NAME_MAX);
        assert_eq!(device_query(), vec![0x16, APP_TARGET_VERSION]);
        assert_eq!(get_radio_stats(), vec![0x38, 1]);
        assert_eq!(get_battery_and_storage(), vec![0x14]);
    }
}
