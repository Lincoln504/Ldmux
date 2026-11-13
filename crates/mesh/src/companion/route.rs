//! **The route request, which is the only way to a path past the advert cap.**
//!
//! A repeater drops an advert at `flood_max_advert` — **eight hops** by default
//! (`examples/simple_repeater/MyMesh.cpp:972`, `src/helpers/RoutingPolicy.h:16-22`) — while a `REQ`
//! flood travels `flood_max`, sixty-four. On a mesh longer than eight hops neither end can learn a
//! road from an advert, so both send floods, and a flood costs every repeater in range a
//! transmission. The request is the mechanism that fixes that: a flooded question that carries the
//! asker's own key, answered by a flooded reply carrying the chain the question crossed.
//!
//! **It is airtime, not reachability.** A direct send crosses the same hops and loses the same
//! per-hop probability; what it saves is the fan-out. That is still worth a round trip at range,
//! because every record after it costs one transmission per hop instead of one per repeater.
//!
//! The two packets are plain datagrams — a `REQ` carrying this end's key and a nonce, and a `REQ`
//! carrying the reply — so a stock companion forwards them and, crucially, a stock companion's
//! receive log hands them to this application (`src/Dispatcher.cpp:199`, `MyMesh.cpp:291-297`). No
//! contact, no shared secret and no custom firmware is needed on any repeater.
//!
//! `docs/RADIO-PLAN.md` AR-17 records why this was deleted once: the plan believed a sender learned
//! its route from its own initiation, which it does not.

use crate::companion::build::{path_byte, BuildError, ROUTE_FLOOD_HEADER};

/// Marks a flooded route request: `[dest][src]["LDRQ"][asker key 32][nonce 4]`.
pub const ROUTE_REQUEST_MAGIC: &[u8; 4] = b"LDRQ";
/// Marks a flooded route reply: `[dest][src]["LDRR"][responder key 32][path byte][path][nonce]`.
pub const ROUTE_REPLY_MAGIC: &[u8; 4] = b"LDRR";


/// The bytes between the packet header and the payload: the sender's and destination's hash bytes.
const HASHES: usize = 2;
/// The size of a peer's public key.
const KEY: usize = 32;
/// The nonce that makes a retry a different packet.
const NONCE: usize = 4;

/// A route request as it arrived on this end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RouteRequest {
    /// The asker's radio key, which is who the reply is addressed to.
    pub peer_key: [u8; KEY],
    /// The chain the request crossed, in travel order towards this end.
    pub path: Vec<u8>,
    /// The hash width that chain is encoded with.
    pub hash_size: u8,
    /// The four bytes that make this request a different packet from the last one.
    pub nonce: [u8; NONCE],
}

/// A route reply as it arrived on this end: the chain to the peer, already reversed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RouteReply {
    /// The path a packet must carry to reach the peer that answered.
    pub path: Vec<u8>,
    /// The hash width that path is encoded with.
    pub hash_size: u8,
}

/// Build the flooded question: "what road reaches you from me".
///
/// `hash_size` is this radio's path width; the request's own path starts empty and grows as each
/// repeater appends its hash, which is the chain the reply carries back.
///
/// # Errors
///
/// [`BuildError::BadHashSize`] when `hash_size` is outside 1..=3.
pub fn build_request(
    hash_size: u8,
    peer_key: &[u8; KEY],
    own_key: &[u8; KEY],
    nonce: &[u8; NONCE],
) -> Result<Vec<u8>, BuildError> {
    let mut payload = Vec::with_capacity(HASHES + ROUTE_REQUEST_MAGIC.len() + KEY + NONCE);
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(ROUTE_REQUEST_MAGIC);
    payload.extend_from_slice(own_key);
    payload.extend_from_slice(nonce);
    flood_packet(hash_size, &payload)
}

/// Build the flooded answer, carrying the chain the request crossed in reverse.
///
/// **Reversed by hop, not by byte**: the request appended one hash per repeater on the way here, so
/// the way back is the same groups in the opposite order.
///
/// # Errors
///
/// [`BuildError::BadHashSize`] when `hash_size` is outside 1..=3.
pub fn build_reply(
    own_key: &[u8; KEY],
    peer_key: &[u8; KEY],
    hash_size: u8,
    arrival: &[u8],
    nonce: &[u8; NONCE],
) -> Result<Vec<u8>, BuildError> {
    let width = usize::from(hash_size.clamp(1, 3));
    let hops = u8::try_from(arrival.len() / width).unwrap_or(u8::MAX);
    let mut outbound = Vec::with_capacity(arrival.len());
    for hop in arrival.chunks(width).rev() {
        outbound.extend_from_slice(hop);
    }
    let mut payload = Vec::with_capacity(HASHES + ROUTE_REPLY_MAGIC.len() + KEY + 1 + outbound.len() + NONCE);
    payload.push(peer_key[0]);
    payload.push(own_key[0]);
    payload.extend_from_slice(ROUTE_REPLY_MAGIC);
    payload.extend_from_slice(own_key);
    payload.push(path_byte(hash_size, hops)?);
    payload.extend_from_slice(&outbound);
    payload.extend_from_slice(nonce);
    flood_packet(hash_size, &payload)
}

/// Read a route request addressed to this end, or `None` for anything else.
#[must_use]
pub fn parse_request(packet: &[u8], own_key: &[u8; KEY]) -> Option<RouteRequest> {
    let (payload, path, hash_size) = flood_payload(packet)?;
    if payload.len() != HASHES + ROUTE_REQUEST_MAGIC.len() + KEY + NONCE {
        return None;
    }
    if payload[0] != own_key[0] || payload.get(2..6)? != ROUTE_REQUEST_MAGIC.as_slice() {
        return None;
    }
    Some(RouteRequest {
        peer_key: payload.get(6..6 + KEY)?.try_into().ok()?,
        path,
        hash_size,
        nonce: payload.get(6 + KEY..6 + KEY + NONCE)?.try_into().ok()?,
    })
}

