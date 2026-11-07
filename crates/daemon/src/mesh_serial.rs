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
//! `docs/RELEASE-AUDIT.md` appendix B, not a limit the firmware enforces. The
//! firmware has its own governor; this number only keeps LDMUX's meter honest.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ldmux_companion::codec::parse_advert_path;
use ldmux_companion::{ByteLink, Companion, RAW_CUSTOM_DIRECT_HEADER, SerialLink, SessionError};
use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event};
use ldmux_mesh::constants::{RECORD_MAX, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI};
use ldmux_mesh::error::MeshError;
use ldmux_mesh::messages::{SendResult, Status};
use tokio::sync::mpsc as async_mpsc;

/// How long the radio thread waits for a command before checking the air.
const COMMAND_WAIT: Duration = Duration::from_millis(10);

/// How often the radio's countdown is republished while the link is idle.
///
/// The session waits on this number: at zero it sends nothing, so nothing is transmitted, so a
/// status tied to transmission would never refresh. Half a second is invisible to a person and
/// costs no airtime.
const STATUS_REFRESH: Duration = Duration::from_millis(500);

/// The firmware's own ceiling for a path, in bytes (`MAX_PATH_SIZE`, `MeshCore.h`).
const MAX_PATH_SIZE: usize = 64;

/// How often this radio floods an advert, or `None` when adverts are off.
///
/// **An advert is how a peer learns a path to us.** A raw record is a direct packet, and a direct
/// packet with no path reaches only as far as the sender's antenna. The phone's radio learns the
/// route from the advert's accumulated path (`CMD_GET_ADVERT_PATH`), so this interval is the
/// worst case for "arrived somewhere new, opened the app": five minutes.
///
/// **Why minutes and not half an hour.** The interval was 1800 s, which is fine for a peer that
/// stays in one place and wrong for a phone: a route heard an hour ago belongs to wherever the
/// phone was an hour ago, and the first thing a moved phone does is fail on it. One advert is
/// about 0.3 s of air at 62.5 kHz SF7, so five minutes costs roughly a tenth of a percent of a
/// 1 % allowance, and MeshCore treats adverts as unacknowledged broadcasts, so this is the only
/// lever there is. `LDMUX_MESH_ADVERT_SECS=0` turns them off for an operator who does not want the
/// node visible on the mesh at all.
fn advert_interval() -> Option<Duration> {
    let secs = std::env::var("LDMUX_MESH_ADVERT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(300);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// How long a dropped carrier waits for the radio thread before leaving it to finish.
///
/// The wait exists so the port is closed and its advisory lock released before a reconnect
/// opens the same device. It is **bounded** because the thread can be inside a write to a
/// device that stopped draining: the write itself is bounded (`companion::link::WRITE_TIMEOUT`),
/// and this wait is shorter still, so a wedged radio costs one short pause rather than a hung
/// supervisor. A thread that outlives the wait closes the port and releases the lock when it
/// does finish; the descriptor is not inherited (`O_CLOEXEC`), so nothing else keeps it.
const CLOSE_WAIT: Duration = Duration::from_millis(250);

/// One second of the hour the budget is measured over.
const HOUR: Duration = Duration::from_secs(3_600);

/// How long the peer's route may go unrefreshed before it is asked for again.
///
/// A route is only true while the repeaters that carried it are still there, and a phone that moved
/// has a different one. The read is a serial command, so it costs nothing on the air; two minutes is
/// short enough that a walk is felt as a pause rather than as a dead link.
const PEER_ROUTE_REFRESH: Duration = Duration::from_secs(120);

/// How soon to ask again while no route is known.
///
/// A phone that has just arrived announces itself, and that advert takes a moment to cross the
/// repeaters, so the first lookup legitimately answers "not heard". Fifteen seconds is short enough
/// that arriving somewhere new is felt as a pause, and the read is a serial command with no airtime
/// at all.
const PEER_ROUTE_RETRY: Duration = Duration::from_secs(15);

/// The least time between two adverts triggered by a route hint.
///
/// A hint is 36 bytes and an advert is about 0.3 s of air at 62.5 kHz, so a peer that repeats hints
/// is an amplifier. Three minutes caps that at twenty adverts an hour (six seconds, about a sixth of
/// a 1 % allowance) and only while this radio has no route to the peer at all, which is the moment
/// the phone actually needs to hear us.
const HINT_ADVERT_FLOOR: Duration = Duration::from_secs(180);

/// The four bytes that mark a record as a **route hint** rather than a session message.
const ROUTE_HINT_MAGIC: &[u8; 4] = b"LDRH";

/// `[LDRH][32-byte radio public key]`: the peer telling this radio which node it is.
///
/// **Why a record and not a field somewhere.** The reply route has to exist before the session
/// does — the handshake's own records need an answer — so the peer's radio identity cannot ride in
/// the session, and it cannot be inferred from an arriving record either, because a direct packet's
/// path is consumed by the repeaters that carry it. Thirty-six unencrypted bytes before the
/// handshake are what make the lookup possible. They are a routing hint and nothing more: a forged
/// one can only redirect where records are *sent*, and everything inside them is sealed by the
/// session.
fn route_hint(record: &[u8]) -> Option<[u8; 32]> {
    if record.len() != 4 + 32 || record.get(..4)? != ROUTE_HINT_MAGIC {
        return None;
    }
    record.get(4..).and_then(|k| k.try_into().ok())
}

/// Ask the radio for the route to `peer_radio`, and keep it when the answer has one.
///
/// **This is a serial command, not a transmission**, so it can be asked as often as is useful; what
/// it must not do is ask while another command's answer window is open, which is why every caller is
/// on the idle path. A `Rejected` answer means the radio has not heard that node's advert yet — the
/// phone may be announcing right now and the flood may still be crossing the repeaters — so the
/// caller retries sooner than the calm refresh.
fn refresh_peer_route<L: ByteLink>(
    companion: &mut Companion<L>,
    peer_radio: Option<&[u8; 32]>,
    peer_path: &mut Option<PeerPath>,
) -> bool {
    let Some(key) = peer_radio else {
        return false;
    };
    match companion.get_advert_path(key) {
        Ok(path) => {
            // **A zero-hop answer clears the route.** The phone coming back into earshot advertises
            // with no path, and keeping the last repeater route would send every reply into a chain
            // that no longer reaches it: the phone would work at range, come home, and go deaf until
            // the daemon restarted.
            *peer_path = PeerPath::reversed(
                usize::from(path.hash_size),
                usize::from(path.hops),
                &path.bytes,
            );
            true
        }
        Err(_) => false,
    }
}

/// The self-imposed airtime policy, in milliseconds per hour: one percent.
const AIRTIME_BUDGET_MS: f64 = 36_000.0;

/// The one command the loop has for the radio.
enum Command {
    Record { seq: u8, record: Vec<u8> },
    /// Tune the radio, and answer on this channel when the firmware has.
    SetPreset {
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
}

/// The radio, seen as a carrier.
pub struct SerialCarrier {
    /// `None` once dropped, so the radio thread sees the channel close.
    commands: Option<Sender<Command>>,
    events: async_mpsc::Receiver<Result<Event, MeshError>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Set by `drop`; the radio thread checks it between commands.
    stop: Arc<AtomicBool>,
    /// Fires once the radio thread has returned, which is when the port is closed.
    stopped: Option<Receiver<()>>,
}

impl SerialCarrier {
    /// Open the device, identify the radio, and start serving it.
    pub async fn open(device: &str) -> Result<(Self, Bound, RadioPreset), MeshError> {
        let path = device.to_owned();
        let prepared = tokio::task::spawn_blocking(move || prepare(&path))
            .await
            .map_err(|e| MeshError::Io(std::io::Error::other(e.to_string())))?;
        let (companion, bound, settings) = prepared?;

        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = async_mpsc::channel(64);
        let (stopped_tx, stopped_rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("ldmux-mesh-serial".to_owned())
            .spawn(move || {
                serve(companion, settings, command_rx, event_tx, thread_stop);
                let _ = stopped_tx.send(());
            })
            .map_err(MeshError::Io)?;
        Ok((
            Self {
                commands: Some(command_tx),
                events: event_rx,
                worker: Some(worker),
                stop,
                stopped: Some(stopped_rx),
            },
            bound,
            settings.preset,
        ))
    }
}

impl Carrier for SerialCarrier {
    async fn set_preset(
        &mut self,
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
    ) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .as_ref()
            .ok_or(MeshError::Closed)?
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
        // Dropping the sender disconnects the channel and the flag covers a thread already
        // inside a command. **Never `join` without a bound**: this runs on the session loop's
        // teardown path, and a thread parked in a write to a radio that stopped draining used
        // to hang it — the snapshot then read `ready / established` for ever, the shells froze
        // and only a daemon restart recovered. The handle is dropped (detached) when the wait
        // expires; the port closes and the lock releases when the thread does return.
        drop(self.commands.take());
        self.stop.store(true, Ordering::Relaxed);
        if let Some(stopped) = self.stopped.take() {
            // The wait is bounded, and on the daemon's multi-thread runtime it runs off the
            // worker (`block_in_place` hands the thread back while it waits). A current-thread
            // runtime — the tests — takes it inline; 250 ms is short enough for that, and it
            // keeps the teardown testable without a second runtime.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                    let _ = tokio::task::block_in_place(|| stopped.recv_timeout(CLOSE_WAIT));
                }
                _ => {
                    let _ = stopped.recv_timeout(CLOSE_WAIT);
                }
            }
        }
        // Dropping the handle detaches rather than joins: a late finish is harmless, and
        // blocking here is the very thing this teardown must not do.
        let _ = self.worker.take();
    }
}

/// What the radio told us at open time, and what the loop needs from it.
#[derive(Clone, Copy)]
struct Settings {
    /// Bytes per path hash, from the firmware's reported path-hash mode.
    hash_size: u8,
    /// Time on air for a full record, from the preset the radio reports.
    airtime_ms: f64,
    /// **What this radio is actually tuned to.** Read from `SELF_INFO` at open and published in
    /// the snapshot, because the two radios must be on the same tuple and nothing else in the
    /// system can say what the host's is: the phone knows its own, the host knew nothing, and a
    /// mismatch looked exactly like being out of range. The user needs to be able to compare the
    /// two numbers.
    preset: RadioPreset,
}

/// The four numbers that decide whether two radios can hear each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioPreset {
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub sf: u8,
    pub cr: u8,
    /// This radio's own public key, from the same `SELF_INFO`.
    ///
    /// The daemon owns the radio, so it can read the key a phone must bind. Requiring the operator
    /// to type it into `LDMUX_MESH_SELF` was a step that could only be got wrong: a stale or
    /// mistyped key makes a pairing code that no phone can use.
    pub public_key: [u8; 32],
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
    let hash_size = device_info.path_hash_mode.map_or(1, |m| m.saturating_add(1)).clamp(1, 3);
    let airtime_ms = self_info.time_on_air_ms(RECORD_MAX);
    let bound = Bound {
        uplink_record_max: u8::try_from(RECORD_MAX).unwrap_or(172),
        radio_state: RadioState::Connected,
        // No gateway is in the path: this process owns the radio, and the firmware is the meter.
        budget: Budget::RadioGoverned,
        airtime_ms: Some(airtime_ms),
    };
    let preset = RadioPreset {
        freq_mhz: self_info.radio_freq_mhz,
        bw_khz: self_info.radio_bw_khz,
        sf: self_info.radio_sf,
        cr: self_info.radio_cr,
        public_key: self_info.public_key,
    };
    Ok((companion, bound, Settings { hash_size, airtime_ms, preset }))
}

