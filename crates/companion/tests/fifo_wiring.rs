//! The host's companion session holds the radio's one answer window.
//!
//! `crates/mesh/src/companion/fifo.rs` is the rule with no I/O. This file is the rule **applied**:
//! every public command in `ldmux_companion::session` goes through one `execute` that owns the
//! window, so a second command cannot be written while the first is unanswered.
//!
//! The field failure these pin is `RC-2` (`docs/RADIO-AUDIT-REVIEW.md`). A contact lookup
//! (`CMD_GET_CONTACT_BY_KEY`, `0x1E`) was still unanswered when a raw send (`0x41`) went out over the
//! top of it. The firmware answered the lookup first — `RESP_CODE_ERR` with `ERR_CODE_NOT_FOUND`,
//! because ldmux keeps no contacts (ADR-0030) — and that error settled the *send*. The daemon
//! recorded a refused record, threw away the learned route, and ended a session whose radio was
//! never at fault.
//!
//! Nothing here waits a policy interval: every deadline is the caller's own, in tens of
//! milliseconds, because the point is the ordering and not the duration.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use ldmux_companion::codec::{codes, commands};
use ldmux_companion::link::{ByteLink, MemoryLink};
use ldmux_companion::{Companion, SessionError};

/// The deadline these tests use, so a refusal is visible without a policy wait.
const BRIEF: Duration = Duration::from_millis(60);

/// A link that hands the radio's answers out **when it is written to**, never before.
///
/// This is what makes "the answer arrived while the next command was in flight" expressible without
/// a thread: an entry is released the moment the client's nth write lands. [`MemoryLink`] alone can
/// only queue bytes before the call starts, which cannot produce a late answer at all.
///
/// It also counts **overlaps**: a write that arrives while an earlier answer is still unread. That
/// is the shape `RC-2` needed — two commands in flight — and it must stay at zero for a whole run,
/// not just for one step.
struct Scripted {
    inner: MemoryLink,
    /// `(release after this many writes, payload before the framing)`.
    releases: VecDeque<(usize, Vec<u8>)>,
    writes: usize,
    overlaps: usize,
}

impl Scripted {
    fn new(releases: Vec<(usize, Vec<u8>)>) -> Self {
        Self {
            inner: MemoryLink::new(),
            releases: releases.into(),
            writes: 0,
            overlaps: 0,
        }
    }

    fn writes(&self) -> usize {
        self.writes
    }

    fn overlaps(&self) -> usize {
        self.overlaps
    }
}

