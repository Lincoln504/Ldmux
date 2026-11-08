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
    /// The reply to [`commands::GET_DEFAULT_FLOOD_SCOPE`]: `name 31`, `key 16`, or the code alone
    /// when the radio has no default scope (`MyMesh.cpp:1964-1972`).
    pub const DEFAULT_FLOOD_SCOPE: u8 = 0x1C;
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

/// Header byte for a flooded packet whose payload is a *route request*: `route = 1` (FLOOD),
/// `payload_type = 0` (REQ), version 0.
///
/// **Why `REQ`, and why a flood from a companion at all.** A raw record cannot be flooded: the
/// firmware delivers `RAW_CUSTOM` only on a direct route (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM`
/// accepts only `isRouteDirect()`), and a repeater does not forward one either (the same case never
/// calls `routeRecvPacket`). Every other payload type *is* flood-routed, and `REQ` is the one
/// MeshCore itself uses for an on-demand route request: `sendRequest` sends a `REQ` as a flood when
/// the peer's path is unknown (`BaseChatMesh.cpp:632-660`). So the route request below is a `REQ`
/// whose "encrypted" tail is ldmux's own four-byte marker plus the requester's radio key — no
/// contact, no telemetry permission, and no firmware change, which is what makes it work on a stock
/// radio.
///
/// **Why this is needed at all.** An advert is the only other flood ldmux can send, and a repeater
/// drops an advert once it has travelled `flood_max_advert` hops — **8 by default**
/// (`examples/simple_repeater/MyMesh.cpp:907`, upstream PR #2702 in v1.16.0). Measured on this
/// machine's local mesh 2026-11-08: adverts arrive at 4, 5, 6 and **8** hops, while `REQ` floods
/// arrive at 18-29 hops. Two radios more than eight hops apart therefore cannot exchange adverts at
/// all, so a route learned only from adverts is unreachable at city distance.
pub const ROUTE_FLOOD_HEADER: u8 = 0x01;

/// The four bytes that mark a flooded **route request**.
pub const ROUTE_REQUEST_MAGIC: &[u8; 4] = b"LDRQ";

/// The four bytes that mark a direct **route reply**, carried as a `RAW_CUSTOM` record.
pub const ROUTE_REPLY_MAGIC: &[u8; 4] = b"LDRR";

/// Route type value for a flood that carries two 16-bit transport codes.
///
/// A scoped flood is `ROUTE_TYPE_TRANSPORT_FLOOD` (0), and the four bytes after the header are
/// `transport_codes[0]` (the region's per-packet code) and `transport_codes[1]` (reserved,
/// `sendFloodScoped` writes zero; `src/Packet.h:16`, `Packet::readFrom`).
pub const ROUTE_TRANSPORT_FLOOD: u8 = 0x00;

/// Header byte for a scoped route request: `route = 0` (TRANSPORT_FLOOD), `payload_type = 0` (REQ).
pub const ROUTE_FLOOD_SCOPED_HEADER: u8 = 0x00;

/// A flood scope: the operator's region name and the 16-byte key its packets are proved with.
///
/// **The name is not on the air.** A scoped packet carries only a 16-bit HMAC of the key over its
/// payload (`TransportKey::calcTransportCode`), so every repeater in the path recomputes the same
/// value from a key it holds, and an observer learns nothing but two bytes. The key is
/// `SHA256("#" + name)` cut to 16 bytes for the public hashtag-style regions everyone uses
/// (`TransportKeyStore::getAutoKeyFor`, `RegionMap::getTransportKeysFor`); a private region whose
/// name begins `$` has a stored key instead, which is why a parsed scope keeps its key rather than
/// re-deriving one from the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloodScope {
    /// The region name as the radio stores it, without the `#` the hash adds (`us`, `us-co`, …).
    pub name: String,
    /// The 16-byte transport key.
    pub key: [u8; 16],
}

