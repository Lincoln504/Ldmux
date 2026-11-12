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

use ldmux_session::arq::{Delivered, Limits, Stats};
use ldmux_session::bootstrap::{AnswerCache, Answered};
use ldmux_session::record::RecordError;
use ldmux_session::handshake::{self, Keypair, PairingEntry, Responder};

use crate::mesh_identity::{Persister, SessionSnapshot};
use ldmux_session::{Inbound, Sealed, Session};

/// Inbound application bytes the daemon keeps for its (future) terminal bridge. Bounded: a
/// consumer that is not draining makes the oldest go, it does not make memory grow.
pub const INBOUND_MAX: usize = 1024;
/// Chunks the application may have queued toward the phone.
pub const OUTBOUND_MAX_BYTES: usize = 8 * 1024;

/// How many control records may ride one tick, whatever the budget says. The handshake reply is one
/// record; the spare two are for a reply that was re-queued after a fresh `msg1` in the same tick.
const CONTROL_RECORDS: u8 = 3;

/// What the gateway's side of one L2 connection knows about the session.
pub struct SessionHost {
    /// Records this end refused to open, so the journal can tell "not sending" from "discarding".
    inbound_dropped: u32,
    /// Records this end opened successfully, which is how a session shows it is carrying.
    records_in: u64,
    responder: Responder,
    /// What this end already answered, so a repeated initiation costs an answer and nothing else.
    answers: AnswerCache,
    pairing: PairingEntry,
    persist: Persister,
    session: Option<Session>,
    /// One record's airtime at the carrier's tuple, for the session's retransmission timing.
    airtime_ms: Option<f64>,
    /// Handshake replies waiting for budget. Sent before any session record.
    outbox: VecDeque<Vec<u8>>,
    next_index: u8,
    record_limit: usize,
    /// Repeaters on the route to the phone, from the carrier's last `Limit`. See [`Self::set_hops`].
    hops: u8,
    /// When the live session was installed, so a replacement can say how long it lasted.
    session_since: Option<std::time::Instant>,
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
            answers: AnswerCache::plan_default(),
            pairing,
            persist,
            airtime_ms,
            session: None,
            outbox: VecDeque::new(),
            next_index: 1,
            record_limit,
            hops: 1,
            session_since: None,
            inbound: VecDeque::new(),
            dropped_inbound: 0,
            inbound_dropped: 0,
            records_in: 0,
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
    /// Records that opened on the live session, and handshakes accepted, so a caller can tell
    /// whether a session carried anything before it was replaced.
    pub fn carried(&self) -> (u32, u64) {
        (self.handshakes, self.records_in)
    }

    pub fn set_record_limit(&mut self, record_limit: usize) {
        self.record_limit = record_limit;
        if let Some(session) = self.session.as_mut() {
            session.set_record_limit(record_limit);
        }
    }

