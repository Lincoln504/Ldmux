//! The daemon's end of the L3 session: the gateway side of the Noise handshake, then the
//! encrypted, ordered stream over opaque L2 records.
//!
//! [`SessionHost`] is synchronous and holds no socket and no clock: the L2 loop in
//! [`crate::mesh`] feeds it records and time and sends what it returns. That keeps every
//! rule here (airtime budget, handshake gating, persistence, bounded queues) testable
//! without a radio, and keeps all radio knowledge on the far side of the L2 contract.
//!
//! Persistence is two files in the state directory, both `0600`:
//!
//! - `mesh-session.key`: the gateway's static key, 64 bytes (private then public), made on
//!   first use. `snow` cannot derive a public key from a private one, so both are stored.
//! - `mesh-pairing.json`: the pinned phone key and the last accepted handshake timestamp.
//!   If this cannot be written, the handshake is **refused**: an unrecorded timestamp
//!   would reopen replay of message 1.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ldmux_session::arq::{Delivered, Limits, Stats};
use ldmux_session::record::RecordError;
use ldmux_session::handshake::{self, HandshakeError, Keypair, PairingEntry, Responder};
use ldmux_session::{Inbound, Sealed, Session};

/// Records an accepted handshake before it is answered.
pub type Persister = Box<dyn FnMut(&PairingEntry) -> std::io::Result<()> + Send>;

/// Inbound application bytes the daemon keeps for its (future) terminal bridge. Bounded: a
/// consumer that is not draining makes the oldest go, it does not make memory grow.
pub const INBOUND_MAX: usize = 1024;
/// Chunks the application may have queued toward the phone.
pub const OUTBOUND_MAX_BYTES: usize = 8 * 1024;

/// How many control records may ride one tick, whatever the budget says. The handshake reply is one
/// record; the spare two are for a reply that was re-queued after a fresh `msg1` in the same tick.
const CONTROL_RECORDS: u8 = 3;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SessionSnapshot {
    /// `off` (no PSK configured), `waiting` (no handshake on this link yet), `established`.
    /// A session that dies (retries exhausted, peer close) goes back to `waiting` with the
    /// reason in `last_error`.
    pub state: &'static str,
    /// The gateway's static public key, hex: what a phone must be given to pair.
    pub public_key: String,
    /// The pinned phone key, hex, once one has handshaken.
    pub peer_key: Option<String>,
    pub handshakes: u32,
    pub handshakes_refused: u32,
    pub sent: u32,
    pub retransmits: u32,
    pub received_bytes: u64,
    pub dropped: u32,
    pub pending: usize,
    pub budget_stalls: u32,
    pub last_error: Option<String>,
}

impl SessionSnapshot {
    #[must_use]
    pub fn off() -> Self {
        Self {
            state: "off",
            ..Self::default()
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "publicKey": self.public_key,
            "peerKey": self.peer_key,
            "handshakes": self.handshakes,
            "handshakesRefused": self.handshakes_refused,
            "sent": self.sent,
            "retransmits": self.retransmits,
            "receivedBytes": self.received_bytes,
            "dropped": self.dropped,
            "pending": self.pending,
            "budgetStalls": self.budget_stalls,
            "lastError": self.last_error,
        })
    }
}

/// What the gateway's side of one L2 connection knows about the session.
pub struct SessionHost {
    responder: Responder,
    pairing: PairingEntry,
    persist: Persister,
    session: Option<Session>,
    /// One record's airtime at the carrier's tuple, for the session's retransmission timing.
    airtime_ms: Option<f64>,
    /// Handshake replies waiting for budget. Sent before any session record.
    outbox: VecDeque<Vec<u8>>,
    next_index: u8,
    record_limit: usize,
    pub inbound: VecDeque<(u8, Vec<u8>)>,
    /// Records dropped because the queue was full. Counted so the loss is visible.
    pub dropped_inbound: u64,
    refused: u32,
    handshakes: u32,
    last_error: Option<String>,
}