/// The radio thread: serve commands and the air until the loop lets go.
fn serve<L: ByteLink>(
    companion: Companion<L>,
    settings: Settings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
    stop: Arc<AtomicBool>,
) {
    serve_loop(companion, settings, commands, events, &stop);
}

/// The radio thread's body, split out so every exit path reaches the `stopped` signal.
fn serve_loop<L: ByteLink>(
    mut companion: Companion<L>,
    mut settings: Settings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
    stop: &AtomicBool,
) {
    // Recomputed on a retune: a budget is records per hour at *this* tuple's airtime, so a
    // radio moved to a slower spreading factor can spend fewer records than this number said.
    let mut budget = budget_per_hour(settings.airtime_ms);
    let mut window = Instant::now();
    let mut sent: u32 = 0;
    // The route to the peer's radio. **A direct packet's path is consumed in transit**, so it
    // cannot be learned from an arriving record: each repeater removes its own hash
    // (`Mesh::removeSelfFromPath`), which leaves a packet that crossed repeaters with an empty path.
    // The route therefore comes from the peer's *advert*, whose path accumulates because flooded
    // packets append one hash per repeater (`Mesh::routeRecvPacket`), and is read back out of the
    // radio's advert-path cache.
    let mut peer_path: Option<PeerPath> = None;
    // Which radio the peer is, learned from its route hint. Without it there is no prefix to look a
    // route up by, so replies stay zero-hop.
    let mut peer_radio: Option<[u8; 32]> = None;
    // When the peer's route was last asked for, so a quiet link does not ask on every tick.
    let mut last_route_ask: Option<Instant> = None;
    // When the last advert went out; `None` means none yet, so the first idle moment sends one.
    let mut last_advert: Option<Instant> = None;
    let advert_every = advert_interval();
    // When the last status went out. **A status is published on a timer as well as after a send**,
    // because the session gates its sends on the countdown this carries: with the countdown at zero
    // the session polls nothing, the radio transmits nothing, and a status that is only published
    // after a transmission could never say the hour had rolled over. Measured 2026-11-06: a phone
    // waiting on "the host has not described a terminal yet" for as long as the countdown stayed
    // stale.
    let mut last_status = Instant::now();

    // One status before anything else: the loop starts with no budget, and an
    // unknown budget is zero, so without this it would never send the first record.
    if events
        .blocking_send(Ok(Event::Status(status(&settings, budget, 0))))
        .is_err()
    {
        return;
    }

    loop {
        // A dropped carrier may be waiting for this thread; do not sit inside a command
        // interval after the loop has let go.
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if window.elapsed() >= HOUR {
            window = Instant::now();
            sent = 0;
        }

        match commands.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Record { seq, record }) => {
                // **The reply follows the path the request arrived by.** Without it a phone behind
                // a repeater is unreachable: a direct packet with no path travels only as far as
                // this radio's own antenna.
                let (hash_size, path) = match &peer_path {
                    Some(p) => (p.hash_size, p.bytes.as_slice()),
                    None => (settings.hash_size, &[][..]),
                };
                let outcome = match companion.send_raw(hash_size, path, &record) {
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
                    .set_radio_params(freq_khz, bw_hz, sf, cr)
                    .map_err(|e| format!("{e:?}"));
                if result.is_ok() {
                    // A retune changes the tuple and nothing else: the radio keeps its key.
                    settings.preset = RadioPreset {
                        freq_mhz: f64::from(freq_khz) / 1000.0,
                        bw_khz: f64::from(bw_hz) / 1000.0,
                        sf,
                        cr,
                        public_key: settings.preset.public_key,
                    };
                    // The airtime estimate follows the new tuple, or the budget keeps measuring
                    // the radio that is no longer in use.
                    settings.airtime_ms = ldmux_companion::codec::time_on_air_ms(
                        settings.preset.bw_khz,
                        settings.preset.sf,
                        settings.preset.cr,
                        RECORD_MAX,
                    );
                    // The report was computed from the old airtime, so it kept saying the old
                    // tuple's allowance while the meter below it measured the new one.
                    budget = budget_per_hour(settings.airtime_ms);
                }
                let _ = reply.send(result);
                let _ = events.blocking_send(Ok(Event::Status(status(
                    &settings, budget, sent,
                ))));
            }
            Err(RecvTimeoutError::Timeout) => {
                // **The route to the peer, from its advert.** Asked for when the peer announces
                // itself and then every `PEER_ROUTE_REFRESH` while the link is quiet, because a
                // phone that moved has a new route and the old one silently carries nothing. This
                // is one serial command: it costs no airtime.
                let due = last_route_ask.is_none_or(|t| {
                    // While there is no route, ask again quickly: the phone may be mid-announce and
                    // the flood still crossing the repeaters. Once one is in hand, ask calmly.
                    let interval = if peer_path.is_some() {
                        PEER_ROUTE_REFRESH
                    } else {
                        PEER_ROUTE_RETRY
                    };
                    t.elapsed() >= interval
                });
                // **Not gated on hearing the peer.** The failure this covers is "we hear them, they
                // cannot hear us": a phone that moved still reaches us, so a gate on inbound
                // silence would never let the route be re-read.
                if peer_radio.is_some() && due {
                    last_route_ask = Some(Instant::now());
                    refresh_peer_route(&mut companion, peer_radio.as_ref(), &mut peer_path);
                }
                // The countdown, refreshed while the link is idle. Not a transmission: it is a
                // message to this process only, and it is what lets the session see the hour roll
                // over when it has been held back for the whole hour.
                if last_status.elapsed() >= STATUS_REFRESH {
                    last_status = Instant::now();
                    let left = budget.saturating_sub(u8::try_from(sent).unwrap_or(u8::MAX));
                    if events
                        .blocking_send(Ok(Event::Status(status(&settings, left, sent))))
                        .is_err()
                    {
                        return;
                    }
                }
                // Only when nothing is queued: an advert waits its turn behind records, because a
                // record is an answer to something the user is waiting for.
                if let Some(every) = advert_every
                    && last_advert.is_none_or(|t| t.elapsed() >= every)
                {
                    let _ = companion.send_self_advert(true);
                    last_advert = Some(Instant::now());
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = events.blocking_send(Err(MeshError::Closed));
                return;
            }
        }

        match companion.next_push(Duration::from_millis(1)) {
            Ok(Some(frame)) => {
                if let Some(path) = parse_advert_path(&frame) {
                    // A late answer to a request that timed out. Assigned the same way
                    // `refresh_peer_route` does, including the clear: two paths to one field that
                    // disagree about a zero-hop answer is how a stale route survives.
                    peer_path = PeerPath::reversed(
                        usize::from(path.hash_size),
                        usize::from(path.hops),
                        &path.bytes,
                    );
                } else if let Some(incoming) = incoming_record(&frame) {
                    // The peer announcing which radio it is, so the route above can be looked up.
                    if let Some(key) = route_hint(&incoming.record) {
                        // **The phone has just said hello, so answer in kind.** Its route to us is
                        // the advert it heard; ours to it is the advert it is about to hear, so one
                        // is flooded now rather than at the next scheduled interval. Then the
                        // cache is asked straight away, because the handshake's own record is the
                        // next thing to send and it needs a route.
                        peer_radio = Some(key);
                        // One advert per three minutes, and only while there is no route to the
                        // peer: a phone whose session keeps failing sends a hint on every attempt,
                        // and each advert is a broadcast on a band other people are using. Once a
                        // route exists the hint costs no air at all.
                        if peer_path.is_none()
                            && last_advert.is_none_or(|t| t.elapsed() >= HINT_ADVERT_FLOOR)
                        {
                            let _ = companion.send_self_advert(true);
                            last_advert = Some(Instant::now());
                        }
                        if refresh_peer_route(&mut companion, peer_radio.as_ref(), &mut peer_path) {
                            last_route_ask = Some(Instant::now());
                        }
                    } else {
                        if events
                            .blocking_send(Ok(Event::Recv(incoming.record)))
                            .is_err()
                        {
                            return;
                        }
                    }
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

/// The path a peer's record arrived by, reversed: the hops to reach *it*.
///
/// MeshCore appends one hash per repeater as a packet travels, so the path on an arriving packet is
/// this node's route *to the sender read backwards*. Reversing the hop order (each hop is
/// `hash_size` bytes) gives a usable path for the reply, which is what makes a phone behind a
/// repeater reachable at all: a direct packet with no path only travels as far as the sender's own
/// radio, so every reply to a repeater-reached phone would be heard by nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerPath {
    hash_size: u8,
    hops: u8,
    /// `hops * hash_size` bytes, in send order.
    bytes: Vec<u8>,
}

impl PeerPath {
    /// The reverse of a path that arrived, or `None` when there is nothing to reverse (zero hops,
    /// a malformed length, or a path longer than the firmware's 64-byte limit).
    ///
    /// **Hash size 4 is refused.** The encoded length byte has two bits for the hash width, giving
    /// 1..=4, and the firmware reserves 4 for the future: `Packet::isValidPathLen` rejects it
    /// outright (`src/Packet.cpp:13-18`), so a frame that claims it is malformed, not a wide path.
    fn reversed(hash_size: usize, hops: usize, path: &[u8]) -> Option<Self> {
        if hops == 0 || !(1..=3).contains(&hash_size) || path.len() != hops * hash_size {
            return None;
        }
        if path.len() > MAX_PATH_SIZE {
            return None;
        }
        let mut bytes = Vec::with_capacity(path.len());
        for hop in path.chunks_exact(hash_size).rev() {
            bytes.extend_from_slice(hop);
        }
        Some(Self {
            hash_size: u8::try_from(hash_size).ok()?,
            hops: u8::try_from(hops).ok()?,
            bytes,
        })
    }
}

/// One received packet, reduced to the record inside it.
#[derive(Debug, PartialEq, Eq)]
struct Incoming {
    record: Vec<u8>,
}

/// One received packet, reduced to the record inside it.
///
/// The guard is the header: only a packet LDMUX itself built (DIRECT with payload RAW_CUSTOM) can
/// be a record. Everything else on the air — adverts, trace routes, another network's channel
/// traffic — is not ours and is ignored rather than handed to the session, which would have to
/// reject it and would show the rejection as a defect.
///
/// **A flooded `RAW_CUSTOM` cannot be one of ours**: the firmware drops flood-routed raw packets
/// before the application sees them (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM` accepts only
/// `isRouteDirect()`), so a record always arrives direct.
///
/// **The path on a direct packet is consumed, not accumulated.** Each repeater removes its own hash
/// before retransmitting (`Mesh::removeSelfFromPath`), so a record that crossed any number of them
/// arrives with an empty path, and the field is skipped rather than read as a route. That is why the
/// reply route comes from the peer's advert (flooded packets *append* a hash per repeater,
/// `Mesh::routeRecvPacket`) and not from an arriving record.
fn incoming_record(frame: &[u8]) -> Option<Incoming> {
    const PUSH_LOG_RX: u8 = 0x88;

    if frame.first() != Some(&PUSH_LOG_RX) || frame.len() < 5 {
        return None;
    }
    // `MyMesh.cpp:286-296`: `[0x88][snr*4][rssi][packet]`. The packet starts at 3.
    let packet = &frame[3..];
    if packet.len() < 2 || packet[0] & 0x3F != RAW_CUSTOM_DIRECT_HEADER & 0x3F {
        return None;
    }
    // Route type in the low two bits: 0 and 3 carry four bytes of transport codes before the path.
    let route = packet[0] & 0x03;
    let mut offset = 1;
    if route == 0 || route == 3 {
        offset += 4;
    }
    let path_byte = *packet.get(offset)?;
    offset += 1;
    let hash = usize::from((path_byte >> 6) + 1); // the two bits can name 4; reserved, so unused
    let hops = usize::from(path_byte & 0x3F);
    let body = offset.checked_add(hops.saturating_mul(hash))?;
    if body > packet.len() {
        return None;
    }
    Some(Incoming {
        record: packet[body..].to_vec(),
    })
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
    fn dropping_a_carrier_whose_thread_is_slow_does_not_block_the_loop() {
        // A real radio that keeps the port open and stops draining is not reproducible here,
        // so the shape is: a thread that has not signalled it stopped. `drop` must return
        // after the bounded wait, not wait for the thread.
        let (command_tx, command_rx) = channel();
        let (_event_tx, event_rx) = async_mpsc::channel(1);
        let (_stopped_tx, stopped_rx) = channel::<()>();
        let worker = std::thread::spawn(move || {
            // Hold the receiver so the sender stays connected, then outlive the wait.
            std::thread::sleep(Duration::from_millis(400));
            drop(command_rx);
        });
        let carrier = SerialCarrier {
            commands: Some(command_tx),
            events: event_rx,
            worker: Some(worker),
            stop: Arc::new(AtomicBool::new(false)),
            stopped: Some(stopped_rx),
        };
        let started = Instant::now();
        drop(carrier);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "drop waited {elapsed:?} on a thread that had not stopped"
        );
        assert!(elapsed >= CLOSE_WAIT, "it did give the thread its bounded wait");
    }

    #[test]
    fn a_direct_raw_packet_yields_its_record() {
        // What `build_raw_direct(1, &[], record)` produces: header, zero-hop path byte.
        let packet = build_raw_direct(1, &[], &[9, 8, 7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![9, 8, 7]);
    }

    #[test]
    fn a_two_byte_hash_path_is_skipped_before_the_record() {
        let packet = build_raw_direct(2, &[0xAA, 0xBB], &[1, 2]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![1, 2]);
    }

    /// **A path on a record is skipped, never learned from.** A direct packet loses one hash per
    /// repeater (`Mesh::removeSelfFromPath`), so what arrives has no route in it at all; the reply
    /// route comes from the peer's advert instead.
    #[test]
    fn a_path_on_a_record_is_skipped_before_the_record_and_not_learned() {
        // One-byte hashes, two hops, exactly what a sender writes before repeaters consume them.
        let packet = build_raw_direct(1, &[0x11, 0x22], &[7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![7], "the record starts after the path");

        // Two-byte hashes, two hops: the offset must count whole hops, not bytes.
        let packet = build_raw_direct(2, &[0xA1, 0xA2, 0xB1, 0xB2], &[7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![7]);
    }

    /// **The route that is learned is the reverse of what an advert carries.** Flooded packets
    /// append a hash per repeater, so the path read out of the advert cache is in travel order and
    /// the reply needs the hop groups the other way round.
    /// A `RESP_CODE_ADVERT_PATH` frame as the firmware writes it: the code, the advert's timestamp,
    /// the encoded path byte, then the hops in travel order.
    fn advert_path_frame(encoded: u8, hops: &[u8]) -> Vec<u8> {
        let mut v = vec![ldmux_companion::codes::ADVERT_PATH];
        v.extend(0u32.to_le_bytes());
        v.push(encoded);
        v.extend_from_slice(hops);
        v
    }

    /// **The route is learned from the radio, and a zero-hop answer clears it.**
    ///
    /// Driven through [`refresh_peer_route`] and a real [`Companion`] over [`MemoryLink`], not
    /// through `PeerPath::reversed` alone: the bug this covers was in the caller — a `None` from the
    /// reverse was ignored, so a phone that came home kept a repeater chain that no longer reached
    /// it and went deaf until the daemon restarted. A test of the helper alone could not see that.
    #[test]
    fn the_route_is_learned_and_a_zero_hop_answer_clears_it() {
        let peer = [7u8; 32];
        let mut link = MemoryLink::new();
        link.push_payload(&advert_path_frame(0x02, &[0x11, 0x22]));
        let mut companion = Companion::new(link);
        let mut path: Option<PeerPath> = None;
        assert!(refresh_peer_route(&mut companion, Some(&peer), &mut path), "an answer arrived");
        let learned = path.clone().expect("two hops is a route");
        assert_eq!(learned.bytes, vec![0x22, 0x11], "reversed into sending order");
        assert_eq!((learned.hash_size, learned.hops), (1, 2));

        // The same phone, back in earshot: its advert arrives with no hops.
        companion.link_mut().push_payload(&advert_path_frame(0x00, &[]));
        assert!(refresh_peer_route(&mut companion, Some(&peer), &mut path), "an answer arrived");
        assert!(path.is_none(), "no hops means no route, not the stale one");

        // ...and each request carries the whole key, which is the firmware's guard
        // (`len >= PUB_KEY_SIZE + 2`): two commands, each `[0x3C][u16 len][0x2A][0][key]`.
        let written = &companion.link_mut().written;
        assert_eq!(written.len(), 2 * (3 + 2 + 32), "two 34-byte command frames");
        for frame in written.chunks(3 + 2 + 32) {
            assert_eq!(frame[3], ldmux_companion::commands::GET_ADVERT_PATH);
            assert_eq!(frame[4], 0, "reserved");
            assert_eq!(&frame[5..], &peer, "the key, not a six-byte prefix");
        }
    }

    #[test]
    fn the_advert_path_is_reversed_by_hop_for_the_reply() {
        // One-byte hashes, two hops.
        let path = PeerPath::reversed(1, 2, &[0x11, 0x22]).expect("two hops is a route");
        assert_eq!(path.bytes, vec![0x22, 0x11]);
        assert_eq!((path.hash_size, path.hops), (1, 2));

        // Two-byte hashes: the pairs must not be split.
        let path = PeerPath::reversed(2, 2, &[0xA1, 0xA2, 0xB1, 0xB2]).expect("two hops is a route");
        assert_eq!(path.bytes, vec![0xB1, 0xB2, 0xA1, 0xA2]);
        assert_eq!((path.hash_size, path.hops), (2, 2));
    }

    #[test]
    fn a_route_hint_names_the_peers_radio() {
        let mut record = ROUTE_HINT_MAGIC.to_vec();
        record.extend_from_slice(&[7u8; 32]);
        assert_eq!(route_hint(&record), Some([7u8; 32]));
        // Anything else is a session record: a short one, a long one, or a different magic.
        assert_eq!(route_hint(&[0u8; 36]), None);
        assert_eq!(route_hint(&record[..35]), None);
        assert_eq!(route_hint(&[9u8; 37]), None);
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
        // 500 kHz / SF8: about 127 ms for 172 bytes, so a one-percent hour is about 283 records
        // and the one-byte wire field reports that as its 255 ceiling. The faster deployed tuple
        // (SF7, 69 ms) would allow 520 and is also capped by the field, not by the policy.
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

    /// **The radio floods an advert so a peer can learn a path.** Without one, a phone behind a
    /// repeater has no route to this host at all: a direct raw packet with an empty path reaches
    /// only the radios in earshot.
    #[test]
    fn the_radio_advertises_flooded() {
        let mut link = MemoryLink::new();
        link.push_payload(&[0x00]); // OK for the advert
        let mut companion = Companion::new(link);
        companion.send_self_advert(true).unwrap();
        // `[0x3C][u16 len][0x07][1]`: CMD_SEND_SELF_ADVERT with the flood mode.
        let written = &companion.link_mut().written;
        assert_eq!(written[0], 0x3C, "frame start");
        let body = &written[3..];
        assert_eq!(body[0], ldmux_companion::commands::SEND_SELF_ADVERT);
        assert_eq!(body[1], 1, "flood, not zero-hop");
    }

    /// Off means off, and a malformed value falls back to the default rather than silently
    /// disabling adverts.
    #[test]
    fn the_advert_interval_is_configurable() {
        // SAFETY: this key is read only by `advert_interval`, and this test is the only place in
        // the binary that writes it, so there is nothing to race with.
        unsafe { std::env::remove_var("LDMUX_MESH_ADVERT_SECS") };
        assert_eq!(advert_interval(), Some(Duration::from_secs(300)), "the default");
        // SAFETY: as above; this test owns the key for its whole body.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "0") };
        assert_eq!(advert_interval(), None, "off means off");
        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "60") };
        assert_eq!(advert_interval(), Some(Duration::from_secs(60)));
        // SAFETY: as above. A malformed value falls back to the default rather than turning
        // adverts off, because "off" is a decision the operator makes on purpose.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "often") };
        assert_eq!(advert_interval(), Some(Duration::from_secs(300)));
        // SAFETY: as above.
        unsafe { std::env::remove_var("LDMUX_MESH_ADVERT_SECS") };
    }

    /// **The countdown is republished while nothing is sent.** The session gates every send on it,
    /// so a status tied to transmission was a status that could never say the hour had rolled over:
    /// at zero the session polled nothing, the radio sent nothing, and the phone waited on a host
    /// whose budget would never come back (measured 2026-11-06, `held back 3959 times`).
    #[test]
    fn the_budget_is_republished_while_the_link_is_idle() {
        let companion = Companion::new(MemoryLink::new());
        let (_command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    airtime_ms: 137.9,
                    preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
                        public_key: [7u8; 32],
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });

        // The opening status, then another with no command in between: the refresh.
        let mut statuses = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        while statuses < 2 && Instant::now() < deadline {
            match event_rx.blocking_recv() {
                Some(Ok(Event::Status(_))) => statuses += 1,
                Some(Ok(_)) => {}
                other => panic!("expected a status, got {other:?}"),
            }
        }
        assert!(statuses >= 2, "the countdown refreshes on its own");
        drop(_command_tx);
        let _ = worker.join();
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
                Settings {
                    hash_size: 1,
                    airtime_ms: 137.9,
                    preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
                        public_key: [7u8; 32],
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
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
                Settings {
                    hash_size: 1,
                    airtime_ms: 137.9,
                    preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
                        public_key: [7u8; 32],
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
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
                Settings {
                    hash_size: 1,
                    airtime_ms: 137.9,
                    preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
                        public_key: [7u8; 32],
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });
        let _ = event_rx.blocking_recv(); // the opening status
        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_status_carries_a_budget_so_the_loop_can_send() {
        let settings = Settings {
                    hash_size: 1,
                    airtime_ms: 137.9,
                    preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
                        public_key: [7u8; 32],
                    },
                };
        let status = status(&settings, 255, 0);
        assert_eq!(status.ops_left_this_hour, Some(255));
        assert_eq!(status.radio_state, RadioState::Connected);
    }
}
