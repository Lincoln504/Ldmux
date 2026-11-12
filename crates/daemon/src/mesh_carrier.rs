//! The companion radio, seen as a [`CarriageSink`] the endpoint can drive.
//!
//! `mesh_serial::SerialCarrier` is the carrier the **old** loop drives, and it decides the carriage
//! itself: it keeps the peer's path, decides when a record floods, and reports what it chose. That
//! is the inversion `docs/RADIO-PLAN.md` §1 `A1` removes. `RadioEndpoint` decides per attempt —
//! direct along a known path, flood otherwise — and tells the carrier which, immediately before
//! handing it the record (`CarriageSink::set_carriage` then `Carrier::send_record`).
//!
//! This module is that telling, and nothing else:
//!
//! * **It builds the packet the carriage names**, with the two builders the phone also uses
//!   (`build_raw_direct`, `build_record_flood_in_scope`), so a record is byte-identical whichever
//!   end composed it.
//! * **It classifies what the radio pushes** — a `0x88` receive log into one record plus the
//!   readings that arrived with it — and never guesses a route from it. Route state is the
//!   endpoint's, and it is updated only after the record authenticates (`RC-3`).
//! * **It owns no policy.** No path memory, no flood ladder, no budget, no retries: those are the
//!   endpoint's, and a second copy of them here is how the old stack came to have two answers to
//!   every question.
//!
//! The blocking port lives on its own thread, as `mesh_serial` does, because `Companion` is a
//! blocking API and a serial read inside a tokio task would block a worker for the duration of a
//! radio timeout.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use ldmux_companion::codec::{
    build_raw_direct, build_record_flood_in_scope, parse_packet, AdvertPath,
};
use ldmux_companion::{ByteLink, Companion, FloodScope, SessionError};
use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event, Signal};
use ldmux_mesh::constants::{RECORD_MAX, RECORD_RECEIVED_MAX, RadioState, SendOutcome};
use ldmux_mesh::error::MeshError;
use ldmux_mesh::messages::SendResult;
use ldmux_mesh::route::Carriage;
use ldmux_session::endpoint::CarriageSink;
use tokio::sync::mpsc as async_mpsc;

/// How long the radio thread waits for a command before looking at the air.
///
/// A received frame must not wait behind a command, and a command must not wait behind a poll:
/// ten milliseconds is shorter than one packet's airtime at every preset, so neither side can
/// starve the other by more than a fraction of a transmission.
const COMMAND_WAIT: Duration = Duration::from_millis(10);

/// How long one record's send may wait for the firmware's acknowledgement.
///
/// The firmware answers from its own queue, not from the air, so this is a bound on a wedged port
/// rather than on a slow link. Ten seconds is the value the existing carrier uses for the same
/// command; a refused send is reported as a `SendResult` and the endpoint's ladder decides what to
/// do, so nothing here retries.
const SEND_WAIT: Duration = Duration::from_secs(10);

/// The payload-type byte of a `RAW_CUSTOM` packet: the firmware's own numbering.
const PAYLOAD_RAW_CUSTOM: u8 = 0x0F;

/// What the carrier needs to compose a packet, fixed for the life of a link.
#[derive(Clone)]
pub struct CarrierSettings {
    /// Bytes per path hash, from the firmware's reported mode (1..=3).
    pub hash_size: u8,
    /// This radio's public key, the flood's source hash.
    pub own_key: [u8; 32],
    /// The peer radio's public key, the flood's destination hash.
    pub peer_key: [u8; 32],
    /// The region this radio's floods are proved with, when it has one.
    pub scope: Option<FloodScope>,
    /// One record's airtime at this tuple, for the session's timers.
    pub airtime_ms: Option<f64>,
}

