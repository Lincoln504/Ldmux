//! The version-15 request/answer service: **one command in, one screen out**.
//!
//! The radio route is a batch link, so the phone does not stream. It composes a draft, taps `Send`,
//! and waits for exactly one screen that says what happened and what the pane looks like now
//! (`docs/RADIO-PLAN.md` §6, ADR-0039). Everything that decision needs lives in one place here,
//! deliberately away from `mesh_bridge.rs`: stage A replaces that file with a transport swap, and
//! logic that had to move with it would be logic written twice.
//!
//! Three rules are the whole module, and each one exists because its absence was a field failure:
//!
//!   * **One command is applied at most once.** A command that lost its acknowledgement is
//!     retransmitted by the phone, and the host must not type it twice. The
//!     `(device, op_id)` ring answers a repeat with the cached status and a fresh screen, and
//!     writes nothing (`RC-7`, `RC-8`).
//!   * **One command produces one downlink burst.** An early answer and a settled answer are two
//!     waves into the same repeater chain, and they collide with each other. So the host waits for
//!     the pane to settle and then sends once — never twice, whatever the pane does afterwards
//!     (`docs/RADIO-PLAN.md` §6.3, the rule the old `Hold`/`Resync` design broke).
//!   * **A partial command never reaches a pty.** A command larger than one message arrives in
//!     chunks; the joined items are what `apply_send` sees, and a command that never completes is
//!     never written (`RC-9`, `AR-3`).
//!
//! Nothing here knows a radio, a socket or a keeper: [`MeshHost`] is the four keeper operations the
//! answer depends on, and the clock is a parameter so a test can move time without sleeping.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ldmux_protocol::mesh_term::{
    ChunkError, Message, Patch, Screen, SendAssembly, applied, snapshot_flag,
};

use crate::mesh_apply::{ApplyError, PaneModes, apply_send};

/// How long the pane must stay unchanged, **after** it changed, before the answer is sent.
///
/// A command that prints (most of them) shows its first change within a few hundred milliseconds
/// and then stops. Half a second of quiet after that change is the shortest window that does not
/// cut a prompt off its own output on a loaded machine.
pub const SETTLE_QUIET_MS: u64 = 500;

/// How long a command that changed nothing gets before its answer goes out anyway.
///
/// `cd`, `export`, a password prompt, a command that only writes to a file: the screen is the same
/// as it was, so there is nothing to wait for. Waiting for a change that will never come is what
/// made an empty command cost the full budget.
pub const SETTLE_NO_CHANGE_MS: u64 = 700;

/// The longest a command's answer is held. Past this the pane is still printing (`cargo build`,
/// `tail -f`, `top`) and the phone is owed a mid-state screen rather than silence.
pub const INLINE_SETTLE_MAX_MS: u64 = 3_500;

/// How many `(device, op_id)` answers are remembered.
///
/// A retransmission arrives within one transaction ladder, so the window only has to outlive a
/// few commands. Sixty-four is the plan's figure and is generous for one operator on one radio.
pub const RING: usize = 64;

/// Most patch bytes one `Snapshot` message carries, before chunking.
///
/// `MAX_SNAPSHOT_INFLATED` bounds a whole patch stream at 2,048 bytes, and a message's body has a
/// 600-byte ceiling, so a screen is split well below either: 512 leaves room for the header and for
/// deflate to expand slightly on incompressible text. Chunks exist so a dense full paint is several
/// messages rather than a refused one.
pub const SNAPSHOT_CHUNK_BYTES: usize = 512;

/// The keeper operations a version-15 answer needs, and no others.
///
/// Narrow on purpose: this is the seam stage A keeps when `mesh_bridge.rs` is deleted, so anything
/// named here has to be something the service genuinely does. The shell-control messages
/// (`Create`, `Close`, `Takeover`, `List`) stay in the caller until then, because they are about
/// which shell the phone is looking at rather than about answering a command.
#[allow(async_fn_in_trait)]
pub trait MeshHost {
    /// The pane's grid, cursor and rows, as the keeper renders it.
    async fn screen(&mut self, id: &str) -> Result<Screen, String>;
    /// Raw bytes for the pty, already encoded for the pane's paste mode.
    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String>;
    /// Key names, for the keeper's one key table to encode against the pane's live mode.
    async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String>;
    /// Fit the pty to the phone's grid, before the screen it is about to be shown.
    async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String>;
}

