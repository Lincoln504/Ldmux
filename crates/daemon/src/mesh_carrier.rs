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

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use ldmux_companion::codec::{build_raw_direct, build_record_flood_in_scope};
use ldmux_companion::{ByteLink, Companion, FloodScope, SessionError};
use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event, Signal};
use ldmux_mesh::constants::{
    RECORD_MAX, RECORD_RECEIVED_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI,
};
use ldmux_mesh::messages::Status;
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

/// How often the host asks its own radio what it has measured.
///
/// **A local command, so it costs no airtime** — only a moment of the one answer window. Fifteen
/// seconds is the phone's cadence, and the two together are what makes "is this link reaching" a
/// measurement on both ends rather than a guess on one.
const STATS_POLL: Duration = Duration::from_secs(15);

/// How long one record's send may wait for the firmware's acknowledgement.
///
/// The firmware answers from its own queue, not from the air, so this is a bound on a wedged port
/// rather than on a slow link. Ten seconds is the value the existing carrier uses for the same
/// command; a refused send is reported as a `SendResult` and the endpoint's ladder decides what to
/// do, so nothing here retries.
const SEND_WAIT: Duration = Duration::from_secs(10);

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
/// **One classifier, both ends.** This function used to be a host-only second copy: it accepted only
/// the `0x88` receive log, only `RAW_CUSTOM` bare payloads, and nothing else. The phone has always
/// read its pushes through [`ldmux_mesh::companion::push::classify`], so the two ends disagreed
/// about what a session record even is, and only one of them was ever tested against the other. A
/// contract with two owners and no cross-boundary test is the defect pattern the radio audit records
/// as P4; measured 2026-11-13, the host's radio handed this thread 28 pushes in 45 s and every one
/// was refused while `mesh status` reported `received 0 bytes`.
#[must_use]
pub fn classify_push(
    frame: &[u8],
    settings: &CarrierSettings,
) -> Option<(Vec<u8>, Vec<Event>, Option<u8>)> {
    let arrival = ldmux_mesh::companion::push::classify(frame)?;
    // A direct arrival states no path and says nothing about the way back; a flood's chain is the
    // way back and is what the reply's budget has to carry. `hops` is what the ceiling is priced in,
    // and it is measured the same way on both ends.
    let hops = arrival.hops();
    let events = vec![
        Event::Recv {
            record: arrival.record.clone(),
            path: arrival.path.clone(),
            flood: arrival.flood,
            snr_db: arrival.snr_db(),
        },
        Event::Signal(Signal {
            noise_floor_dbm: 0,
            rssi_dbm: Some(arrival.rssi_dbm),
            // The firmware reports quarters of a dB, which is the unit `Signal` keeps.
            snr_quarter_db: arrival.snr_quarter_db,
            tx_air_secs: 0,
            rx_air_secs: 0,
            from_peer: true,
        }),
        Event::Limit {
            record_max: record_ceiling(settings, hops),
            hops,
        },
    ];
    Some((arrival.record, events, arrival.sender))
}

/// The largest record this arrival's path allows, from the one budget the whole tree uses.
///
/// A direct arrival has had its path consumed, so it says nothing about the outbound path and the
/// outbound limit stays what the endpoint learned; a flood's chain *is* the way back, so its length
/// is what the sender may assume.
fn record_ceiling(settings: &CarrierSettings, hops: u8) -> u8 {
    // A reply follows the reverse of the chain this record arrived by, so the chain is what the
    // outbound packet must carry. The receive ceiling caps it from above: a record larger than 171
    // bytes is never reported upward at all, whatever the path (`MyMesh::logRxRaw`).
    let path_bytes = usize::from(hops) * usize::from(settings.hash_size.clamp(1, 3));
    let limit = RECORD_MAX.saturating_sub(path_bytes).min(RECORD_RECEIVED_MAX);
    u8::try_from(limit).unwrap_or(u8::try_from(RECORD_RECEIVED_MAX).unwrap_or(171))
}

