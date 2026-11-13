//! What a pushed packet means, for either end.
//!
//! **A record arrives as one of two pushes, and `0x88` is not one of them.** The firmware reports a
//! packet addressed to this client as `PUSH_CODE_RAW_DATA` (`0x84`) when it is a direct
//! `RAW_CUSTOM` and as `PUSH_CODE_CONTROL_DATA` (`0x8E`) when it is a `REQ` (which is how a flood
//! travels): `MyMesh::onRawDataRecv` (`examples/companion_radio/MyMesh.cpp:832-853`) and
//! `MyMesh::onControlDataRecv` (`:810-830`). `PUSH_CODE_LOG_RX_DATA` (`0x88`) is a **debug tap** —
//! `MyMesh::logRxRaw` (`:291-301`) writes every packet the radio demodulates, neighbours included,
//! and only when the radio's receive log is switched on.
//!
//! This module was built on `0x88` alone. Measured in the field 2026-11-12, with both radios on
//! 910.525/62.5/SF7/CR5 in one room: the phone transmitted a 137-byte record, its radio answered
//! `OK`, the host's radio pushed what it heard, and the host reported **0 bytes received and 0
//! handshakes** for as long as it ran, because every real arrival came as `0x84` and the classifier
//! returned `None`. The 0x88 frames either end did see were other operators' adverts on the public
//! channel.
//!
//! What this module does is decide, once, whether a push carries a session record — and if it does,
//! hand up the record **and the shape it arrived in**, because the endpoint's rules are about the
//! shape: a direct packet with hops left is in transit to somebody else, a flood's chain is the way
//! back, and only a direct arrival that finished its path proves anything (`RC-3`).
//!
//! **The wrapper's magic decides, not the payload type.** A direct session record is a `RAW_CUSTOM`
//! packet; a flooded one rides `REQ` with the `LDMR` wrapper inside it, because the firmware
//! flood-routes `REQ` and never `RAW_CUSTOM`. A classifier keyed on the payload type would accept
//! every direct arrival and silently ignore every flood — the same class of loss as `RC-6`
//! (`docs/RADIO-PLAN.md` §12.7b, `AR-10`).
//!
//! It is here rather than in a carrier because both ends classify: the host from a USB port and the
//! phone from a BLE characteristic (`§8.1`, `IC-4`).

use crate::budget::{FLOOD_WRAPPER, MAX_PATH_BYTES};

/// `PAYLOAD_TYPE_RAW_CUSTOM` (`src/Packet.h:32`) — the vehicle a direct session record rides.
pub const PAYLOAD_TYPE_RAW_CUSTOM: u8 = 0x0F;
/// `PAYLOAD_TYPE_REQ` (`src/Packet.h:23`) — the vehicle the flood carrier rides.
///
/// **The firmware never hands it up as a payload, and it is still delivered.** `REQ` is in the
/// decrypt-only family (`Mesh.cpp:131-147`: `dest_hash`, `src_hash`, then a MAC per peer), so a
/// plaintext `LDMR` wrapper inside it fails every peer's MAC and the packet is forwarded as a flood
/// rather than handed up as a payload. It reaches the application through the **receive log**
/// instead, and that is not a debug tap an operator has to switch on: `Dispatcher::checkRecv` calls
/// `logRxRaw` for every demodulated packet (`src/Dispatcher.cpp:199`), and `MyMesh::logRxRaw` writes
/// `PUSH_CODE_LOG_RX_DATA` whenever the serial is connected, with no preference gate
/// (`examples/companion_radio/MyMesh.cpp:291-297`). So a flood is delivered as `0x88`, carrying the
/// raw packet including the `LDMR` wrapper, and this module reads it there.
pub const PAYLOAD_TYPE_REQ: u8 = 0x00;
/// `PAYLOAD_TYPE_CONTROL` (`src/Packet.h:30`) — the only payload type the `0x8E` push reports.
pub const PAYLOAD_TYPE_CONTROL: u8 = 0x0B;

