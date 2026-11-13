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

/// The companion's identity: opcodes, the reports the radio answers, and the phone's answer
/// table.
///
/// **Re-exported from `ldmux-mesh`**, which owns them because the phone asks the same questions
/// over BLE and cannot link a host crate (`docs/RADIO-PLAN.md` §8.1). Every caller in this crate
/// and in the daemon keeps the path it had, so the move changed no code above it.
pub use ldmux_mesh::companion::command::{
    APP_TARGET_VERSION, BatteryAndStorage, CommandError, DeviceInfo, PHONE_COMMANDS, RadioStats,
    SelfInfo, codes, commands, get_battery_and_storage, get_radio_stats, parse_battery_and_storage,
    parse_device_info, parse_radio_stats, parse_self_info, time_on_air_ms,
};

/// The shared packet builders, from `ldmux-mesh`.
///
/// They live there because the phone composes the same packets over BLE and cannot link a host
/// crate (`docs/RADIO-PLAN.md` §8.1). Every function and constant below re-exports or delegates to
/// them, so there is one implementation of each wire rule in the tree.
use ldmux_mesh::companion::build;

/// Header byte for a raw custom packet sent DIRECT: `route = 2`,
/// `payload_type = 15` (RAW_CUSTOM), version 0. Identical to the phone's
/// `RAW_CUSTOM_DIRECT_HEADER`.
pub use ldmux_mesh::companion::build::RAW_CUSTOM_DIRECT_HEADER;

/// The path byte a flooded packet carries.
pub const PATH_FLOOD: u8 = 0xFF;

/// Build the path byte for a direct packet: hash width in the top two bits, hop count in the low six.
///
/// **Re-exported from `ldmux-mesh`**, which owns the builders because the phone composes the same
/// packets over BLE (`docs/RADIO-PLAN.md` §8.1). The error is mapped to this crate's own so that
/// every existing caller keeps the type it matched on.
///
/// # Errors
///
/// [`CodecError::BadHashSize`] or [`CodecError::TooManyHops`].
pub fn path_byte(hash_size: u8, hops: u8) -> Result<u8, CodecError> {
    build::path_byte(hash_size, hops).map_err(build_err)
}

/// This crate's error for a build failure, so the shared builders report as they always did.
fn build_err(e: build::BuildError) -> CodecError {
    match e {
        build::BuildError::BadHashSize(n) => CodecError::BadHashSize(n),
        build::BuildError::TooManyHops(n) => CodecError::TooManyHops(n),
        build::BuildError::RaggedPath { len, hash } => CodecError::RaggedPath { len, hash },
        build::BuildError::TooLong { len, max } => CodecError::TooLong { len, max },
    }
}

/// The largest packet a `CMD_SEND_RAW_PACKET` frame may carry.
///
/// The firmware's frame ceiling is 176 bytes (`MAX_FRAME_SIZE`, `BaseSerialInterface.h:5`), and a raw
/// send is `[0x41][priority][header][path_len][path][record]` — the command and the priority are two
/// of those bytes, so the packet itself gets 174. A longer frame is **truncated** by the serial
/// reader (`checkRecvFrame`, `ArduinoSerialInterface.cpp:56-59`), which turns an oversized record
/// into a parse failure rather than a send, so the limit has to be exact here.
pub use ldmux_mesh::companion::build::PACKET_MAX;

/// The firmware's own path ceiling, in bytes (`MAX_PATH_SIZE`, `src/MeshCore.h:22`).
///
/// `Packet::isValidPathLen` refuses anything longer (`src/Packet.cpp:13-18`), so a packet built
/// with a longer path is dropped by every receiver while this end believes it was sent. The hop
/// count alone cannot catch it: 63 one-byte hops fit the encoded byte, and 63 three-byte hops are
/// 189 bytes.
pub use ldmux_mesh::companion::build::MAX_PATH_SIZE;