impl FloodScope {
    /// The scope for a public region name: `SHA256("#name")` cut to 16 bytes.
    ///
    /// The leading `#` is the firmware's own convention and is added even when the operator's name
    /// has none (`RegionMap::getTransportKeysFor`: a name that is not `$…` is hashed as `#name`), so
    /// `us-co` and `#us-co` name the same region and a client can derive the key without the radio
    /// having the region configured at all.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        use sha2::Digest as _;
        let bare = name.trim().trim_start_matches('#');
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"#");
        hasher.update(bare.as_bytes());
        let digest = hasher.finalize();
        let mut key = [0u8; 16];
        key.copy_from_slice(&digest[..16]);
        Self {
            name: bare.to_owned(),
            key,
        }
    }

    /// A scope with a key the radio supplied, which is the only way a private (`$…`) region's key
    /// can be known.
    #[must_use]
    pub fn with_key(name: impl Into<String>, key: [u8; 16]) -> Self {
        Self {
            name: name.into(),
            key,
        }
    }
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

/// Parse `RESP_CODE_DEFAULT_FLOOD_SCOPE`: the radio's default flood scope, or `Some(None)` when the
/// radio has none (the firmware answers with the code alone).
#[must_use]
pub fn parse_default_flood_scope(frame: &[u8]) -> Option<Option<FloodScope>> {
    if frame.first() != Some(&codes::DEFAULT_FLOOD_SCOPE) {
        return None;
    }
    if frame.len() < 1 + 31 + 16 {
        // The code alone is the firmware's "null" answer (`MyMesh.cpp:1971`).
        return Some(None);
    }
    let name = text(&frame[1..1 + 31]);
    if name.is_empty() {
        return Some(None);
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(frame.get(1 + 31..1 + 31 + 16)?);
    Some(Some(FloodScope { name, key }))
}

/// Build `CMD_SET_DEFAULT_FLOOD_SCOPE`: `name 31` (NUL padded) then `key 16`, or the code alone to
/// clear it (`MyMesh.cpp:1947-1963`).
///
/// The firmware refuses a name of 31 bytes or more with `ERR_CODE_ILLEGAL_ARG`, so a longer name is
/// truncated to the 30 it accepts rather than sent and refused.
#[must_use]
pub fn build_default_flood_scope(scope: Option<&FloodScope>) -> Vec<u8> {
    let Some(scope) = scope else {
        return vec![commands::SET_DEFAULT_FLOOD_SCOPE];
    };
    let mut out = Vec::with_capacity(1 + 31 + 16);
    out.push(commands::SET_DEFAULT_FLOOD_SCOPE);
    let name = scope.name.as_bytes();
    let n = name.len().min(30);
    out.extend_from_slice(&name[..n]);
    out.resize(1 + 31, 0);
    out.extend_from_slice(&scope.key);
    out
}

/// The public region names worth testing an overheard scoped flood against.
///
/// **Matching one is evidence, not a configuration.** A scoped flood proves the tag it was scoped to
/// is carried by the repeaters that forwarded it, so a match is the difference between "this mesh
/// scopes" and "this mesh scopes to `us-co`". The list cannot be complete — region names are local,
/// lower-case and operator-chosen (`us`, `us-co`, `pnw`, `nl-ut`, `ch-de`) and the largest US
/// community guide says outright that there is no national standard — so a miss leaves the node
/// unscoped, which is where every client starts and what the firmware's own companion default is.
///
/// Order matters: the broadest tag that matches is adopted first, because scoping a terminal to a
/// metro (`sea`, `pdx`) would cut it off from everything outside that city.
pub const KNOWN_SCOPE_TAGS: &[&str] = &[
    // National and supranational.
    "us",
    "usa",
    "ca",
    "uk",
    "eu",
    "ch",
    "nl",
    "de",
    "au",
    "nz",
    // Whole-state and province tags — the level most US and EU guides recommend for a client.
    "us-co",
    "us-ca",
    "us-tx",
    "us-wa",
    "us-or",
    "us-id",
    "us-mt",
    "us-az",
    "us-nv",
    "us-ut",
    "us-ny",
    "us-fl",
    "us-ga",
    "us-nc",
    "us-pa",
    "us-oh",
    "us-mi",
    "us-il",
    "us-wi",
    "us-mn",
    "us-mo",
    "us-ks",
    "us-ne",
    "us-ok",
    "us-nm",
    "us-va",
    "us-md",
    "us-ma",
    "us-ct",
    "us-nj",
    "us-ri",
    "us-nh",
    "us-vt",
    "us-me",
    "us-sc",
    "us-tn",
    "us-ky",
    "us-in",
    "us-ia",
    "us-ar",
    "us-la",
    "us-ms",
    "us-al",
    "us-ak",
    "us-hi",
    "us-dc",
    // Names some communities publish instead of the ISO form, and the largest regional tags.
    "colorado",
    "pnw",
    "bos",
    "northeast",
    "east",
    "west",
    "nl-ut",
    "ch-de",
    "ch-fr",
    "ch-it",
];

/// Which known region name an overheard scoped packet was scoped to, if any.
///
/// The packet's two-byte code is compared against the code every candidate *would* have produced for
/// this payload, which is the only way to attribute a scope to a name: the name is never sent. A
/// 16-bit code has one chance in 65 536 of matching by accident per candidate, so a match across a
/// short list is good evidence and a repeated match across different payloads settles it.
#[must_use]
pub fn identify_scope(view: &PacketView<'_>, candidates: &[&str]) -> Option<FloodScope> {
    let codes = view.transport_codes?;
    let wanted = codes[0];
    for name in candidates {
        let scope = FloodScope::from_name(name);
        if transport_code(&scope.key, view.payload_type, view.payload) == wanted {
            return Some(scope);
        }
    }
    None
}

/// One received packet, split into the fields a route or a record needs.
///
/// `path` is in **travel order**: a flooded packet accumulates one hash per repeater, so the hops
/// read in the order they were crossed. Sending back along it reverses the hop *groups*
/// (`PeerPath::reversed`, `RadioRoute.reversed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketView<'a> {
    /// `ROUTE_TYPE_*` from the low two bits of the header.
    pub route: u8,
    /// `PAYLOAD_TYPE_*` from the header.
    pub payload_type: u8,
    /// The two transport codes, for a `ROUTE_TYPE_TRANSPORT_FLOOD`/`_DIRECT` packet; `None` for a
    /// plain flood or a plain direct packet, which carry no scope at all.
    pub transport_codes: Option<[u16; 2]>,
    /// The packet's path, with the encoding the wire carried.
    pub path: AdvertPath,
    /// Everything after the path.
    pub payload: &'a [u8],
}