impl SessionHost {
    pub fn new(
        static_key: Keypair,
        pairing: PairingEntry,
        record_limit: usize,
        persist: Persister,
        // How long a record takes on the air, so retransmission timing follows the radio.
        airtime_ms: Option<f64>,
    ) -> Self {
        Self {
            responder: Responder::new(static_key),
            pairing,
            persist,
            airtime_ms,
            session: None,
            outbox: VecDeque::new(),
            next_index: 1,
            record_limit,
            inbound: VecDeque::new(),
            dropped_inbound: 0,
            refused: 0,
            handshakes: 0,
            last_error: None,
        }
    }

    /// Re-size the session's records because **the path changed**.
    ///
    /// The limit is advertised on bind, but the route to the peer is learned after that and moves
    /// with it: every path byte costs a record byte. A session created at 171 bytes and then routed
    /// through a repeater can no longer build a full record, so the carrier reports the new limit
    /// and this hands it to the session. Nothing is lost if there is no session yet — the figure is
    /// kept for the next handshake.
    pub fn set_record_limit(&mut self, record_limit: usize) {
        self.record_limit = record_limit;
        if let Some(session) = self.session.as_mut() {
            session.set_record_limit(record_limit);
        }
    }

    /// One record arrived from the tunnel. `now_ms` is monotonic (ARQ timers); `wall_ms` is Unix
    /// time (the handshake timestamp window).
    ///
    /// **A `msg1` while a session is live replaces it.** The host is the responder and keeps no
    /// rekey timer: `Session::needs_rekey` (twelve hours, or 2^16 records) is answered on the
    /// phone's side, and the phone opens a fresh handshake when it fires. Replacement here is
    /// what makes that timer sufficient — the old session's outbox is dropped, a new index is
    /// taken, and a record still in flight under the old key fails its tag rather than opening.
    pub fn on_record(&mut self, now_ms: u64, wall_ms: u64, record: &[u8]) {
        match record.first().copied() {
            Some(handshake::KIND_MSG1) => self.on_handshake(wall_ms, now_ms, record),
            Some(ldmux_session::record::KIND_TRANSPORT) => {
                let Some(session) = self.session.as_mut() else {
                    // Transport data with no session: stale after a restart. Not an error
                    // worth a message per record; the phone will handshake again.
                    self.last_error = Some("transport record with no session".to_owned());
                    return;
                };
                match session.receive(now_ms, record) {
                    Ok(Inbound::Data(chunks)) => self.take(chunks),
                    Ok(Inbound::Quiet) => {}
                    Ok(Inbound::Closed) => {
                        self.session = None;
                        self.last_error = Some("the phone closed the session".to_owned());
                    }
                    // **A duplicate is the repair path, not a fault.** The phone retransmits
                    // because an acknowledgement was lost, and that retransmission arrives as
                    // `Duplicate` or `TooOld`. Reporting it as `last_error` made the one
                    // diagnostic line the operator reads say the link was dropping records every
                    // time it recovered from a lost ack; the session's own `dropped` counter still
                    // counts it. A bad tag, a wrong index or a malformed record is a fault and is
                    // still named.
                    Err(ldmux_session::SessionError::Record(
                        RecordError::Duplicate | RecordError::TooOld,
                    )) => {}
                    Err(e) => self.last_error = Some(format!("record dropped: {e:?}")),
                }
            }
            _ => self.last_error = Some("unknown record kind".to_owned()),
        }
    }

    fn take(&mut self, chunks: Vec<Delivered>) {
        for c in chunks {
            // The ARQ above has already acknowledged this record to the phone, so a drop here
            // is bytes that reach the session layer and never reach the pty, with nothing said.
            // Count and name it: a large paste or a slow keeper shows up as a number in
            // `mesh status` instead of as a shell that quietly ate half a line.
            if self.inbound.len() >= INBOUND_MAX {
                self.inbound.pop_front();
                self.dropped_inbound = self.dropped_inbound.saturating_add(1);
                if self.dropped_inbound.is_power_of_two() {
                    eprintln!(
                        "ldmuxd: mesh inbound queue full, dropped the oldest record \
                         ({} dropped so far)",
                        self.dropped_inbound
                    );
                }
            }
            self.inbound.push_back((c.shell, c.payload));
        }
    }