impl ByteLink for Scripted {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // An answer still sitting unread means the previous command had not finished being answered.
        if !self.inner.inbound.is_empty() {
            self.overlaps += 1;
        }
        self.inner.write_all(bytes)?;
        self.writes += 1;
        while let Some((after, _)) = self.releases.front() {
            if *after > self.writes {
                break;
            }
            let (_, payload) = self.releases.pop_front().expect("front was checked");
            self.inner.push_payload(&payload);
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

/// A `SELF_INFO` payload the parser accepts, copied from the session's own unit test so this file
/// fails for behaviour and not for a fixture that drifted.
fn self_info_payload() -> Vec<u8> {
    let mut v = vec![codes::SELF_INFO, 1, 22, 22];
    v.extend((1..=32).map(|i| i as u8));
    v.extend(0i32.to_le_bytes());
    v.extend(0i32.to_le_bytes());
    v.extend([0u8, 1, 0, 0]);
    v.extend(914_250u32.to_le_bytes());
    v.extend(500_000u32.to_le_bytes());
    v.extend([8u8, 5]);
    v.extend(b"HELTEC-BASE");
    v
}

fn not_found() -> Vec<u8> {
    vec![codes::ERROR, 2]
}

// -------------------------------------------------------------------------------------------
// `RC-2`: the error settles the command that asked it, and the send runs afterwards.
// -------------------------------------------------------------------------------------------

/// The field sequence, with the window in place.
///
/// The lookup is answered with `ERR_CODE_NOT_FOUND` — the ordinary answer for a peer the radio has
/// never heard, and not a fault. It settles the **lookup**. The send is then written on a clear
/// window and is settled by its own `OK`.
///
/// What this pins is that the two are separate commands with separate answers. It cannot reproduce
/// the overlap itself, because the overlap is precisely what the window makes unwritable; the count
/// of writes is the evidence that they were not concurrent.
#[test]
fn rc2_a_query_error_settles_the_query_and_then_a_send_runs() {
    let mut companion = Companion::new(Scripted::new(vec![
        (1, not_found()),
        (2, vec![codes::OK]),
    ]));

    let err = companion
        .get_advert_path(&[7u8; 32])
        .expect_err("a cache miss is an error, and it belongs to the lookup");
    assert!(
        matches!(err, SessionError::Rejected { code: 2, .. }),
        "the lookup must own its own error, got {err}"
    );

    companion
        .send_raw_within(1, &[], &[1, 2, 3], BRIEF)
        .expect("a send after a settled lookup is accepted by its own OK");

    assert_eq!(companion.link_mut().writes(), 2, "one write per command");
    assert_eq!(companion.link_mut().overlaps(), 0);
}

// -------------------------------------------------------------------------------------------
// A reply that names another command leaves this one waiting.
// -------------------------------------------------------------------------------------------

/// A `DEVICE_INFO` arriving while `APP_START` waits is a fact about the radio, not this command's
/// answer. It is set aside, and the wait continues until the real one lands.
#[test]
fn a_reply_for_another_command_is_kept_aside_and_settles_nothing() {
    let mut companion = Companion::new(Scripted::new(vec![
        (1, vec![codes::DEVICE_INFO, 13, 175, 40]),
        (1, self_info_payload()),
    ]));

    let info = companion
        .app_start("ldmux")
        .expect("the app start's own answer still settles it");
    assert_eq!(info.name, "HELTEC-BASE");

    let kept = companion
        .next_push(Duration::from_millis(5))
        .expect("a read");
    assert_eq!(
        kept.expect("the foreign reply is kept for the caller")[0],
        codes::DEVICE_INFO,
        "the frame that was set aside is the one handed back"
    );
}

// -------------------------------------------------------------------------------------------
// A deadline frees the window exactly once.
// -------------------------------------------------------------------------------------------

/// A command that runs out of time releases the window, and the command after it runs.
///
/// The drain window is waited out inside `execute`, deliberately: a caller that had to schedule
/// around a quarter second of the previous command's lateness would be a caller that has to know
/// this crate's bookkeeping.
#[test]
fn a_deadline_frees_the_window_and_the_next_command_runs() {
    let mut companion = Companion::new(Scripted::new(vec![(2, vec![codes::OK])]));

    let err = companion
        .send_raw_within(1, &[], &[1, 2, 3], BRIEF)
        .expect_err("a silent radio is a timeout");
    assert!(matches!(err, SessionError::Timeout { .. }), "got {err}");

    companion
        .send_raw_within(1, &[], &[4, 5, 6], BRIEF)
        .expect("the window was free after the timeout");
    assert_eq!(companion.link_mut().writes(), 2);
}

// -------------------------------------------------------------------------------------------
// A late answer cannot settle the next command.
// -------------------------------------------------------------------------------------------

/// **The residual half of `RC-2`, and the reason the window has a drain.**
///
/// A command gives up. Its answer arrives while the *next* command is in flight. The firmware echoes
/// no sequence number, so that frame is indistinguishable from an answer to the new command — and
/// the new command must not be settled by it. It times out instead, which is a refused attempt the
/// caller can retry, rather than a silent success on somebody else's reply.
#[test]
fn a_late_answer_for_an_expired_command_does_not_settle_the_next_one() {
    // The answer is released by the second write, so the first command never sees it.
    let mut companion = Companion::new(Scripted::new(vec![(2, self_info_payload())]));

    let first = companion.send_raw_within(1, &[], &[1, 2, 3], BRIEF);
    assert!(
        matches!(first, Err(SessionError::Timeout { .. })),
        "the first command has to time out for this test to mean anything: {first:?}"
    );

    let second = companion.send_raw_within(1, &[], &[4, 5, 6], BRIEF);
    assert!(
        matches!(second, Err(SessionError::Timeout { .. })),
        "a send was settled by another command's answer: {second:?}"
    );
}

// -------------------------------------------------------------------------------------------
// The invariant over a whole run, not per step.
// -------------------------------------------------------------------------------------------

/// Two hundred commands, each answered only when it is written, with the overlap count asserted over
/// the **whole** run rather than per step.
///
/// A per-step check cannot see two commands that merely overlapped; the link can, because it is the
/// thing that was written to.
#[test]
fn two_hundred_commands_are_never_written_over_one_another() {
    let releases: Vec<(usize, Vec<u8>)> = (1..=200).map(|n| (n, vec![codes::OK])).collect();
    let mut companion = Companion::new(Scripted::new(releases));

    for n in 0..200u16 {
        let record = [u8::try_from(n % 251).unwrap_or(0), 0xAB];
        companion
            .send_raw_within(1, &[], &record, BRIEF)
            .unwrap_or_else(|e| panic!("command {n} failed: {e}"));
    }

    assert_eq!(companion.link_mut().writes(), 200, "one write per command");
    assert_eq!(
        companion.link_mut().overlaps(),
        0,
        "a command was written while an earlier answer was still unread"
    );
    assert!(
        !companion.has_pending(),
        "an answer was left over, so a command was answered by something other than its own reply"
    );
}

// -------------------------------------------------------------------------------------------
// The wire is unchanged by the window.
// -------------------------------------------------------------------------------------------

/// The bytes on the wire are what they always were: a raw send is still
/// `[0x3E][len][0x41][priority][packet]`.
///
/// The window is bookkeeping, and a change to the frame would be a second, silent change bundled
/// with it.
#[test]
fn the_window_does_not_change_the_frame() {
    let mut companion = Companion::new(Scripted::new(vec![(1, vec![codes::OK])]));
    companion
        .send_raw_within(2, &[0xAA, 0xBB], &[1, 2, 3], BRIEF)
        .expect("accepted");
    let written = companion.link_mut().inner.written.clone();
    assert_eq!(written[0], 0x3C, "the start byte");
    assert_eq!(written[3], commands::SEND_RAW_PACKET);
    assert_eq!(written[4], 0, "priority");
    assert_eq!(written[5], 0x3E, "raw custom, direct");
    assert_eq!(written[6], 0x41, "one hop of two-byte hashes");
    assert_eq!(&written[7..9], &[0xAA, 0xBB]);
    assert_eq!(&written[9..12], &[1, 2, 3]);
}

// -------------------------------------------------------------------------------------------
// A dead port must not keep the window.
// -------------------------------------------------------------------------------------------

/// A link whose first read fails, as an unplugged serial port does.
struct Unplugged {
    inner: MemoryLink,
    fail_reads: usize,
    written: Vec<u8>,
}

impl ByteLink for Unplugged {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written.extend_from_slice(bytes);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.fail_reads > 0 {
            self.fail_reads -= 1;
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "unplugged"));
        }
        self.inner.read(buf)
    }
}

/// A transport failure gives the window back, so the next command is a normal command and not a
/// permanent `Busy`.
///
/// Without this the slot would stay taken for the life of the session: the first failure would
/// report the real reason and every one after it would report that a command was still outstanding,
/// which is true and useless.
#[test]
fn a_transport_failure_does_not_leave_the_window_taken() {
    let mut companion = Companion::new(Unplugged {
        inner: MemoryLink::new(),
        fail_reads: 1,
        written: Vec::new(),
    });

    let first = companion.send_raw_within(1, &[], &[1, 2, 3], BRIEF);
    assert!(
        matches!(first, Err(SessionError::Io(_))),
        "a dead port is an i/o error, got {first:?}"
    );

    // The port comes back. The command after it must run rather than be refused.
    let second = companion.send_raw_within(1, &[], &[4, 5, 6], BRIEF);
    assert!(
        matches!(second, Err(SessionError::Timeout { .. })),
        "the window outlived the failure: {second:?}"
    );
}