/// Compose the packet a carriage names.
///
/// Pure, so both ends and the tests build the same bytes: a `Direct` packet is
/// `[header][path_len][path][record]` and a `Flood` is the `LDMR` wrapper around the same record
/// with a fresh nonce, because MeshCore dedupes on `SHA256(payload_type ‖ payload)` and a
/// retransmission that reused its bytes would never leave the first repeater.
///
/// # Errors
///
/// [`MeshError::Unsupported`] when the record does not fit the carriage: 174 bytes of packet is the
/// firmware's frame ceiling, and an oversized record is refused here rather than transmitted and
/// silently dropped at the receiver (`RC-6`).
pub fn build_packet(
    settings: &CarrierSettings,
    carriage: &Carriage,
    record: &[u8],
    nonce: &[u8; 4],
) -> Result<Vec<u8>, MeshError> {
    let built = match carriage {
        Carriage::Direct { path } => {
            build_raw_direct(settings.hash_size, path, record)
        }
        Carriage::Flood => build_record_flood_in_scope(
            settings.hash_size,
            &settings.peer_key,
            &settings.own_key,
            settings.scope.as_ref(),
            nonce,
            record,
        ),
    };
    built.map_err(|e| MeshError::Unsupported(format!("the packet does not fit: {e}")))
}

/// One `0x88` push, classified into what the endpoint can act on.
///
/// Returns the events in the order the caller must apply them: the record first (so route state is
/// updated from an authenticated arrival) and the readings after it. `None` means the push is not a
/// session record — a route request, an advert, a stranger's channel traffic — which is the
/// carrier's business to ignore and nobody else's.
///
/// The log frame is `[0x88][snr × 4][rssi][packet]` (`MyMesh.cpp:286-296`), and the record is
/// either a `RAW_CUSTOM` packet's payload or the tail of an `LDMR` flood after its six bytes of
/// `[dest][source][magic][nonce]`.
#[must_use]
pub fn classify_push(frame: &[u8], settings: &CarrierSettings) -> Option<(Vec<u8>, Vec<Event>)> {
    let (snr_quarter, rssi) = match frame {
        [0x88, snr, rssi, ..] => (*snr, *rssi),
        _ => return None,
    };
    let packet = frame.get(3..)?;
    let view = parse_packet(packet)?;
    // **The wrapper's magic decides, not the payload type.** A direct session record is a
    // `RAW_CUSTOM` packet; a flooded one rides `REQ` with the `LDMR` wrapper inside it (the firmware
    // flood-routes `REQ` and never `RAW_CUSTOM`). Reading only the payload type would therefore
    // ignore every flood — which is exactly the class of silent loss this module exists to end.
    let body = view.payload;
    let record: &[u8] = match body.get(2..6) {
        Some(magic) if magic == ldmux_companion::codec::RECORD_FLOOD_MAGIC => body.get(10..)?,
        _ if view.payload_type == PAYLOAD_RAW_CUSTOM => body,
        _ => return None,
    };
    if record.is_empty() {
        return None;
    }
    let hops = view.path.hops;
    let events = vec![
        Event::Recv(record.to_vec()),
        Event::Signal(Signal {
            noise_floor_dbm: 0,
            rssi_dbm: Some(rssi as i8),
            // The firmware reports quarters of a dB, which is the unit `Signal` keeps.
            snr_quarter_db: snr_quarter as i8,
            tx_air_secs: 0,
            rx_air_secs: 0,
            from_peer: true,
        }),
        Event::Limit {
            record_max: record_ceiling(settings, view.path.clone()),
            hops,
        },
    ];
    Some((record.to_vec(), events))
}

/// The largest record this arrival's path allows, from the one budget the whole tree uses.
///
/// A direct arrival has had its path consumed, so it says nothing about the outbound path and the
/// outbound limit stays what the endpoint learned; a flood's chain *is* the way back, so its length
/// is what the sender may assume.
fn record_ceiling(settings: &CarrierSettings, path: AdvertPath) -> u8 {
    // A reply follows the reverse of the chain this record arrived by, so the chain is what the
    // outbound packet must carry. The receive ceiling caps it from above: a record larger than 171
    // bytes is never reported upward at all, whatever the path (`MyMesh::logRxRaw`).
    let path_bytes = usize::from(path.hops) * usize::from(settings.hash_size.clamp(1, 3));
    let limit = RECORD_MAX.saturating_sub(path_bytes).min(RECORD_RECEIVED_MAX);
    u8::try_from(limit).unwrap_or(u8::try_from(RECORD_RECEIVED_MAX).unwrap_or(171))
}