    /// Clear the pinned key after a new pairing grant. The caller persists it through the same
    /// closure that records a handshake.
    pub fn clear_pin(&mut self) -> std::io::Result<()> {
        let mut updated = self.pairing.clone();
        updated.pinned = None;
        (self.persist)(&updated)?;
        self.pairing = updated;
        Ok(())
    }

    fn on_handshake(&mut self, wall_ms: u64, now_ms: u64, msg1: &[u8]) {
        let index = self.next_index;
        let accepted =
            match self
                .responder
                .accept(std::slice::from_ref(&self.pairing), wall_ms, msg1, index)
            {
                Ok(a) => a,
                Err(e) => {
                    self.refused += 1;
                    self.last_error = Some(format!("handshake refused: {}", describe(e)));
                    return;
                }
            };
        let mut updated = self.pairing.clone();
        updated.pinned = Some(accepted.remote_static);
        updated.last_timestamp_ms = accepted.timestamp_ms;
        if let Err(e) = (self.persist)(&updated) {
            self.refused += 1;
            self.last_error = Some(format!("handshake refused: could not record it ({e})"));
            return;
        }
        match Session::with_airtime(
            accepted.established,
            self.record_limit,
            now_ms,
            self.airtime_ms,
        ) {
            Ok(session) => {
                self.pairing = updated;
                self.session = Some(session);
                self.outbox.clear();
                self.outbox.push_back(accepted.reply);
                self.next_index = self.next_index.wrapping_add(1).max(1);
                self.handshakes += 1;
                self.last_error = None;
            }
            Err(e) => {
                self.refused += 1;
                self.last_error = Some(format!("session refused: {e:?}"));
            }
        }
    }

    /// Queue application bytes toward the phone. Fails, visibly, when there is no session or the
    /// queue is full; it never buffers across a gap.
    pub fn send(&mut self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Err("no session is established".to_owned());
        };
        // Measured from the queue that actually holds the bytes, not from a counter of our own
        // that only reset when the queue was completely empty.
        if session.arq().queued_bytes() + bytes.len() > OUTBOUND_MAX_BYTES {
            return Err("the outbound queue is full".to_owned());
        }
        session.send(shell, bytes).map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    /// Records to hand to the tunnel now: handshake replies first, then data bounded by
    /// `min(records_left, l2_slots)`.
    ///
    /// **A handshake reply is control traffic and is never gated by the budget.** It used to share the
    /// same gate as data, and `records_left` is `budget.unwrap_or(0)` on the calling side - so a
    /// gateway that had not yet reported a budget blocked the very exchange that carries the
    /// information needed to report one. Acks and retransmissions are already exempt on the phone
    /// (`AirtimePolicy::ownedChannel`); this is the host's half of the same rule: replies, acks and
    /// heartbeats are sent, full stop.
    pub fn poll(&mut self, now_ms: u64, limits: Limits) -> Vec<Vec<u8>> {
        self.poll_with_seq(now_ms, limits)
            .into_iter()
            .map(|s| s.bytes)
            .collect()
    }

    /// As [`SessionHost::poll`], but each record keeps the ARQ sequence number that names it.
    ///
    /// The L2 loop needs the number: the radio's `SEND_RESULT` echoes whatever the caller wrote
    /// into the send frame, and a refusal can only be handed back to the ARQ by the slot's own
    /// sequence. Queued handshake replies are control traffic with no ARQ slot, so their `seq` is
    /// `None` and a refusal of one is only reported.
    pub fn poll_with_seq(&mut self, now_ms: u64, limits: Limits) -> Vec<Sealed> {
        let mut out: Vec<Sealed> = Vec::new();
        let mut control = CONTROL_RECORDS;
        while control > 0 && usize::from(limits.l2_slots) > out.len() {
            match self.outbox.pop_front() {
                Some(bytes) => {
                    // A handshake reply the caller queued: control traffic, never charged to the
                    // airtime budget (the exchange that establishes the session must go out).
                    out.push(Sealed { seq: None, bytes, new_data: false });
                    control -= 1;
                }
                None => break,
            }
        }
        let data_slots = limits.l2_slots.saturating_sub(out.len() as u8);
        let budget = limits.records_left.min(data_slots);
        if let Some(session) = self.session.as_mut() {
            // A handshake reply still waiting for budget must not be overtaken by data.
            if self.outbox.is_empty() {
                let rest = Limits {
                    records_left: budget,
                    l2_slots: budget,
                };
                match session.poll_with_seq(now_ms, rest) {
                    Ok(records) => out.extend(records),
                    Err(e) => self.last_error = Some(format!("seal failed: {e:?}")),
                }

            }
            if let Some(failure) = session.arq().failure() {
                self.last_error = Some(match failure {
                    ldmux_session::arq::Failure::RetriesExhausted => {
                        "the phone stopped acknowledging: retries exhausted".to_owned()
                    }
                    ldmux_session::arq::Failure::RadioRefused => format!(
                        "the radio kept refusing a record without transmitting it for {} s",
                        ldmux_session::arq::REFUSAL_DEADLINE_MS / 1000
                    ),
                });
                self.session = None;
            }
        }
        out
    }