/// The four bytes that mark a flooded session record, inside the packet's payload.
///
/// Re-exported from the builder that writes it, so the reader and the writer cannot disagree.
pub use crate::companion::build::RECORD_FLOOD_MAGIC;

/// The push code of a received direct packet (`PUSH_CODE_RAW_DATA`, `MyMesh.cpp:121`).
pub const PUSH_RAW_DATA: u8 = 0x84;
/// The push code of a received **control** packet (`PUSH_CODE_CONTROL_DATA`, v8+, `MyMesh.cpp:131`).
///
/// **This is not how a flood arrives.** The firmware pushes it from `onControlDataRecv`
/// (`Mesh.cpp:70-76`), which runs only for `PAYLOAD_TYPE_CONTROL` with `payload[0] & 0x80` on a
/// route-**direct** packet: it is a zero-hop control message, not a `REQ`.
pub const PUSH_CONTROL_DATA: u8 = 0x8E;
/// The push code of the receive **log** (`PUSH_CODE_LOG_RX_DATA`, `MyMesh.cpp:125`). A debug tap:
/// it reports every packet the radio demodulates, not the ones addressed to this client.
pub const PUSH_LOG_RX_DATA: u8 = 0x88;

/// `ROUTE_TYPE_FLOOD`: a flood whose path is built up as it travels.
pub const ROUTE_TYPE_FLOOD: u8 = 0x01;
/// `ROUTE_TYPE_DIRECT`: a direct route, path supplied by the sender.
pub const ROUTE_TYPE_DIRECT: u8 = 0x02;

/// One pushed packet, as a session record and the shape it arrived in.
#[derive(Clone, PartialEq, Debug)]
pub struct PushArrival {
    /// The sealed record, with the flood wrapper removed.
    pub record: Vec<u8>,
    /// Whether it came as a flood.
    pub flood: bool,
    /// The path as received: the chain a flood crossed, or the hashes a direct packet still has to
    /// travel. A direct packet that has reached its destination has none left, which is what makes
    /// `hops == 0` the proof of a working route.
    pub path: Vec<u8>,
    /// Bytes per hop the path is encoded with (1..=3).
    pub path_width: u8,
    /// The last link's SNR in quarters of a dB, which is how the firmware reports it.
    pub snr_quarter_db: i8,
    /// The last link's RSSI in dBm.
    pub rssi_dbm: i8,
    /// The wrapper's sender byte, when the record arrived wrapped: one byte of the sender's key.
    ///
    /// **The only way a host with no route learns the phone's radio key.** Without it the answer
    /// goes to hash zero and the phone ignores it, which is the asymmetric-link case the teaching
    /// flood exists for (`docs/RADIO-PLAN.md` §12.7b).
    pub sender: Option<u8>,
    /// The path's length in bytes as the push stated it, when the push does not carry the bytes.
    ///
    /// `0x84` states none (a direct packet that reached us has consumed its path) and `0x8E` states
    /// a length but not the hashes (`onControlDataRecv` writes `packet->path_len` and then the
    /// payload). The length is still the number a reply's budget has to respect, so it is kept even
    /// when the chain itself is unknown — a reply to a flood is a flood, and needs no chain.
    pub path_bytes: u16,
}

impl PushArrival {
    /// How many hops the arrival describes.
    #[must_use]
    pub fn hops(&self) -> u8 {
        let width = usize::from(self.path_width.clamp(1, 3));
        if self.path.is_empty() {
            // No hashes, only a length: `0x8E`. Divided by the width the packet was encoded with.
            return u8::try_from(usize::from(self.path_bytes) / width).unwrap_or(u8::MAX);
        }
        u8::try_from(self.path.len() / width).unwrap_or(u8::MAX)
    }

    /// The SNR in dB, the unit every event in this crate speaks.
    #[must_use]
    pub fn snr_db(&self) -> f32 {
        f32::from(self.snr_quarter_db) / 4.0
    }

