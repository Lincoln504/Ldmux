//! The companion command window: one command in flight, settled only by its own answer.
//!
//! MeshCore's companion protocol has **one** command-response window and no sequence numbers on the
//! wire. A client that writes two commands close together does not get two answers in order — it
//! gets whichever the firmware's dispatch produced, and on nRF52 BLE the two writes can coalesce in
//! `bleuart` or overflow the 176-byte frame and be dropped
//! (`src/helpers/nrf52/SerialBLEInterface.cpp:375-393`).
//!
//! The field failure this prevents is worth stating exactly, because it looked like a radio fault
//! and was a queue fault. The phone's session let a raw send (`CMD_SEND_RAW_PACKET`, `0x41`) start
//! while a contact lookup (`CMD_GET_CONTACT_BY_KEY`, `0x1E`) was still unanswered, because the
//! lookup was not on the list of commands the client thought it had to wait for. The firmware
//! answered the lookup first — with `RESP_CODE_ERR` and `ERR_CODE_NOT_FOUND`, because ldmux keeps no
//! contacts (ADR-0030) — and that error frame settled the *raw send*. The send was reported as
//! refused, the learned route was thrown away, and a working link tore itself down. Nothing in the
//! radio was wrong.
//!
//! So this is a state machine with three states and no clock of its own:
//!
//! * **idle** — nothing outstanding, a command may be written;
//! * **waiting** — exactly one command outstanding, with the answer codes that settle it and a
//!   deadline; anything else that arrives is reported as [`Answer::Unexpected`] and does not settle
//!   it;
//! * **draining** — a deadline expired, and the answer that was already on its way may still arrive.
//!   No command may be written until the drain window passes, because the firmware echoes no
//!   sequence number and a late `ERR` for the timed-out command is byte-identical to an `ERR` for
//!   the next one. Waiting is the only honest answer to "was that yours or mine".
//!
//! The opcodes themselves are **not** here: which code answers which command is the caller's table
//! ([`CommandSpec`]), because the host and the phone issue different subsets and neither may own the
//! other's wire. `crates/companion/src/codec.rs` holds the host's table, beside the opcodes it
//! names.

/// The answer codes that settle **any** command, whatever its table says.
///
/// Every command in the firmware's dispatch ends in one of them: a handler writes `OK`, or an error
/// code, or `DISABLED` when it cannot serve the request in the radio's current state
/// (`writeOKFrame`/`writeErrFrame`/`writeDisabledFrame`,
/// `examples/companion_radio/MyMesh.cpp:155-172`). They are listed by the caller rather than
/// hardcoded here so that a firmware that adds a fourth terminal code can be followed without
/// editing this crate.
pub const UNIVERSAL_ANSWERS: &[u8] = &[0x00, 0x01, 0x0F];

/// How long an answer may be in flight after a deadline before this end may write again.
///
/// The firmware answers from its own loop and does not queue: a command written while a previous
/// frame is still being read arrives after the answer it races. The slowest drain in the tree is the
/// ESP32 BLE interface, which empties a four-frame queue at 60 ms intervals
/// (`FRAME_QUEUE_SIZE`, `src/helpers/esp32/SerialBLEInterface.h:27`), so a late answer lands within a
/// few of those. A quarter second is two of them plus a serial round trip, which is generous on
/// purpose: the cost of waiting is one command's latency once, and the cost of not waiting is a
/// mis-settled command that tears down a session.
pub const DEFAULT_DRAIN_MS: u64 = 250;

/// How long a command may wait for an answer before its deadline expires.
///
/// A local figure and never an airtime: the firmware's dispatch is a switch on a byte, so an answer
/// that has not arrived in this long is not coming. Callers that know better override it per command
/// in their table.
pub const DEFAULT_TIMEOUT_MS: u32 = 2_000;

/// One command's answer contract.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The command byte, as the firmware's dispatch reads it
    /// (`handleCmdFrame`, `examples/companion_radio/MyMesh.cpp:1055-2065`).
    pub command: u8,
    /// The answer codes that complete **this** command beyond [`CommandSpec::universal`].
    ///
    /// A typed reply, not a terminal code: `CMD_APP_START` is answered by `RESP_CODE_SELF_INFO`
    /// (`MyMesh.cpp:1076-1114`), never by `OK`.
    pub settles_on: &'static [u8],
    /// How long to wait, or [`DEFAULT_TIMEOUT_MS`] when zero.
    pub timeout_ms: u32,
}