/// One `Send` as the service needs it: which link channel, which peer, which shell, and what the
/// pane says about itself.
///
/// A struct rather than six positional arguments, because two of them are `u8` grids and two are
/// identities: a transposed pair would write to the wrong shell, and the compiler cannot catch that
/// in a list of numbers.
#[derive(Debug, Clone, Copy)]
pub struct SendRequest<'a> {
    /// The link's channel for this attach, which is what a reply is addressed with.
    pub shell: u8,
    /// The peer the command came from. One radio link carries one phone, so this is
    /// [`crate::mesh_bridge::PEER_LINK`] today; it is a field because the ring's identity is what
    /// makes a duplicate safe.
    pub device: u32,
    /// The shell's id, as the attach named it — the authoritative one, not the one in the message.
    pub id: &'a str,
    /// The phone's grid, when it stated one.
    pub grid: Option<(u8, u8)>,
    /// What the pane says about itself.
    pub pane: PaneModes,
}

/// What the caller does with one arriving message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// More chunks are needed, or the answer is still settling. Send nothing: the silence is the
    /// burst rule, not a lost message.
    Pending,
    /// Hand these to the link, in order, as one reply.
    Answer(Vec<Message>),
}

/// One answered command, kept so a retransmission is not applied twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Answer {
    device: u32,
    op_id: u32,
    status: u8,
}

/// A command that has been written and is waiting for its pane to settle.
#[derive(Debug)]
struct Waiting {
    op_id: u32,
    status: u8,
    /// When the bytes reached the pty, which every deadline is measured from.
    applied_at: Instant,
    /// When the screen last differed from what the phone holds. `None` means it has not differed
    /// yet, which is the "settled with no change" case.
    changed_at: Option<Instant>,
    /// The screen the phone already holds, for the diff and for the change test.
    shown: Option<Screen>,
    /// The `snapshot_id` the phone last painted, so a diff can name its base.
    base_id: u32,
}

/// The last screen sent for a shell, so the next answer can be a diff rather than a repaint.
#[derive(Debug, Clone)]
struct Sent {
    snapshot_id: u32,
    screen: Screen,
}

/// The version-15 service: chunk assemblies, the at-most-once ring, and the settle engine.
#[derive(Debug, Default)]
pub struct MeshRpc {
    /// Commands being assembled from chunks, keyed by `(device, op_id)`.
    assemblies: HashMap<(u32, u32), SendAssembly>,
    /// The answers already given, oldest first.
    answered: VecDeque<Answer>,
    /// Commands written and awaiting their screen, keyed by the link's shell index.
    waiting: HashMap<u8, Waiting>,
    /// What the phone holds, keyed by the link's shell index.
    sent: HashMap<u8, Sent>,
    /// Shells whose phone has **used** the request/answer model at least once.
    ///
    /// A version byte says what a build can decode; it does not say that the phone has stopped
    /// relying on the frame loop. A phone that attaches as version 15 and then keeps typing
    /// `Input` is still watching `Rows`/`Commit`, and serving it neither would leave it with no
    /// picture at all. So the host keeps the frame loop until the phone demonstrates the new
    /// model by sending a `Send` or a `Fetch`, and only then stops streaming for that shell.
    /// That is a migration with no flag day and no way to brick a client that has not been
    /// updated yet.
    driven: std::collections::BTreeSet<u8>,
    next_snapshot: u32,
}

impl MeshRpc {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything about a shell: its pending command, its screen identity and the ring.
    ///
    /// Called when a shell detaches or the session dies. The **ring is not cleared**: a phone that
    /// lost its session will retransmit the last command on the next one, and the answer to it is
    /// still the truth. Only the shell's screen identity is forgotten, because the new session's
    /// phone has painted nothing.
    pub fn forget_shell(&mut self, shell: u8) {
        self.waiting.remove(&shell);
        self.sent.remove(&shell);
    }

    /// Whether a command is waiting for its screen on this shell.
    #[must_use]
    pub fn busy(&self, shell: u8) -> bool {
        self.waiting.contains_key(&shell)
    }

    /// Whether this shell's phone has ever used the request/answer model.
    ///
    /// Read by the bridge to decide whether to keep streaming `Rows`/`Commit`. See [`Self::driven`]
    /// for why a version claim alone is not enough.
    #[must_use]
    pub fn has_driven(&self, shell: u8) -> bool {
        self.driven.contains(&shell)
    }

    /// Record that this shell's phone used the request/answer model.
    pub fn mark_driven(&mut self, shell: u8) {
        self.driven.insert(shell);
    }