    /// The largest record the outbound packet may carry, given the path this arrival named.
    ///
    /// A reply follows the reverse of a flood's chain, so the chain is what the outbound packet must
    /// carry and its length comes off the record budget. A direct arrival has had its path consumed
    /// and says nothing about the outbound path, so it changes nothing here.
    #[must_use]
    pub fn outbound_record_max(&self) -> Option<u8> {
        if !self.flood {
            return None;
        }
        let limit = crate::budget::RecordBudget::flood_scoped(usize::from(self.path_bytes))
            .limit()
            .min(crate::constants::RECORD_RECEIVED_MAX);
        u8::try_from(limit).ok()
    }
}

/// One packet on the wire, as far as a receiver has to decode it.
struct Wire<'a> {
    flood: bool,
    payload_type: u8,
    path: &'a [u8],
    path_width: u8,
    payload: &'a [u8],
}

/// Decode `[header][path_len][path][payload]`.
///
/// A `TRANSPORT_FLOOD`/`TRANSPORT_DIRECT` packet carries two 16-bit codes before the path
/// (`Packet::readFrom`), and a width of 4 is reserved by the firmware, so both are refused here
/// rather than mis-read.
fn wire(packet: &[u8]) -> Option<Wire<'_>> {
    let header = *packet.first()?;
    let route = header & 0x03;
    let payload_type = (header >> 2) & 0x0F;
    let mut i = 1;
    if route == 0 || route == 3 {
        // The two transport codes: `packet[i..i + 4]`.
        packet.get(i..i + 4)?;
        i += 4;
    }
    let encoded = *packet.get(i)?;
    i += 1;
    let path_width = (encoded >> 6) + 1;
    if path_width == 4 {
        return None;
    }
    let hops = encoded & 0x3F;
    let want = usize::from(hops) * usize::from(path_width);
    if want > MAX_PATH_BYTES {
        return None;
    }
    let path = packet.get(i..i + want)?;
    i += want;
    Some(Wire {
        flood: matches!(route, ROUTE_TYPE_FLOOD) || route == 0,
        payload_type,
        path,
        path_width,
        payload: packet.get(i..)?,
    })
}

/// One push, classified.
///
/// `None` means the push is not a session record — a route request, an advert, a stranger's channel
/// traffic — which the caller ignores. The record is returned with the readings that arrived with
/// it, so route state can be updated from an authenticated arrival and nothing else (`RC-3`).
///
/// # The three shapes
///
/// | push | frame | what the payload is |
/// |---|---|---|
/// | `0x84` RAW_DATA | `[0x84][snr×4][rssi][0xFF][payload]` | a `RAW_CUSTOM` payload, so a direct record |
/// | `0x8E` CONTROL_DATA | `[0x8E][snr×4][rssi][path_len][payload]` | a `REQ` payload, so a flood's wrapped record |
/// | `0x88` LOG_RX_DATA | `[0x88][snr×4][rssi][whole packet]` | the packet with its header, a debug tap |
///
/// The first two are how data actually arrives; the third is kept because it is the only view that
/// shows the packet's own header, which is what a field diagnosis needs.
#[must_use]
pub fn classify(frame: &[u8]) -> Option<PushArrival> {
    match *frame.first()? {
        PUSH_RAW_DATA => {
            let [_, snr, rssi, _reserved, ref payload @ ..] = *frame else {
                return None;
            };
            // `onRawDataRecv` writes a reserved byte and then the payload alone: no header, no path.
            let (record, sender) = flood_or_raw(payload, PAYLOAD_TYPE_RAW_CUSTOM)?;
            Some(PushArrival {
                record: record.to_vec(),
                sender,
                flood: false,
                path: Vec::new(),
                path_width: 1,
                path_bytes: 0,
                snr_quarter_db: snr as i8,
                rssi_dbm: rssi as i8,
            })
        }
        // **A control push is not a record carrier.** `onControlDataRecv` runs for
        // `PAYLOAD_TYPE_CONTROL` with `payload[0] & 0x80` on a route-direct packet only
        // (`Mesh.cpp:70-76`), so it is a zero-hop control message, and a record placed there would
        // have to spend its first byte on a flag and could never travel past one hop.
        PUSH_CONTROL_DATA => None,
        PUSH_LOG_RX_DATA => {
            let view = wire(frame.get(3..)?)?;
            let (record, sender) = flood_or_raw(view.payload, view.payload_type)?;
            Some(PushArrival {
                record: record.to_vec(),
                sender,
                flood: view.flood,
                path: view.path.to_vec(),
                path_width: view.path_width,
                path_bytes: u16::try_from(view.path.len()).unwrap_or(u16::MAX),
                snr_quarter_db: frame[1] as i8,
                rssi_dbm: frame[2] as i8,
            })
        }
        _ => None,
    }
}