    /// How many repeaters the route to the phone crosses, from the carrier.
    ///
    /// **The first retransmission has to cover the whole path.** MeshCore's own direct-send budget is
    /// six airtimes *per hop* (`DIRECT_SEND_PERHOP_FACTOR`); a flat six airtimes is right for a
    /// neighbour and too short for a route, and the retransmissions it spends are records the
    /// allowance paid for. The measured round trip corrects the timer after the first exchange,
    /// which is why the first one is the one worth getting right. Kept for the next handshake too.
    pub fn set_hops(&mut self, hops: u8) {
        self.hops = hops.max(1);
        if let Some(session) = self.session.as_mut() {
            session.set_hops(self.hops);
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
            // One initiation, whole or in two parts. Both forms are the phone's, and both are
            // answered by the same idempotent cache (`RC-5`).
            Some(handshake::KIND_MSG1 | ldmux_session::direction::KIND_MSG1_FRAG) => {
                self.on_handshake(wall_ms, now_ms, record);
            }
            // The host originates both of these, so hearing one is this end's own transmission
            // coming back off a repeater. Named rather than falling through, because "ignored by
            // direction" and "a kind we do not know" are different facts about the link.
            Some(handshake::KIND_MSG2 | ldmux_session::direction::KIND_NEED_HANDSHAKE) => {}
            Some(ldmux_session::record::KIND_TRANSPORT) => {
                let Some(session) = self.session.as_mut() else {
                    // Transport data with no session: stale after a restart. Not an error
                    // worth a message per record; the phone will handshake again.
                    self.last_error = Some("transport record with no session".to_owned());
                    return;
                };
                match session.receive(now_ms, record) {
                    Ok(Inbound::Data(chunks)) => {
                        // **A record that opens is the only proof the chain carries.** Counted so
                        // the carrier can tell "the phone accepted our reply and went quiet" from
                        // "the phone never got it", and rotate the path in the second case.
                        self.records_in = self.records_in.saturating_add(1);
                        self.take(chunks)
                    }
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
                    Err(e) => {
                        self.last_error = Some(format!("record dropped: {e:?}"));
                        // **Said in the journal, not only in a status field.** A dropped record is
                        // the difference between "the phone is not sending" and "the phone's records
                        // are arriving and we are discarding them" — the two readings that a whole
                        // range investigation on 2026-11-10 could not separate, because the only
                        // trace was a field overwritten by the next event. Powers of two, so a bad
                        // key that drops a hundred records writes seven lines and not a hundred.
                        self.inbound_dropped = self.inbound_dropped.saturating_add(1);
                        if self.inbound_dropped.is_power_of_two() {
                            eprintln!(
                                "ldmuxd: mesh record dropped ({n} so far): {e:?}",
                                n = self.inbound_dropped
                            );
                        }
                    }
                }
            }
            // **A raw custom packet that is not ours is not a fault.** `RAW_CUSTOM` is a shared
            // channel: another tool on the mesh can send one, and a single flipped header bit
            // produces the same shape. Neither says anything about this session, and recording it
            // as `last_error` made the one diagnostic line the operator reads — `session replaced
            // after N s (hops=H, reason)` — name a stranger's packet instead of the real cause
            // (measured 2026-11-10: a fifteen-minute session at 28 hops replaced with
            // "unknown record kind" as its reason).
            _ => {}
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
        // A new grant is a new pin, so what was answered under the old one must not be replayed
        // back into a session. Clearing here is what makes "unpair, then pair again" clean.
        self.answers.clear();
        Ok(())
    }

    /// Answer one initiation, of either form, from the cache that makes it idempotent.
    ///
    /// **A repeated initiation is an answer, not a session.** Three copies of one `msg1` used to
    /// build three sessions with three indices, three outboxes cleared and three `fsync`s, so the
    /// phone and the host agreed on a session only when the arrival order happened to line up — and
    /// when it did not, every record the phone sent was refused by index or by tag (`RC-5`). The
    /// cache answers a repeat with the same answer it gave the first time, and nothing else changes:
    /// no session built, no index advanced, no outbox cleared, nothing written to disk.
    ///
    /// It also carries the two things a repeated initiation must never do: end a live session (an
    /// older initiation is refused as superseded) and amplify (one answer per peer per
    /// `ANSWER_RATE_MS`).
    fn on_handshake(&mut self, wall_ms: u64, now_ms: u64, msg1: &[u8]) {
        let index = self.next_index;
        let answered = self.answers.answer(
            &self.responder,
            std::slice::from_ref(&self.pairing),
            index,
            now_ms,
            wall_ms,
            msg1,
        );
        let answered = match answered {
            Ok(a) => a,
            Err(refusal) => {
                self.refused += 1;
                self.last_error = Some(format!("handshake refused: {}", refusal.describe()));
                return;
            }
        };
        let (reply, accepted) = match answered {
            Answered::Cached { reply } => {
                // The live session stands and its answer is re-sent with a fresh nonce: the copy
                // before this one may have been lost, and one record is what a retry costs.
                self.outbox.push_back(reply);
                return;
            }
            Answered::Held => return,
            Answered::Fresh {
                reply,
                established,
                remote_static,
                timestamp_ms,
                pairing,
                ..
            } => (
                reply,
                (established, remote_static, timestamp_ms, pairing),
            ),
        };
        let (established, remote_static, timestamp_ms, pairing) = accepted;
        let mut updated = self.pairing.clone();
        updated.pinned = Some(remote_static);
        updated.last_timestamp_ms = timestamp_ms;
        if let Err(e) = (self.persist)(&updated) {
            self.refused += 1;
            self.last_error = Some(format!("handshake refused: could not record it ({e})"));
            return;
        }
        debug_assert_eq!(pairing, 0, "one pairing file, so the only match is index 0");
        match Session::with_airtime(*established, self.record_limit, now_ms, self.airtime_ms) {
            Ok(session) => {
                // **A replaced session says how long it lived.** The phone re-handshakes for two very
                // different reasons — it lost a record or it decided the link was dead — and the
                // difference is the age of the session it abandoned plus how far away it was.
                // Without this line the journal only shows a rising handshake count, which is what
                // an investigation at range had to work from on 2026-11-09.
                if self.session.is_some() {
                    let lived = self.session_since.map_or(0, |t| t.elapsed().as_secs());
                    eprintln!(
                        "ldmuxd: session replaced after {lived}s (hops={}, {})",
                        self.hops,
                        self.last_error.as_deref().unwrap_or("no error recorded")
                    );
                }
                self.session_since = Some(std::time::Instant::now());
                self.pairing = updated;
                self.session = Some(session);
                self.outbox.clear();
                self.outbox.push_back(reply);
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
                // **The slot count is not the budget.** Passing `budget` for both told the ARQ there
                // was no room for a retransmission, a pure ack or the close once the hour was spent —
                // traffic the ARQ deliberately does not charge — so a spent EU budget stalled the
                // link instead of just the new data. The phone already passes them separately.
                let rest = Limits {
                    records_left: budget,
                    l2_slots: data_slots,
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
                    // The peer's queue had no room to hold the tail of a record that had to be
                    // split: the application sent faster than the link can carry, which is not a
                    // dead link. Reported so the operator knows to slow down, and the session is
                    // rebuilt because bytes were owed and are gone.
                    ldmux_session::arq::Failure::QueueFull => {
                        "the send queue filled while a record had to be split".to_owned()
                    }
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

    /// How long this session waits before retransmitting a record, in ms.
    ///
    /// **A route's first retry is its round trip.** The ARQ scales it at six airtimes per hop, so a
    /// caller that wants to say "the carriage has gone silent" has to ask the session rather than
    /// use a constant: on ten repeaters one round trip is thirty-six seconds, and a flat
    /// seventy-five would end the connection between the first retry and its answer.
    #[must_use]
    pub fn first_rto_ms(&self) -> u64 {
        self.session
            .as_ref()
            .map_or(0, |s| s.arq().config().rto_initial_ms)
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
            fast_retransmits: stats.fast_retransmits,
            received_bytes: stats.delivered_bytes,
            dropped: self.session.as_ref().map_or(0, |s| s.dropped),
            pending: self.session.as_ref().map_or(0, |s| s.arq().pending()),
            budget_stalls: stats.budget_stalls,
            last_error: self.last_error.clone(),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        // The answer carries a trailing nonce so a retried copy is not dropped as a duplicate; the
        // Noise content is the 50 bytes in front of it.
        assert_eq!(out[0].len(), ldmux_session::bootstrap::MSG2_NONCED_LEN);
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
        let mut sender = Session::new(
            init.finish(&reply[0][..ldmux_session::bootstrap::MSG2_CONTENT_LEN])
                .unwrap(),
            172,
            0,
        )
        .unwrap();
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
        // Twelve hours later on both clocks: the monotonic one is what the answer rate is measured
        // against, and a second handshake is well past that window.
        let later_now = 12 * 60 * 60 * 1000;
        let (_i2, m2) = Initiator::start(&phone, &gw_pub, &PSK, later).unwrap();
        h.on_record(later_now, later, &m2);
        assert_eq!(h.handshakes, 2, "the second handshake was accepted");
        assert!(h.established());
        assert_eq!(h.outbox.len(), 1, "and answered, with the old reply cleared");
        assert_eq!(
            saved.lock().unwrap().len(),
            2,
            "each accepted handshake records its timestamp"
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
}
