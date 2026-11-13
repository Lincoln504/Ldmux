//! The radio the phone owns, as state: one command in flight, and what its answers said.
//!
//! This is the half of the phone's route that is **not** the session. `RadioEndpoint` carries sealed
//! records to a peer; this carries the questions a client asks its own radio — what it is tuned to,
//! what the air measured, how full its battery is — and keeps the last reading.
//!
//! It exists because the answers are a protocol too (`docs/RADIO-PLAN.md` §12.7d, `AR-13`): radio
//! management lived in `AndroidRadioLink`, so that file could not die, the settings screens would
//! have had a second owner of one GATT link, and a second owner writing commands outside one window
//! is `RC-2` with a different name.
//!
//! # One window, and records are in it
//!
//! [`CompanionFifo`] allows exactly one command in flight, because the firmware echoes no sequence
//! number: a second write's answer cannot be told from the first's. **A record send is a command**
//! (`CMD_SEND_RAW_PACKET`, answered `OK`/`SENT`/`ERR`), so it takes the same window. That is not
//! tidiness: if a record were written outside the window, its `OK` would settle whichever question
//! happened to be outstanding, which is precisely the mis-settlement this module is built to stop.
//!
//! The cost is that a record waits for the previous command's answer before it is written. On BLE
//! that answer is a few milliseconds, and the alternative is a link that reports a success nobody
//! asked for.
//!
//! # Purity
//!
//! No clock of its own and no I/O: every method takes `now_ms`, and the frames come out of
//! [`Radio::take_frame`] for the caller to write.

use std::collections::VecDeque;

use ldmux_mesh::companion::command::{
    self, BatteryAndStorage, CommandError, DeviceInfo, PHONE_COMMANDS, RadioStats, SelfInfo,
};
use ldmux_mesh::companion::fifo::{Answer, CommandId, CompanionFifo, Enqueue};
use ldmux_mesh::companion::framing;

/// How often the radio's advert cache is read while no path is held.
///
/// Slow on purpose: an advert arrives when the peer decides to send one, and each read takes the
/// one answer window for a moment.
pub const PATH_ASK_MS: u64 = 20_000;

/// The name this client reports in `APP_START`. The firmware prints it and nothing else depends on
/// it; what matters is that the frame carries *a* name (see `command::app_start`).
pub const APP_NAME: &str = "ldmux";

/// How long the radio's own readings are kept before they are asked for again.
///
/// A local command, so this costs no airtime — it costs the **command window**, which is why it is
/// not shorter. Fifteen seconds is longer than a settings screen is looked at and short enough that
/// the airtime figure tracks a command being typed; a record waiting behind one status ask is a few
/// milliseconds nobody will notice.
pub const STATUS_POLL_MS: u64 = 15_000;

/// One question this end has asked, or wants to ask.
#[derive(Debug)]
struct Queued {
    /// The command byte, which is also the first byte of the frame.
    opcode: u8,
    /// The whole payload: opcode first, exactly as the radio reads it.
    payload: Vec<u8>,
    /// The deadline, or zero for the table's own.
    timeout_ms: u32,
}

/// What the radio last said about itself.
///
/// Every field is optional separately, because they arrive from different questions and a radio that
/// answered one may not answer the others: a firmware without `GET_STATS` still reports its tuple.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct RadioStatus {
    /// The radio answered `APP_START`, which is the only evidence this end has that it is there.
    pub connected: bool,
    /// Its tuple and its identity, from `SELF_INFO`.
    pub self_info: Option<SelfInfo>,
    /// What the firmware can do, from `DEVICE_INFO`.
    pub device_info: Option<DeviceInfo>,
    /// What the air measured, from `STATS_RADIO`.
    pub radio_stats: Option<RadioStats>,
    /// The battery and the contact store, from `GET_BATT_AND_STORAGE`.
    pub battery: Option<BatteryAndStorage>,
    /// The RSSI of the last packet **this end** heard, which is fresher than the radio's own.
    ///
    /// A `0x88` push carries a reading for the packet it describes, and a busy mesh means the radio's
    /// answer to `GET_STATS` is about whatever a neighbour said last. The push is about a record, so
    /// it wins while it is newer.
    pub last_rssi_dbm: Option<i8>,
    /// The SNR of that packet, in quarters of a dB.
    pub last_snr_quarter_db: Option<i8>,
    /// How many records the caller says may still be sent this hour.
    ///
    /// **Set by the caller, not derived here.** The allowance is a band's duty-cycle rule, which is
    /// a fact about the law and the frequency rather than about the radio, and the engine does not
    /// own it (`docs/RADIO-PLAN.md` §12.7d). What the engine owns is the radio's own airtime below,
    /// which is what a caller with an allowance meters against.
    pub ops_left: Option<u8>,
}

