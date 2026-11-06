//! The host's own radio, on a serial port, as a carrier.
//!
//! This is the other side of `crates/mesh`'s [`Carrier`] seam. The gateway carrier
//! dials a separate process that owns the radio; this one owns the radio itself
//! through `crates/companion`, so the host needs no gateway, no socket, no token
//! and no second language to carry a byte.
//!
//! The companion protocol is synchronous and the daemon is tokio, so the radio
//! lives on one dedicated thread and talks to the loop over channels:
//!
//! ```text
//! session loop ── Command::Record(seq, bytes) ──▶ radio thread ──▶ serial
//! session loop ◀── Event::Recv / SendResult / Status ── radio thread ◀── serial
//! ```
//!
//! Two things are deliberately borrowed from the gateway's behaviour so the loop
//! above cannot tell the carriers apart:
//!
//! * a `Status` follows every send, carrying the budget that is left, because the
//!   loop treats an unknown budget as zero and would otherwise never send again;
//! * a refused send is a `SendResult`, not a carrier error, because the retry
//!   ladder has to give the slot back rather than tear the link down.
//!
//! The budget is computed from the preset the radio reports rather than assumed:
//! `36 s/hour / airtime(172 bytes)`, which is the 1 % self-imposed policy in
//! `docs/separation/05-RADIO-TUPLE.md`, not a limit the firmware enforces. The
//! firmware has its own governor; this number only keeps LDMUX's meter honest.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use ldmux_companion::{ByteLink, Companion, SerialLink, SessionError};
use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event};
use ldmux_mesh::constants::{RECORD_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI};
use ldmux_mesh::frame::MeshError;
use ldmux_mesh::messages::{SendResult, Status};
use tokio::sync::mpsc as async_mpsc;

/// How long the radio thread waits for a command before checking the air.
const COMMAND_WAIT: Duration = Duration::from_millis(10);

/// One second of the hour the budget is measured over.
const HOUR: Duration = Duration::from_secs(3_600);

/// The self-imposed airtime policy, in milliseconds per hour: one percent.
const AIRTIME_BUDGET_MS: f64 = 36_000.0;

/// The one command the loop has for the radio.
enum Command {
    Record { seq: u8, record: Vec<u8> },
}