/// The radio, driven by an endpoint that names each record's carriage.
pub struct RadioCarrier {
    commands: Sender<Command>,
    events: async_mpsc::Receiver<Result<Event, MeshError>>,
    /// The carriage the endpoint named for the next record, consumed by the send.
    ///
    /// It is `Option` and it is taken rather than read: a send whose carriage was never named is a
    /// bug in the caller, and guessing (flood, say) would spend twenty times the airtime on a
    /// mistake nobody would see.
    next_carriage: Option<Carriage>,
}

/// The one thing the loop can be told to do besides send.
enum Command {
    /// Transmit this record on this carriage.
    Send {
        seq: u8,
        record: Vec<u8>,
        carriage: Carriage,
    },
}

impl RadioCarrier {
    /// Start serving an already-open companion session.
    ///
    /// The thread owns the port for the life of the carrier; when every sender is dropped the
    /// thread finishes its current command and returns, which closes the port and releases the
    /// `flock`. The handle is detached on purpose: a teardown that waits is a teardown that can
    /// hang, and the port's own lock is what makes a late close harmless.
    pub fn spawn<L: ByteLink + Send + 'static>(
        companion: Companion<L>,
        settings: CarrierSettings,
    ) -> (Self, Bound) {
        let bound = Bound {
            uplink_record_max: u8::try_from(RECORD_MAX).unwrap_or(171),
            radio_state: RadioState::Connected,
            budget: Budget::RadioGoverned,
            airtime_ms: settings.airtime_ms,
        };
        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = async_mpsc::channel(64);
        std::thread::Builder::new()
            .name("ldmux-mesh-carrier".to_owned())
            .spawn(move || serve(companion, settings, command_rx, event_tx))
            .expect("a radio thread can be spawned");
        (
            Self {
                commands: command_tx,
                events: event_rx,
                next_carriage: None,
            },
            bound,
        )
    }
}

impl Carrier for RadioCarrier {
    /// Hand one record to the radio, on the carriage [`CarriageSink::set_carriage`] named.
    ///
    /// The carriage travels with the command rather than being kept as a field, because the two
    /// calls are one decision: an endpoint that set a carriage and then failed to send must not
    /// leave the carrier believing the next record is a flood.
    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
        // The endpoint always sets the carriage immediately before this call; the field is what
        // that call left behind.
        let carriage = self.next_carriage.take().ok_or_else(|| {
            MeshError::Unsupported(
                "the record's carriage was never named; an endpoint must call set_carriage first"
                    .to_owned(),
            )
        })?;
        self.commands
            .send(Command::Send {
                seq,
                record: record.to_vec(),
                carriage,
            })
            .map_err(|_| MeshError::Closed)
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        match self.events.recv().await {
            Some(result) => result,
            None => Err(MeshError::Closed),
        }
    }
}

impl CarriageSink for RadioCarrier {
    async fn set_carriage(&mut self, carriage: &Carriage) -> Result<(), MeshError> {
        self.next_carriage = Some(carriage.clone());
        Ok(())
    }
}