impl RadioStatus {
    /// The tuple the radio is tuned to, as `(freq_khz, bw_hz, sf, cr)`.
    #[must_use]
    pub fn preset(&self) -> Option<(u32, u32, u8, u8)> {
        let info = self.self_info.as_ref()?;
        if info.radio_sf == 0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let freq_khz = (info.radio_freq_mhz * 1000.0).round() as u32;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bw_hz = (info.radio_bw_khz * 1000.0).round() as u32;
        Some((freq_khz, bw_hz, info.radio_sf, info.radio_cr))
    }

    /// The path hash width the firmware reported, in bytes (1..=3), when it reported one.
    #[must_use]
    pub fn path_hash_width(&self) -> Option<u8> {
        self.device_info
            .as_ref()
            .and_then(|d| d.path_hash_mode)
            .map(|mode| mode.saturating_add(1))
            .filter(|width| (1..=3).contains(width))
    }

    /// The radio's own transmit and receive airtime since it booted, in seconds.
    #[must_use]
    pub fn airtime_secs(&self) -> (u32, u32) {
        self.radio_stats
            .map_or((0, 0), |s| (s.tx_air_secs, s.rx_air_secs))
    }
}

/// Why a frame could not be taken as an answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RadioError {
    /// The payload is past what one companion frame can carry.
    TooLong,
    /// A command was asked for while one was still outstanding and the caller needed it now.
    Busy(CommandId),
    /// The radio refused the command, with the firmware's own code.
    Refused(u8),
    /// The answer could not be read as the report it should have been.
    Malformed(CommandError),
}

impl std::fmt::Display for RadioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => write!(f, "the command is larger than one companion frame"),
            Self::Busy(_) => write!(f, "the radio is still answering the last command"),
            Self::Refused(code) => write!(f, "the radio refused it (code {code})"),
            Self::Malformed(e) => write!(f, "the radio's answer did not read: {e}"),
        }
    }
}

impl std::error::Error for RadioError {}

/// Counters, so a field log can say what the radio answered.
#[derive(Clone, Default, Debug)]
pub struct RadioCounters {
    /// Commands this end asked for.
    pub commands: u64,
    /// Commands that were answered inside their deadline.
    pub answered: u64,
    /// Paths learned from an advert's own route, which is what lets a record travel past one hop.
    pub path_learned: u64,
    /// Commands whose deadline passed before their answer arrived.
    pub expired: u64,
    /// Frames that arrived with no command outstanding, or for another command.
    pub unexpected: u64,
    /// Answers the radio refused (`RESP_CODE_ERR`).
    pub refused: u64,
    /// Records handed over as `CMD_SEND_RAW_PACKET`.
    pub records: u64,
    /// Frames that were not a well-formed companion frame.
    pub malformed: u64,
}

/// The phone's own radio: the command window, the queue behind it, and the last reading.
#[derive(Debug)]
pub struct Radio {
    fifo: CompanionFifo,
    /// Questions waiting for the window, oldest first.
    queue: VecDeque<Queued>,
    /// The command holding the window, kept so its answer can be read into [`RadioStatus`].
    in_flight: Option<Queued>,
    /// Frames the caller must write, in the order the window accepted them.
    ready: VecDeque<Vec<u8>>,
    status: RadioStatus,
    /// When the readings are next due.
    next_poll_ms: Option<u64>,
    /// When to read the radio's advert cache again, while no path is held.
    next_path_ms: Option<u64>,
    /// The peer's public key, which is what the advert cache is asked about.
    peer_key: [u8; 32],
    /// The path an advert taught, reversed into the direction a packet travels.
    learned: Option<command::AdvertPath>,
    /// Whether the bring-up has been queued since the last connect.
    started: bool,
    counters: RadioCounters,
}

impl Default for Radio {
    fn default() -> Self {
        Self::new()
    }
}