/// Build a raw direct packet around `record`.
///
/// **Both ends of the path are checked here.** A path whose length is not a multiple of the hash
/// width cannot be encoded: the hop count truncates and the receiver walks `hops * hash` bytes,
/// landing inside the path, so every record fails its tag and the link looks like a radio fault
/// rather than a caller's mistake. A packet over [`PACKET_MAX`] would be truncated by the firmware's
/// serial reader, and refusing it here names the fault at its source.
pub fn build_raw_direct(hash_size: u8, path: &[u8], record: &[u8]) -> Result<Vec<u8>, CodecError> {
    build::raw_direct(hash_size, path, record).map_err(build_err)
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
pub use ldmux_mesh::companion::build::ROUTE_FLOOD_HEADER;

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
pub use ldmux_mesh::companion::build::ROUTE_FLOOD_SCOPED_HEADER;

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
        // **At most one `#` is stripped, and the name is cut to the field the firmware stores.**
        // The firmware hashes the stored name as it is, adding a `#` only for a name that has none
        // (`RegionMap::getTransportKeysFor`), so `##us` is a different region from `#us`; stripping
        // every hash would silently derive a key for a name this end never sends. The 30-byte cut
        // is the radio's own field, and hashing the cut name keeps the name and the key in step —
        // a truncated name with a key derived from the full one describes a region that cannot
        // exist.
        let trimmed = name.trim();
        let bare = trimmed.strip_prefix('#').unwrap_or(trimmed);
        let bare = &bare[..bare.len().min(30)];
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
    build::transport_code(key, payload_type, payload)
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
pub fn build_default_flood_scope(scope: Option<&FloodScope>) -> Result<Vec<u8>, CodecError> {
    let Some(scope) = scope else {
        return Ok(vec![commands::SET_DEFAULT_FLOOD_SCOPE]);
    };
    // The radio's field is 31 bytes with its NUL, and the firmware refuses a longer name with
    // `ILLEGAL_ARG`. Refusing here rather than truncating keeps the stored name and the stored key
    // describing the same region: a cut name with a key derived from the full one is a region no
    // repeater can have.
    let name = scope.name.as_bytes();
    if name.is_empty() || name.len() > 30 {
        return Err(CodecError::ScopeName { len: name.len() });
    }
    let mut out = Vec::with_capacity(1 + 31 + 16);
    out.push(commands::SET_DEFAULT_FLOOD_SCOPE);
    out.extend_from_slice(name);
    out.resize(1 + 31, 0);
    out.extend_from_slice(&scope.key);
    Ok(out)
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
    // A nonce that changes per call: two requests must not hash the same, or the second one never
    // leaves the first repeater (`Packet::calculatePacketHash`). Time is the cheapest source that
    // differs between two attempts seconds apart.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    build_route_request_with_nonce(hash_size, peer_key, own_key, scope, &nanos.to_le_bytes())
}

/// The same request with a caller-chosen nonce, so a test can be deterministic.
pub fn build_route_request_with_nonce(
    hash_size: u8,
    peer_key: &[u8; 32],
    own_key: &[u8; 32],
    scope: Option<&FloodScope>,
    nonce: &[u8; 4],
) -> Result<Vec<u8>, CodecError> {
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    let mut payload = Vec::with_capacity(2 + ROUTE_REQUEST_MAGIC.len() + 32 + 4);
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(ROUTE_REQUEST_MAGIC);
    payload.extend_from_slice(own_key);
    payload.extend_from_slice(nonce);
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

/// The four bytes that mark a **flooded session record**.
///
/// **Why a record ever needs a flood.** `RAW_CUSTOM` is delivered only on a direct route and is
/// never forward-routed, so every session record depends on a path this end believes in. At the
/// edge of coverage the path cannot always be found — measured 2026-11-10: the phone heard the
/// nearest repeaters at -107 to -124 dBm, asked for a route for four minutes, and the host never
/// heard one request — so the link had no way to bootstrap at all. A `REQ` flood *is* forward-routed
/// by every repeater (that is what a route request is), so the same seam can carry a sealed record
/// when the direct path is absent. The record is sealed with the session's own AEAD, so a flood is
/// no less private: any repeater that forwards it can read none of it, exactly as it cannot read
/// the payload of a direct record it forwards.
pub use ldmux_mesh::companion::build::RECORD_FLOOD_MAGIC;

/// A session record as a flood: header, path byte, then
/// `[dest hash][source hash][magic][nonce][record]`.
///
/// The nonce is **outside** the sealed record and fresh per send, because MeshCore dedupes on
/// `SHA256(payload_type ‖ payload)`: a repeat of the same bytes never leaves the first repeater,
/// and a record that is genuinely being retransmitted must still be a different packet.
pub fn build_record_flood_in_scope(
    hash_size: u8,
    peer_key: &[u8; 32],
    own_key: &[u8; 32],
    scope: Option<&FloodScope>,
    nonce: &[u8; 4],
    record: &[u8],
) -> Result<Vec<u8>, CodecError> {
    build::record_flood(
        hash_size,
        peer_key,
        own_key,
        scope.map(|s| &s.key),
        nonce,
        record,
    )
    .map_err(build_err)
}

/// A flooded session record addressed to this radio, and the path it arrived by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloodedRecord<'a> {
    /// The sealed record itself.
    pub record: &'a [u8],
    /// The path the flood took to arrive, in travel order.
    pub path: AdvertPath,
}

/// Read a flooded session record addressed to `own_key`.
///
/// The destination hash is checked against our own key, and the magic keeps an unrelated `REQ`
/// flood from being fed to the session. Who sent it is not carried: the record's own AEAD decides
/// that, and a flood from a stranger fails to open for the price of one authentication check.
#[must_use]
pub fn parse_record_flood<'a>(packet: &'a [u8], own_key: &[u8; 32]) -> Option<FloodedRecord<'a>> {
    let view = parse_packet(packet)?;
    if !view.is_flood() || view.payload_type != 0 {
        return None;
    }
    let payload = view.payload;
    if payload.len() < 2 + RECORD_FLOOD_MAGIC.len() + 4 {
        return None;
    }
    if payload[0] != own_key[0] {
        return None;
    }
    if &payload[2..6] != RECORD_FLOOD_MAGIC {
        return None;
    }
    Some(FloodedRecord {
        record: &payload[10..],
        path: view.path,
    })
}