/// The radio, driven by an endpoint that names each record's carriage.
/// What the radio itself is set to, and what this hour has cost.
///
/// The carrier is the accountant: it counts what it actually transmitted, retransmissions and acks
/// included, and publishes the remainder. A second count anywhere else charges the same record
/// twice, which is the stall of 2026-11-06 in reverse.
struct Radio {
    settings: CarrierSettings,
    /// The firmware's client-repeat flag, carried into every preset write.
    ///
    /// `CMD_SET_RADIO_PARAMS`'s sixth byte is persisted as `setRepeatEn(repeat != 0)`, so a frame
    /// that stops at the coding rate silently clears an operator's repeat setting. `None` leaves the
    /// stored value alone rather than guessing.
    repeat_enabled: Option<bool>,
    /// The tuple, kept so a retune can recompute the airtime estimate and the allowance.
    freq_mhz: f64,
    bw_khz: f64,
    sf: u8,
    cr: u8,
    tx_power_dbm: i8,
    /// Records this hour's allowance permits, and how many have gone.
    budget: Option<u32>,
    sent: u32,
    /// The hour the count belongs to, and where it is remembered across a restart.
    window_start: u64,
    window_record: Option<PathBuf>,
    /// Where the peer's radio key is remembered.
    peer_record: Option<PathBuf>,
    /// The road a route reply taught, which is asked for while it is absent.
    learned_route: Option<(Vec<u8>, u8)>,
    /// Route requests answered, so the log can say the mechanism ran.
    route_replies: u32,
    /// **What the port handed over, and what this end could not turn into a record.** A radio that
    /// hears the air and a link that receives nothing are the same picture from `mesh status`, and
    /// they are not the same fault: one is the peer never transmitting, the other is the port or the
    /// classifier dropping it. `refused_code` is the first byte of the last frame `classify_push`
    /// would not take, so the shape of what arrived is visible without a packet log.
    pushed: u32,
    refused: u32,
    refused_code: u8,
    refused_len: u16,
    /// Frames whose bytes contain the `LDMR` marker anywhere: a flooded session record, whether or
    /// not this end can decode the frame around it. `ldmr > 0` with `bytes_received == 0` means the
    /// peer's records are arriving and being lost here; `ldmr == 0` means the air never brought one.
    ldmr: u32,
    /// Frames the packet header calls `RAW_CUSTOM` (`0x0F`): the direct carrier.
    raw_custom: u32,
    /// **What the air is carrying, by payload type.** A radio that hears the public mesh and a
    /// radio that hears nothing are different faults, and so are "the mesh is busy with channel
    /// traffic" and "the mesh is quiet". The firmware's own numbering (`src/Packet.h`): 0 `REQ`,
    /// 2 `TXT_MSG`, 4 `ADVERT`, 5 `GRP_TXT`, 8 `PATH`, 9 `TRACE`, 11 `CONTROL`, 15 `RAW_CUSTOM`.
    by_type: [u32; 16],
    /// The radio's own last measurement, from `GET_STATS radio`.
    ///
    /// **Absent until the radio answers, and absent is not zero.** The status used to print a
    /// `0 dBm` noise floor and `0 s` of airtime on every record arrival, because nothing ever asked
    /// the radio: on 2026-11-12 the CLI described a link as "over a 0 dBm noise floor", which is
    /// physically impossible, and reported 0 s transmitted after an hour of transmissions.
    measured: Option<ldmux_companion::codec::RadioStats>,
    /// The battery and contact store, when a caller asks for them.
    ///
    /// Kept separate from [`Self::measured`] because the firmware reports a **voltage**, not a
    /// percentage, and a percentage invented from millivolts would be a number this end made up.
    battery: Option<ldmux_companion::codec::BatteryAndStorage>,
}

impl Radio {
    /// The status this radio actually knows: no RSSI or battery reading, and a budget computed from
    /// its own preset rather than reported by a gateway.
    fn status(&self) -> Status {
        let left = self
            .budget
            .map(|b| u8::try_from(b.saturating_sub(self.sent)).unwrap_or(u8::MAX));
        // **The radio's own airtime when it has been measured**, and this end's count otherwise:
        // a meter the radio reports is the ground truth, and ldmux's count is an estimate of
        // records × airtime that only exists because a radio may not answer.
        let tenths = match self.measured {
            Some(stats) => f64::from(stats.tx_air_secs) * 10.0,
            None => {
                (f64::from(self.sent) * self.settings.airtime_ms.unwrap_or(0.0) / 100.0).round()
            }
        };
        Status {
            radio_state: RadioState::Connected,
            rssi: self.measured.map_or(UNKNOWN_RSSI, |m| m.last_rssi),
            // **The firmware reports no percentage**, only a voltage; a battery reading invented
            // from millivolts would be a number this end made up, so the field stays unknown.
            battery_percent: self.battery.map_or(UNKNOWN_BATTERY, |_| UNKNOWN_BATTERY),
            tx_airtime_tenths_this_hour: u16::try_from(tenths as u64).unwrap_or(u16::MAX),
            ops_left_this_hour: left,
        }
    }

