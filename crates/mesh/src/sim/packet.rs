//! A MeshCore packet, as the firmware builds and reads it.
//!
//! The simulator models the air, so it has to model what goes on the air. This is that: the header
//! byte's route and payload type, the path-length byte's two packed fields, the path, the payload —
//! and the two derived numbers decisions are made from, the **raw length** the receive handler
//! filters on and the **deduplication hash** every node's table is keyed by.
//!
//! Nothing here re-implements MeshCore's cryptography. A payload is opaque bytes, because the
//! simulator's job is delivery, not secrecy, and a harness that decrypted would be testing a
//! second implementation of the thing under test.

use sha2::{Digest as _, Sha256};

/// `MAX_PACKET_HASHES` is the table size; this is the width of one entry.
///
/// `SimpleMeshTables` stores `MAX_PACKET_HASHES * MAX_HASH_SIZE` bytes and compares a whole entry
/// (`src/helpers/SimpleMeshTables.h`), so a duplicate is eight bytes of SHA-256 matching, not a
/// whole packet comparison.
pub const MAX_HASH_SIZE: usize = 8;

/// The firmware's deduplication ring length (`MAX_PACKET_HASHES = 128+32`,
/// `src/helpers/SimpleMeshTables.h`). It has **no timeout**: an entry leaves only when 160 further
/// packets have displaced it, which is why a byte-identical retransmission is swallowed for as long
/// as that takes.
pub const MAX_PACKET_HASHES: usize = 128 + 32;

/// The longest path a packet can carry, in bytes (`MAX_PATH_SIZE`, `src/MeshCore.h:22`).
pub const MAX_PATH_SIZE: usize = 64;

// The three frame ceilings are **not** defined here. They are the same three numbers the record
// budget is derived from, and a simulator with its own copy would be a second opinion about the
// thing every budget in this crate is measured against — so `budget` owns them and this module reads
// them from there. See `crate::budget` for the firmware citations.
pub use crate::budget::{
    COMPANION_FRAME_MAX as MAX_FRAME_SIZE, LOG_RX_HEADER, RECEIVE_MAX as RECEIVED_MAX,
};

/// `ROUTE_TYPE_FLOOD`: a flood whose path is built up as it travels.
pub const ROUTE_TYPE_FLOOD: u8 = 0x01;
/// `ROUTE_TYPE_DIRECT`: a direct route, path supplied by the sender.
pub const ROUTE_TYPE_DIRECT: u8 = 0x02;

/// `PAYLOAD_TYPE_ADVERT` (`src/Packet.h:23`). Carried separately because it has its own hop cap.
pub const PAYLOAD_TYPE_ADVERT: u8 = 0x04;

/// `PAYLOAD_TYPE_RAW_CUSTOM` (`src/Packet.h:32`) — the vehicle a session record rides.
pub const PAYLOAD_TYPE_RAW_CUSTOM: u8 = 0x0F;

/// Which of MeshCore's four route types a packet claims.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Flood, no transport codes.
    Flood,
    /// Flood, scoped by two transport codes.
    TransportFlood,
    /// Direct, no transport codes.
    Direct,
    /// Direct, scoped by two transport codes.
    TransportDirect,
}

impl Route {
    /// The two bits the header carries.
    #[must_use]
    pub fn bits(self) -> u8 {
        match self {
            Self::TransportFlood => 0x00,
            Self::Flood => 0x01,
            Self::Direct => 0x02,
            Self::TransportDirect => 0x03,
        }
    }

    /// `Packet::isRouteFlood` (`src/Packet.h:64`).
    #[must_use]
    pub fn is_flood(self) -> bool {
        matches!(self, Self::Flood | Self::TransportFlood)
    }

    /// `Packet::isRouteDirect` (`src/Packet.h:65`).
    #[must_use]
    pub fn is_direct(self) -> bool {
        matches!(self, Self::Direct | Self::TransportDirect)
    }

    /// `Packet::hasTransportCodes` (`src/Packet.h:67`) — four extra bytes on the wire.
    #[must_use]
    pub fn has_transport_codes(self) -> bool {
        matches!(self, Self::TransportFlood | Self::TransportDirect)
    }
}

/// One packet: what it claims to be, the path it carries, and the opaque payload.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packet {
    pub route: Route,
    pub payload_type: u8,
    /// The path, in travel order, concatenated hashes of the node's own `path_width` bytes each.
    pub path: Vec<u8>,
    /// The region a scoped packet names, as an opaque id standing in for its two transport codes.
    ///
    /// The real field is two 16-bit codes an HMAC over the payload produces
    /// (`TransportKeyStore.cpp`). A repeater forwards a scoped flood only when it can resolve the
    /// region, so what decides delivery is *identity* — which is all this carries. Verifying the
    /// HMAC is the companion codec's job, not a simulator's.
    pub scope: Option<u16>,
    /// The payload, opaque to this crate.
    pub payload: Vec<u8>,
}

impl Packet {
    /// A flood carrying `payload`, unscoped.
    #[must_use]
    pub fn flood(payload_type: u8, payload: Vec<u8>) -> Self {
        Self {
            route: Route::Flood,
            payload_type,
            path: Vec::new(),
            scope: None,
            payload,
        }
    }

    /// A flood carrying `payload`, scoped to `scope`. Four bytes longer on the wire.
    #[must_use]
    pub fn flood_scoped(payload_type: u8, scope: u16, payload: Vec<u8>) -> Self {
        Self {
            route: Route::TransportFlood,
            payload_type,
            path: Vec::new(),
            scope: Some(scope),
            payload,
        }
    }