impl Entry {
    /// An entry that is settled by the universal codes alone.
    #[must_use]
    pub const fn terminal(command: u8, timeout_ms: u32) -> Self {
        Self {
            command,
            settles_on: &[],
            timeout_ms,
        }
    }

    /// An entry with a typed reply of its own.
    #[must_use]
    pub const fn typed(command: u8, settles_on: &'static [u8], timeout_ms: u32) -> Self {
        Self {
            command,
            settles_on,
            timeout_ms,
        }
    }
}

/// The whole answer contract for one end of the link.
///
/// Constructed once as a constant and handed to every call, so the table is data a reader can audit
/// rather than a `match` inside the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommandSpec {
    /// Codes that settle any command ([`UNIVERSAL_ANSWERS`] for stock firmware).
    pub universal: &'static [u8],
    /// One entry per command this end issues.
    pub entries: &'static [Entry],
}

impl CommandSpec {
    /// The entry for `command`, if this end's table names one.
    #[must_use]
    pub fn entry(&self, command: u8) -> Option<&Entry> {
        self.entries.iter().find(|e| e.command == command)
    }

    /// Whether `answer` completes `command`.
    ///
    /// A command with no entry is settled only by a universal code. That is not a gap: the firmware
    /// answers any command it does not recognise with `ERR_CODE_UNSUPPORTED_CMD`
    /// (`MyMesh.cpp:2061-2063`), which is universal.
    #[must_use]
    pub fn accepts(&self, command: u8, answer: u8) -> bool {
        self.universal.contains(&answer)
            || self
                .entry(command)
                .is_some_and(|e| e.settles_on.contains(&answer))
    }

    /// How long `command` may wait for its answer.
    #[must_use]
    pub fn timeout_ms(&self, command: u8) -> u32 {
        match self.entry(command).map(|e| e.timeout_ms) {
            Some(0) | None => DEFAULT_TIMEOUT_MS,
            Some(ms) => ms,
        }
    }
}

/// Identifies one command across its whole life, so a result or a timeout can be reported for the
/// command it belongs to even after the slot has moved on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct CommandId(u64);

impl CommandId {
    /// The number, for a log line.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What [`CompanionFifo::enqueue`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Enqueue {
    /// The command owns the window; nothing else may be written until it is settled or expires.
    Accepted(CommandId),
    /// A command is still outstanding. The caller keeps its own order and retries on the next turn.
    Busy(CommandId),
    /// A deadline expired `until_ms`, and the answer it was waiting for may still be in flight.
    Draining {
        /// When the window may be used again.
        until_ms: u64,
    },
}

/// A command that has been answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settled {
    /// Which command.
    pub id: CommandId,
    /// Its opcode.
    pub command: u8,
    /// The code that settled it.
    pub answer: u8,
}

/// What [`CompanionFifo::answering`] did with a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    /// It completed the outstanding command.
    Settled(Settled),
    /// It belongs to no outstanding command and was not consumed.
    ///
    /// This is the `RC-2` case reported rather than swallowed: a typed reply for a command this end
    /// never wrote (a lookup the previous, unsequenced code left in flight), or a `0x84` push that
    /// arrived while a command was outstanding. The slot stays occupied.
    Unexpected {
        /// The code that arrived.
        answer: u8,
        /// The command still waiting, if any.
        outstanding: Option<CommandId>,
    },
    /// It arrived inside a drain window, where a late answer for the expired command cannot be told
    /// apart from an answer to nothing. Dropped on purpose.
    Discarded {
        /// The code that arrived.
        answer: u8,
        /// The window's remaining end.
        until_ms: u64,
    },
}

/// One command that outlived its deadline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Expired {
    /// Which command.
    pub id: CommandId,
    /// Its opcode.
    pub command: u8,
    /// How far past its deadline this was noticed.
    pub late_by_ms: u64,
    /// Until when the window stays closed.
    pub drain_until_ms: u64,
}

/// The command window. One command in flight, no exceptions.
#[derive(Clone, Debug)]
pub struct CompanionFifo {
    in_flight: Option<InFlight>,
    drain_until_ms: Option<u64>,
    next_id: u64,
    drain_ms: u64,
}