/// The radio thread: harvest the air, then serve one command, for ever.
fn serve<L: ByteLink>(
    mut companion: Companion<L>,
    settings: CarrierSettings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
) {
    loop {
        // **The air first.** A received frame already queued on the port would otherwise wait
        // behind this thread's own send, which is the defect that made a stats poll cost a packet.
        // A closed event channel means every caller has gone: the port is done with.
        if harvest(&mut companion, &settings, &events).is_err() {
            return;
        }
        match commands.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Send {
                seq,
                record,
                carriage,
            }) => {
                let outcome = transmit(&mut companion, &settings, &carriage, &record);
                if events
                    .blocking_send(Ok(Event::SendResult(SendResult { seq, outcome })))
                    .is_err()
                {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Read everything the radio has pushed, and hand it on.
fn harvest<L: ByteLink>(
    companion: &mut Companion<L>,
    settings: &CarrierSettings,
    events: &async_mpsc::Sender<Result<Event, MeshError>>,
) -> Result<(), ()> {
    loop {
        let pushed = match companion.next_push(Duration::from_millis(0)) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(e) => {
                let _ = events.blocking_send(Err(MeshError::Io(std::io::Error::other(
                    e.to_string(),
                ))));
                return Ok(());
            }
        };
        let Some((_record, classified)) = classify_push(&pushed, settings) else {
            continue;
        };
        for event in classified {
            if events.blocking_send(Ok(event)).is_err() {
                return Err(());
            }
        }
    }
}

/// One send, on the carriage the endpoint chose.
fn transmit<L: ByteLink>(
    companion: &mut Companion<L>,
    settings: &CarrierSettings,
    carriage: &Carriage,
    record: &[u8],
) -> SendOutcome {
    let nonce = fresh_nonce();
    let packet = match build_packet(settings, carriage, record, &nonce) {
        Ok(packet) => packet,
        Err(_) => return SendOutcome::ErrTooLong,
    };
    match companion.send_packet_within(&packet, SEND_WAIT) {
        Ok(()) => match carriage {
            Carriage::Direct { .. } => SendOutcome::SentDirect,
            Carriage::Flood => SendOutcome::SentFlood,
        },
        Err(SessionError::Rejected { code: 2, .. }) => SendOutcome::ErrNotFound,
        Err(SessionError::Rejected { code: 4, .. }) => SendOutcome::ErrBusy,
        Err(SessionError::Rejected { code: 6, .. }) => SendOutcome::ErrTooLong,
        Err(SessionError::Disabled) => SendOutcome::ErrBusy,
        Err(_) => SendOutcome::LocalTimeout,
    }
}

/// A fresh four-byte nonce for one flood.
///
/// It exists only so that a retransmission is a different packet: MeshCore's duplicate ring hashes
/// `payload_type ‖ payload`, so identical bytes never leave the first repeater. It carries no
/// security and needs none — the record inside is sealed by the session.
fn fresh_nonce() -> [u8; 4] {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut out = [0u8; 4];
    out[..4].copy_from_slice(&now.to_le_bytes());
    out[0] ^= pid as u8;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> CarrierSettings {
        CarrierSettings {
            hash_size: 1,
            own_key: [0x11; 32],
            peer_key: [0x22; 32],
            scope: None,
            airtime_ms: Some(602.6),
        }
    }

    /// A direct packet is `[header][path][record]` with the path the endpoint named — the bytes the
    /// firmware parses with `tryParsePacket` and queues without looking at the payload.
    #[test]
    fn a_direct_carriage_builds_the_path_the_endpoint_named() {
        let record = b"sealed-record";
        let path = vec![0xA1u8, 0xB2, 0xC3];
        let packet = build_packet(
            &settings(),
            &Carriage::Direct { path: path.clone() },
            record,
            &[0; 4],
        )
        .expect("a direct packet fits");
        let view = parse_packet(&packet).expect("the packet parses");
        assert_eq!(view.payload_type, PAYLOAD_RAW_CUSTOM);
        assert_eq!(view.path.bytes, path, "the path the endpoint chose is the path sent");
        assert_eq!(view.path.hops, 3);
        assert_eq!(view.payload, record);
    }

    /// A flood is the same record inside the `LDMR` wrapper, and a changed nonce is a changed packet
    /// — which is the only reason a retransmission survives MeshCore's deduplication.
    #[test]
    fn a_flood_carriage_wraps_the_record_and_a_retry_is_a_different_packet() {
        let record = b"sealed-record";
        let first = build_packet(&settings(), &Carriage::Flood, record, &[1, 2, 3, 4])
            .expect("a flood fits");
        let second = build_packet(&settings(), &Carriage::Flood, record, &[5, 6, 7, 8])
            .expect("a flood fits");
        assert_ne!(first, second, "a retry reused its bytes and would be dropped");
        let view = parse_packet(&first).expect("the packet parses");
        assert_eq!(
            view.route, 1,
            "a flood must take the flood route, or no repeater forwards it"
        );
        assert_eq!(
            &view.payload[2..6],
            ldmux_companion::codec::RECORD_FLOOD_MAGIC,
            "a flood is told from a direct record by its magic, not by its payload type"
        );
        assert_eq!(&view.payload[10..], record);
    }

    /// A record too large for the frame is refused here, not transmitted and silently dropped at the
    /// far end (`RC-6`: the receiver's `logRxRaw` ceiling).
    #[test]
    fn a_record_past_the_frame_ceiling_is_refused_rather_than_sent() {
        let record = vec![0u8; 200];
        let refused = build_packet(&settings(), &Carriage::Flood, &record, &[0; 4]);
        assert!(
            matches!(refused, Err(MeshError::Unsupported(_))),
            "a 200-byte record was composed: {refused:?}"
        );
    }

    /// The receive log becomes the record and the readings that arrived with it, in that order.
    #[test]
    fn a_receive_log_push_becomes_one_record_and_its_readings() {
        let record = b"sealed";
        let inner = build_packet(
            &settings(),
            &Carriage::Direct { path: vec![0x33] },
            record,
            &[0; 4],
        )
        .expect("a packet");
        // `[0x88][snr x4][rssi][packet]`, SNR as -3.25 dB and RSSI as -101 dBm.
        let mut frame = vec![0x88, 0xF3, 0x9B];
        frame.extend_from_slice(&inner);
        let (parsed, events) = classify_push(&frame, &settings()).expect("a session record");
        assert_eq!(parsed, record);
        assert!(
            matches!(&events[0], Event::Recv(r) if r == record),
            "the record is not the first event: {events:?}"
        );
        assert!(
            matches!(&events[1], Event::Signal(s) if s.snr_quarter_db == -13 && s.rssi_dbm == Some(-101)),
            "the readings did not come with the record: {events:?}"
        );
        assert!(
            matches!(&events[2], Event::Limit { hops: 1, .. }),
            "the hop count the record arrived by was not reported: {events:?}"
        );
    }

    /// The flood wrapper is recognised from the receive log too, which is the shape that reaches this
    /// radio in the field: a session record that arrived by flood rides `REQ`, not `RAW_CUSTOM`.
    #[test]
    fn a_flooded_record_arriving_by_flood_is_classified_as_a_record() {
        let record = b"sealed-flood";
        let inner = build_packet(&settings(), &Carriage::Flood, record, &[9, 9, 9, 9])
            .expect("a flood fits");
        let mut frame = vec![0x88, 0x10, 0x9C];
        frame.extend_from_slice(&inner);
        let (parsed, _events) = classify_push(&frame, &settings()).expect("a flooded record");
        assert_eq!(parsed, record);
    }

    /// A packet that is not a session record is ignored rather than mistaken for one: an advert or a
    /// stranger's channel traffic must never reach the session.
    #[test]
    fn a_push_that_is_not_a_session_record_is_ignored() {
        let mut advert = vec![0x04u8]; // ADVERT payload type, flood route
        advert.push(0); // path: zero hops
        advert.extend_from_slice(&[0u8; 100]);
        let mut frame = vec![0x88, 0x00, 0x80];
        frame.extend_from_slice(&advert);
        assert!(
            classify_push(&frame, &settings()).is_none(),
            "an advert was handed to the session as a record"
        );
        assert!(
            classify_push(&[0x83, 0, 0], &settings()).is_none(),
            "a non-receive-log frame was handed to the session"
        );
    }
}