/// Read a route reply from the peer this end asked, or `None` for anything else.
#[must_use]
pub fn parse_reply(packet: &[u8], peer_key: &[u8; KEY]) -> Option<RouteReply> {
    let (payload, _, _) = flood_payload(packet)?;
    if payload.get(2..6)? != ROUTE_REPLY_MAGIC.as_slice() {
        return None;
    }
    // The responder names itself, so an answer meant for another asker is not taken as this one's.
    if payload.get(6..6 + KEY)? != peer_key.as_slice() {
        return None;
    }
    let rest = payload.get(6 + KEY..)?;
    let encoded = *rest.first()?;
    let hash_size = (encoded >> 6) + 1;
    if hash_size == 4 {
        return None;
    }
    let width = usize::from(hash_size);
    let hops = usize::from(encoded & 0x3F);
    let path = rest.get(1..1 + hops * width)?;
    Some(RouteReply {
        path: path.to_vec(),
        hash_size,
    })
}

/// One unscoped flood carrying `payload`, with an empty path of this radio's width.
fn flood_packet(hash_size: u8, payload: &[u8]) -> Result<Vec<u8>, BuildError> {
    let mut out = Vec::with_capacity(2 + payload.len());
    out.push(ROUTE_FLOOD_HEADER);
    out.push(path_byte(hash_size, 0)?);
    out.extend_from_slice(payload);
    Ok(out)
}

/// The payload of an unscoped flood, with the chain it crossed and its hash width.
fn flood_payload(packet: &[u8]) -> Option<(&[u8], Vec<u8>, u8)> {
    if packet.first().copied() != Some(ROUTE_FLOOD_HEADER) {
        return None;
    }
    let encoded = *packet.get(1)?;
    let hash_size = (encoded >> 6) + 1;
    if hash_size == 4 {
        return None;
    }
    let width = usize::from(hash_size);
    let hops = usize::from(encoded & 0x3F);
    let path_end = 2 + hops * width;
    let path = packet.get(2..path_end)?.to_vec();
    // The route type and the payload type are both in that first header byte, which is why this
    // never has to look at the payload to know it is a `REQ` flood. The magic inside is the filter.
    let payload = packet.get(path_end..)?;
    Some((payload, path, hash_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; KEY] {
        [byte; KEY]
    }

    /// **The chain is reversed by hop, not by byte.** Both ends must agree, or every reply names a
    /// road no repeater recognises.
    #[test]
    fn a_request_is_answered_with_the_chain_reversed_by_hop() {
        let asker = key(0x11);
        let responder = key(0x22);
        let request = build_request(1, &responder, &asker, &[1, 2, 3, 4]).expect("a request");

        // It arrives at the responder after two repeaters appended their hashes.
        let mut arrival = request.clone();
        arrival[1] = path_byte(1, 2).expect("two hops");
        arrival.splice(2..2, [0xAA, 0xBB]);

        let parsed = parse_request(&arrival, &responder).expect("the responder reads it");
        assert_eq!(parsed.peer_key, asker);
        assert_eq!(parsed.path, vec![0xAA, 0xBB]);
        assert_eq!(parsed.hash_size, 1);

        let reply = build_reply(&responder, &asker, 1, &parsed.path, &parsed.nonce).expect("a reply");
        let back = parse_reply(&reply, &responder).expect("the asker reads it");
        assert_eq!(
            back.path,
            vec![0xBB, 0xAA],
            "the way back is the way here in the opposite order"
        );
    }

    /// A two-byte mesh reverses two-byte groups, which is the width the field's radios use.
    #[test]
    fn a_two_byte_path_is_reversed_by_groups() {
        let arrival = vec![0x01, 0x02, 0x03, 0x04];
        let reply = build_reply(&key(1), &key(2), 2, &arrival, &[0; NONCE]).expect("a reply");
        let back = parse_reply(&reply, &key(1)).expect("the asker reads it");
        assert_eq!(back.path, vec![0x03, 0x04, 0x01, 0x02]);
        assert_eq!(back.hash_size, 2);
    }

    /// A reply from another responder is not this asker's, and a request for another node is not
    /// this end's business.
    #[test]
    fn a_packet_for_somebody_else_is_not_taken() {
        let request = build_request(1, &key(0x22), &key(0x11), &[0; NONCE]).expect("a request");
        assert!(parse_request(&request, &key(0x33)).is_none());
        assert!(parse_request(&request, &key(0x22)).is_some());

        let reply = build_reply(&key(0x44), &key(0x11), 1, &[0xAA], &[0; NONCE]).expect("a reply");
        assert!(parse_reply(&reply, &key(0x55)).is_none());
        assert!(parse_reply(&reply, &key(0x44)).is_some());
    }

    /// The two packets are floods, which is the whole point: a flood travels sixty-four hops while an
    /// advert dies at eight.
    #[test]
    fn both_packets_are_unscoped_floods() {
        let request = build_request(1, &key(2), &key(1), &[0; NONCE]).expect("a request");
        assert_eq!(request[0], ROUTE_FLOOD_HEADER);
        let reply = build_reply(&key(2), &key(1), 1, &[], &[0; NONCE]).expect("a reply");
        assert_eq!(reply[0], ROUTE_FLOOD_HEADER);
    }
}