/// The record inside a payload, by the wrapper's magic first and the payload type second.
///
/// **The magic decides, not the payload type.** A flood's `REQ` carries the `LDMR` wrapper; a
/// direct `RAW_CUSTOM` carries the record alone. A classifier keyed on the payload type alone would
/// accept every direct arrival and silently ignore every flood — `AR-10`.
fn flood_or_raw(body: &[u8], payload_type: u8) -> Option<(&[u8], Option<u8>)> {
    let (record, sender): (&[u8], Option<u8>) = match body.get(2..6) {
        Some(magic) if magic == RECORD_FLOOD_MAGIC => (body.get(FLOOD_WRAPPER..)?, body.get(1).copied()),
        // A bare payload is a record **only when the packet says `RAW_CUSTOM`**.
        //
        // **`REQ` is not this protocol's name for its record; the wrapper is.** `PAYLOAD_TYPE_REQ`
        // is MeshCore's own request payload, and the public mesh carries other operators' `REQ`
        // floods continuously: path requests (our own `LDRQ` included), adverts, strangers' traffic.
        // Taking every `REQ` as a session record feeds all of it to the transaction layer, which
        // counts each one as received and then drops it with no verdict at all: measured
        // 2026-11-13 on the phone, `received=25 admitted=0` with every refusal counter still zero,
        // and the frames were `LDRQ` route requests with a decoded path.
        _ if payload_type == PAYLOAD_TYPE_RAW_CUSTOM => (body, None),
        _ => return None,
    };
    if record.is_empty() { None } else { Some((record, sender)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::build::{raw_direct, record_flood};

    /// The log frame the firmware pushes around a packet.
    fn push(packet: &[u8]) -> Vec<u8> {
        let mut out = vec![PUSH_LOG_RX_DATA, 0xF4, 0xA6]; // -3 dB SNR, -90 dBm
        out.extend_from_slice(packet);
        out
    }

    /// The frame the firmware pushes for a packet addressed to this client.
    ///
    /// `onRawDataRecv` (`MyMesh.cpp:832-853`): `[0x84][snr×4][rssi][0xFF reserved][payload]`. The
    /// payload is the `RAW_CUSTOM` payload alone, so there is no header and no path.
    fn raw_push(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![PUSH_RAW_DATA, 0xF4, 0xA6, 0xFF];
        out.extend_from_slice(payload);
        out
    }

    /// `onControlDataRecv` (`MyMesh.cpp:810-830`): `[0x8E][snr×4][rssi][path_len][payload]`.
    fn control_push(payload: &[u8], path_len: u8) -> Vec<u8> {
        let mut out = vec![PUSH_CONTROL_DATA, 0xF4, 0xA6, path_len];
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn a_direct_record_arrives_as_raw_data_and_not_as_the_debug_log() {
        // **The field failure of 2026-11-12, as a test.** Both ends classified `0x88` only, which is
        // `logRxRaw`'s debug tap, so a real `0x84` arrival was dropped and the host reported "0 bytes
        // received" while a phone transmitted successfully beside it.
        let arrival = classify(&raw_push(b"sealed")).expect("a 0x84 push is a session record");
        assert_eq!(arrival.record, b"sealed");
        assert!(!arrival.flood);
        assert_eq!(arrival.hops(), 0);
        assert_eq!(arrival.snr_db(), -3.0);
        assert_eq!(arrival.rssi_dbm, -90);
        assert_eq!(arrival.sender, None, "a direct record carries no sender byte");
    }

    #[test]
    fn a_control_push_is_not_a_record_carrier() {
        // `onControlDataRecv` is for `PAYLOAD_TYPE_CONTROL` with the top payload bit set, on a
        // route-direct packet only (`Mesh.cpp:70-76`) — a zero-hop control message, not a record.
        // A record put there would spend a byte on the flag and could never pass one hop.
        let mut peer = [0u8; 32];
        peer[0] = 0x11;
        let mut own = [0u8; 32];
        own[0] = 0x22;
        let packet = record_flood(1, &peer, &own, None, &[0, 0, 0, 0], b"sealed").unwrap();
        let payload = &packet[2..];
        assert_eq!(
            classify(&control_push(payload, 3)),
            None,
            "0x8E carries control messages, never a session record"
        );
    }

    #[test]
    fn a_flood_is_read_from_the_receive_log_and_keeps_its_sender() {
        // **A flood arrives as the receive log, and that is a delivery path.** The firmware never
        // hands a plaintext `REQ` up as a payload (`Mesh.cpp:131-147`), but `logRxRaw` runs for every
        // demodulated packet (`src/Dispatcher.cpp:199`) and `MyMesh::logRxRaw` writes it whenever the
        // serial is connected (`MyMesh.cpp:291-297`), so the record reaches the app as `0x88` with its
        // wrapper intact. The flood carrier is therefore a real fallback, not a debug-only one.
        let mut peer = [0u8; 32];
        peer[0] = 0x11;
        let mut own = [0u8; 32];
        own[0] = 0x22;
        let packet = record_flood(1, &peer, &own, None, &[0, 0, 0, 0], b"sealed").unwrap();
        let arrival = classify(&push(&packet)).expect("a flooded record is visible in the log");
        assert_eq!(arrival.record, b"sealed");
        assert!(arrival.flood);
        assert_eq!(arrival.sender, Some(0x22), "the wrapper's sender byte is the phone's key hash");
        assert_eq!(arrival.path_bytes, 0, "measured from the packet's own path, which is empty here");
        assert!(arrival.outbound_record_max().is_some(), "a flood prices the reply");
    }

    #[test]
    fn a_neighbours_advert_in_the_debug_log_is_not_a_record() {
        // 0x88 is still understood, because it is the only view with the packet's own header — but
        // it only yields a record when the wrapper or the payload type says so.
        let advert = [0x11u8, 0x00, 0x00, 0xAA];
        assert_eq!(classify(&push(&advert)), None);
    }

    #[test]
    fn a_direct_record_that_reached_us_is_a_direct_arrival_with_no_hops_left() {
        let packet = raw_direct(1, &[], b"sealed").unwrap();
        let arrival = classify(&push(&packet)).expect("a direct record is a session record");
        assert!(!arrival.flood);
        assert_eq!(arrival.record, b"sealed");
        assert_eq!(arrival.hops(), 0);
        assert_eq!(arrival.path_width, 1);
        assert_eq!(arrival.snr_db(), -3.0);
        assert_eq!(arrival.rssi_dbm, -90);
        // A direct arrival says nothing about the outbound path.
        assert_eq!(arrival.outbound_record_max(), None);
    }

    #[test]
    fn a_direct_record_with_hops_left_is_in_transit_and_says_so() {
        let packet = raw_direct(1, &[1, 2], b"sealed").unwrap();
        let arrival = classify(&push(&packet)).expect("still a record");
        assert!(!arrival.flood);
        assert_eq!(arrival.hops(), 2);
    }

    #[test]
    fn a_flooded_record_is_classified_even_though_it_rides_a_route_request() {
        // This is AR-10 as a test: the payload type of a flooded record is `REQ`, not `RAW_CUSTOM`,
        // so a classifier keyed on the payload type would drop it.
        let packet = record_flood(1, &[1; 32], &[2; 32], None, &[0; 4], b"sealed").unwrap();
        assert_ne!(packet[0] & 0x3C, PAYLOAD_TYPE_RAW_CUSTOM << 2);
        let arrival = classify(&push(&packet)).expect("a flooded record is a session record");
        assert!(arrival.flood);
        assert_eq!(arrival.record, b"sealed");
        // 161 − 4 bytes of scope − an empty arrival path: the conservative figure, because a reply
        // may go out scoped whether or not the record it answers was.
        assert_eq!(arrival.outbound_record_max(), Some(157));
    }

    #[test]
    fn a_floods_path_is_the_way_back_and_sets_the_outbound_budget() {
        let mut packet = record_flood(1, &[1; 32], &[2; 32], None, &[0; 4], b"sealed").unwrap();
        // Twenty hops of one byte each, as a receiver would see them.
        packet[1] = 20;
        let mut walked = packet[..2].to_vec();
        walked.extend_from_slice(&[9u8; 20]);
        walked.extend_from_slice(&packet[2..]);
        let arrival = classify(&push(&walked)).expect("a walked flood is a session record");
        assert!(arrival.flood);
        assert_eq!(arrival.hops(), 20);
        // 161 − 4 − 20.
        assert_eq!(arrival.outbound_record_max(), Some(137));
    }

    #[test]
    fn an_advert_is_not_a_session_record() {
        // `PAYLOAD_TYPE_ADVERT` with a plain payload: somebody else's business.
        let packet = vec![0x01 | (0x04 << 2), 0];
        assert_eq!(classify(&push(&packet)), None);
    }

    #[test]
    fn a_channel_message_is_not_a_session_record() {
        let packet = vec![0x01 | (0x05 << 2), 0, 0xAA, 0xBB];
        assert_eq!(classify(&push(&packet)), None);
    }

    #[test]
    fn another_operators_route_request_is_not_a_session_record() {
        // **The public mesh's own traffic, taken as ours.** A `REQ` flood is MeshCore's request
        // payload, and the air is full of them: every ldmux node's `LDRQ`, every advert, every
        // stranger's request. The wrapper's magic is the only thing that says a `REQ` is carrying a
        // session record, so a plain `REQ` must be refused rather than handed up as one.
        //
        // Measured 2026-11-13 on the phone: `received=25 admitted=0`, with `bad`, `dup`,
        // `wrongIdx`, `noSess` and `refusal` all zero. The frames were `LDRQ` floods.
        let mut packet = vec![0x01u8, 0x03, 0xE5, 0x20, 0x10];
        packet.extend_from_slice(b"EaLDRQa_C\x1f");
        assert_eq!(classify(&push(&packet)), None);
        // **A `0x84` push is different and must stay accepted.** The firmware only emits it for a
        // `RAW_CUSTOM` packet addressed to this client (`onRawDataRecv`), so the payload of one is a
        // record by construction: a foreign `REQ` flood is forwarded, never handed up this way.
        let raw = [0x84u8, 0x13, 0xAA, 0xFF, 0xE5, 0x20, b'L', b'D', b'R', b'Q', 0x01];
        assert_eq!(classify(&raw).map(|a| a.record), Some(raw[4..].to_vec()));
    }

    #[test]
    fn a_frame_that_is_not_a_receive_log_is_ignored() {
        assert_eq!(classify(&[0x83, 0, 1, 2]), None);
        assert_eq!(classify(&[]), None);
    }

    #[test]
    fn a_reserved_path_width_is_refused_rather_than_misread() {
        let mut packet = raw_direct(1, &[], b"sealed").unwrap();
        packet[1] = 0xC0; // width 4
        assert_eq!(classify(&push(&packet)), None);
    }
}