    /// The radio refused a record without transmitting it.
    ///
    /// Routed to the ARQ so the refusal gives its attempt back: a packet that never reached the
    /// air is not a transmission, and the retry ladder must not advance over one. Returns `true`
    /// when that sequence number was in flight.
    pub fn on_send_refused(&mut self, now_ms: u64, seq: u16) -> bool {
        self.session
            .as_mut()
            .is_some_and(|s| s.arq_mut().on_refused(seq, now_ms))
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.session.as_ref().and_then(|s| s.arq().next_deadline())
    }

    #[must_use]
    pub fn established(&self) -> bool {
        self.session.is_some()
    }

    /// `true` when there is something that needs `poll` soon.
    #[must_use]
    pub fn wants_poll(&self) -> bool {
        !self.outbox.is_empty() || self.session.as_ref().is_some_and(|s| s.arq().pending() > 0)
    }

    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        let stats: Stats = self
            .session
            .as_ref()
            .map(|s| s.arq().stats())
            .unwrap_or_default();
        SessionSnapshot {
            state: if self.session.is_some() {
                "established"
            } else {
                "waiting"
            },
            public_key: hex(&self.responder.static_key.public),
            peer_key: self.pairing.pinned.map(|k| hex(&k)),
            handshakes: self.handshakes,
            handshakes_refused: self.refused,
            sent: stats.sent,
            retransmits: stats.retransmits,
            received_bytes: stats.delivered_bytes,
            dropped: self.session.as_ref().map_or(0, |s| s.dropped),
            pending: self.session.as_ref().map_or(0, |s| s.arq().pending()),
            budget_stalls: stats.budget_stalls,
            last_error: self.last_error.clone(),
        }
    }
}