    /// Feed one `Send`. `Ok(Outcome::Pending)` means "say nothing yet".
    ///
    /// The steps run in the operator's composition order, and a command is written **only** when
    /// every one of its chunks is present.
    ///
    /// # Errors
    ///
    /// Returns the status byte to answer with when the command is refused: a chunk that belongs to
    /// another command, a command over the size either route accepts, or a keeper refusal.
    pub async fn on_send<H: MeshHost>(
        &mut self,
        request: SendRequest<'_>,
        message: &Message,
        host: &mut H,
        now: Instant,
    ) -> Result<Outcome, u8> {
        let SendRequest {
            shell,
            device,
            id,
            grid,
            pane,
        } = request;
        self.mark_driven(shell);
        let Message::Send {
            op_id,
            have_snapshot_id,
            ..
        } = message
        else {
            return Ok(Outcome::Pending);
        };
        let items = {
            let assembly = self.assemblies.entry((device, *op_id)).or_default();
            match assembly.accept(message) {
                Ok(Some(items)) => {
                    self.assemblies.remove(&(device, *op_id));
                    items
                }
                Ok(None) => return Ok(Outcome::Pending),
                Err(ChunkError::TooLarge) => {
                    self.assemblies.remove(&(device, *op_id));
                    return Err(applied::TOO_LONG);
                }
                // Two commands interleaved, or one chunk claiming another's length. The assembly is
                // released rather than kept: a caller that cannot tell which command it holds would
                // otherwise join them, and joining two commands into one write is the failure this
                // module exists to prevent.
                Err(ChunkError::WrongCommand | ChunkError::WrongCount) => {
                    self.assemblies.remove(&(device, *op_id));
                    return Err(applied::FAILED);
                }
            }
        };

        // **At most once.** A retransmission of a command already written is answered with its
        // status and the current screen, and the pty is not touched.
        let status = if let Some(previous) = self.answered_for(device, *op_id) {
            previous
        } else {
            if let Some((cols, rows)) = grid {
                // The pane is fitted **before** the command runs, so the output is wrapped at the
                // phone's width rather than at whatever the desk left. A failure here is not fatal:
                // the command still runs, and the screen that comes back is the pane's real grid.
                let _ = host.resize(id, cols, rows).await;
            }
            let written = match apply_send(&items, pane) {
                Ok(steps) => {
                    let mut ok = true;
                    for step in steps {
                        match step {
                            crate::mesh_apply::ApplyStep::Write(bytes) => {
                                if host.input(id, &bytes).await.is_err() {
                                    ok = false;
                                    break;
                                }
                            }
                            crate::mesh_apply::ApplyStep::Keys(names) => {
                                if host.keys(id, &names).await.is_err() {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    }
                    ok
                }
                Err(ApplyError::Empty) => false,
            };
            let status = if written {
                applied::WRITTEN
            } else {
                applied::FAILED
            };
            self.remember(device, *op_id, status);
            status
        };

        // A duplicate is answered at once: the screen it describes has not changed since the
        // original answer, so waiting again would only delay the phone's confirmation.
        if status == applied::DUPLICATE {
            return Ok(Outcome::Answer(
                self.answer(
                    shell,
                    id,
                    Reply {
                        op_id: *op_id,
                        status,
                        base_id: *have_snapshot_id,
                    },
                    host,
                    Settled::Yes,
                )
                .await,
            ));
        }
        if status != applied::WRITTEN {
            // Nothing was written, so there is nothing to settle: the phone is told what happened
            // and given the screen as it stands.
            return Ok(Outcome::Answer(
                self.answer(
                    shell,
                    id,
                    Reply {
                        op_id: *op_id,
                        status,
                        base_id: *have_snapshot_id,
                    },
                    host,
                    Settled::Yes,
                )
                .await,
            ));
        }

        let shown = host.screen(id).await.ok();
        self.waiting.insert(
            shell,
            Waiting {
                op_id: *op_id,
                status,
                applied_at: now,
                changed_at: None,
                shown,
                base_id: *have_snapshot_id,
            },
        );
        Ok(Outcome::Pending)
    }

    /// Poll a waiting command. `Some` means the single answer is ready to send.
    ///
    /// Called once a pass. Reads the pane **only** while a command waits, which is what keeps an
    /// idle radio shell costing nothing.
    pub async fn settle<H: MeshHost>(
        &mut self,
        shell: u8,
        id: &str,
        host: &mut H,
        now: Instant,
    ) -> Option<Vec<Message>> {
        let waiting = self.waiting.get_mut(&shell)?;
        let current = match host.screen(id).await {
            Ok(screen) => screen,
            Err(_) => {
                // The shell is gone. Answering with a closed shell is not this module's job, but
                // holding the phone until the budget expires would be: release the wait so the
                // caller's own liveness check can speak.
                self.waiting.remove(&shell);
                return None;
            }
        };
        if waiting.shown.as_ref() != Some(&current) {
            waiting.changed_at = Some(now);
            waiting.shown = Some(current);
        }
        let settled = match waiting.changed_at {
            Some(changed) if elapsed(now, changed) >= SETTLE_QUIET_MS => Settled::Yes,
            Some(_) => Settled::NotYet,
            None if elapsed(now, waiting.applied_at) >= SETTLE_NO_CHANGE_MS => Settled::Yes,
            None => Settled::NotYet,
        };
        let verdict = match settled {
            Settled::Yes => Settled::Yes,
            Settled::No | Settled::NotYet
                if elapsed(now, waiting.applied_at) >= INLINE_SETTLE_MAX_MS =>
            {
                Settled::No
            }
            Settled::No | Settled::NotYet => return None,
        };
        let done = self.waiting.remove(&shell)?;
        Some(
            self.answer(
                shell,
                id,
                Reply {
                    op_id: done.op_id,
                    status: done.status,
                    base_id: done.base_id,
                },
                host,
                verdict,
            )
            .await,
        )
    }

    /// The screen a `Fetch` is owed, sent at once.
    ///
    /// A fetch never touches a draft: it exists so an operator can look at a shell without sending
    /// what is typed in it (`docs/RADIO-PLAN.md` §6.1). Its screen is marked **not settled**,
    /// because the phone asked what the pane looks like rather than for a verdict on a command.
    pub async fn on_fetch<H: MeshHost>(
        &mut self,
        shell: u8,
        id: &str,
        message: &Message,
        host: &mut H,
    ) -> Outcome {
        self.mark_driven(shell);
        let Message::Fetch {
            have_snapshot_id,
            cols,
            rows,
            ..
        } = message
        else {
            return Outcome::Pending;
        };
        let have_snapshot_id = *have_snapshot_id;
        let grid = (*cols > 0 && *rows > 0).then_some((*cols, *rows));
        if let Some((cols, rows)) = grid {
            let _ = host.resize(id, cols, rows).await;
        }
        Outcome::Answer(
            self.answer(
                shell,
                id,
                Reply {
                    op_id: 0,
                    status: applied::NONE,
                    base_id: have_snapshot_id,
                },
                host,
                Settled::No,
            )
            .await,
        )
    }

    /// Build the one message set a screen is sent as.
    async fn answer<H: MeshHost>(
        &mut self,
        shell: u8,
        id: &str,
        reply: Reply,
        host: &mut H,
        settled: Settled,
    ) -> Vec<Message> {
        let Reply {
            op_id,
            status,
            base_id,
        } = reply;
        let Ok(screen) = host.screen(id).await else {
            return Vec::new();
        };
        self.next_snapshot = self.next_snapshot.wrapping_add(1).max(1);
        let snapshot_id = self.next_snapshot;
        // **A diff only when the phone holds a screen this one derives from.** A first paint, a
        // geometry change and an alternate-screen switch are all full paints, because the phone has
        // nothing to patch onto: a diff against a screen it never painted leaves rows stale for
        // ever, which is the defect `Profile`-without-`Rows` used to produce.
        let previous = self.sent.get(&shell).filter(|sent| {
            sent.snapshot_id == base_id
                && sent.screen.cols == screen.cols
                && sent.screen.rows == screen.rows
                && sent.screen.alternate == screen.alternate
                && base_id != 0
        });
        let (full, patches) = match previous {
            Some(sent) => (false, diff_rows(&sent.screen, &screen)),
            None => (true, full_rows(&screen)),
        };
        let mut flags = 0u8;
        if full {
            flags |= snapshot_flag::FULL;
        }
        if screen.alternate {
            flags |= snapshot_flag::ALTERNATE;
        }
        if screen.dead {
            flags |= snapshot_flag::DEAD;
        }
        if settled == Settled::Yes {
            flags |= snapshot_flag::SETTLED;
        }
        if status != applied::NONE {
            flags |= snapshot_flag::HAS_APPLIED;
        }
        self.sent.insert(
            shell,
            Sent {
                snapshot_id,
                screen: screen.clone(),
            },
        );
        let chunks = chunk_patches(patches);
        let count = u8::try_from(chunks.len()).unwrap_or(u8::MAX);
        chunks
            .into_iter()
            .enumerate()
            .map(|(index, patches)| Message::Snapshot {
                flags,
                chunk_idx: u8::try_from(index).unwrap_or(0),
                chunk_count: count.max(1),
                snapshot_id,
                base_id: if full { 0 } else { base_id },
                applied_op_id: op_id,
                applied_status: status,
                cols: screen.cols,
                rows: screen.rows,
                cursor_x: screen.cursor_x,
                cursor_y: screen.cursor_y,
                first_line: screen.first_line,
                shell: id.to_owned(),
                patches,
            })
            .collect()
    }

    /// The answer a command gets when it never ran: one `Snapshot` with the status and no rows.
    ///
    /// A refusal has no pane to read, so there is nothing to capture and nothing to diff. The phone
    /// needs exactly one thing here — which command was refused and why — and a `snapshot_id` of
    /// zero tells it that no screen identity changed, so its next `Send` still diffs against what it
    /// holds.
    #[must_use]
    pub fn refusal(op_id: u32, base_id: u32, status: u8) -> Message {
        Message::Snapshot {
            flags: snapshot_flag::HAS_APPLIED | snapshot_flag::SETTLED,
            chunk_idx: 0,
            chunk_count: 1,
            snapshot_id: 0,
            base_id,
            applied_op_id: op_id,
            applied_status: status,
            cols: 0,
            rows: 0,
            cursor_x: 0,
            cursor_y: 0,
            first_line: 0,
            shell: String::new(),
            patches: Vec::new(),
        }
    }

    /// The status already given to this command, if it was written before.
    fn answered_for(&mut self, device: u32, op_id: u32) -> Option<u8> {
        self.answered
            .iter()
            .find(|a| a.device == device && a.op_id == op_id)
            .map(|a| {
                // A repeat is answered with `DUPLICATE`, not with the original `WRITTEN`: the phone
                // must be able to tell "it ran" from "it had already run", because the second is
                // what makes a lost acknowledgement safe.
                if a.status == applied::WRITTEN {
                    applied::DUPLICATE
                } else {
                    a.status
                }
            })
    }

    fn remember(&mut self, device: u32, op_id: u32, status: u8) {
        self.answered.push_back(Answer {
            device,
            op_id,
            status,
        });
        while self.answered.len() > RING {
            self.answered.pop_front();
        }
    }
}

/// What a `Snapshot` says about the command it answers.
///
/// The three are one value because they are one fact: a status with the wrong `op_id` tells the
/// phone the wrong command ran, and a `base_id` with the wrong status makes it patch a screen from
/// one command with another's diff.
#[derive(Debug, Clone, Copy)]
struct Reply {
    /// The command this answers; `0` for a `Fetch`, which answers none.
    op_id: u32,
    /// One of [`applied`].
    status: u8,
    /// The snapshot the patches diff against; `0` for a full paint.
    base_id: u32,
}

/// Whether a captured screen is the settled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    Yes,
    No,
    NotYet,
}

/// Milliseconds from `from` to `now`, saturating to zero for a clock that went backwards.
fn elapsed(now: Instant, from: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

/// Every row, for a first paint.
fn full_rows(screen: &Screen) -> Vec<Patch> {
    screen
        .lines
        .iter()
        .enumerate()
        .filter_map(|(y, text)| {
            // A blank row needs no patch when the whole screen is being replaced: the phone clears
            // every row first, so an empty patch would cost three bytes to say nothing.
            if text.is_empty() {
                return None;
            }
            Some(Patch {
                y: u8::try_from(y).ok()?,
                append: false,
                text: text.clone(),
            })
        })
        .collect()
}

/// Only the rows that differ, for a diff against the screen the phone holds.
///
/// A row that shrank must still be sent, even when the new text is a prefix of the old: the phone
/// replaces the row rather than editing it, so an unchanged prefix with a shorter tail needs the
/// patch to become shorter at all.
fn diff_rows(before: &Screen, after: &Screen) -> Vec<Patch> {
    after
        .lines
        .iter()
        .enumerate()
        .filter_map(|(y, text)| {
            let old = before.lines.get(y).map_or("", String::as_str);
            if old == text {
                return None;
            }
            Some(Patch {
                y: u8::try_from(y).ok()?,
                append: false,
                text: text.clone(),
            })
        })
        .collect()
}

/// Split a patch stream into pieces that each fit one message comfortably.
///
/// A patch is never split: a row is one unit, and half a row would paint half a line. So a single
/// row longer than the ceiling travels alone, and the encoder's own bound is what refuses it if it
/// is past what any message may carry.
fn chunk_patches(patches: Vec<Patch>) -> Vec<Vec<Patch>> {
    let mut chunks: Vec<Vec<Patch>> = Vec::new();
    let mut current: Vec<Patch> = Vec::new();
    let mut used = 0usize;
    for patch in patches {
        let cost = patch.wire_len();
        if !current.is_empty() && used + cost > SNAPSHOT_CHUNK_BYTES {
            chunks.push(std::mem::take(&mut current));
            used = 0;
        }
        used += cost;
        current.push(patch);
    }
    // A screen with nothing to say is still one chunk: `chunk_count` is at least one, and an empty
    // patch list is a legitimate answer (a settled command whose rows the phone already has).
    chunks.push(current);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_protocol::mesh_term::SendItem;
    use std::time::Duration;

    /// A host that records what it was asked to do and answers with a scripted screen.
    #[derive(Default)]
    struct FakeHost {
        screens: Vec<Screen>,
        written: Vec<String>,
        resized: Vec<(u8, u8)>,
        fail_write: bool,
    }

    impl FakeHost {
        fn with(screens: Vec<Screen>) -> Self {
            Self {
                screens,
                ..Self::default()
            }
        }
        fn last(&self) -> Screen {
            self.screens
                .last()
                .cloned()
                .unwrap_or_else(|| screen(&[""], 20, 3))
        }
    }

    impl MeshHost for FakeHost {
        async fn screen(&mut self, _id: &str) -> Result<Screen, String> {
            Ok(self.last())
        }
        async fn input(&mut self, _id: &str, bytes: &[u8]) -> Result<(), String> {
            if self.fail_write {
                return Err("no".to_owned());
            }
            self.written.push(String::from_utf8_lossy(bytes).into_owned());
            Ok(())
        }
        async fn keys(&mut self, _id: &str, names: &[String]) -> Result<(), String> {
            if self.fail_write {
                return Err("no".to_owned());
            }
            self.written.extend(names.iter().cloned());
            Ok(())
        }
        async fn resize(&mut self, _id: &str, cols: u8, rows: u8) -> Result<(), String> {
            self.resized.push((cols, rows));
            Ok(())
        }
    }

    /// A screen at the given grid, built the way the keeper's text becomes one.
    fn screen(lines: &[&str], cols: u8, rows: u8) -> Screen {
        Screen::from_keeper(&lines.join("\n"), u64::from(cols), u64::from(rows), (0, 0), false, false, 0)
    }

    fn pane() -> PaneModes {
        PaneModes {
            bracketed_paste: false,
        }
    }

    fn fetch(have: u32, cols: u8, rows: u8) -> Message {
        Message::Fetch {
            have_snapshot_id: have,
            cols,
            rows,
            shell: "abcd-1".to_owned(),
        }
    }

    /// The request a bridge builds for a command from shell channel `shell` of device `device`.
    fn req<'a>(shell: u8, device: u32, id: &'a str, grid: Option<(u8, u8)>) -> SendRequest<'a> {
        SendRequest {
            shell,
            device,
            id,
            grid,
            pane: pane(),
        }
    }

    fn command(op_id: u32, items: Vec<SendItem>) -> Message {
        Message::Send {
            op_id,
            chunk_idx: 0,
            chunk_count: 1,
            have_snapshot_id: 0,
            cols: 20,
            rows: 3,
            flags: 0,
            shell: "abcd-1".to_owned(),
            items,
        }
    }

    fn keys(names: &[&str]) -> SendItem {
        SendItem::Keys(names.iter().map(|n| (*n).to_owned()).collect())
    }

    /// A command is written once, and its answer is the settled screen.
    #[tokio::test]
    async fn a_command_is_written_then_answered_once_its_pane_settles() {
        let mut host = FakeHost::with(vec![screen(&["$ ls", "", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        let out = rpc
            .on_send(
                req(0, 7, "abcd-1", Some((20, 3))),
                &command(1, vec![SendItem::Text(b"ls".to_vec()), keys(&["Enter"])]),
                &mut host,
                start,
            )
            .await
            .expect("the command is accepted");
        assert_eq!(out, Outcome::Pending, "a written command waits for its pane");
        assert_eq!(host.written, vec!["ls".to_owned(), "Enter".to_owned()]);
        assert_eq!(host.resized, vec![(20, 3)], "the pane is fitted first");

        // Nothing changed, so the no-change deadline is what releases it.
        assert!(rpc.settle(0, "abcd-1", &mut host, start).await.is_none());
        let answered = rpc
            .settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(SETTLE_NO_CHANGE_MS),
            )
            .await
            .expect("the answer is due");
        let Message::Snapshot {
            flags,
            applied_status,
            applied_op_id,
            ..
        } = &answered[0]
        else {
            panic!("a v15 answer is a Snapshot, not {:?}", answered[0]);
        };
        assert_eq!(*applied_op_id, 1);
        assert_eq!(*applied_status, applied::WRITTEN);
        assert_ne!(flags & snapshot_flag::SETTLED, 0, "a quiet pane is settled");
        assert!(!rpc.busy(0), "the shell is free after its answer");
    }

    /// The same command twice writes once, and says so.
    #[tokio::test]
    async fn a_repeated_command_is_never_written_twice() {
        let mut host = FakeHost::with(vec![screen(&["$ ", "", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        let message = command(9, vec![SendItem::Text(b"rm -rf /tmp/x".to_vec())]);
        rpc.on_send(req(0, 7, "abcd-1", None), &message, &mut host, start)
            .await
            .expect("accepted");
        let _ = rpc
            .settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(SETTLE_NO_CHANGE_MS),
            )
            .await
            .expect("answered");
        let before = host.written.len();

        let Outcome::Answer(reply) = rpc
            .on_send(req(0, 7, "abcd-1", None), &message, &mut host, start)
            .await
            .expect("a repeat is answered, not refused")
        else {
            panic!("a duplicate is answered at once");
        };
        assert_eq!(
            host.written.len(),
            before,
            "the duplicate must not reach the pty"
        );
        let Message::Snapshot {
            applied_status, ..
        } = &reply[0]
        else {
            panic!("expected a Snapshot");
        };
        assert_eq!(*applied_status, applied::DUPLICATE);
    }

    /// A second device's identical `op_id` is a different command.
    #[tokio::test]
    async fn another_device_uses_the_same_op_id_independently() {
        let mut host = FakeHost::with(vec![screen(&["$ ", "", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        let message = command(4, vec![SendItem::Text(b"a".to_vec())]);
        rpc.on_send(req(0, 1, "abcd-1", None), &message, &mut host, start)
            .await
            .expect("accepted");
        let _ = rpc
            .settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(SETTLE_NO_CHANGE_MS),
            )
            .await;
        assert_eq!(host.written, vec!["a".to_owned()]);
        let out = rpc
            .on_send(req(0, 2, "abcd-1", None), &message, &mut host, start)
            .await
            .expect("accepted");
        assert_eq!(out, Outcome::Pending, "device 2's command runs");
        assert_eq!(host.written, vec!["a".to_owned(), "a".to_owned()]);
    }

    /// A chunked paste is one write, and a partial one is never written.
    #[tokio::test]
    async fn a_chunked_paste_is_applied_once_and_whole() {
        let mut host = FakeHost::with(vec![screen(&["$ ", "", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        let first = Message::Send {
            op_id: 12,
            chunk_idx: 0,
            chunk_count: 2,
            have_snapshot_id: 0,
            cols: 20,
            rows: 3,
            flags: 0,
            shell: "abcd-1".to_owned(),
            items: vec![SendItem::Paste(b"one\n".to_vec())],
        };
        let second = Message::Send {
            op_id: 12,
            chunk_idx: 1,
            chunk_count: 2,
            have_snapshot_id: 0,
            cols: 20,
            rows: 3,
            flags: 0,
            shell: "abcd-1".to_owned(),
            items: vec![SendItem::Paste(b"two\n".to_vec())],
        };
        assert_eq!(
            rpc.on_send(req(0, 7, "abcd-1", None), &first, &mut host, start)
                .await
                .expect("first chunk"),
            Outcome::Pending
        );
        assert!(host.written.is_empty(), "half a paste must not be written");
        rpc.on_send(req(0, 7, "abcd-1", None), &second, &mut host, start)
            .await
            .expect("second chunk");
        assert_eq!(
            host.written.len(),
            1,
            "the joined paste is one write, not two: {:?}",
            host.written
        );
        assert_eq!(host.written[0], "one\rtwo\r");
    }

    /// A screen still changing at the budget cap is sent as a mid-state, once.
    #[tokio::test]
    async fn a_changing_pane_is_answered_at_the_cap_and_only_once() {
        let mut host = FakeHost::with(vec![
            screen(&["$ ", "", ""], 20, 3),
            screen(&["$ build", "", ""], 20, 3),
            screen(&["$ build ok", "", ""], 20, 3),
        ]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        rpc.on_send(
            req(0, 7, "abcd-1", None),
            &command(3, vec![SendItem::Text(b"make".to_vec()), keys(&["Enter"])]),
            &mut host,
            start,
        )
        .await
        .expect("accepted");
        // Two passes, each seeing a new screen, keep it waiting.
        host.screens.rotate_left(1);
        assert!(
            rpc.settle(0, "abcd-1", &mut host, start + Duration::from_millis(200))
                .await
                .is_none()
        );
        host.screens.rotate_left(1);
        assert!(
            rpc.settle(0, "abcd-1", &mut host, start + Duration::from_millis(400))
                .await
                .is_none()
        );
        assert!(
            rpc.settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(INLINE_SETTLE_MAX_MS + 1)
            )
            .await
            .is_some(),
            "the cap releases the answer"
        );
        assert!(
            rpc.settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(INLINE_SETTLE_MAX_MS + 500)
            )
            .await
            .is_none(),
            "and there is never a second unsolicited wave"
        );
    }

    /// A `Fetch` answers with the screen and touches no draft.
    #[tokio::test]
    async fn a_fetch_answers_at_once_and_writes_nothing() {
        let mut host = FakeHost::with(vec![screen(&["$ one", "$ two", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .on_fetch(0, "abcd-1", &fetch(0, 40, 10), &mut host)
            .await
        else {
            panic!("a fetch is answered immediately");
        };
        assert!(host.written.is_empty(), "a fetch never writes");
        assert_eq!(host.resized, vec![(40, 10)]);
        let Message::Snapshot {
            flags,
            applied_status,
            cols,
            rows,
            patches,
            ..
        } = &reply[0]
        else {
            panic!("expected a Snapshot");
        };
        assert_eq!(*applied_status, applied::NONE);
        assert_eq!((*cols, *rows), (20, 3));
        assert_eq!(flags & snapshot_flag::SETTLED, 0, "a fetch states no verdict");
        assert_eq!(patches.len(), 2, "two non-empty rows");
    }

    /// The second answer to a shell is a diff against the first.
    #[tokio::test]
    async fn the_next_screen_patches_only_the_rows_that_changed() {
        let mut host = FakeHost::with(vec![
            screen(&["$ ls", "a", ""], 20, 3),
            screen(&["$ ls", "a", "b"], 20, 3),
        ]);
        let mut rpc = MeshRpc::new();
        let start = Instant::now();
        rpc.on_send(
            req(0, 7, "abcd-1", None),
            &command(1, vec![SendItem::Text(b"ls".to_vec())]),
            &mut host,
            start,
        )
        .await
        .expect("accepted");
        let first = rpc
            .settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(SETTLE_NO_CHANGE_MS),
            )
            .await
            .expect("answered");
        let Message::Snapshot {
            snapshot_id,
            flags,
            patches,
            ..
        } = &first[0]
        else {
            panic!("expected a Snapshot");
        };
        assert_ne!(flags & snapshot_flag::FULL, 0, "a first paint is full");
        let base = *snapshot_id;
        let full_rows = patches.len();

        host.screens.push(screen(&["$ ls", "a", "c"], 20, 3));
        rpc.on_send(
            req(0, 7, "abcd-1", None),
            &Message::Send {
                op_id: 2,
                chunk_idx: 0,
                chunk_count: 1,
                have_snapshot_id: base,
                cols: 20,
                rows: 3,
                flags: 0,
                shell: "abcd-1".to_owned(),
                items: vec![SendItem::Text(b"pwd".to_vec())],
            },
            &mut host,
            start,
        )
        .await
        .expect("accepted");
        let second = rpc
            .settle(
                0,
                "abcd-1",
                &mut host,
                start + Duration::from_millis(2 * SETTLE_NO_CHANGE_MS),
            )
            .await
            .expect("answered");
        let Message::Snapshot {
            base_id, patches, ..
        } = &second[0]
        else {
            panic!("expected a Snapshot");
        };
        assert_eq!(*base_id, base, "the diff names the screen it patches");
        assert_eq!(patches.len(), 1, "one row changed out of {full_rows}");
        assert_eq!(patches[0].y, 2);
    }

    /// A dense screen is several chunks, and each fits a message.
    #[tokio::test]
    async fn a_dense_screen_is_chunked_within_a_message() {
        let wide: Vec<String> = (0..40).map(|i| format!("{i:0>2} {}", "x".repeat(30))).collect();
        let rows: Vec<&str> = wide.iter().map(String::as_str).collect();
        let mut host = FakeHost::with(vec![screen(&rows, 80, 40)]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc.on_fetch(0, "abcd-1", &fetch(0, 0, 0), &mut host).await
        else {
            panic!("answered");
        };
        assert!(reply.len() > 1, "a dense paint is several messages");
        let count = reply.len();
        for (index, message) in reply.iter().enumerate() {
            let Message::Snapshot {
                chunk_idx,
                chunk_count,
                snapshot_id,
                ..
            } = message
            else {
                panic!("expected a Snapshot");
            };
            assert_eq!(usize::from(*chunk_idx), index);
            assert_eq!(usize::from(*chunk_count), count);
            assert!(*snapshot_id > 0);
            // The encoder is the authority on whether this fits, so ask it.
            assert!(
                message.encode().is_ok(),
                "chunk {index} does not encode"
            );
        }
    }

    /// A keeper refusal is answered as a refusal, not as a written command.
    #[tokio::test]
    async fn a_refused_write_is_reported_rather_than_settled() {
        let mut host = FakeHost::with(vec![screen(&["$ ", "", ""], 20, 3)]);
        host.fail_write = true;
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .on_send(
                req(0, 7, "abcd-1", None),
                &command(5, vec![SendItem::Text(b"x".to_vec())]),
                &mut host,
                Instant::now(),
            )
            .await
            .expect("answered")
        else {
            panic!("a failed write is answered at once");
        };
        let Message::Snapshot {
            applied_status, ..
        } = &reply[0]
        else {
            panic!("expected a Snapshot");
        };
        assert_eq!(*applied_status, applied::FAILED);
    }

    /// The ring is bounded, and the oldest answer falls out.
    #[tokio::test]
    async fn the_ring_keeps_only_the_newest_answers() {
        let mut host = FakeHost::with(vec![screen(&["$ ", "", ""], 20, 3)]);
        let mut rpc = MeshRpc::new();
        for op_id in 1..=(RING as u32 + 5) {
            rpc.on_fetch(0, "abcd-1", &fetch(0, 0, 0), &mut host).await;
            rpc.remember(99, op_id, applied::WRITTEN);
        }
        assert_eq!(rpc.answered.len(), RING);
        assert!(
            rpc.answered_for(99, 1).is_none(),
            "the oldest answer has fallen out of the ring"
        );
        assert_eq!(rpc.answered_for(99, RING as u32 + 5), Some(applied::DUPLICATE));
    }
}