    /// A direct packet along `path`.
    #[must_use]
    pub fn direct(payload_type: u8, path: Vec<u8>, payload: Vec<u8>) -> Self {
        Self {
            route: Route::Direct,
            payload_type,
            path,
            scope: None,
            payload,
        }
    }

    /// The header byte: route in bits 0–1, payload type in 2–5, version in 6–7.
    #[must_use]
    pub fn header(&self) -> u8 {
        self.route.bits() | (self.payload_type << 2)
    }

    /// The path-length byte: hash count in bits 0–5, hash size minus one in bits 6–7.
    ///
    /// The encoded byte is what a direct packet's path width is carried in, so a node whose width
    /// disagrees with the sender's reads a different path and drops the packet. That
    /// disagreement is real and is why [`crate::sim`] can model it at all.
    #[must_use]
    pub fn path_byte(&self, path_width: u8) -> u8 {
        let count = self.hops(path_width) & 0x3F;
        count | ((path_width.saturating_sub(1) & 0x03) << 6)
    }

    /// `Packet::getRawLength` (`src/Packet.cpp:49`): header, path-length byte, path, payload, and
    /// four more bytes when the packet carries transport codes.
    #[must_use]
    pub fn raw_len(&self) -> usize {
        2 + self.path.len() + self.payload.len() + if self.route.has_transport_codes() { 4 } else { 0 }
    }

    /// How many hashes the path holds at this width. A path that is not a whole number of hashes
    /// resolves to the count that fits, which is what the firmware's bit field means.
    #[must_use]
    pub fn hops(&self, path_width: u8) -> u8 {
        let width = usize::from(path_width.max(1));
        u8::try_from(self.path.len() / width).unwrap_or(u8::MAX)
    }

    /// The deduplication hash: eight bytes of `SHA256(payload_type || payload)`.
    ///
    /// `Packet::calculatePacketHash` (`src/Packet.cpp:39-47`) folds in `path_len` **only for
    /// `PAYLOAD_TYPE_TRACE`**, so two copies of a packet with different paths hash identically and
    /// the second is dropped. That is the whole reason every retransmission has to be a different
    /// packet, and the reason a fresh nonce appears in every flood wrapper.
    ///
    /// `PAYLOAD_TYPE_TRACE` is the one packet type whose hash this does **not** reproduce: the
    /// firmware hashes its encoded path-length byte, and this type derives that byte from the path
    /// rather than storing it. ldmux originates no trace packet, so modelling that would translate
    /// a mechanism nothing here uses.
    #[must_use]
    pub fn hash(&self) -> [u8; MAX_HASH_SIZE] {
        let mut sha = Sha256::new();
        sha.update([self.payload_type]);
        sha.update(&self.payload);
        let digest = sha.finalize();
        let mut out = [0u8; MAX_HASH_SIZE];
        out.copy_from_slice(&digest[..MAX_HASH_SIZE]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The longest packet that reaches an application is 173 bytes, and one byte more is silent
    /// loss. This is `RC-6`'s arithmetic in one line, and it is the number every record budget in
    /// `docs/RADIO-PLAN.md` §4.1 is derived from.
    #[test]
    fn the_receive_ceiling_is_173_bytes() {
        assert_eq!(RECEIVED_MAX, 173);
        let at_the_limit = Packet::flood(PAYLOAD_TYPE_RAW_CUSTOM, vec![0; RECEIVED_MAX - 2]);
        assert_eq!(at_the_limit.raw_len(), 173);
        let one_over = Packet::flood(PAYLOAD_TYPE_RAW_CUSTOM, vec![0; RECEIVED_MAX - 1]);
        assert_eq!(one_over.raw_len(), 174);
    }

    /// A flood's path is one hash per hop, and the length byte packs the count and the width into
    /// one byte: count in the low six bits, width minus one in the top two.
    #[test]
    fn the_path_byte_packs_count_and_width() {
        let mut p = Packet::flood(PAYLOAD_TYPE_RAW_CUSTOM, vec![]);
        p.path = vec![0; 3];
        assert_eq!(p.path_byte(1), 3);
        assert_eq!(p.path_byte(2), 0b0100_0001);
        assert_eq!(p.path_byte(3), 0b1000_0001);
        assert_eq!(p.hops(1), 3);
        assert_eq!(p.hops(2), 1);
    }

    /// Two paths, one payload, one hash — so the deduplication table drops the second copy even
    /// though the packet arrived by a different road. Asserting it here is what keeps a reader from
    /// assuming the hash covers the path.
    #[test]
    fn the_hash_covers_the_payload_and_not_the_path() {
        let one = Packet::direct(PAYLOAD_TYPE_RAW_CUSTOM, vec![1, 2], vec![9; 40]);
        let other = Packet::direct(PAYLOAD_TYPE_RAW_CUSTOM, vec![7, 7, 7], vec![9; 40]);
        assert_eq!(one.hash(), other.hash());
        let different = Packet::direct(PAYLOAD_TYPE_RAW_CUSTOM, vec![1, 2], vec![8; 40]);
        assert_ne!(one.hash(), different.hash());
    }

    /// A scoped packet is four bytes longer than the same payload unscoped, because the two
    /// transport codes sit between the header and the path-length byte. A budget computed without
    /// them is four bytes optimistic on exactly the meshes that use a region.
    #[test]
    fn transport_codes_cost_four_bytes() {
        let unscoped = Packet::flood(PAYLOAD_TYPE_RAW_CUSTOM, vec![0; 100]);
        let scoped = Packet::flood_scoped(PAYLOAD_TYPE_RAW_CUSTOM, 3, vec![0; 100]);
        assert_eq!(scoped.raw_len(), unscoped.raw_len() + 4);
    }
}