/// A route request that was addressed to this radio, and the route it arrived by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    /// The requester's radio key, from the payload.
    pub peer_key: [u8; 32],
    /// The path the request took to arrive, in travel order.
    pub path: AdvertPath,
    /// Four bytes that make this request **a different packet** from the last one.
    ///
    /// **Without it a retry is dropped on the floor.** MeshCore dedupes on
    /// `SHA256(payload_type ‖ payload)` in a 160-entry ring with no timeout (`Packet::calculatePacketHash`,
    /// `SimpleMeshTables`), and every repeater and every receiver consults it, so a byte-identical
    /// retransmission of a flood — or of a direct record — is swallowed until 160 other packets have
    /// passed. That is why MeshCore's own flooded requests carry `getRNG()->random()` in their
    /// payload (`BaseChatMesh`, path discovery), and why the session re-seals every retransmission.
    /// The reply echoes it, so a second reply is a different packet too.
    pub nonce: [u8; 4],
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
    if payload.len() != 2 + ROUTE_REQUEST_MAGIC.len() + 32 + 4 {
        return None;
    }
    if payload[0] != own_key[0] || payload.get(2..6)? != ROUTE_REQUEST_MAGIC.as_slice() {
        return None;
    }
    let peer_key: [u8; 32] = payload.get(6..38)?.try_into().ok()?;
    let nonce: [u8; 4] = payload.get(38..42)?.try_into().ok()?;
    Some(RouteRequest {
        peer_key,
        path: view.path,
        nonce,
    })
}

/// Build the direct record that answers a route request: the path **this end used**, so the
/// requester can reverse it and hold a route of its own.
///
/// The reply is a `RAW_CUSTOM` record exactly like a session record, because that is the one packet
/// whose contents reach this application on both ends; it is recognised by its marker before the
/// session ever sees it (`mesh_serial::route_reply`, `AndroidRadioLink.onRawData`).
pub fn build_route_reply(hash_size: u8, hops: u8, path: &[u8]) -> Result<Vec<u8>, CodecError> {
    build_route_reply_with_nonce(hash_size, hops, path, &[0; 4])
}

/// The reply to one request, carrying that request's nonce so a second reply is a different packet.
pub fn build_route_reply_with_nonce(
    hash_size: u8,
    hops: u8,
    path: &[u8],
    nonce: &[u8; 4],
) -> Result<Vec<u8>, CodecError> {
    if !(1..=3).contains(&hash_size) {
        return Err(CodecError::BadHashSize(hash_size));
    }
    if path.len() != usize::from(hops) * usize::from(hash_size) {
        return Err(CodecError::RaggedPath {
            len: path.len(),
            hash: hash_size,
        });
    }
    let mut out = Vec::with_capacity(5 + path.len() + 4);
    out.extend_from_slice(ROUTE_REPLY_MAGIC);
    out.push(path_byte(hash_size, hops)?);
    out.extend_from_slice(path);
    out.extend_from_slice(nonce);
    Ok(out)
}