/// The radio, seen as a carrier.
pub struct SerialCarrier {
    /// `None` once dropped, so the radio thread sees the channel close.
    commands: Option<Sender<Command>>,
    events: async_mpsc::Receiver<Result<Event, MeshError>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SerialCarrier {
    /// Open the device, identify the radio, and start serving it.
    pub async fn open(device: &str) -> Result<(Self, Bound), MeshError> {
        let path = device.to_owned();
        let prepared = tokio::task::spawn_blocking(move || prepare(&path))
            .await
            .map_err(|e| MeshError::Io(std::io::Error::other(e.to_string())))?;
        let (companion, bound, settings) = prepared?;

        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = async_mpsc::channel(64);
        let worker = std::thread::Builder::new()
            .name("ldmux-mesh-serial".to_owned())
            .spawn(move || serve(companion, settings, command_rx, event_tx))
            .map_err(MeshError::Io)?;
        Ok((
            Self {
                commands: Some(command_tx),
                events: event_rx,
                worker: Some(worker),
            },
            bound,
        ))
    }
}

impl Carrier for SerialCarrier {
    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
        let commands = self.commands.as_ref().ok_or(MeshError::Closed)?;
        commands
            .send(Command::Record {
                seq,
                record: record.to_vec(),
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

impl Drop for SerialCarrier {
    fn drop(&mut self) {
        // Dropping the sender disconnects the channel, which is the thread's cue to
        // stop; joining it is what guarantees the serial port is closed and its
        // exclusive lock released before a reconnect tries to open the same device.
        // The thread's longest wait is one `COMMAND_WAIT`, so this cannot stall.
        drop(self.commands.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// What the radio told us at open time, and what the loop needs from it.
#[derive(Clone, Copy)]
struct Settings {
    /// Bytes per path hash, from the firmware's reported path-hash mode.
    hash_size: u8,
    /// Time on air for a full record, from the preset the radio reports.
    airtime_ms: f64,
}

/// Open the port and identify the radio. Runs off the async runtime: it blocks.
fn prepare(device: &str) -> Result<(Companion<SerialLink>, Bound, Settings), MeshError> {
    let link = SerialLink::open(device).map_err(MeshError::Io)?;
    link.lock_exclusive().map_err(|e| {
        MeshError::Io(std::io::Error::other(format!(
            "{device}: another process holds this radio ({e})"
        )))
    })?;
    let mut companion = Companion::new(link);
    let self_info = companion.app_start("ldmux").map_err(session_error)?;
    // The version this sends is what the firmware latches to decide whether it
    // emits its V3 frames (MyMesh.cpp:1028-1031); `crates/companion` sends 3.
    let device_info = companion.device_query().map_err(session_error)?;
    let hash_size = device_info.path_hash_mode.map_or(1, |m| m.saturating_add(1)).clamp(1, 4);
    let airtime_ms = self_info.time_on_air_ms(RECORD_MAX);
    let bound = Bound {
        uplink_record_max: u8::try_from(RECORD_MAX).unwrap_or(172),
        radio_state: RadioState::Connected,
        // No gateway is in the path: this process owns the radio, and the firmware is the meter.
        budget: Budget::RadioGoverned,
    };
    Ok((companion, bound, Settings { hash_size, airtime_ms }))
}

/// The radio thread: serve commands and the air until the loop lets go.
fn serve<L: ByteLink>(
    mut companion: Companion<L>,
    settings: Settings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
) {
    let budget = budget_per_hour(settings.airtime_ms);
    let mut window = Instant::now();
    let mut sent: u32 = 0;

    // One status before anything else: the loop starts with no budget, and an
    // unknown budget is zero, so without this it would never send the first record.
    if events
        .blocking_send(Ok(Event::Status(status(&settings, budget, 0))))
        .is_err()
    {
        return;
    }

    loop {
        if window.elapsed() >= HOUR {
            window = Instant::now();
            sent = 0;
        }

        match commands.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Record { seq, record }) => {
                let outcome = match companion.send_raw(settings.hash_size, &[], &record) {
                    Ok(()) => {
                        sent = sent.saturating_add(1);
                        SendOutcome::SentDirect
                    }
                    Err(e) => refused(&e),
                };
                if events
                    .blocking_send(Ok(Event::SendResult(SendResult { seq, outcome })))
                    .is_err()
                {
                    return;
                }
                let left = budget.saturating_sub(u8::try_from(sent).unwrap_or(u8::MAX));
                if events
                    .blocking_send(Ok(Event::Status(status(&settings, left, sent))))
                    .is_err()
                {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = events.blocking_send(Err(MeshError::Closed));
                return;
            }
        }

        match companion.next_push(Duration::from_millis(1)) {
            Ok(Some(frame)) => {
                if let Some(record) = incoming_record(&frame)
                    && events.blocking_send(Ok(Event::Recv(record))).is_err()
                {
                    return;
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = events.blocking_send(Err(MeshError::Io(std::io::Error::other(
                    e.to_string(),
                ))));
                return;
            }
        }
    }
}

/// One received packet, reduced to the record inside it.
///
/// The guard is the header: only a packet LDMUX itself built (`0x3E`, DIRECT with
/// payload RAW_CUSTOM) can be a record. Everything else on the air — adverts,
/// trace routes, another network's channel traffic — is not ours and is ignored
/// rather than handed to the session, which would have to reject it and would show
/// the rejection as a defect.
fn incoming_record(frame: &[u8]) -> Option<Vec<u8>> {
    const PUSH_LOG_RX: u8 = 0x88;
    const RAW_CUSTOM_DIRECT: u8 = 0x3E;

    if frame.first() != Some(&PUSH_LOG_RX) || frame.len() < 5 {
        return None;
    }
    // `MyMesh.cpp:286-296`: `[0x88][snr*4][rssi][packet]`. The packet starts at 3.
    let packet = &frame[3..];
    if packet.len() < 2 || packet[0] != RAW_CUSTOM_DIRECT {
        return None;
    }
    let mut offset = 1;
    // Transport-coded routes carry a four-byte region code before the path byte.
    let route = packet[0] & 0x03;
    if route == 0 || route == 3 {
        offset += 4;
    }
    let path_byte = *packet.get(offset)?;
    offset += 1;
    let hash = usize::from((path_byte >> 6) + 1); // the two bits encode 1..=4
    let hops = usize::from(path_byte & 0x3F);
    let body = offset.checked_add(hops.saturating_mul(hash))?;
    if body > packet.len() {
        return None;
    }
    Some(packet[body..].to_vec())
}

/// A carrier-level failure, as the loop's error type.
fn session_error(e: SessionError) -> MeshError {
    MeshError::Io(std::io::Error::other(e.to_string()))
}

/// A refused send, in the vocabulary the retry ladder understands.
fn refused(e: &SessionError) -> SendOutcome {
    match e {
        SessionError::Rejected { code, .. } => match code {
            2 => SendOutcome::ErrNotFound,
            6 => SendOutcome::ErrTooLong,
            _ => SendOutcome::ErrBusy,
        },
        _ => SendOutcome::ErrBusy,
    }
}

/// Records of the full size that fit a one-percent airtime budget in an hour.
fn budget_per_hour(airtime_ms: f64) -> u8 {
    if airtime_ms <= 0.0 {
        return u8::MAX;
    }
    // The contract carries the budget in one byte, so anything above 255 is
    // reported as 255. On this bench's preset the true figure is about 261 a hour
    // and the cap is what the wire can say, not what the policy allows.
    let n = (AIRTIME_BUDGET_MS / airtime_ms).floor();
    u8::try_from(n as u64).unwrap_or(u8::MAX).max(1)
}

/// The status a serial radio actually knows: no RSSI or battery reading, and a
/// budget computed from its own preset rather than reported by a gateway.
fn status(settings: &Settings, left: u8, sent: u32) -> Status {
    let tenths = (f64::from(sent) * settings.airtime_ms / 100.0).round();
    Status {
        radio_state: RadioState::Connected,
        rssi: UNKNOWN_RSSI,
        battery_percent: UNKNOWN_BATTERY,
        tx_airtime_tenths_this_hour: u16::try_from(tenths as u64).unwrap_or(u16::MAX),
        ops_left_this_hour: Some(left),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_companion::{MemoryLink, build_raw_direct};

    fn log_frame(packet: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x88, 0xE0, 0xA1];
        frame.extend_from_slice(packet);
        frame
    }

    #[test]
    fn a_direct_raw_packet_yields_its_record() {
        // What `build_raw_direct(1, &[], record)` produces: header, zero-hop path byte.
        let packet = build_raw_direct(1, &[], &[9, 8, 7]).unwrap();
        let record = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(record, vec![9, 8, 7]);
    }

    #[test]
    fn a_two_byte_hash_path_is_skipped_before_the_record() {
        let packet = build_raw_direct(2, &[0xAA, 0xBB], &[1, 2]).unwrap();
        let record = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(record, vec![1, 2]);
    }

    #[test]
    fn another_protocols_packet_is_not_a_record() {
        // An advert: DIRECT route, payload type 4, version 0 -> 0x12.
        let advert = log_frame(&[0x12, 0x00, 0x01, 0x02]);
        assert_eq!(incoming_record(&advert), None);
    }

    #[test]
    fn a_frame_that_is_not_a_log_push_is_not_a_record() {
        assert_eq!(incoming_record(&[0x05, 0x01, 0x02, 0x03, 0x04]), None);
    }

    #[test]
    fn a_truncated_path_is_refused_rather_than_sliced_wrongly() {
        // Path byte says eight hops of two bytes; the packet has four bytes.
        let packet = vec![0x3E, 0b01_001000, 0x01, 0x02];
        assert_eq!(incoming_record(&log_frame(&packet)), None);
    }

    #[test]
    fn the_budget_comes_from_the_preset_not_a_constant() {
        // 914.25 MHz / 500 kHz / SF8: about 138 ms for 172 bytes -> about 261 an hour,
        // which the one-byte wire field reports as its ceiling.
        let fast = budget_per_hour(137.9);
        assert_eq!(fast, u8::MAX);
        // Twice the airtime is half the records, and it fits below the cap.
        assert!(budget_per_hour(275.8) < fast);
    }

    #[test]
    fn a_refusal_maps_to_the_outcome_the_retry_ladder_expects() {
        assert_eq!(
            refused(&SessionError::Rejected { code: 2, meaning: "NOT_FOUND" }),
            SendOutcome::ErrNotFound
        );
        assert_eq!(
            refused(&SessionError::Rejected { code: 6, meaning: "ILLEGAL_ARG" }),
            SendOutcome::ErrTooLong
        );
        assert_eq!(
            refused(&SessionError::Rejected { code: 4, meaning: "BAD_STATE" }),
            SendOutcome::ErrBusy
        );
    }

    #[test]
    fn a_record_is_sent_and_its_answer_and_budget_are_reported() {
        let mut link = MemoryLink::new();
        link.push_payload(&[0x00]); // the radio accepts the raw send
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings { hash_size: 1, airtime_ms: 137.9 },
                command_rx,
                event_tx,
            );
        });

        command_tx
            .send(Command::Record { seq: 7, record: vec![1, 2, 3] })
            .unwrap();
        // A status first: the loop starts with no budget, and unknown is zero.
        match event_rx.blocking_recv() {
            Some(Ok(Event::Status(s))) => assert_eq!(s.ops_left_this_hour, Some(u8::MAX)),
            other => panic!("expected an opening status, got {other:?}"),
        }
        match event_rx.blocking_recv() {
            Some(Ok(Event::SendResult(r))) => {
                assert_eq!(r.seq, 7);
                assert_eq!(r.outcome, SendOutcome::SentDirect);
            }
            other => panic!("expected a send result, got {other:?}"),
        }
        match event_rx.blocking_recv() {
            Some(Ok(Event::Status(s))) => assert_eq!(
                s.ops_left_this_hour,
                Some(u8::MAX - 1),
                "one record spent one of the hour's budget"
            ),
            other => panic!("expected a status after the send, got {other:?}"),
        }

        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_received_packet_reaches_the_loop_as_its_record() {
        let packet = build_raw_direct(1, &[], &[9, 8, 7]).unwrap();
        let mut link = MemoryLink::new();
        link.push_payload(&log_frame(&packet)); // framed as the radio frames it
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings { hash_size: 1, airtime_ms: 137.9 },
                command_rx,
                event_tx,
            );
        });

        // The opening status comes first; the record follows on the next turn of
        // the loop, without any command being sent.
        let mut record = None;
        for _ in 0..4 {
            match event_rx.blocking_recv() {
                Some(Ok(Event::Recv(r))) => {
                    record = Some(r);
                    break;
                }
                Some(Ok(_)) => continue,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(record, Some(vec![9, 8, 7]));

        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn letting_go_of_the_carrier_ends_the_radio_thread() {
        let companion = Companion::new(MemoryLink::new());
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings { hash_size: 1, airtime_ms: 137.9 },
                command_rx,
                event_tx,
            );
        });
        let _ = event_rx.blocking_recv(); // the opening status
        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_status_carries_a_budget_so_the_loop_can_send() {
        let settings = Settings { hash_size: 1, airtime_ms: 137.9 };
        let status = status(&settings, 255, 0);
        assert_eq!(status.ops_left_this_hour, Some(255));
        assert_eq!(status.radio_state, RadioState::Connected);
    }
}