fn describe(e: HandshakeError) -> &'static str {
    match e {
        HandshakeError::Malformed => "malformed",
        HandshakeError::BadFilter => "unknown or wrong pre-shared key",
        HandshakeError::Stale => "timestamp outside the window (is the phone's clock right?)",
        HandshakeError::Replay => "replayed or out-of-order timestamp",
        HandshakeError::PinMismatch => "a different phone key presented the pairing's key",
        HandshakeError::Crypto => "cryptographic failure",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Both files, loaded or created.
pub struct SessionFiles {
    pub key: Keypair,
    pub pairing: PairingEntry,
    pairing_path: PathBuf,
}

impl SessionFiles {
    /// The pairing as last written to disk. Each L2 connection starts from this, not from the
    /// copy loaded at startup: the handshake timestamp an earlier connection accepted must
    /// still bar a replay of that same message 1 after a reconnect.
    /// Clear the pinned phone key while keeping the replay timestamp. A new pairing grant must
    /// let a re-enrolled phone present a new static key.
    pub fn clear_pin(&self) -> std::io::Result<()> {
        let mut pairing = self.current_pairing();
        pairing.pinned = None;
        write_pairing(&self.pairing_path, &pairing)
    }

    pub fn current_pairing(&self) -> PairingEntry {
        match fs::read_to_string(&self.pairing_path)
            .ok()
            .as_deref()
            .and_then(parse_pairing)
        {
            Some((pinned, last)) => PairingEntry {
                psk: self.pairing.psk,
                pinned,
                last_timestamp_ms: last,
            },
            None => self.pairing.clone(),
        }
    }
}

/// The files the mesh route cannot lose: the static key the phone pinned, the pinned phone
/// key with its replay floor, and the pairing's secret.
pub const PERSISTED_FILES: [&str; 3] = ["mesh-session.key", "mesh-pairing.json", "mesh-psk"];

/// Move those three from `legacy` (a directory that does not survive a reboot) to
/// `persist`, and return the names moved.
///
/// **Why this exists.** Under systemd the state directory is `%t/ldmux`, a tmpfs, so it is
/// empty on every boot. The bearer token was moved out of it for that reason
/// (`deploy/ldmux.service`); the mesh static key was left behind. After a reboot the host
/// generated a new key, the phone's pinned copy no longer matched, `read_message` failed as
/// `Crypto`, no second handshake message was sent, the phone burned its three attempts and
/// landed on `SessionFailed` — and nothing anywhere said the machine had simply rebooted.
///
/// Best effort and idempotent: a file already at the destination wins and the legacy copy is
/// left in place (a copy that reappeared is worth seeing, not deleting). A rename across
/// filesystems is refused by the kernel with `EXDEV`, so the fallback copies, fsyncs and
/// unlinks through the same 0600 writer the file was made with.
pub fn adopt_legacy(legacy: &Path, persist: &Path) -> Vec<String> {
    let mut moved = Vec::new();
    if legacy == persist {
        return moved;
    }
    for name in PERSISTED_FILES {
        let from = legacy.join(name);
        let to = persist.join(name);
        if to.exists() || !from.exists() {
            continue;
        }
        if std::fs::rename(&from, &to).is_ok() {
            moved.push(name.to_string());
            continue;
        }
        // Different filesystems, or the rename was refused: copy through the private writer
        // and only then drop the old copy.
        let Ok(bytes) = std::fs::read(&from) else {
            continue;
        };
        if write_private(&to, &bytes).is_err() {
            continue;
        }
        let _ = std::fs::remove_file(&from);
        moved.push(name.to_string());
    }
    moved
}

impl SessionFiles {
    /// `psk` comes from the environment; everything else lives in `persist_dir`, which must
    /// survive a reboot (see [`adopt_legacy`] for why).
    pub fn load(state_dir: &Path, psk: [u8; 32]) -> Result<Self, String> {
        let key_path = state_dir.join("mesh-session.key");
        let key = match fs::read(&key_path) {
            Ok(b) if b.len() == 64 => {
                let mut private = [0u8; 32];
                let mut public = [0u8; 32];
                private.copy_from_slice(&b[..32]);
                public.copy_from_slice(&b[32..]);
                Keypair::from_parts(private, public)
            }
            Ok(_) => return Err(format!("{} is not a 64-byte key file", key_path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Keypair::generate().map_err(|e| format!("{e:?}"))?;
                let mut bytes = Vec::with_capacity(64);
                bytes.extend_from_slice(&key.private);
                bytes.extend_from_slice(&key.public);
                write_private(&key_path, &bytes).map_err(|e| e.to_string())?;
                key
            }
            Err(e) => return Err(format!("{}: {e}", key_path.display())),
        };
        let pairing_path = state_dir.join("mesh-pairing.json");
        let (pinned, last) = match fs::read_to_string(&pairing_path) {
            Ok(text) => parse_pairing(&text)
                .ok_or_else(|| format!("{} is unreadable", pairing_path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, 0),
            Err(e) => return Err(format!("{}: {e}", pairing_path.display())),
        };
        Ok(Self {
            key,
            pairing: PairingEntry {
                psk,
                pinned,
                last_timestamp_ms: last,
            },
            pairing_path,
        })
    }

    /// A copy of the static key (each L2 connection builds its own host).
    #[must_use]
    pub fn clone_key(&self) -> Keypair {
        Keypair::from_parts(self.key.private, self.key.public)
    }

    /// The closure the host calls to record an accepted handshake.
    pub fn persister(&self) -> Persister {
        let path = self.pairing_path.clone();
        Box::new(move |p: &PairingEntry| write_pairing(&path, p))
    }
}

fn write_pairing(path: &Path, pairing: &PairingEntry) -> std::io::Result<()> {
    let json = serde_json::json!({
        "pinned": pairing.pinned.map(|k| hex(&k)),
        "lastTimestampMs": pairing.last_timestamp_ms,
    });
    write_private(path, json.to_string().as_bytes())
}

fn parse_pairing(text: &str) -> Option<(Option<[u8; 32]>, u64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let last = v.get("lastTimestampMs")?.as_u64()?;
    let pinned = match v.get("pinned")? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.len() == 64 => {
            let mut k = [0u8; 32];
            for (i, b) in k.iter_mut().enumerate() {
                *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
            }
            Some(k)
        }
        _ => return None,
    };
    Some((pinned, last))
}

/// Write a secret file `0600` from the first byte, atomically (write a sibling, then rename).
/// Write a file that only this uid may read, so a crash mid-write cannot leave half of it.
///
/// **The bytes reach the disk before the name does, and the directory is synced after the rename.**
/// The rename is atomic against a *reader*, not against a power cut: without the file's own `fsync`
/// the directory entry can land while the data is still in the page cache, and ext4's documented
/// outcome for that pair is a zero-length file. Without the directory's `fsync` the rename itself may
/// not survive, which for `mesh-pairing.json` means losing the pinned phone key and the replay floor.
/// Both of those are exactly what these files exist to prevent, so this is the one place that writes
/// them.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_session::arq::Limits;
    use ldmux_session::handshake::Initiator;
    use std::sync::{Arc, Mutex};

    const PSK: [u8; 32] = [5; 32];
    const WALL: u64 = 1_800_000_000_000;

    fn host(persist_ok: bool) -> (SessionHost, [u8; 32], Arc<Mutex<Vec<PairingEntry>>>) {
        let key = Keypair::generate().unwrap();
        let public = key.public;
        let saved = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&saved);
        let persist: Persister = Box::new(move |p| {
            if !persist_ok {
                return Err(std::io::Error::other("disk full"));
            }
            s.lock().unwrap().push(p.clone());
            Ok(())
        });
        let h = SessionHost::new(
            key,
            PairingEntry {
                psk: PSK,
                pinned: None,
                last_timestamp_ms: 0,
            },
            172,
            persist,
            None,
        );
        (h, public, saved)
    }

    #[test]
    fn a_handshake_is_answered_whatever_the_budget_and_data_is_still_gated() {
        let (mut h, gw_pub, saved) = host(true);
        let phone = Keypair::generate().unwrap();
        let (_i, m1) = Initiator::start(&phone, &gw_pub, &PSK, WALL).unwrap();
        h.on_record(0, WALL, &m1);
        assert!(h.established());
        assert_eq!(
            saved.lock().unwrap().len(),
            1,
            "the timestamp is persisted before the reply"
        );
        // **No airtime does not hold the reply.** A handshake answer is control traffic: gating it on
        // a budget meant that a gateway which had not yet reported one blocked the exchange that
        // carries the information needed to report one (measured 2026-11-01: five accepted
        // handshakes, zero records sent, the phone re-keying for ever).
        let out = h.poll(
            0,
            Limits {
                records_left: 0,
                l2_slots: 8,
            },
        );
        assert_eq!(out.len(), 1, "the reply goes out with no budget at all");
        assert_eq!(out[0].len(), 50);
        assert!(!h.wants_poll(), "and nothing is left waiting");

        // Data, by contrast, is still gated: with the reply gone and no budget, the session record
        // must not be sealed.
        h.send(0, &[7u8; 4]).unwrap();
        assert!(
            h.poll(
                1,
                Limits {
                    records_left: 0,
                    l2_slots: 8
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_wiped_runtime_dir_leaves_the_identity_and_the_psk_alone() {
        // The bug this pins: under systemd the state dir is `%t/ldmux`, a tmpfs, so every boot
        // emptied it, the static key was re-minted, the phone's pinned host key stopped matching
        // and the radio route died with no message. The credential files now live in the persist
        // dir; a runtime dir that vanished must change nothing.
        let root = std::env::temp_dir().join(format!("ldmux-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let persist = root.join("persist");
        let runtime = root.join("run");
        std::fs::create_dir_all(&persist).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();

        let first = SessionFiles::load(&persist, PSK).unwrap();
        let public = first.key.public;
        assert!(persist.join("mesh-session.key").exists());
        // The runtime dir is emptied, as a reboot does, and the daemon starts again.
        std::fs::remove_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let moved = adopt_legacy(&runtime, &persist);
        assert!(moved.is_empty(), "nothing to adopt: {moved:?}");
        let second = SessionFiles::load(&persist, PSK).unwrap();
        assert_eq!(second.key.public, public);
        assert_eq!(second.pairing.psk, PSK);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_files_left_in_a_tmpfs_state_dir_are_adopted_once() {
        let root = std::env::temp_dir().join(format!("ldmux-adopt2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let persist = root.join("persist");
        let legacy = root.join("run");
        std::fs::create_dir_all(&persist).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();

        // A daemon from before the split: everything is in the runtime dir.
        let old = SessionFiles::load(&legacy, PSK).unwrap();
        let public = old.key.public;
        let mut pairing = old.current_pairing();
        pairing.pinned = Some([9; 32]);
        old.persister()(&pairing)
            .expect("the test's own directory accepts the pairing");

        let moved = adopt_legacy(&legacy, &persist);
        assert_eq!(moved.len(), 2, "{moved:?}");
        let adopted = SessionFiles::load(&persist, PSK).unwrap();
        assert_eq!(adopted.key.public, public, "the phone's pin still matches");
        assert_eq!(adopted.current_pairing().pinned, Some([9; 32]));

        // Idempotent: a second pass moves nothing.
        assert!(adopt_legacy(&legacy, &persist).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn when_the_pairing_cannot_be_recorded_the_handshake_is_refused() {
        let (mut h, gw_pub, _) = host(false);
        let phone = Keypair::generate().unwrap();
        let (_i, m1) = Initiator::start(&phone, &gw_pub, &PSK, WALL).unwrap();
        h.on_record(0, WALL, &m1);
        assert!(!h.established());
        assert!(
            h.snapshot()
                .last_error
                .unwrap()
                .contains("could not record")
        );
    }

    #[test]
    fn a_wrong_psk_or_stale_clock_is_refused_with_a_reason_and_counted() {
        let (mut h, gw_pub, _) = host(true);
        let phone = Keypair::generate().unwrap();
        let (_i, bad) = Initiator::start(&phone, &gw_pub, &[1; 32], WALL).unwrap();
        h.on_record(0, WALL, &bad);
        assert!(h.snapshot().last_error.unwrap().contains("pre-shared key"));
        let (_i, old) = Initiator::start(&phone, &gw_pub, &PSK, WALL - 3_600_000).unwrap();
        h.on_record(0, WALL, &old);
        assert!(h.snapshot().last_error.unwrap().contains("clock"));
        assert_eq!(h.snapshot().handshakes_refused, 2);
        assert!(!h.established());
    }

    #[test]
    fn a_new_grant_clears_the_pinned_phone_key_and_persists_it() {
        let (mut h, _, saved) = host(true);
        h.pairing.pinned = Some([7; 32]);
        h.clear_pin().unwrap();
        assert_eq!(h.pairing.pinned, None);
        assert_eq!(saved.lock().unwrap().last().unwrap().pinned, None);
    }

    #[test]
    fn nothing_is_sent_without_a_session_and_the_queue_is_bounded() {
        let (mut h, gw_pub, _) = host(true);
        assert!(h.send(0, b"x").is_err());
        let phone = Keypair::generate().unwrap();
        let (_i, m1) = Initiator::start(&phone, &gw_pub, &PSK, WALL).unwrap();
        h.on_record(0, WALL, &m1);
        assert!(h.send(0, &vec![0u8; OUTBOUND_MAX_BYTES]).is_ok());
        assert!(h.send(0, b"x").unwrap_err().contains("full"));
    }

    #[test]
    fn the_files_are_0600_and_the_key_and_pin_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("ldmux-session-files-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let a = SessionFiles::load(&dir, PSK).unwrap();
        let mode = |n: &str| fs::metadata(dir.join(n)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("mesh-session.key"), 0o600);
        let mut persist = a.persister();
        persist(&PairingEntry {
            psk: PSK,
            pinned: Some([9; 32]),
            last_timestamp_ms: 77,
        })
        .unwrap();
        assert_eq!(mode("mesh-pairing.json"), 0o600);
        let b = SessionFiles::load(&dir, PSK).unwrap();
        assert_eq!(
            b.key.public, a.key.public,
            "the gateway key is stable across restarts"
        );
        assert_eq!(b.pairing.pinned, Some([9; 32]));
        assert_eq!(b.pairing.last_timestamp_ms, 77);
        b.clear_pin().unwrap();
        let c = SessionFiles::load(&dir, PSK).unwrap();
        assert_eq!(c.pairing.pinned, None);
        assert_eq!(c.pairing.last_timestamp_ms, 77, "the replay bar is kept");
        fs::write(dir.join("mesh-session.key"), b"short").unwrap();
        assert!(
            SessionFiles::load(&dir, PSK).is_err(),
            "a corrupt key is an error, not a new key"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
    /// A fresh `msg1` on a live session replaces it, which is what the phone's twelve-hour rekey
    /// depends on.
    ///
    /// The host is the responder and has no rekey timer of its own: `Session::needs_rekey` is
    /// answered on the phone's side (twelve hours or 2^16 records) and the phone starts a new
    /// handshake. If this side refused a handshake while a session was live, that timer could
    /// never complete, and a long-lived link would keep its keys for ever.
    #[test]
    fn a_retransmitted_record_is_the_repair_path_and_is_not_an_error() {
        let (mut h, gw_pub, _saved) = host(true);
        let phone = Keypair::generate().unwrap();
        let (init, m1) = Initiator::start(&phone, &gw_pub, &PSK, WALL).unwrap();
        h.on_record(0, WALL, &m1);
        let reply = h.poll(
            0,
            Limits {
                records_left: 255,
                l2_slots: 8,
            },
        );
        assert_eq!(reply.len(), 1, "the handshake reply is the only record");
        let mut sender = Session::new(init.finish(&reply[0]).unwrap(), 172, 0).unwrap();
        sender.send(3, b"hello").unwrap();
        let records = sender
            .poll(
                0,
                Limits {
                    records_left: 255,
                    l2_slots: 8,
                },
            )
            .unwrap();
        let first = records.first().expect("one sealed record");
        h.on_record(1, WALL, first);
        // The ack was lost, so the phone sends the identical record again.
        h.on_record(2, WALL, first);
        assert_eq!(
            h.last_error, None,
            "a duplicate is how a lost ack is repaired, not a fault worth naming"
        );
        assert_eq!(
            h.snapshot().dropped, 1,
            "it is counted, just not reported as the link's last error"
        );
    }

    #[test]
    fn a_second_handshake_replaces_the_live_session() {
        let (mut h, gw_pub, saved) = host(true);
        let phone = Keypair::generate().unwrap();
        let (_i1, m1) = Initiator::start(&phone, &gw_pub, &PSK, WALL).unwrap();
        h.on_record(0, WALL, &m1);
        assert_eq!(h.handshakes, 1);
        assert_eq!(h.outbox.len(), 1, "the first reply is queued");
        // The phone's timer fires: a second msg1, twelve hours later (the timestamp window
        // rejects a repeat of the first) and from the same key.
        let later = WALL + 12 * 60 * 60 * 1000;
        let (_i2, m2) = Initiator::start(&phone, &gw_pub, &PSK, later).unwrap();
        h.on_record(0, later, &m2);
        assert_eq!(h.handshakes, 2, "the second handshake was accepted");
        assert!(h.established());
        assert_eq!(h.outbox.len(), 1, "and answered, with the old reply cleared");
        assert_eq!(
            saved.lock().unwrap().len(),
            2,
            "each accepted handshake records its timestamp"
        );
    }

}
