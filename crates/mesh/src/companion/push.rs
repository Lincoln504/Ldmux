//! What one `0x88` receive-log frame means, for either end.
//!
//! The companion reports every packet it hears as `[0x88][snr × 4][rssi][packet]`
//! (`MyMesh::logRxRaw`, `examples/companion_radio/MyMesh.cpp:286-296`), and most of what a busy mesh
//! carries is none of our business: adverts, route requests, other people's channel traffic. What
//! this module does is decide, once, whether a push carries a session record — and if it does, hand
//! up the record **and the shape it arrived in**, because the endpoint's rules are about the shape:
//! a direct packet with hops left is in transit to somebody else, a flood's chain is the way back,
//! and only a direct arrival that finished its path proves anything (`RC-3`).
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

/// The four bytes that mark a flooded session record, inside the packet's payload.
///
/// Re-exported from the builder that writes it, so the reader and the writer cannot disagree.
pub use crate::companion::build::RECORD_FLOOD_MAGIC;

/// The push code of a receive-log frame (`PUSH_CODE_LOG_RX_DATA`).
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
}

impl PushArrival {
    /// How many hops the arrival describes.
    #[must_use]
    pub fn hops(&self) -> u8 {
        let width = usize::from(self.path_width.clamp(1, 3));
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
        let limit = crate::budget::RecordBudget::flood_scoped(self.path.len())
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

/// One `0x88` push, classified.
///
/// `None` means the push is not a session record — a route request, an advert, a stranger's channel
/// traffic — which the caller ignores. The record is returned with the readings that arrived with
/// it, so route state can be updated from an authenticated arrival and nothing else (`RC-3`).
#[must_use]
pub fn classify(frame: &[u8]) -> Option<PushArrival> {
    let [PUSH_LOG_RX_DATA, snr, rssi, ..] = *frame else {
        return None;
    };
    let view = wire(frame.get(3..)?)?;
    let body = view.payload;
    let record: &[u8] = match body.get(2..6) {
        Some(magic) if magic == RECORD_FLOOD_MAGIC => body.get(FLOOD_WRAPPER..)?,
        _ if view.payload_type == PAYLOAD_TYPE_RAW_CUSTOM => body,
        _ => return None,
    };
    if record.is_empty() {
        return None;
    }
    Some(PushArrival {
        record: record.to_vec(),
        flood: view.flood,
        path: view.path.to_vec(),
        path_width: view.path_width,
        snr_quarter_db: snr as i8,
        rssi_dbm: rssi as i8,
    })
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