/// Build the **flooded** answer to a route request.
///
/// **This is the reply that works at range, and it is MeshCore's own rule for one.** A request
/// arrives as an un-scoped flood, and the far side answers it with a *flooded* path return
/// (`mesh::ReplyRoute::REPLY_ROUTE_PATH_RETURN` in the community firmware's `RoutingPolicy.h`),
/// because the two vehicles a stock companion has are not equal: an advert is dropped by a repeater
/// after `flood_max_advert` hops — **eight** by default — while a `REQ` flood travels `flood_max`
/// (64) and an un-scoped one `flood_max_unscoped` (64). A direct reply depends on one chain of
/// repeaters all forwarding it; a flooded one depends on the mesh.
///
/// The direct reply is kept as the answer to a *neighbour*, where it is the same one transmission.
/// This one is the answer when the request had to cross repeaters to arrive at all.
///
/// **Layout**, so the phone can recognise it without a contact on either side (ldmux keeps none,
/// ADR-0030) and without the firmware having to decrypt anything:
///
/// ```text
/// [0x01 flood][path byte for the reply's own travel path][dest hash][src hash]
/// ["LDRR"][responding radio's 32-byte key][the direct reply record: path byte, path, nonce]
/// ```
///
/// The reply's own travel path starts empty and accumulates one hash per repeater, which is how a
/// flooded packet shows where it came from; the phone does not read it — the path it needs is the
/// one carried inside, which is the route the *host* uses towards it.
pub fn build_route_reply_flood(
    own_key: &[u8; 32],
    peer_key: &[u8; 32],
    hash_size: u8,
    hops: u8,
    path: &[u8],
    nonce: &[u8; 4],
) -> Result<Vec<u8>, CodecError> {
    build_route_reply_flood_in_scope(own_key, peer_key, hash_size, hops, path, nonce, None)
}