impl PacketView<'_> {
    /// Whether repeaters flood-routed this packet to us, as opposed to it arriving direct.
    #[must_use]
    pub fn is_flood(&self) -> bool {
        self.route == 0x01 || self.route == 0x00
    }
}

/// Split one packet (`[header][transport codes?][path byte][path][payload]`) into its fields.
///
/// The layout is the firmware's own (`Packet::readFrom`, `src/Packet.cpp:65-85`), and the path width
/// comes from the packet rather than from any radio's setting: forwarding is width-agnostic from
/// v1.14, so a one-byte phone and a two-byte repeater are both on the same air.
#[must_use]
pub fn parse_packet(packet: &[u8]) -> Option<PacketView<'_>> {
    let header = *packet.first()?;
    let route = header & 0x03;
    let payload_type = (header >> 2) & 0x0F;
    let mut i = 1;
    // ROUTE_TYPE_TRANSPORT_FLOOD (0) and ROUTE_TYPE_TRANSPORT_DIRECT (3) carry two 16-bit codes
    // before the path (`Packet::readFrom`).
    let mut transport_codes = None;
    if route == 0 || route == 3 {
        let lo = packet.get(i..i + 4)?;
        transport_codes = Some([
            u16::from_le_bytes([lo[0], lo[1]]),
            u16::from_le_bytes([lo[2], lo[3]]),
        ]);
        i += 4;
    }
    let encoded = *packet.get(i)?;
    i += 1;
    let hash_size = (encoded >> 6) + 1;
    if hash_size == 4 {
        return None;
    }
    let hops = encoded & 0x3F;
    let want = usize::from(hops) * usize::from(hash_size);
    if want > MAX_PATH_SIZE {
        return None;
    }
    let bytes = packet.get(i..i + want)?.to_vec();
    i += want;
    Some(PacketView {
        route,
        payload_type,
        transport_codes,
        path: AdvertPath {
            received_at: 0,
            hash_size,
            hops,
            bytes,
        },
        payload: packet.get(i..)?,
    })
}