/// The single outstanding command.
#[derive(Clone, Copy, Debug)]
struct InFlight {
    id: CommandId,
    command: u8,
    deadline_ms: u64,
}

impl Default for CompanionFifo {
    fn default() -> Self {
        Self::new(DEFAULT_DRAIN_MS)
    }
}

impl CompanionFifo {
    /// A window that stays closed for `drain_ms` after a deadline expires.
    #[must_use]
    pub fn new(drain_ms: u64) -> Self {
        Self {
            in_flight: None,
            drain_until_ms: None,
            next_id: 0,
            drain_ms,
        }
    }

    /// How many commands are outstanding. Never more than one, which is the invariant.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        usize::from(self.in_flight.is_some())
    }

    /// The command holding the window, if any.
    #[must_use]
    pub fn in_flight(&self) -> Option<CommandId> {
        self.in_flight.map(|f| f.id)
    }

    /// When the outstanding command's deadline falls, if one is waiting.
    #[must_use]
    pub fn deadline_ms(&self) -> Option<u64> {
        self.in_flight.map(|f| f.deadline_ms)
    }

    /// Takes the window for `command` and writes it, or refuses.
    ///
    /// `now_ms` is the caller's clock. The returned [`Enqueue::Accepted`] means the command may be
    /// written **now**; a caller that writes before calling this is the defect this type exists for.
    pub fn enqueue(&mut self, spec: &CommandSpec, command: u8, now_ms: u64) -> Enqueue {
        if let Some(id) = self.in_flight() {
            return Enqueue::Busy(id);
        }
        if let Some(until_ms) = self.drain_until_ms {
            if now_ms < until_ms {
                return Enqueue::Draining { until_ms };
            }
            self.drain_until_ms = None;
        }
        self.next_id += 1;
        let id = CommandId(self.next_id);
        self.in_flight = Some(InFlight {
            id,
            command,
            deadline_ms: now_ms.saturating_add(u64::from(spec.timeout_ms(command))),
        });
        Enqueue::Accepted(id)
    }

    /// Offers a frame's answer to the window.
    ///
    /// A code that does not complete the outstanding command is reported, never consumed: the
    /// command keeps waiting for the answer it actually asked for.
    pub fn answering(&mut self, spec: &CommandSpec, answer: u8, now_ms: u64) -> Answer {
        if let Some(until_ms) = self.drain_until_ms {
            if now_ms < until_ms {
                return Answer::Discarded { answer, until_ms };
            }
            self.drain_until_ms = None;
        }
        let Some(in_flight) = self.in_flight else {
            return Answer::Unexpected {
                answer,
                outstanding: None,
            };
        };
        if !spec.accepts(in_flight.command, answer) {
            return Answer::Unexpected {
                answer,
                outstanding: Some(in_flight.id),
            };
        }
        self.in_flight = None;
        Answer::Settled(Settled {
            id: in_flight.id,
            command: in_flight.command,
            answer,
        })
    }

    /// Expires the outstanding command if its deadline has passed.
    ///
    /// Returns `Some` **once** per command: the slot is cleared and the drain window opened, so a
    /// second call has nothing to expire.
    pub fn expire(&mut self, now_ms: u64) -> Option<Expired> {
        let in_flight = self.in_flight?;
        if now_ms < in_flight.deadline_ms {
            return None;
        }
        self.in_flight = None;
        let drain_until_ms = now_ms.saturating_add(self.drain_ms);
        self.drain_until_ms = Some(drain_until_ms);
        Some(Expired {
            id: in_flight.id,
            command: in_flight.command,
            late_by_ms: now_ms.saturating_sub(in_flight.deadline_ms),
            drain_until_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table shaped like the host's: two commands with a typed reply, one terminal command.
    const SPEC: CommandSpec = CommandSpec {
        universal: UNIVERSAL_ANSWERS,
        entries: &[
            Entry::typed(0x01, &[0x05], 1_000), // APP_START -> SELF_INFO
            Entry::typed(0x2A, &[0x16], 1_000), // GET_ADVERT_PATH -> ADVERT_PATH
            Entry::terminal(0x41, 500),         // SEND_RAW_PACKET -> OK
        ],
    };

    const OK: u8 = 0x00;
    const ERR: u8 = 0x01;
    const SELF_INFO: u8 = 0x05;
    const ADVERT_PATH: u8 = 0x16;

    fn accepted(fifo: &mut CompanionFifo, command: u8, now_ms: u64) -> CommandId {
        match fifo.enqueue(&SPEC, command, now_ms) {
            Enqueue::Accepted(id) => id,
            other => panic!("expected the window, got {other:?}"),
        }
    }

    /// A second write while one is outstanding is refused, with the id of what owns the window.
    #[test]
    fn a_second_command_is_refused_while_one_is_outstanding() {
        let mut fifo = CompanionFifo::default();
        let first = accepted(&mut fifo, 0x01, 0);
        assert_eq!(fifo.enqueue(&SPEC, 0x41, 1), Enqueue::Busy(first));
        assert_eq!(fifo.outstanding(), 1);
    }

    /// The command's own typed reply settles it and frees the window.
    #[test]
    fn the_commands_own_answer_settles_it_and_advances_the_window() {
        let mut fifo = CompanionFifo::default();
        let id = accepted(&mut fifo, 0x01, 0);
        assert_eq!(
            fifo.answering(&SPEC, SELF_INFO, 10),
            Answer::Settled(Settled {
                id,
                command: 0x01,
                answer: SELF_INFO
            })
        );
        assert_eq!(fifo.outstanding(), 0);
        assert!(matches!(
            fifo.enqueue(&SPEC, 0x41, 11),
            Enqueue::Accepted(_)
        ));
    }

    /// An error is a **terminal** code: the firmware answers every command with `OK`, an error, or
    /// `DISABLED`, so an error settles whichever command holds the window.
    ///
    /// That is safe only because of the invariant above it — there is never a second command for it
    /// to land on. `RC-2` was the absence of that invariant: a lookup's `ERR_CODE_NOT_FOUND` settled
    /// a raw send that had been written over the top of it. With one window the lookup is settled
    /// first and the send is not yet written.
    #[test]
    fn an_error_settles_the_only_command_there_can_be() {
        let mut fifo = CompanionFifo::default();
        let id = accepted(&mut fifo, 0x41, 0);
        // The send cannot be overlapped, which is what makes the error unambiguous.
        assert_eq!(fifo.enqueue(&SPEC, 0x2A, 0), Enqueue::Busy(id));
        assert_eq!(
            fifo.answering(&SPEC, ERR, 5),
            Answer::Settled(Settled {
                id,
                command: 0x41,
                answer: ERR
            })
        );
        // With nothing outstanding, the same code belongs to no command and is reported, not
        // swallowed: a caller that sees this knows something it did not write is still on the wire.
        assert_eq!(
            fifo.answering(&SPEC, ERR, 6),
            Answer::Unexpected {
                answer: ERR,
                outstanding: None
            }
        );
    }

    /// A typed reply for a command that is not the one waiting is reported, not consumed — the
    /// `0x2A` answer arriving while `0x41` waits.
    #[test]
    fn a_reply_for_another_command_is_reported() {
        let mut fifo = CompanionFifo::default();
        let id = accepted(&mut fifo, 0x41, 0);
        assert_eq!(
            fifo.answering(&SPEC, ADVERT_PATH, 3),
            Answer::Unexpected {
                answer: ADVERT_PATH,
                outstanding: Some(id)
            }
        );
        // The advert path is not a universal code, so it settles nothing.
        assert_eq!(fifo.outstanding(), 1);
    }

    /// A deadline expires once, and the window stays closed until the drain has passed.
    #[test]
    fn a_deadline_expires_exactly_once_and_closes_the_window() {
        let mut fifo = CompanionFifo::new(250);
        let id = accepted(&mut fifo, 0x41, 0); // 500 ms
        assert!(fifo.expire(499).is_none());
        let expired = fifo.expire(520).expect("past its deadline");
        assert_eq!(expired.id, id);
        assert_eq!(expired.command, 0x41);
        assert_eq!(expired.late_by_ms, 20);
        assert_eq!(expired.drain_until_ms, 770);
        assert!(fifo.expire(521).is_none(), "the deadline expired twice");
        assert_eq!(
            fifo.enqueue(&SPEC, 0x01, 600),
            Enqueue::Draining { until_ms: 770 }
        );
        assert!(matches!(fifo.enqueue(&SPEC, 0x01, 770), Enqueue::Accepted(_)));
    }

    /// The property that makes a drain window necessary: the firmware echoes no sequence number, so
    /// a late answer for the expired command is byte-identical to an answer for the next one.
    #[test]
    fn a_late_answer_never_settles_the_next_command() {
        let mut fifo = CompanionFifo::new(250);
        accepted(&mut fifo, 0x41, 0);
        fifo.expire(1_000).expect("expired");
        // The answer that was on its way, arriving after the deadline.
        assert!(matches!(
            fifo.answering(&SPEC, OK, 1_010),
            Answer::Discarded { answer: OK, .. }
        ));
        assert_eq!(fifo.outstanding(), 0);
        // Once the drain has passed, the same code is an answer to nothing rather than a settlement.
        assert_eq!(
            fifo.answering(&SPEC, OK, 1_251),
            Answer::Unexpected {
                answer: OK,
                outstanding: None
            }
        );
        let next = accepted(&mut fifo, 0x01, 1_252);
        assert_eq!(
            fifo.answering(&SPEC, SELF_INFO, 1_260),
            Answer::Settled(Settled {
                id: next,
                command: 0x01,
                answer: SELF_INFO
            })
        );
    }

    /// A command with no entry in the table is settled only by a universal code, and gets the
    /// default timeout. The firmware answers anything unrecognised with `ERR_CODE_UNSUPPORTED_CMD`.
    #[test]
    fn an_unlisted_command_is_settled_by_a_universal_code_only() {
        let mut fifo = CompanionFifo::default();
        let id = accepted(&mut fifo, 0x07, 0);
        assert_eq!(fifo.deadline_ms(), Some(u64::from(DEFAULT_TIMEOUT_MS)));
        assert_eq!(
            fifo.answering(&SPEC, SELF_INFO, 1),
            Answer::Unexpected {
                answer: SELF_INFO,
                outstanding: Some(id)
            }
        );
        assert!(matches!(fifo.answering(&SPEC, ERR, 2), Answer::Settled(_)));
    }

    /// The invariant over a whole session: run two hundred commands with everything the firmware
    /// does — typed replies, foreign frames, deadlines — and never have two outstanding at once.
    #[test]
    fn two_hundred_commands_never_put_two_in_flight() {
        let mut fifo = CompanionFifo::new(250);
        let mut clock = 0_u64;
        let mut settled = 0;
        let mut expired = 0;
        let commands = [0x01_u8, 0x2A, 0x41, 0x07];

        for step in 0..200_u64 {
            let command = commands[usize::try_from(step % 4).expect("small")];
            // Retry until the window takes the command: that is what a real caller does.
            let id = loop {
                assert!(fifo.outstanding() <= 1, "two commands in flight at {clock}");
                match fifo.enqueue(&SPEC, command, clock) {
                    Enqueue::Accepted(id) => break id,
                    Enqueue::Busy(_) | Enqueue::Draining { .. } => clock += 100,
                }
            };
            assert_eq!(fifo.outstanding(), 1);
            assert_eq!(fifo.in_flight(), Some(id));

            match step % 5 {
                // A typed reply, or a universal one.
                0 | 1 => {
                    let answer = match command {
                        0x01 => SELF_INFO,
                        0x2A => ADVERT_PATH,
                        _ => OK,
                    };
                    // A foreign frame first, which must not settle anything.
                    let foreign = if command == 0x01 { ADVERT_PATH } else { SELF_INFO };
                    if let Answer::Unexpected { outstanding, .. } =
                        fifo.answering(&SPEC, foreign, clock)
                    {
                        assert_eq!(outstanding, Some(id));
                    }
                    assert!(matches!(
                        fifo.answering(&SPEC, answer, clock + 1),
                        Answer::Settled(Settled { id: got, .. }) if got == id
                    ));
                    settled += 1;
                }
                // Silence, past the deadline: the command expires and the window drains.
                2 => {
                    assert!(fifo.expire(clock + 10_000).is_some());
                    expired += 1;
                }
                // A duplicate of a code that already settled nothing.
                _ => {
                    assert!(matches!(
                        fifo.answering(&SPEC, OK, clock),
                        Answer::Settled(_) | Answer::Unexpected { .. } | Answer::Discarded { .. }
                    ));
                }
            }
            assert!(fifo.outstanding() <= 1);
            clock += 1_000;
        }

        assert!(settled > 0 && expired > 0, "{settled} settled, {expired} expired");
    }
}