impl Radio {
    /// An idle radio that has asked nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fifo: CompanionFifo::default(),
            queue: VecDeque::new(),
            in_flight: None,
            ready: VecDeque::new(),
            status: RadioStatus::default(),
            next_poll_ms: None,
            next_path_ms: None,
            peer_key: [0u8; 32],
            learned: None,
            started: false,
            counters: RadioCounters::default(),
        }
    }

    /// The last reading.
    #[must_use]
    pub fn status(&self) -> &RadioStatus {
        &self.status
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> &RadioCounters {
        &self.counters
    }

    /// How many commands are outstanding. Never more than one, which is the invariant.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.fifo.outstanding()
    }

    /// The caller's record budget, as the status reports it.
    pub fn set_ops_left(&mut self, left: u8) {
        self.status.ops_left = Some(left);
    }

    /// Begin asking the radio about itself.
    ///
    /// Queued once per connect: `APP_START` first, because the firmware latches the protocol version
    /// from `DEVICE_QUERY` and everything else is answered in the shapes that version selects, and
    /// then the three readings.
    pub fn bring_up(&mut self, now_ms: u64) {
        if self.started {
            return;
        }
        self.started = true;
        self.ask(command::app_start(APP_NAME), 5_000);
        self.ask(command::device_query(), 0);
        self.ask(command::get_battery_and_storage(), 0);
        self.ask(command::get_radio_stats(), 0);
        // **An advert is how a path exists at all.** The firmware never delivers a flooded
        // plaintext record (`companion::push`, AR-10), so a record reaches past one hop only along a
        // path, and the mesh's own advert cache is where a path comes from. Asked once per link: the
        // advert is a broadcast with no acknowledgement, and the cache fills a moment later.
        self.ask(command::self_advert(true), 5_000);
        self.next_path_ms = Some(now_ms.saturating_add(PATH_ASK_MS));
        self.next_poll_ms = Some(now_ms.saturating_add(STATUS_POLL_MS));
    }

    /// Forget the radio: a new link means a new `APP_START` and nothing known about it.
    pub fn reset(&mut self) {
        self.fifo.abandon();
        self.queue.clear();
        self.in_flight = None;
        self.ready.clear();
        self.status = RadioStatus::default();
        self.next_poll_ms = None;
        self.next_path_ms = None;
        self.learned = None;
        self.started = false;
    }

    /// Ask one command. `timeout_ms == 0` means the table's own deadline.
    ///
    /// The frame is not written here: [`Radio::pump`] writes it when the window is free, which is
    /// what keeps one command in flight without this method ever blocking.
    pub fn ask(&mut self, payload: Vec<u8>, timeout_ms: u32) {
        let Some(&opcode) = payload.first() else {
            return;
        };
        self.queue.push_back(Queued {
            opcode,
            payload,
            timeout_ms,
        });
        self.counters.commands += 1;
    }

    /// Queue one record for transmission.
    ///
    /// A record is a command (`CMD_SEND_RAW_PACKET`); it takes the same window as a status ask, for
    /// the reason in this module's header.
    pub fn send_record(&mut self, packet: &[u8]) {
        let mut payload = Vec::with_capacity(packet.len() + 2);
        payload.push(command::commands::SEND_RAW_PACKET);
        payload.push(0); // priority: MeshCore's own queue priority, never raised here
        payload.extend_from_slice(packet);
        self.ask(payload, 0);
        self.counters.records += 1;
    }

    /// Close the window on anything expired, queue what is due, and write what the window allows.
    ///
    /// Returns the number of frames made ready.
    pub fn pump(&mut self, now_ms: u64) -> usize {
        if self.fifo.expire(now_ms).is_some() {
            self.counters.expired += 1;
            self.in_flight = None;
        }
        // **The path, while there is none.** Asked on a slow cadence, because an advert arrives
        // when a neighbour decides to send one and a cache read is a local command that costs the
        // one answer window anyway. Once a path is held it is not asked again until the link resets.
        if self.started
            && self.learned.is_none()
            && self.peer_key.iter().any(|b| *b != 0)
            && self.next_path_ms.is_some_and(|due| now_ms >= due)
        {
            self.next_path_ms = Some(now_ms.saturating_add(PATH_ASK_MS));
            self.ask(command::get_advert_path(&self.peer_key), 0);
        }
        // The readings come due again.
        if self.started && self.next_poll_ms.is_some_and(|due| now_ms >= due) {
            self.next_poll_ms = Some(now_ms.saturating_add(STATUS_POLL_MS));
            self.ask(command::get_battery_and_storage(), 0);
            self.ask(command::get_radio_stats(), 0);
        }
        if self.fifo.outstanding() != 0 {
            return 0;
        }
        let Some(next) = self.queue.pop_front() else {
            return 0;
        };
        let timeout = if next.timeout_ms == 0 {
            PHONE_COMMANDS.timeout_ms(next.opcode)
        } else {
            next.timeout_ms
        };
        match self.fifo.enqueue_within(next.opcode, timeout, now_ms) {
            Enqueue::Accepted(_) => {
                if next.payload.len() > framing::MAX_PAYLOAD {
                    // A payload past the frame ceiling is our bug, not the radio's: the command is
                    // dropped and the window released rather than written half a one.
                    self.fifo.abandon();
                    self.counters.malformed += 1;
                    return 0;
                }
                self.ready.push_back(next.payload.clone());
                self.in_flight = Some(next);
                1
            }
            Enqueue::Busy(_) | Enqueue::Draining { .. } => {
                self.queue.push_front(next);
                0
            }
        }
    }

    /// Take one frame the caller must write.
    ///
    /// The **payload**, opcode first: a BLE characteristic carries it as it is, and a serial caller
    /// wraps it once with `framing::encode`. Producing the serial framing here would have a phone
    /// write three bytes the radio reads as a command.
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    /// Put a frame back at the front, for a caller whose buffer was too small.
    ///
    /// Dropping it would lose a record; writing half of it would lose the command. The order is the
    /// one the window accepted, so a frame that goes back keeps its place.
    pub fn put_back(&mut self, frame: Vec<u8>) {
        self.ready.push_front(frame);
    }

    /// Hand one companion **answer** frame to the window.
    ///
    /// A push (`0x88` and friends) must be classified by the caller first: a push is not an answer,
    /// and offering one here would report it as unexpected on every busy mesh.
    pub fn on_answer(&mut self, payload: &[u8], now_ms: u64) -> Answer {
        let Some(&code) = payload.first() else {
            self.counters.malformed += 1;
            return Answer::Unexpected {
                answer: 0,
                outstanding: self.fifo.in_flight(),
            };
        };
        let answer = self.fifo.answering(&PHONE_COMMANDS, code, now_ms);
        match answer {
            Answer::Settled(_) => {
                self.counters.answered += 1;
                if let Some(request) = self.in_flight.take() {
                    self.apply(request.opcode, code, payload);
                }
            }
            Answer::Unexpected { .. } => self.counters.unexpected += 1,
            Answer::Discarded { .. } => {}
        }
        answer
    }

    /// Read a settled answer into the status.
    fn apply(&mut self, opcode: u8, code: u8, payload: &[u8]) {
        use command::codes;
        // A refusal is not a malformed answer: say which command, and keep everything else.
        if code == codes::ERROR {
            self.counters.refused += 1;
            return;
        }
        let outcome = match opcode {
            command::commands::APP_START => command::parse_self_info(payload).map(|info| {
                self.status.connected = true;
                self.status.self_info = Some(info);
            }),
            command::commands::DEVICE_QUERY => {
                match command::parse_device_info(payload) {
                    Some(info) => {
                        self.status.device_info = Some(info);
                        Ok(())
                    }
                    // Older firmware answers a `DEVICE_INFO` this parser cannot read past its first
                    // field; that is a radio with fewer facts, not a fault.
                    None => Err(CommandError::Truncated),
                }
            }
            command::commands::GET_BATT_AND_STORAGE => {
                command::parse_battery_and_storage(payload).map(|b| self.status.battery = Some(b))
            }
            command::commands::GET_STATS => command::parse_radio_stats(payload).map(|s| {
                self.status.radio_stats = Some(s);
            }),
            command::commands::GET_ADVERT_PATH => {
                match command::parse_advert_path(payload) {
                    Some(path) => {
                        self.learned = Some(path);
                        self.counters.path_learned += 1;
                        Ok(())
                    }
                    None => Err(CommandError::Truncated),
                }
            }
            // A send's answer carries no reading: `OK` or `SENT` is the whole of it.
            _ => Ok(()),
        };
        let _ = outcome.map_err(|_| self.counters.malformed += 1);
    }

    /// Record a reading from a receive-log push: the packet this end just heard.
    pub fn note_heard(&mut self, rssi_dbm: i8, snr_quarter_db: i8) {
        self.status.last_rssi_dbm = Some(rssi_dbm);
        self.status.last_snr_quarter_db = Some(snr_quarter_db);
    }

    /// When the caller should look again even if nothing arrives.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        let soonest = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        };
        soonest(soonest(self.fifo.deadline_ms(), self.next_poll_ms), self.next_path_ms)
    }

    /// The path an advert taught, reversed into the direction a packet travels.
    ///
    /// `None` until the radio has heard the peer's advert, which is what makes a record able to
    /// cross more than one hop.
    #[must_use]
    pub fn learned_path(&self) -> Option<(Vec<u8>, u8)> {
        let path = self.learned.as_ref()?;
        Some((path.outbound(), path.hash_size))
    }

    /// Tell the radio whose advert to look for, and start asking.
    pub fn set_peer_key(&mut self, peer_key: [u8; 32], now_ms: u64) {
        self.peer_key = peer_key;
        if self.learned.is_none() {
            self.next_path_ms = Some(now_ms);
        }
    }
}