    /// Publish the radio's own measurement as the link's reading.
    ///
    /// `from_peer` is false: this is the host's own radio reporting what *it* heard, which is the
    /// only evidence that separates "the record is malformed" from "the radio is deaf".
    fn report_measured(&self) -> Option<Event> {
        let stats = self.measured?;
        Some(Event::Signal(Signal {
            noise_floor_dbm: stats.noise_floor_dbm,
            rssi_dbm: Some(stats.last_rssi),
            snr_quarter_db: stats.last_snr_quarter_db,
            tx_air_secs: stats.tx_air_secs,
            rx_air_secs: stats.rx_air_secs,
            from_peer: false,
        }))
    }

    /// Charge one transmitted record, and remember the hour's spend.
    fn charge(&mut self) {
        self.sent = self.sent.saturating_add(1);
        if let Some(path) = self.window_record.as_deref() {
            save_window(path, self.window_start, self.sent);
        }
    }
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

/// What the loop can be told to do besides send.
enum Command {
    /// Transmit this record on this carriage.
    Send {
        seq: u8,
        record: Vec<u8>,
        carriage: Carriage,
    },
    /// Tune the radio, and answer on this channel when the firmware has.
    SetPreset {
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// Set the path hash width in bytes (1..=3), answered the same way.
    SetPathWidth {
        bytes: u8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// Set transmit power in dBm, answered the same way.
    SetPower {
        dbm: i8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
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
        radio: RadioSettings,
    ) -> (Self, Bound) {
        let bound = Bound {
            uplink_record_max: u8::try_from(RECORD_MAX).unwrap_or(171),
            radio_state: RadioState::Connected,
            budget: Budget::RadioGoverned,
            airtime_ms: settings.airtime_ms,
        };
        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = async_mpsc::channel(64);
        let state = Radio {
            measured: None,
            battery: None,
            settings,
            repeat_enabled: radio.repeat_enabled,
            freq_mhz: radio.freq_mhz,
            bw_khz: radio.bw_khz,
            sf: radio.sf,
            cr: radio.cr,
            tx_power_dbm: radio.tx_power_dbm,
            budget: budget_per_hour(radio.airtime_ms, radio.freq_mhz),
            sent: 0,
            window_start: 0,
            window_record: radio.window_record,
            peer_record: radio.peer_record,
            learned_route: None,
            route_replies: 0,
            pushed: 0,
            refused: 0,
            refused_code: 0,
            refused_len: 0,
            ldmr: 0,
            raw_custom: 0,
            by_type: [0; 16],
        };
        std::thread::Builder::new()
            .name("ldmux-mesh-carrier".to_owned())
            .spawn(move || serve(companion, state, command_rx, event_tx))
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

/// What the radio reported about itself at open, beyond the carrier's own settings.
pub struct RadioSettings {
    /// The firmware's client-repeat flag, carried into every preset write.
    pub repeat_enabled: Option<bool>,
    /// The tuple the radio is on, for the airtime estimate and the allowance.
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub sf: u8,
    pub cr: u8,
    pub tx_power_dbm: i8,
    /// One full record's airtime at that tuple.
    pub airtime_ms: f64,
    /// Where the hour's spend is remembered, so a restart resumes the same hour.
    pub window_record: Option<PathBuf>,
    /// Where the peer's radio key is remembered, so a restart can address a flood to it before the
    /// phone has spoken.
    pub peer_record: Option<PathBuf>,
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

    async fn set_preset(
        &mut self,
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
    ) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .send(Command::SetPreset {
                freq_khz,
                bw_hz,
                sf,
                cr,
                reply,
            })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!("the radio refused the tuple: {e}"))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the retune")),
        }
    }

    async fn set_path_width(&mut self, bytes: u8) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .send(Command::SetPathWidth { bytes, reply })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!(
                "the radio refused the path width: {e}"
            ))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the path width")),
        }
    }

    async fn set_tx_power(&mut self, dbm: i8) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .send(Command::SetPower { dbm, reply })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!(
                "the radio refused the power: {e}"
            ))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the power change")),
        }
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
    mut radio: Radio,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
) {
    // The hour's spend survives a restart, or the allowance is per process rather than per hour.
    if let Some(path) = radio.window_record.as_deref()
        && let Some((start, sent)) = load_window(path)
    {
        let now = unix_now();
        if now.saturating_sub(start) < 3600 {
            radio.window_start = start;
            radio.sent = sent;
        } else {
            radio.window_start = now;
        }
    } else {
        radio.window_start = unix_now();
    }
    if events
        .blocking_send(Ok(Event::Status(radio.status())))
        .is_err()
    {
        return;
    }
    // **The radio is measured, not assumed.** One poll on bring-up and then every few seconds, so
    // `ldmux mesh status` reports the receiver's own numbers: a deaf radio shows its noise floor
    // and "no reading yet" honestly, instead of the zeros the status used to print for everything.
    let mut next_poll = std::time::Instant::now();
    // The road the radio's advert cache holds, so a change is reported once rather than every poll.
    let mut last_route: Option<(Vec<u8>, u8)> = None;
    loop {
        // **The air first.** A received frame already queued on the port would otherwise wait
        // behind this thread's own send, which is the defect that made a stats poll cost a packet.
        // A closed event channel means every caller has gone: the port is done with.
        if harvest(&mut companion, &mut radio, &events).is_err() {
            return;
        }
        if std::time::Instant::now() >= next_poll {
            next_poll = std::time::Instant::now() + STATS_POLL;
            if let Ok(stats) = companion.radio_stats() {
                radio.measured = Some(stats);
                if let Some(event) = radio.report_measured() {
                    let _ = events.blocking_send(Ok(event));
                }
            }
            let _ = events.blocking_send(Ok(Event::Status(radio.status())));
            // **What the port delivered, against what the radio heard.** `rx_air_secs` climbing
            // while `pushed` stands still is a port fault; both standing still is a radio that is
            // not demodulating. Without the pair, "the peer never sent" is an assumption.
            let types: String = radio
                .by_type
                .iter()
                .enumerate()
                .filter(|(_, n)| **n > 0)
                .map(|(kind, n)| format!("{kind}:{n}"))
                .collect::<Vec<_>>()
                .join(",");
            eprintln!(
                "ldmuxd: radio harvest pushed={} refused={} ldmr={} rawCustom={} types=[{types}] (last 0x{:02x} {}B) rx_air={}s",
                radio.pushed,
                radio.refused,
                radio.ldmr,
                radio.raw_custom,
                radio.refused_code,
                radio.refused_len,
                radio.measured.map_or(0, |m| m.rx_air_secs),
            );
            // **The road, asked for while there is none.** The advert cache lives in the radio, and
            // the host has no session-side copy of it, so this is the only way a host learns a
            // durable path to the phone: without it the host's route table held only what a flood
            // had taught, and once that expired it sealed its transaction parts with a guessed empty
            // path that no repeater forwards (measured 2026-11-13: `34B carriage=direct path=[]`
            // beside `route 2 repeaters`). The ask is a local cache read, not a transmission.
            // **This end never asks for a route on its own.** The ask is a 44-byte unscoped flood
            // that every repeater in range forwards, and a host that asks on a timer is a host
            // flooding a public mesh: measured 2026-11-13, the operator's neighbours reported the
            // testing as breaking the mesh. The host is reactive by construction — it answers a
            // phone's request and never originates one — so the road it needs is the one the
            // phone's own record arrived by, and the advert cache read below is a local command
            // that costs no airtime.
            if last_route.is_none()
                && radio.settings.peer_key.iter().any(|b| *b != 0)
                && let Ok(learned) = companion.get_advert_path(&radio.settings.peer_key)
            {
                    let outbound = learned.outbound();
                    let width = learned.hash_size;
                    last_route = Some((outbound.clone(), width));
                if events
                    .blocking_send(Ok(Event::Route {
                        path: outbound,
                        width,
                    }))
                    .is_err()
                {
                    return;
                }
            }
        }
        match commands.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Send {
                seq,
                record,
                carriage,
            }) => {
                // **What went on the air, and by which road.** The host's half of a link that never
                // comes up is invisible from `mesh status` alone: a reply sent along a stale path and
                // a reply never sent look identical there, and this line is the difference.
                // **The record's own index and counter, because a duplicate is only diagnosable with
                // the numbers.** The layout is `[kind][index][counter 4]`, so both are in the clear.
                let index = record.get(1).copied().unwrap_or(0);
                let counter = record
                    .get(2..6)
                    .and_then(|b| <[u8; 4]>::try_from(b).ok())
                    .map_or(0, u32::from_le_bytes);
                eprintln!(
                    "ldmuxd: host radio send {seq} {}B carriage={} idx={index} ctr={counter}",
                    record.len(),
                    match &carriage {
                        Carriage::Direct { path } => format!("direct path={path:02x?}"),
                        Carriage::Flood => "flood".to_string(),
                    }
                );
                let outcome = transmit(&mut companion, &radio.settings, &carriage, &record);
                radio.charge();
                if events
                    .blocking_send(Ok(Event::SendResult(SendResult { seq, outcome })))
                    .is_err()
                {
                    return;
                }
                if events
                    .blocking_send(Ok(Event::Status(radio.status())))
                    .is_err()
                {
                    return;
                }
            }
            Ok(Command::SetPreset {
                freq_khz,
                bw_hz,
                sf,
                cr,
                reply,
            }) => {
                // The firmware validates the tuple and answers OK or an error, so a refusal is
                // reported rather than leaving the radio silently where it was.
                let result = companion
                    .set_radio_params_keeping(freq_khz, bw_hz, sf, cr, radio.repeat_enabled)
                    .map_err(|e| format!("{e:?}"));
                if result.is_ok() {
                    radio.freq_mhz = f64::from(freq_khz) / 1000.0;
                    radio.bw_khz = f64::from(bw_hz) / 1000.0;
                    radio.sf = sf;
                    radio.cr = cr;
                    // The airtime estimate follows the new tuple, or the budget keeps measuring the
                    // radio that is no longer in use.
                    radio.settings.airtime_ms = Some(ldmux_companion::codec::time_on_air_ms(
                        radio.bw_khz,
                        sf,
                        cr,
                        RECORD_MAX,
                    ));
                    radio.budget = budget_per_hour(
                        radio.settings.airtime_ms.unwrap_or(0.0),
                        radio.freq_mhz,
                    );
                }
                let _ = reply.send(result);
                let _ = events.blocking_send(Ok(Event::Status(radio.status())));
            }
            Ok(Command::SetPathWidth { bytes, reply }) => {
                // The firmware's mode is zero-based (`0..2` for one to three bytes). The wire and
                // the CLI speak bytes, because that is the unit a person compares against a
                // neighbour's radio.
                let bytes = bytes.clamp(1, 3);
                let result = match companion.set_path_hash_mode(bytes - 1) {
                    Ok(()) => {
                        radio.settings.hash_size = bytes;
                        Ok(())
                    }
                    Err(e) => Err(format!("{e:?}")),
                };
                let _ = reply.send(result);
                let _ = events.blocking_send(Ok(Event::Status(radio.status())));
            }
            Ok(Command::SetPower { dbm, reply }) => {
                let result = match companion.set_tx_power(dbm) {
                    Ok(()) => {
                        radio.tx_power_dbm = dbm;
                        Ok(())
                    }
                    Err(e) => Err(format!("{e:?}")),
                };
                let _ = reply.send(result);
                let _ = events.blocking_send(Ok(Event::Status(radio.status())));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Read everything the radio has pushed, and hand it on.
fn harvest<L: ByteLink>(
    companion: &mut Companion<L>,
    radio: &mut Radio,
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
        radio.pushed = radio.pushed.saturating_add(1);
        // **A route request is answered before it is anything else.** It is a flood carrying the
        // asker's key, so the reply goes back along the chain it crossed; a reply is the road this
        // end was asking for. Both are `REQ` floods, so they arrive here through the receive log
        // exactly as a flooded record does, and both are recognised by their own magic.
        if let Some(packet) = pushed.get(3..) {
            if let Some(request) = ldmux_mesh::companion::route::parse_request(
                packet,
                &radio.settings.own_key,
            ) {
                if let Ok(reply) = ldmux_mesh::companion::route::build_reply(
                    &radio.settings.own_key,
                    &request.peer_key,
                    request.hash_size,
                    &request.path,
                    &request.nonce,
                ) {
                    let _ = companion.send_packet(&reply);
                    radio.route_replies = radio.route_replies.saturating_add(1);
                    // **The mechanism is invisible without this line.** A request answered and a
                    // request never heard look identical from `mesh status`, and at range the
                    // difference is the whole of whether a path can ever exist.
                    eprintln!(
                        "ldmuxd: host answered a route request ({} hop(s) back, {} answered so far)",
                        request.path.len(),
                        radio.route_replies
                    );
                }
                continue;
            }
            if let Some(reply) = ldmux_mesh::companion::route::parse_reply(
                packet,
                &radio.settings.peer_key,
            ) {
                radio.learned_route = Some((reply.path.clone(), reply.hash_size));
                eprintln!(
                    "ldmuxd: host learned a route ({} hop(s), width {}B)",
                    reply.path.len() / usize::from(reply.hash_size.clamp(1, 3)),
                    reply.hash_size
                );
                if events
                    .blocking_send(Ok(Event::Route {
                        path: reply.path,
                        width: reply.hash_size,
                    }))
                    .is_err()
                {
                    return Err(());
                }
                continue;
            }
        }
        let Some((_record, classified, sender)) = classify_push(&pushed, &radio.settings) else {
            radio.refused = radio.refused.saturating_add(1);
            radio.refused_code = pushed.first().copied().unwrap_or(0);
            radio.refused_len = u16::try_from(pushed.len()).unwrap_or(u16::MAX);
            if pushed.windows(4).any(|w| w == b"LDMR") {
                radio.ldmr = radio.ldmr.saturating_add(1);
            }
            if let Some(header) = pushed.get(3) {
                let kind = usize::from((header >> 2) & 0x0F);
                radio.by_type[kind] = radio.by_type[kind].saturating_add(1);
                if kind == 0x0F {
                    radio.raw_custom = radio.raw_custom.saturating_add(1);
                }
            }
            continue;
        };
        // **A flood names its sender, and that is how a host with no route learns the phone's radio
        // key.** The wrapper carries one byte of each key, so the second byte is the sender's hash;
        // without it the answer flood goes to hash zero and the phone ignores it — the asymmetric
        // case the teaching flood exists for.
        if let Some(hash) = sender
            && radio.settings.peer_key[0] != hash
        {
            radio.settings.peer_key[0] = hash;
            // **One byte is the whole of what a flood needs.** The wrapper names its destination by
            // the peer's first key byte, so that is what is remembered: a restart can address a
            // flood before the phone has spoken, and the rest of the record stays zero because
            // nothing else reads it.
            if let Some(path) = radio.peer_record.as_deref() {
                let mut key = [0u8; 32];
                key[0] = hash;
                let _ = std::fs::write(path, hex_key(&key));
            }
        }
        for event in classified {
            // **A record arrival carries the two readings it can measure, never a made-up zero.**
            // The noise floor and the radio's airtime counters come from `GET_STATS`, so a record's
            // `Signal` borrows the last measurement rather than publishing `0 dBm` over a link that
            // has none: the CLI printed exactly that on 2026-11-12, and a 0 dBm noise floor is
            // physically impossible.
            let event = match (event, radio.measured) {
                (Event::Signal(mut signal), Some(stats)) => {
                    signal.noise_floor_dbm = stats.noise_floor_dbm;
                    signal.tx_air_secs = stats.tx_air_secs;
                    signal.rx_air_secs = stats.rx_air_secs;
                    Event::Signal(signal)
                }
                (event, _) => event,
            };
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

/// The airtime allowance this daemon holds itself to, in milliseconds per hour: the law, or nothing.
///
/// **The allowance is the law, or nothing at all.** Inside a European sub-band with a duty cycle the
/// meter is on and the figure is that band's own limit — for 869.4-869.65, a tenth of its 10 %
/// because that band is shared with alarms and ldmux would rather be a quiet neighbour than a
/// compliant one. Outside every such window there is no limit to enforce, so there is **no
/// allowance at all**: the record counter is kept as a measurement and nothing is refused.
fn airtime_budget_ms(freq_mhz: f64) -> Option<f64> {
    match freq_mhz {
        // 868.0-868.6 MHz: 1 % by law (ERC/REC 70-03 Annex 1, h1.3.1).
        f if (868.0..=868.6).contains(&f) => Some(ONE_PERCENT_MS),
        // 869.4-869.65: the law allows 10 %, and ldmux holds itself to a tenth of it.
        f if (869.4..=869.65).contains(&f) => Some(AIRTIME_BUDGET_MS),
        // 868.7-869.2: a tenth of one percent — the alarm band, 3.6 s an hour.
        f if (868.7..=869.2).contains(&f) => Some(TENTH_PERCENT_MS),
        _ => None,
    }
}

/// The allowance inside 869.4-869.65 MHz: one percent of an hour, a tenth of the ten the band
/// allows. Also the figure the app meters with while the phone owns the radio, and a test on the
/// phone's side reads this literal to keep the two languages in step.
const AIRTIME_BUDGET_MS: f64 = 36_000.0;

/// The allowance inside 868.0-868.6 MHz: one percent, which is the law there rather than a policy.
const ONE_PERCENT_MS: f64 = 36_000.0;

/// The allowance inside 868.7-869.2 MHz: a tenth of one percent, which is the law there.
const TENTH_PERCENT_MS: f64 = 3_600.0;

/// How many full records fit the allowance in one hour.
fn budget_per_hour(airtime_ms: f64, freq_mhz: f64) -> Option<u32> {
    let allowance = airtime_budget_ms(freq_mhz)?;
    if airtime_ms <= 0.0 {
        return None;
    }
    let n = (allowance / airtime_ms).floor();
    Some(u32::try_from(n as u64).unwrap_or(u32::MAX).max(1))
}

pub(crate) fn hex_key(key: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in key {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The hour's spend as `unix_start sent`, or `None` when absent, unreadable or malformed.
fn load_window(path: &std::path::Path) -> Option<(u64, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace();
    let start = it.next()?.parse::<u64>().ok()?;
    let sent = it.next()?.parse::<u32>().ok()?;
    Some((start, sent))
}

/// Record the hour's spend. Best effort: a state directory that cannot be written must not stop the
/// radio from working, it only means the next restart forgets again.
///
/// **Written per record, deliberately.** A 20-byte file costs about 5 us and the page cache absorbs
/// it, and the alternative — a counter flushed on the status beat — loses up to half a second of
/// spend if the process dies, which is the one case this file exists for.
fn save_window(path: &std::path::Path, start_unix: u64, sent: u32) {
    let _ = std::fs::write(path, format!("{start_unix} {sent}"));
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
        let view = ldmux_companion::codec::parse_packet(&packet).expect("the packet parses");
        assert_eq!(view.payload_type, 0x0F, "a direct carriage is PAYLOAD_TYPE_RAW_CUSTOM");
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
        let view = ldmux_companion::codec::parse_packet(&first).expect("the packet parses");
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
        let (parsed, events, _sender) = classify_push(&frame, &settings()).expect("a session record");
        assert_eq!(parsed, record);
        assert!(
            matches!(&events[0], Event::Recv { record: r, .. } if r == record),
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
        let (parsed, _events, sender) = classify_push(&frame, &settings()).expect("a flooded record");
        assert_eq!(parsed, record);
        // The fixture builds the packet this end would send, so the wrapper's second byte is *our*
        // key: `[dest][source][magic][nonce]`. An arriving flood carries the peer's there, which is
        // what the host learns.
        assert_eq!(
            sender,
            Some(0x11),
            "a flood names its sender, which is how a host learns the phone's radio key"
        );
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