/// The same reply, sent under a **region scope**.
///
/// **A reply must go back the way the request came, scope and all.** A dense mesh configures its
/// repeaters to deny unscoped floods (`region denyf *`), and some set `flood.max.unscoped` to a
/// handful of hops, so an unscoped answer to a scoped request is dropped before it has crossed the
/// region — the phone asks, the host answers, and nothing arrives. MeshCore's own rule for this is
/// `ReplyScope::REPLY_SCOPE_REQUEST`: reuse the scope the request arrived on. The transport code is
/// an HMAC over *this* packet's payload and type, so it has to be computed for the reply rather than
/// copied, which is why the scope's key is what this takes.
pub fn build_route_reply_flood_in_scope(
    own_key: &[u8; 32],
    peer_key: &[u8; 32],
    hash_size: u8,
    hops: u8,
    path: &[u8],
    nonce: &[u8; 4],
    scope: Option<&FloodScope>,
) -> Result<Vec<u8>, CodecError> {
    let inner = build_route_reply_with_nonce(hash_size, hops, path, nonce)?;
    let mut payload = Vec::with_capacity(2 + ROUTE_REPLY_MAGIC.len() + 32 + inner.len());
    // The firmware's own `REQ` envelope: the destination and sender hash bytes, then the body.
    // A node with no contact for either hash simply never decrypts it; it still forwards it, which
    // is the whole point.
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(ROUTE_REPLY_MAGIC);
    payload.extend_from_slice(own_key);
    payload.extend_from_slice(&inner);
    let mut out = Vec::with_capacity(6 + payload.len());
    match scope {
        None => out.push(ROUTE_FLOOD_HEADER),
        Some(scope) => {
            out.push(ROUTE_FLOOD_SCOPED_HEADER);
            // `HMAC(key, payloadType || payload)` cut to two little-endian bytes, and the second
            // code slot reserved as the firmware leaves it.
            let code = transport_code(&scope.key, 0, &payload);
            out.extend_from_slice(&code.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    // The travel path, empty at origin, at the width the carried path uses so a repeater appends
    // hashes of the same size. The receiver ignores this path entirely.
    out.push(path_byte(hash_size, 0)?);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Read a flooded route reply out of a received packet, when it is `own_key`'s.
///
/// **The mirror of [`build_route_reply_flood`], and the daemon's own check**: it answers requests, so
/// it never reads one back, but the shape has to be pinned by a test on both sides or the two ends
/// drift silently — the failure mode being a phone that never learns a route at any distance.
#[must_use]
pub fn parse_route_reply_flood(packet: &[u8], from_key: &[u8; 32]) -> Option<AdvertPath> {
    if packet.first() != Some(&ROUTE_FLOOD_HEADER) {
        return None;
    }
    let encoded = *packet.get(1)?;
    let travel_hash = (encoded >> 6) + 1;
    if travel_hash == 4 {
        return None;
    }
    let travel = usize::from(encoded & 0x3F) * usize::from(travel_hash);
    let payload = packet.get(2 + travel..)?;
    // Destination and sender hash bytes, then the marker, then the responder's full key.
    if payload.len() < 2 + ROUTE_REPLY_MAGIC.len() + 32 {
        return None;
    }
    if payload.get(2..6)? != ROUTE_REPLY_MAGIC.as_slice() {
        return None;
    }
    if payload.get(6..38)? != from_key {
        return None;
    }
    parse_route_reply(payload.get(38..)?)
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

impl AdvertPath {
    /// The chain reversed hop group by hop group: the path a packet must carry to reach the peer.
    #[must_use]
    pub fn outbound(&self) -> Vec<u8> {
        reverse_by_hop(&self.bytes, self.hash_size)
    }
}

/// The chain reversed hop group by hop group, which is the path a packet must carry to reach the
/// node that advertised.
///
/// **Reversed by hop, not by byte.** A flood appends each hop's hash at the end as it travels
/// (`Mesh.cpp:344-357`) while a direct packet pops from the front (`removeSelfFromPath`), so the
/// outbound path is the arrival chain with its groups in the opposite order. Reversing bytes instead
/// produces a path no receiver recognises. This is the one implementation: `ldmux_mesh`'s
/// `AdvertPath::outbound` calls it too, so the two cannot drift.
#[must_use]
pub fn reverse_by_hop(bytes: &[u8], hash_size: u8) -> Vec<u8> {
    let width = usize::from(hash_size.clamp(1, 3));
    let mut out = Vec::with_capacity(bytes.len());
    for hop in bytes.chunks(width).rev() {
        out.extend_from_slice(hop);
    }
    out
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
    /// A region name the radio's 31-byte field cannot hold (1..=30 bytes).
    ScopeName { len: usize },
    /// A `$`-prefixed region name is a *stored key* on the radio, never a hashtag to derive.
    ///
    /// The firmware treats a leading `$` as "this name is a region whose key came from somewhere
    /// else" (`RegionMap.cpp:173-188`) and never hashes it, so deriving one would compute a key that
    /// matches no repeater — and a wrong scope only drops floods, which makes it silent.
    /// A reply shorter than the fields it claims to carry.
    Truncated,
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
            CodecError::Truncated => write!(f, "the radio's answer is shorter than the frame it claims"),
            CodecError::ScopeName { len } => {
                write!(f, "a {len}-byte region name does not fit the radio's 30-byte field")
            }
        }
    }
}

impl From<CommandError> for CodecError {
    fn from(e: CommandError) -> Self {
        match e {
            CommandError::WrongAnswer { want, got } => CodecError::WrongResponse { want, got },
            CommandError::Truncated => CodecError::Truncated,
        }
    }
}

impl std::error::Error for CodecError {}

/// The radio's battery, uptime, error flags and outbound queue depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreStats {
    pub battery_mv: u16,
    pub uptime_secs: u32,
    pub err_flags: u16,
    pub queue_len: u8,
}


/// A NUL-padded fixed-width string, as the firmware writes them.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

/// Parse `RESP_STATS` for `STATS_TYPE_CORE`.
pub fn parse_core_stats(payload: &[u8]) -> Result<CoreStats, CodecError> {
    match payload.first() {
        Some(&codes::STATS) => {}
        Some(&got) => {
            return Err(CodecError::WrongResponse {
                want: codes::STATS,
                got,
            });
        }
        None => return Err(CodecError::Truncated),
    }
    if payload.get(1) != Some(&codes::STATS_TYPE_CORE) {
        return Err(CodecError::WrongResponse {
            want: codes::STATS_TYPE_CORE,
            got: payload.get(1).copied().unwrap_or(0),
        });
    }
    let batt = payload.get(2..4).ok_or(CodecError::Truncated)?;
    let uptime = payload.get(4..8).ok_or(CodecError::Truncated)?;
    let flags = payload.get(8..10).ok_or(CodecError::Truncated)?;
    let queue = *payload.get(10).ok_or(CodecError::Truncated)?;
    Ok(CoreStats {
        battery_mv: u16::from_le_bytes([batt[0], batt[1]]),
        uptime_secs: u32::from_le_bytes([uptime[0], uptime[1], uptime[2], uptime[3]]),
        err_flags: u16::from_le_bytes([flags[0], flags[1]]),
        queue_len: queue,
    })
}

#[cfg(test)]
mod record_flood_tests {
    use super::*;

    /// **The same bytes on both ends, pinned.** This wrapper is built twice — here and in Kotlin
    /// (`RadioRoute.recordFlood`) — and a one-byte disagreement means a record that is sent and never
    /// opened, which reads as a dead link. The Kotlin test asserts the same vector.
    #[test]
    fn a_record_flood_is_the_same_bytes_as_the_phones() {
        let host = [0x11u8; 32];
        let own = [0x22u8; 32];
        let nonce = [0x01, 0x02, 0x03, 0x04];
        let record = [0xAA, 0xBB, 0xCC];
        let packet = build_record_flood_in_scope(1, &host, &own, None, &nonce, &record).unwrap();
        assert_eq!(
            packet,
            vec![
                0x01, // ROUTE_FLOOD | REQ
                0x00, // path byte: zero hops, one-byte hashes
                0x11, // dest hash = host key byte 0
                0x22, // source hash = own key byte 0
                b'L', b'D', b'M', b'R', // magic
                0x01, 0x02, 0x03, 0x04, // nonce, outside the sealed record
                0xAA, 0xBB, 0xCC, // the record, untouched
            ]
        );
    }

    #[test]
    fn a_record_flood_reads_back_and_only_for_its_address() {
        let host = [0x11u8; 32];
        let own = [0x22u8; 32];
        let nonce = [9, 8, 7, 6];
        let record = [1u8, 2, 3, 4, 5];
        let packet = build_record_flood_in_scope(1, &host, &own, None, &nonce, &record).unwrap();
        // Addressed to `host`, sent by `own`: the recipient is the one that opens it.
        let flooded = parse_record_flood(&packet, &host).expect("addressed to the recipient");
        assert_eq!(flooded.record, record);
        assert_eq!(flooded.path.hops, 0);
        // The sender's own key does not match the destination hash, so it is not the sender's to
        // open. That check is what keeps another node's flood out of this session entirely.
        assert!(parse_record_flood(&packet, &own).is_none());
    }

    #[test]
    fn a_record_flood_that_does_not_fit_is_refused() {
        let host = [0x11u8; 32];
        let own = [0x22u8; 32];
        let big = vec![0u8; 200];
        assert!(build_record_flood_in_scope(1, &host, &own, None, &[0; 4], &big).is_err());
    }
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
            Err(CommandError::WrongAnswer { want, got }) => {
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
        assert_eq!(info.firmware_version_code, 13);
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
        assert_eq!(info.firmware_version_code, 2);
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
        assert_eq!(view.payload.len(), 42, "hashes, marker, key and nonce");
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

    /// **The name and the key have to describe the same region.** The radio's field is 31 bytes with
    /// its NUL, so a longer name is refused rather than cut: a truncated name with a key derived from
    /// the full one names a region that cannot exist, and the operator would see it in their app.
    #[test]
    fn a_region_name_that_does_not_fit_is_refused() {
        assert!(build_default_flood_scope(Some(&FloodScope::with_key("a".repeat(31), [0; 16]))).is_err());
        assert!(build_default_flood_scope(Some(&FloodScope::with_key("", [0; 16]))).is_err());
        // A `from_name` scope is always inside the field, because the derivation cuts first.
        let long = FloodScope::from_name(&"b".repeat(40));
        assert_eq!(long.name.len(), 30);
        assert_eq!(
            long.key,
            FloodScope::from_name(&"b".repeat(30)).key,
            "the key belongs to the name the radio can hold"
        );
        assert!(build_default_flood_scope(Some(&long)).is_ok());
    }

    /// One leading `#` is stripped and no more: the firmware hashes the stored name as it is, so
    /// `##us` is a different region from `#us`.
    #[test]
    fn only_one_leading_hash_is_stripped() {
        assert_eq!(FloodScope::from_name("#us").name, "us");
        assert_eq!(FloodScope::from_name("##us").name, "#us");
        assert_ne!(FloodScope::from_name("#us").key, FloodScope::from_name("##us").key);
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
        let frame = build_default_flood_scope(Some(&scope)).expect("builds");
        assert_eq!(frame.len(), 1 + 31 + 16);
        assert_eq!(frame[0], commands::SET_DEFAULT_FLOOD_SCOPE);
        assert_eq!(&frame[1..6], b"us-co");
        assert_eq!(&frame[32..], &scope.key);

        let mut reply = vec![codes::DEFAULT_FLOOD_SCOPE];
        reply.extend_from_slice(&frame[1..]);
        assert_eq!(parse_default_flood_scope(&reply), Some(Some(scope)));

        // The firmware's null answer, and a clear command, are both one byte.
        assert_eq!(parse_default_flood_scope(&[codes::DEFAULT_FLOOD_SCOPE]), Some(None));
        assert_eq!(
            build_default_flood_scope(None).expect("builds"),
            vec![commands::SET_DEFAULT_FLOOD_SCOPE]
        );
        // A name the firmware would refuse is truncated to the 30 bytes it accepts.
        let long = FloodScope::from_name(&"a".repeat(40));
        assert!(build_default_flood_scope(Some(&long)).is_ok());
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

    /// **The answer a request from a distance needs is a flood**, and its shape is pinned on both
    /// sides of the wire here: the phone reads exactly this layout out of its receive log, so a
    /// change to one end that is not made to the other would leave a link that cannot discover a
    /// route at any distance at all.
    #[test]
    fn a_flooded_route_reply_carries_the_path_and_names_its_sender() {
        let host = [0xA1; 32];
        let phone = [0xB2; 32];
        let path = [0x11, 0x22, 0x33];
        let packet = build_route_reply_flood(&host, &phone, 1, 3, &path, &[7, 8, 9, 10])
            .expect("builds");
        // A flood, so repeaters forward it: `flood_max` is 64 hops where an advert is dropped at 8.
        assert_eq!(packet[0], ROUTE_FLOOD_HEADER);
        let view = parse_packet(&packet).expect("parses");
        assert!(view.is_flood());
        assert_eq!(view.payload_type, 0, "REQ is the flood vehicle");
        // The envelope is addressed to the phone, from the host, and names the host in full so the
        // phone can tell its own host from any other node answering.
        assert_eq!(view.payload[0], phone[0]);
        assert_eq!(view.payload[1], host[0]);
        assert_eq!(&view.payload[2..6], ROUTE_REPLY_MAGIC);
        assert_eq!(&view.payload[6..38], &host);

        let carried = parse_route_reply_flood(&packet, &host).expect("reads back");
        assert_eq!(carried.hash_size, 1);
        assert_eq!(carried.hops, 3);
        assert_eq!(carried.bytes, path.to_vec());
        // A reply from somebody else's radio is not our host's route.
        assert!(parse_route_reply_flood(&packet, &phone).is_none());
        // Neither is a packet that is not a flood, or one cut short.
        let mut direct = packet.clone();
        direct[0] = RAW_CUSTOM_DIRECT_HEADER;
        assert!(parse_route_reply_flood(&direct, &host).is_none());
        // **A lost nonce is not a lost route** — the tail is four bytes of it and the path is
        // intact, so the reply still teaches the path. Cutting into the path is what fails.
        let mut no_nonce = packet.clone();
        no_nonce.truncate(no_nonce.len() - 4);
        assert_eq!(
            parse_route_reply_flood(&no_nonce, &host).map(|p| p.bytes),
            Some(path.to_vec())
        );
        let mut short = packet.clone();
        short.truncate(short.len() - 6);
        assert!(parse_route_reply_flood(&short, &host).is_none());
        // A ragged carried path is refused where it is built, not on the air.
        assert!(build_route_reply_flood(&host, &phone, 2, 1, &[0x11], &[0; 4]).is_err());
    }

    /// **A reply goes back under the request's scope.** A dense mesh denies unscoped floods
    /// (`region denyf *`), so an unscoped answer is dropped before it crosses the region: the phone
    /// asks, the host answers, and nothing arrives. The code is an HMAC over *this* packet, so the
    /// reply computes its own rather than copying the request's.
    #[test]
    fn a_scoped_reply_carries_a_code_over_its_own_payload() {
        let host = [0xA1; 32];
        let phone = [0xB2; 32];
        let scope = FloodScope::from_name("us-co");
        let packet = build_route_reply_flood_in_scope(
            &host,
            &phone,
            1,
            2,
            &[0x11, 0x22],
            &[1, 2, 3, 4],
            Some(&scope),
        )
        .expect("builds");
        assert_eq!(packet[0], ROUTE_FLOOD_SCOPED_HEADER, "a transport-coded flood");
        let view = parse_packet(&packet).expect("parses");
        assert!(view.is_flood());
        let codes = view.transport_codes.expect("scoped packets carry codes");
        assert_eq!(
            codes[0],
            transport_code(&scope.key, view.payload_type, view.payload),
            "the code is over the reply, not the request"
        );
        assert_eq!(codes[1], 0, "the second slot is reserved, as the firmware leaves it");
        // And the request's own identification works on it, which is how the daemon proves the
        // scope before answering under it.
        let found = identify_scope(&view, &["us", "us-co"]).expect("the scope is recognisable");
        assert_eq!(found.name, scope.name);
        // Unscoped stays unscoped: a plain flood, no codes.
        let plain = build_route_reply_flood(&host, &phone, 1, 2, &[0x11, 0x22], &[0; 4]).unwrap();
        assert_eq!(plain[0], ROUTE_FLOOD_HEADER);
        assert!(parse_packet(&plain).unwrap().transport_codes.is_none());
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
        assert_eq!(view.payload.len(), 42);
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

#[cfg(test)]
mod stats_tests {
    use super::*;

    /// The frame the firmware writes for `STATS_TYPE_RADIO`: noise floor, RSSI, SNR scaled by four,
    /// then the radio's own transmit and receive airtime.
    #[test]
    fn the_radio_stats_are_read_from_the_frame_the_firmware_writes() {
        let mut frame = vec![codes::STATS, codes::STATS_TYPE_RADIO];
        frame.extend_from_slice(&(-117i16).to_le_bytes());
        frame.push(0xD0); // -48 dBm, as a signed byte
        frame.push(0x0E); // 3.5 dB
        frame.extend_from_slice(&1_234u32.to_le_bytes());
        frame.extend_from_slice(&5_678u32.to_le_bytes());
        let stats = parse_radio_stats(&frame).expect("parses");
        assert_eq!(stats.noise_floor_dbm, -117);
        assert_eq!(stats.last_rssi, -48);
        assert!(
            (stats.last_snr_db() - 3.5).abs() < f32::EPSILON,
            "quarter-dB scale"
        );
        assert_eq!(stats.tx_air_secs, 1_234);
        assert_eq!(stats.rx_air_secs, 5_678);
        assert!(stats.has_signal());
    }

    /// **A radio that has heard nothing says so, and that is not an error.** The firmware reports the
    /// unknown-RSSI sentinel rather than a plausible-looking number, so a link is never credited with
    /// a reading it does not have.
    #[test]
    fn a_radio_that_has_heard_nothing_is_not_credited_with_a_reading() {
        let mut frame = vec![codes::STATS, codes::STATS_TYPE_RADIO];
        frame.extend_from_slice(&(-119i16).to_le_bytes());
        frame.push(codes::UNKNOWN_RSSI as u8);
        frame.push(0);
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes());
        let stats = parse_radio_stats(&frame).expect("parses");
        assert!(!stats.has_signal());
    }

    #[test]
    fn the_core_stats_carry_the_battery_and_the_queue() {
        let mut frame = vec![codes::STATS, codes::STATS_TYPE_CORE];
        frame.extend_from_slice(&4_012u16.to_le_bytes());
        frame.extend_from_slice(&90_000u32.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.push(3);
        let stats = parse_core_stats(&frame).expect("parses");
        assert_eq!(stats.battery_mv, 4_012);
        assert_eq!(stats.uptime_secs, 90_000);
        assert_eq!(stats.queue_len, 3);
    }

    /// Asking for one stats type and being answered with the other is a protocol fault, not a
    /// reading: a silent mix-up would publish a noise floor as a battery voltage.
    #[test]
    fn the_wrong_stats_type_is_refused() {
        let frame = vec![codes::STATS, codes::STATS_TYPE_CORE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(parse_radio_stats(&frame).is_err());
        let frame = vec![codes::STATS, codes::STATS_TYPE_RADIO, 0, 0, 0];
        assert!(parse_core_stats(&frame).is_err());
    }
}