/// Build the flooded packet that asks a peer for its route to us.
///
/// Payload, which is the whole of what the far end reads: `[dest hash][src hash][LDRQ][key]`. The
/// hashes are the firmware's own: the first byte of each public key (`PATH_HASH_SIZE` is 1,
/// `MeshCore.h:18`, and payload hashes are one byte at `PAYLOAD_VER_1` whatever the path width is).
/// The key is the requester's **radio** key, so the peer can answer without a separate hint record
/// and can persist who it is talking to.
pub fn build_route_request(
    hash_size: u8,
    peer_key: &[u8; 32],
    own_key: &[u8; 32],
) -> Result<Vec<u8>, CodecError> {
    build_route_request_in_scope(hash_size, peer_key, own_key, None)
}

/// The same request, scoped to a region when the mesh needs it.
///
/// A scoped flood is the only form a repeater will forward on a mesh that denies unscoped traffic
/// (`region denyf *`), and it also escapes a low `flood.max.unscoped`: the region's own
/// `flood.max` applies instead. The transport code is computed over **exactly** the payload that
/// follows the path, because that is what every repeater recomputes.
pub fn build_route_request_in_scope(
    hash_size: u8,
    peer_key: &[u8; 32],
    own_key: &[u8; 32],
    scope: Option<&FloodScope>,
) -> Result<Vec<u8>, CodecError> {
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    let mut payload = Vec::with_capacity(2 + ROUTE_REQUEST_MAGIC.len() + 32);
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(ROUTE_REQUEST_MAGIC);
    payload.extend_from_slice(own_key);
    let mut out = Vec::with_capacity(6 + payload.len());
    match scope {
        None => out.push(ROUTE_FLOOD_HEADER),
        Some(scope) => {
            out.push(ROUTE_FLOOD_SCOPED_HEADER);
            let code = transport_code(&scope.key, 0, &payload);
            out.extend_from_slice(&code.to_le_bytes());
            // The second code slot is reserved: the firmware writes zero there too
            // (`sendFloodScoped`, `MyMesh.cpp:508-513`).
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    out.push(path_byte(hash_size, 0)?);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// A route request that was addressed to this radio, and the route it arrived by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    /// The requester's radio key, from the payload.
    pub peer_key: [u8; 32],
    /// The path the request took to arrive, in travel order.
    pub path: AdvertPath,
}

/// Read a flooded route request addressed to `own_key`.
///
/// The destination hash is checked against our own key's first byte rather than being trusted from
/// the payload, so a request addressed to another node is never answered by us. A malformed length
/// is a refusal, not a guess: the request is small and fixed-shape, so anything else is not ours.
#[must_use]
pub fn parse_route_request(packet: &[u8], own_key: &[u8; 32]) -> Option<RouteRequest> {
    let view = parse_packet(packet)?;
    if !view.is_flood() || view.payload_type != 0 {
        return None;
    }
    let payload = view.payload;
    if payload.len() != 2 + ROUTE_REQUEST_MAGIC.len() + 32 {
        return None;
    }
    if payload[0] != own_key[0] || payload.get(2..6)? != ROUTE_REQUEST_MAGIC.as_slice() {
        return None;
    }
    let peer_key: [u8; 32] = payload.get(6..38)?.try_into().ok()?;
    Some(RouteRequest {
        peer_key,
        path: view.path,
    })
}

/// Build the direct record that answers a route request: the path **this end used**, so the
/// requester can reverse it and hold a route of its own.
///
/// The reply is a `RAW_CUSTOM` record exactly like a session record, because that is the one packet
/// whose contents reach this application on both ends; it is recognised by its marker before the
/// session ever sees it (`mesh_serial::route_reply`, `AndroidRadioLink.onRawData`).
pub fn build_route_reply(hash_size: u8, hops: u8, path: &[u8]) -> Result<Vec<u8>, CodecError> {
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    if path.len() != usize::from(hops) * usize::from(hash_size) {
        return Err(CodecError::RaggedPath {
            len: path.len(),
            hash: hash_size,
        });
    }
    let mut out = Vec::with_capacity(5 + path.len());
    out.extend_from_slice(ROUTE_REPLY_MAGIC);
    out.push(path_byte(hash_size, hops)?);
    out.extend_from_slice(path);
    Ok(out)
}

/// Read a route reply's path: the route to the peer that sent it, already in travel order.
#[must_use]
pub fn parse_route_reply(record: &[u8]) -> Option<AdvertPath> {
    if record.get(..4)? != ROUTE_REPLY_MAGIC.as_slice() {
        return None;
    }
    let encoded = *record.get(4)?;
    let hash_size = (encoded >> 6) + 1;
    if hash_size == 4 {
        return None;
    }
    let hops = encoded & 0x3F;
    let want = usize::from(hops) * usize::from(hash_size);
    if want > MAX_PATH_SIZE {
        return None;
    }
    let bytes = record.get(5..5 + want)?.to_vec();
    Some(AdvertPath {
        received_at: 0,
        hash_size,
        hops,
        bytes,
    })
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
    // The firmware's coding rate is the denominator index RadioLib reads it as: 5 is 4/5, and the
    // symbol multiplier is that number, which is the datasheet's `CR + 4` with CR counted from 1.
    let cr_f = f64::from(cr).clamp(5.0, 8.0);
    let symbol_ms = (2f64.powf(sf_f)) / bw_hz * 1000.0;
    // **32 preamble symbols at SF8 and below**, 16 above: `preambleLengthForSF` in the
    // firmware (`src/helpers/radiolib/RadioLibWrappers.h:56`). It was 8 here, which understated a
    // record by ≈50 ms at 62.5 kHz — an 8 % error in the hour's allowance, and the phone's own
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


    /// **A region's key is SHA-256 of its name with the `#` the firmware adds**, and the scope tag
    /// itself never goes on the air. The value is pinned to the firmware's own derivation so a
    /// change here cannot silently make every scoped packet unrecognisable to every repeater.
    #[test]
    fn a_scope_key_is_the_firmware_derivation() {
        // `python3 -c 'import hashlib; print(hashlib.sha256(b"#us-co").hexdigest()[:32])'`
        assert_eq!(
            FloodScope::from_name("us-co").key,
            [
                0x62, 0xca, 0xa5, 0xae, 0xbc, 0xa0, 0x85, 0x2f, 0xa4, 0x7c, 0x96, 0x42, 0xc5, 0xc6,
                0x30, 0xac
            ]
        );
        // The `#` is optional in the name the operator types, and hashing is case-sensitive.
        assert_eq!(FloodScope::from_name("us-co"), FloodScope::from_name("#us-co"));
        assert_ne!(FloodScope::from_name("us-co"), FloodScope::from_name("US-CO"));
        assert_eq!(FloodScope::from_name("#us-co").name, "us-co");
    }

    /// The packet code is an HMAC over `payload_type || payload` and nothing else, with the two
    /// reserved values remapped. Only the payload is hashed, which is why a code computed at the
    /// origin still verifies at hop ten (the path grows, the payload does not).
    #[test]
    fn a_transport_code_covers_the_payload_and_its_type() {
        let scope = FloodScope::from_name("us-co");
        let code = transport_code(&scope.key, 0, b"payload");
        // Pinned across both implementations — the app's test asserts the same number from the same
        // key and payload — so a difference in hashing can never hide behind "it compiles".
        assert_eq!(code, 0x9C1A);
        assert_eq!(transport_code(&FloodScope::from_name("us").key, 0, b"payload"), 0x1E95);
        assert_eq!(code, transport_code(&scope.key, 0, b"payload"), "stable");
        assert_ne!(code, transport_code(&scope.key, 0, b"other"), "over the payload");
        assert_ne!(code, transport_code(&scope.key, 4, b"payload"), "the type is hashed too");
        assert_ne!(code, transport_code(&FloodScope::from_name("us-ca").key, 0, b"payload"));
        assert_ne!(code, 0);
        assert_ne!(code, 0xFFFF);
    }

    /// A scoped request carries the code in the four bytes after the header, and the far end can
    /// read the scope back out of it — which is the whole point of the shape.
    #[test]
    fn a_scoped_request_proves_its_region() {
        let peer = [0x21u8; 32];
        let own = [0x84u8; 32];
        let scope = FloodScope::from_name("us-co");
        let packet = build_route_request_in_scope(1, &peer, &own, Some(&scope)).unwrap();
        assert_eq!(packet[0], ROUTE_FLOOD_SCOPED_HEADER);
        let view = parse_packet(&packet).unwrap();
        // The path byte is after the four code bytes, so a parser that forgot them would read the
        // first code as a path length and mangle everything after it.
        assert_eq!(view.route, ROUTE_TRANSPORT_FLOOD);
        assert_eq!(view.payload_type, 0);
        assert_eq!(view.path.hops, 0);
        assert_eq!(view.payload.len(), 38);
        assert_eq!(
            view.transport_codes.unwrap()[0],
            transport_code(&scope.key, 0, view.payload)
        );
        // The far end identifies the region by recomputing candidate codes, and refuses to guess.
        assert_eq!(identify_scope(&view, &["us", "us-co"]).unwrap().name, "us-co");
        assert!(identify_scope(&view, &["us", "usa", "colorado"]).is_none());
        // ...and the request still parses as a request addressed to the peer.
        assert_eq!(parse_route_request(&packet, &peer).unwrap().peer_key, own);
    }

    /// A plain flood carries no codes, so it can never be attributed to a region.
    #[test]
    fn an_unscoped_request_has_no_scope_to_prove() {
        let packet = build_route_request(1, &[1u8; 32], &[2u8; 32]).unwrap();
        let view = parse_packet(&packet).unwrap();
        assert!(view.transport_codes.is_none());
        assert!(identify_scope(&view, KNOWN_SCOPE_TAGS).is_none());
    }

    /// The radio's own default scope is the source of truth, in both directions: the null answer is
    /// one byte, and a set one carries the name and the key.
    #[test]
    fn the_radios_default_scope_round_trips() {
        let scope = FloodScope::from_name("us-co");
        let frame = build_default_flood_scope(Some(&scope));
        assert_eq!(frame.len(), 1 + 31 + 16);
        assert_eq!(frame[0], commands::SET_DEFAULT_FLOOD_SCOPE);
        assert_eq!(&frame[1..6], b"us-co");
        assert_eq!(&frame[32..], &scope.key);

        let mut reply = vec![codes::DEFAULT_FLOOD_SCOPE];
        reply.extend_from_slice(&frame[1..]);
        assert_eq!(parse_default_flood_scope(&reply), Some(Some(scope)));

        // The firmware's null answer, and a clear command, are both one byte.
        assert_eq!(parse_default_flood_scope(&[codes::DEFAULT_FLOOD_SCOPE]), Some(None));
        assert_eq!(build_default_flood_scope(None), vec![commands::SET_DEFAULT_FLOOD_SCOPE]);
        // A name the firmware would refuse is truncated to the 30 bytes it accepts.
        let long = FloodScope::from_name(&"a".repeat(40));
        assert_eq!(build_default_flood_scope(Some(&long)).len(), 1 + 31 + 16);
    }

    #[test]
    fn a_route_request_round_trips_for_the_key_it_names() {
        let peer = [0xABu8; 32];
        let own = [0x11u8; 32];
        let packet = build_route_request(1, &peer, &own).expect("builds");
        // Route flood, payload REQ, no hops: the requester does not know the route yet.
        assert_eq!(packet[0], ROUTE_FLOOD_HEADER);
        assert_eq!(packet[0] & 0x03, 0x01);
        assert_eq!((packet[0] >> 2) & 0x0F, 0x00);
        assert_eq!(packet[1], 0x00);
        let req = parse_route_request(&packet, &peer).expect("addressed to the peer");
        assert_eq!(req.peer_key, own);
        assert_eq!(req.path.hops, 0);
    }

    /// **Addressed, not broadcast.** A request names one key; another radio must not answer it,
    /// or every node in range would flood a reply to a packet that was never for it.
    #[test]
    fn another_radios_key_does_not_parse_as_the_destination() {
        let packet = build_route_request(1, &[0xAB; 32], &[0x11; 32]).unwrap();
        assert!(parse_route_request(&packet, &[0xCD; 32]).is_none());
    }

    /// Two radios on one desk still pass through repeaters on a real mesh, and a request that has
    /// travelled carries its path; the reply needs that path intact and reversed by hop group.
    #[test]
    fn a_request_that_travelled_keeps_its_hops() {
        let peer = [0x5Au8; 32];
        let own = [0x77u8; 32];
        let mut packet = build_route_request(2, &peer, &own).unwrap();
        // Pretend two repeaters appended their two-byte hashes on the way here.
        packet[1] = path_byte(2, 2).unwrap();
        packet.splice(2..2, [0xA1, 0xA2, 0xB1, 0xB2]);
        let req = parse_route_request(&packet, &peer).expect("parses with a path");
        assert_eq!(req.path.hash_size, 2);
        assert_eq!(req.path.hops, 2);
        assert_eq!(req.path.bytes, vec![0xA1, 0xA2, 0xB1, 0xB2]);
    }

    /// A packet that is not flood-routed never reaches us through a repeater, so it cannot be a
    /// request; and a truncated one is a refusal rather than a half-read.
    #[test]
    fn a_direct_or_truncated_request_is_refused() {
        let peer = [0x5Au8; 32];
        let own = [0x77u8; 32];
        let mut direct = build_route_request(1, &peer, &own).unwrap();
        direct[0] = 0x02; // DIRECT, REQ
        assert!(parse_route_request(&direct, &peer).is_none());
        let mut short = build_route_request(1, &peer, &own).unwrap();
        short.truncate(short.len() - 1);
        assert!(parse_route_request(&short, &peer).is_none());
    }

    #[test]
    fn a_route_reply_round_trips_and_refuses_a_ragged_path() {
        let record = build_route_reply(2, 1, &[0x12, 0x34]).expect("builds");
        assert_eq!(&record[..4], ROUTE_REPLY_MAGIC);
        let path = parse_route_reply(&record).expect("parses");
        assert_eq!(path.hash_size, 2);
        assert_eq!(path.hops, 1);
        assert_eq!(path.bytes, vec![0x12, 0x34]);
        // One and a half hashes is not a path.
        assert!(build_route_reply(2, 1, &[0x12]).is_err());
        assert!(parse_route_reply(&[0x4C, 0x44, 0x52, 0x52, 0x41, 0x00]).is_none());
    }

    /// The path width travels **in the packet**, because forwarding is width-agnostic and the two
    /// ends need not agree on their own advert width.
    #[test]
    fn a_packet_view_reads_the_width_the_wire_carried() {
        let packet = build_route_request(2, &[0x01; 32], &[0x02; 32]).unwrap();
        let view = parse_packet(&packet).expect("parses");
        assert_eq!(view.route, 0x01);
        assert_eq!(view.payload_type, 0);
        assert_eq!(view.path.hash_size, 2);
        assert!(view.is_flood());
        // A transport-coded flood carries four bytes of codes before the path byte.
        let mut coded = packet.clone();
        coded[0] = 0x00; // ROUTE_TYPE_TRANSPORT_FLOOD
        coded.splice(1..1, [0u8; 4]);
        let view = parse_packet(&coded).expect("parses with transport codes");
        assert_eq!(view.path.hash_size, 2);
        assert_eq!(view.payload.len(), 38);
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
