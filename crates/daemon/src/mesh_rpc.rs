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
//! Nothing here knows a radio, a socket or a keeper: [`MeshHost`] is the keeper and claim-table
//! operations an answer depends on, and the clock is a parameter so a test can move time without
//! sleeping.
//!
//! **Every version-15 message arrives through [`MeshRpc::on_message`].** The transport that carries
//! them is a transaction stream with no page byte (`AR-11`), so a message names its own shell and
//! there is nothing for a caller to route: the service owns which shell each channel is attached
//! to, and the shell-control messages (`Attach`, `Takeover`, `List`, `Create`, `Close`, `MakeLive`,
//! `WantHistory`, `Detach`) live here rather than in the frame loop that is being retired
//! (`docs/RADIO-PLAN.md` §10a).

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ldmux_protocol::mesh_term::{
    ChunkError, MAX_SEGMENT, Message, Patch, Screen, SendAssembly, SessionSummary, VERSION, applied,
    clamp_row,
    closed, marks_pages, session_pages, snapshot_flag,
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

/// The keeper and claim-table operations a version-15 message needs, and no others.
///
/// Narrow on purpose: this is the seam stage A keeps when `mesh_bridge.rs` is deleted, so anything
/// named here has to be something the service genuinely does. Every method is one call the daemon
/// already serves over its control socket; the service decides *when* to make it and what the
/// answer means, which is the part that could not live in the transport.
#[allow(async_fn_in_trait)]
pub trait MeshHost {
    /// The pane's grid, cursor, rows and paste mode, as the keeper renders it.
    async fn screen(&mut self, id: &str) -> Result<Screen, String>;
    /// Raw bytes for the pty, already encoded for the pane's paste mode.
    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String>;
    /// Key names, for the keeper's one key table to encode against the pane's live mode.
    async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String>;
    /// Fit the pty to the phone's grid, before the screen it is about to be shown.
    async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String>;
    /// What the phone's picker lists: every running shell with its folder, origin and holder.
    ///
    /// `None` when the host could not be asked, which is a different fact from an empty list —
    /// drawn as empty it emptied the launcher while every shell was alive
    /// (`mesh_bridge.rs`, `Bridge::handle`, the `Message::List` arm).
    async fn listing(&mut self) -> Option<Vec<SessionSummary>>;
    /// The marks that are true right now, as `(shell id, kind)`.
    ///
    /// Read once per list, because that is the radio route's delivery model: the host cannot reach
    /// a phone that is not there, so the table it is handed is what it missed (the `Message::List`
    /// arm of `mesh_bridge.rs`).
    async fn marks(&mut self) -> Vec<(String, u8)> {
        Vec::new()
    }
    /// Who holds `id` now: `"mesh"`, `"desktop"`, `"phone"`, or `""` for nobody.
    ///
    /// The write path asks this and not [`Self::listing`], which is a keeper round trip per shell
    /// in front of every keystroke (`mesh_bridge.rs::Host::holder`). `None` means the host cannot
    /// say,
    /// which is *not* "someone else has it" (`Bridge::displaced`).
    async fn holder(&mut self, id: &str) -> Option<String> {
        let _ = id;
        None
    }
    /// Claim `id` for this link, displacing an idle holder, and say what happened.
    ///
    /// **The host decides whether the holder is in use**, not this service: that is a rule of the
    /// claim table, and a second copy of it here drifts (`mesh_bridge.rs::Host::claim`).
    async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String>;
    /// Give `id` back to nobody. `false` means the host refused, which is a state to report rather
    /// than a value to drop (`mesh_bridge.rs::Host::release`).
    async fn release(&mut self, id: &str) -> bool;
    /// Start a shell in `cwd` (empty = the host's default) and answer its id.
    async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
        let _ = (cwd, cols, rows);
        Err("this host cannot start shells".to_owned())
    }
    /// End `id`, or put it down when a desk window holds it (ADR-0010).
    async fn close(&mut self, id: &str) -> Result<(), String> {
        let _ = id;
        Err("this host cannot end shells".to_owned())
    }
    /// Open `id` in a desk window on the host.
    async fn make_live(&mut self, id: &str) -> Result<(), String> {
        let _ = id;
        Err("this host cannot open desk windows".to_owned())
    }
    /// A page of the shell's own scrollback: `(start, older, lines)` in the keeper's numbering.
    ///
    /// `None` means the host could not answer, which is not "nothing above this"
    /// (`mesh_bridge.rs::Host::history`).
    async fn history(
        &mut self,
        id: &str,
        from: u32,
        rows: u8,
    ) -> Option<(u32, bool, Vec<String>)> {
        let _ = (id, from, rows);
        None
    }
}

/// What the host said about a claim.
///
/// Here rather than in `mesh_bridge.rs` because this is the file that outlives it: the frame loop
/// is deleted in stage A and the claim rule is not (`docs/RADIO-PLAN.md` §10a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The shell is this link's now. Whoever held it was idle, or was displaced by consent.
    Claimed,
    /// An in-use holder refuses, and the user is offered `take it back`.
    Held { holder: String },
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
    /// Nothing is owed now: more chunks are needed, or the answer is still settling, or the
    /// message's whole effect is a side effect (`MakeLive`, `Detach`). Send nothing — the silence
    /// is the burst rule, not a lost message. For a `Send` the caller learns that a command is
    /// waiting from [`MeshRpc::busy`], not from this value.
    Pending,
    /// Hand these to the link, in order, as one reply.
    Answer(Vec<Message>),
}

/// The peer a version-15 command came from.
///
/// **One radio link carries one phone at a time**, because a session is established with one static
/// key and a second phone cannot share it. So the peer is the link, and the at-most-once ring keys
/// every command to the same device. The field exists rather than being left out because the ring's
/// identity is what makes a duplicate safe, and a multiplexed link would key it per peer without
/// touching the service.
pub const PEER_LINK: u32 = 0;

/// The holder string this link's own claims are recorded under.
///
/// The daemon's claim table spells the radio link `"mesh"` (`mesh_bridge.rs::Host::holder`, and the
/// fixture that records claims), and a
/// comparison is what decides whether a shell has been taken from the phone. One constant here so
/// the three comparisons cannot be spelled three ways while the frame loop is still standing.
const MESH_HOLDER: &str = "mesh";

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

/// A shell this service has attached for one channel.
///
/// **The service owns the attachment**, because a version-15 message names its shell and the
/// transport that used to keep the page byte is gone (`AR-11`). It holds exactly what the answers
/// need: the host's id, what the pane said about itself, the grid to fit before the next paint, and
/// the size to put back when the page goes away.
#[derive(Debug, Clone)]
struct Attached {
    /// The host's own id for the shell.
    id: String,
    /// The pane's paste mode, read when it was attached.
    ///
    /// Read here and not lazily because the first keystroke can beat the next paint: a paste that
    /// arrived before the mode was known executed every line (`Bridge::attach_with`).
    pane: PaneModes,
    /// The grid the phone asked the pane to take, when it stated one.
    grid: Option<(u8, u8)>,
    /// The pane's size before this link's fit, put back on `Detach`.
    original: Option<(u8, u8)>,
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
    /// The shell each channel is attached to, keyed by the link's shell index.
    attached: HashMap<u8, Attached>,
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

    /// The host's id for the shell this channel is attached to, if it is attached.
    #[must_use]
    pub fn attached_to(&self, shell: u8) -> Option<&str> {
        self.attached.get(&shell).map(|a| a.id.as_str())
    }

    /// **Every version-15 message, dispatched.**
    ///
    /// The single entry point a transport calls. `shell` is the link channel the transaction
    /// arrived on and `device` the peer it came from; neither is in the message, because a link
    /// carries one phone and the answer is addressed the way the question was.
    ///
    /// `Ok(Outcome::Pending)` means "nothing is owed now" — see [`Outcome::Pending`].
    ///
    /// **Each arm names the `mesh_bridge.rs` function it was ported from, not a line number.**
    /// That file is deleted in stage A and its line numbers move with every edit to it, so a
    /// citation that is right today is a lie tomorrow; a function name is what a reader can still
    /// find with `rg`.
    ///
    /// # Errors
    ///
    /// The `applied` status to answer a `Send` with when the command is refused.
    pub async fn on_message<H: MeshHost>(
        &mut self,
        shell: u8,
        device: u32,
        message: &Message,
        host: &mut H,
        now: Instant,
    ) -> Result<Outcome, u8> {
        match message {
            m @ Message::Send { .. } => self.dispatch_send(shell, device, m, host, now).await,
            m @ Message::Fetch { .. } => Ok(self.dispatch_fetch(shell, m, host).await),
            Message::Attach {
                version,
                selector,
                cols,
                rows,
            } => Ok(self.attach(shell, *version, selector, false, (*cols, *rows), host).await),
            Message::Takeover { selector } => {
                // **The grid the phone already stated**, because a takeover is not a place to
                // renegotiate the protocol and the phone's own `Fetch` covers a change
                // (`Bridge::handle`, the `Message::Takeover` arm). A channel with no attachment
                // states 0, and the pane keeps the size the read finds.
                let grid = self.attached.get(&shell).and_then(|a| a.grid);
                let grid = grid.unwrap_or((0, 0));
                Ok(self.attach(shell, VERSION, selector, true, grid, host).await)
            }
            Message::List => Ok(self.list(host).await),
            Message::Create { cwd, .. } => Ok(self.create(shell, cwd, host).await),
            Message::Close { selector } => Ok(self.close(shell, selector, host).await),
            Message::MakeLive { selector } => {
                self.make_live(shell, selector, host).await;
                Ok(Outcome::Pending)
            }
            Message::WantHistory { from, rows } => {
                Ok(self.want_history(shell, *from, *rows, host).await)
            }
            Message::Detach => {
                self.detach(shell, host).await;
                Ok(Outcome::Pending)
            }
            // **The frame loop's vocabulary has no meaning here.** `Input`, `Key`, `Resize`,
            // `Resync`, `Hold`, `Sessions`, `Marks`, `Frame` are what a version-14 phone streams,
            // and the transport that carries them is retired with the loop
            // (`docs/RADIO-PLAN.md` §10a). Ignoring one is the whole effect it can have.
            _ => Ok(Outcome::Pending),
        }
    }

    /// Let go of every shell this service holds, putting each pane's size back.
    ///
    /// A link that re-handshakes, or a stream that cannot be decoded, ends every attach as surely
    /// as `Detach` does — and a claim left behind locks a desk window out of a shell nobody is
    /// watching (`Bridge::drop_all`).
    pub async fn drop_all<H: MeshHost>(&mut self, host: &mut H) {
        let shells: Vec<u8> = self.attached.keys().copied().collect();
        for shell in shells {
            self.detach(shell, host).await;
            self.forget_shell(shell);
        }
    }

    /// `Send`, with the attachment, the holder check and the peer's grid.
    ///
    /// # Errors
    ///
    /// The `applied` status to answer with when the command is refused.
    async fn dispatch_send<H: MeshHost>(
        &mut self,
        shell: u8,
        device: u32,
        message: &Message,
        host: &mut H,
        now: Instant,
    ) -> Result<Outcome, u8> {
        let Message::Send {
            op_id,
            have_snapshot_id,
            cols,
            rows,
            ..
        } = message
        else {
            return Ok(Outcome::Pending);
        };
        let Some(attached) = self.attached.get(&shell).cloned() else {
            // Not attached: the phone is told so rather than left waiting for a screen that belongs
            // to no pane, and nothing is written (`Bridge::v15_send`).
            return Ok(Outcome::Answer(vec![Self::refusal(
                *op_id,
                *have_snapshot_id,
                applied::NO_SHELL,
            )]));
        };
        if self.displaced(&attached.id, host).await {
            return Ok(self.refuse_held_write(shell, host).await);
        }
        // **The message's grid, when it states one.** Its own doc says the pty is fitted before the
        // command runs; the remembered grid is the fallback for a message that states none, which
        // is the only thing the bridge had (`Bridge::v15_send`).
        let stated = stated_grid(*cols, *rows);
        if let Some(grid) = stated
            && let Some(a) = self.attached.get_mut(&shell)
        {
            a.grid = Some(grid);
        }
        let request = SendRequest {
            shell,
            device,
            id: &attached.id,
            grid: stated.or(attached.grid),
            pane: attached.pane,
        };
        self.on_send(request, message, host, now).await
    }

    /// `Fetch`, with the attachment and the same holder rule as a write.
    async fn dispatch_fetch<H: MeshHost>(
        &mut self,
        shell: u8,
        message: &Message,
        host: &mut H,
    ) -> Outcome {
        let Some(attached) = self.attached.get(&shell).cloned() else {
            return Outcome::Pending;
        };
        if self.displaced(&attached.id, host).await {
            return self.refuse_held_write(shell, host).await;
        }
        self.on_fetch(shell, &attached.id, message, host).await
    }

    /// `Attach`, with or without the consent that displaces whoever holds the shell.
    ///
    /// Ported from `Bridge::attach_with`, with the frame loop's part left behind: there is no
    /// profile, no grace and no differ, because the answer is one `Snapshot` of the whole screen.
    /// What is kept is the order of decisions, which is what the comments there are about.
    async fn attach<H: MeshHost>(
        &mut self,
        shell: u8,
        version: u8,
        selector: &str,
        takeover: bool,
        grid: (u8, u8),
        host: &mut H,
    ) -> Outcome {
        // **The one gate that stays.** A version-15 transport does not negotiate a phone's version
        // again (`docs/RADIO-PLAN.md` §10a), but a claim from a build that speaks version 14 must
        // get one message saying so rather than half a screen it cannot read.
        if version < VERSION {
            return Outcome::Answer(vec![Message::Closed {
                reason: closed::VERSION,
            }]);
        }
        let Some(listing) = host.listing().await else {
            // Unknown is not "not found": answering `Closed` here told the phone the shell had
            // ended when the host had merely failed to answer (`Bridge::attach_with`).
            return Outcome::Pending;
        };
        let matches: Vec<String> = listing
            .into_iter()
            .map(|row| row.id)
            .filter(|id| selector.is_empty() || id.starts_with(selector))
            .collect();
        let id = match matches.as_slice() {
            [] => {
                return Outcome::Answer(vec![Message::Closed {
                    reason: closed::NOT_FOUND,
                }]);
            }
            [one] => one.clone(),
            _ => {
                return Outcome::Answer(vec![Message::Closed {
                    reason: closed::AMBIGUOUS,
                }]);
            }
        };
        // **The host decides whether the holder is in use** (`Bridge::attach_with`).
        match host.claim(&id, takeover).await {
            Ok(ClaimOutcome::Claimed) => {}
            Ok(ClaimOutcome::Held { .. }) => {
                return Outcome::Answer(vec![Message::Closed {
                    reason: closed::HELD,
                }]);
            }
            Err(_) => {
                return Outcome::Answer(vec![Message::Closed {
                    reason: closed::UNAVAILABLE,
                }]);
            }
        }
        // **A re-attach replaces the claim; it must release the old one.** Otherwise a reconnect
        // that raced its own detach leaves the previous session claimed by a viewer that no longer
        // exists (`Bridge::attach_with`). The old pane keeps its own size: the phone is moving
        // to another shell, not leaving this one.
        if let Some(old) = self.attached.remove(&shell)
            && old.id != id
        {
            host.release(&old.id).await;
        }
        // **The size to put back is read before anything fits the pane** (`Bridge::attach_with`),
        // and the paste mode with it: read after the fit, the restore records the phone's own grid,
        // and a paste that beats the next paint runs every line.
        let read = host.screen(&id).await.ok();
        let pane = PaneModes {
            bracketed_paste: read.as_ref().is_some_and(|s| s.bracketed_paste),
        };
        let original = read.map(|s| (s.cols, s.rows));
        let stated = stated_grid(grid.0, grid.1);
        let remembered = stated.or_else(|| self.attached.get(&shell).and_then(|a| a.grid));
        if let Some((cols, rows)) = remembered {
            // A refused fit is not fatal: the first screen then reports the pane's real grid.
            let _ = host.resize(&id, cols, rows).await;
        }
        self.attached.insert(
            shell,
            Attached {
                id: id.clone(),
                pane,
                grid: remembered,
                original,
            },
        );
        self.mark_driven(shell);
        // **One whole screen, at the pane's real size.** The frame loop's profile-then-rows dance
        // existed because a phone could not ask for its grid until it knew the pane's; a stated
        // grid has already asked (`Bridge::attach_with`).
        Outcome::Answer(
            self.answer(
                shell,
                &id,
                Reply {
                    op_id: 0,
                    status: applied::NONE,
                    base_id: 0,
                },
                host,
                Settled::Yes,
            )
            .await,
        )
    }

    /// `List`: the running shells and the marks that are true now, in one reply.
    async fn list<H: MeshHost>(&mut self, host: &mut H) -> Outcome {
        // **A failed listing must still answer.** Pending sends nothing at all, and the phone cannot
        // tell "the host did not answer" from "there is nothing to answer with": it waits on a request
        // the host heard and dropped. Measured 2026-11-13, a phone that had established, whose List
        // the host acknowledged, and which then sat on "connecting" for ever because `listing()`
        // returned None. An empty table is the honest answer — it says "no shells" rather than
        // nothing — and the empty entry list is what makes the phone leave its waiting state.
        let listing = host.listing().await.unwrap_or_default();
        // **Every field, unconditionally.** `cwd`, `origin`, `owner` and `shell` were version
        // flags only because one path served version 5, 6, 7 and 10 phones at once
        // (`mesh_bridge.rs`, the `Message::List` arm); a version-15 phone reads all four.
        let mut answer = session_pages(&listing, true, true, true, true);
        // **The marks ride with the list**, in the same reply: the launcher is the one screen that
        // shows every shell, and two replies were two records (the `Message::List` arm of
        // `mesh_bridge.rs`).
        answer.extend(marks_pages(host.marks().await));
        Outcome::Answer(answer)
    }

    /// `Create`: a shell born at the phone's grid.
    async fn create<H: MeshHost>(&mut self, shell: u8, cwd: &str, host: &mut H) -> Outcome {
        // **The grid this channel stated, else the host's default.** A launcher channel usually has
        // no attachment, which is why the fallback exists (the `Message::Create` arm of
        // `Bridge::handle`).
        let (cols, rows) = self
            .attached
            .get(&shell)
            .and_then(|a| a.grid)
            .unwrap_or((80, 24));
        match host.create(cwd, cols, rows).await {
            Ok(id) => Outcome::Answer(vec![Message::Created { id }]),
            // A host that cannot start one says so; the phone must not sit waiting.
            Err(_) => Outcome::Answer(vec![Message::Closed {
                reason: closed::UNAVAILABLE,
            }]),
        }
    }

    /// `Close`: end the shell, unless another device holds it, then answer the new list.
    async fn close<H: MeshHost>(&mut self, shell: u8, selector: &str, host: &mut H) -> Outcome {
        let Some(id) = self.resolve(shell, selector, host).await else {
            return Outcome::Pending;
        };
        // **A shell another device holds is not this phone's to end.** The tailnet route refuses
        // the same press and offers a takeover; without this rule the same button destroyed a
        // shell a desk window was working in (`Bridge::handle`, the `Message::Close` arm).
        let holder = host.holder(&id).await.unwrap_or_default();
        if !holder.is_empty() && holder != MESH_HOLDER {
            return Outcome::Answer(vec![Message::Closed {
                reason: closed::HELD,
            }]);
        }
        if host.close(&id).await.is_err() {
            return Outcome::Pending;
        }
        // **Release only the attach that named this shell**: a channel re-attached elsewhere must
        // not have its new claim released (the same arm). Nothing is put back, because
        // a shell that has ended has no size.
        self.release_attachment(shell, &id, host).await;
        match host.listing().await {
            Some(listing) => Outcome::Answer(session_pages(&listing, true, true, true, true)),
            None => Outcome::Pending,
        }
    }

    /// `MakeLive`: open the shell in a desk window. Nothing is said back.
    async fn make_live<H: MeshHost>(&mut self, shell: u8, selector: &str, host: &mut H) {
        let Some(id) = self.resolve(shell, selector, host).await else {
            return;
        };
        let _ = host.make_live(&id).await;
    }

    /// `WantHistory`: the shell's own scrollback, paged by bytes.
    ///
    /// Ported verbatim from `Bridge::handle`'s `Message::WantHistory` arm, including the paging
    /// arithmetic: a body is
    /// at most 600 bytes, each page states the absolute line of its first row, and `older` rides
    /// every page because it is a fact about the answer rather than about one page.
    async fn want_history<H: MeshHost>(
        &mut self,
        shell: u8,
        from: u32,
        rows: u8,
        host: &mut H,
    ) -> Outcome {
        let Some(id) = self.attached.get(&shell).map(|a| a.id.clone()) else {
            return Outcome::Pending;
        };
        let Some((start, older, lines)) = host.history(&id, from, rows).await else {
            return Outcome::Pending;
        };
        let mut pages: Vec<Message> = Vec::new();
        let mut page: Vec<String> = Vec::new();
        let mut used = 5usize;
        // `at` is the absolute line of `page[0]`: a page advances by its **line count**, not by its
        // byte count.
        let mut at = start;
        for line in lines {
            // **A row is clamped here, at the wire**: a row that cannot be encoded takes the whole
            // page with it, and a skipped reply is a page that silently never arrives.
            let line = clamp_row(&line);
            let cost = 1 + line.len();
            if !page.is_empty() && used + cost > 560 {
                let from = at;
                at += u32::try_from(page.len()).unwrap_or(u32::MAX);
                pages.push(Message::HistoryPage {
                    from,
                    last: false,
                    older,
                    lines: std::mem::take(&mut page),
                });
                used = 5;
            }
            used += cost;
            page.push(line);
        }
        pages.push(Message::HistoryPage {
            from: at,
            last: true,
            older,
            lines: page,
        });
        Outcome::Answer(pages)
    }

    /// `Detach`: give the claim back and put the pane's own size back.
    async fn detach<H: MeshHost>(&mut self, shell: u8, host: &mut H) {
        let Some(a) = self.attached.remove(&shell) else {
            return;
        };
        host.release(&a.id).await;
        // **Only after a fit of ours.** A shell the phone merely looked at keeps the desk's size,
        // and a call that changes nothing is still a keeper round trip (`Bridge::restore_grid`).
        if a.grid.is_some()
            && let Some((cols, rows)) = a.original
        {
            let _ = host.resize(&a.id, cols, rows).await;
        }
    }

    /// Give the claim back when the attachment on this channel is `id`, and forget the channel.
    async fn release_attachment<H: MeshHost>(&mut self, shell: u8, id: &str, host: &mut H) {
        let Some(a) = self.attached.get(&shell).filter(|a| a.id == id).cloned() else {
            return;
        };
        self.attached.remove(&shell);
        host.release(&a.id).await;
    }

    /// Whether the shell has moved to another viewer since it was attached.
    async fn displaced<H: MeshHost>(&mut self, id: &str, host: &mut H) -> bool {
        match host.holder(id).await {
            Some(holder) => !holder.is_empty() && holder != MESH_HOLDER,
            // Unknown is not "someone else": a host that cannot answer must not cut the phone off
            // mid-word (`Bridge::displaced`).
            None => false,
        }
    }

    /// Tell the phone the shell it is still attached to is another device's, and let go of it.
    ///
    /// The write path obeys the same rule as the attach path, and the claim plus the pane's old
    /// size are given back exactly as `Detach` gives them back — otherwise the desk window that
    /// took the shell is left with a pane fitted to a phone that is no longer attached
    /// (`Bridge::refuse_held_write`).
    async fn refuse_held_write<H: MeshHost>(&mut self, shell: u8, host: &mut H) -> Outcome {
        self.detach(shell, host).await;
        Outcome::Answer(vec![Message::Closed {
            reason: closed::HELD,
        }])
    }

    /// The shell a selector names, or `None` when it names nothing running.
    ///
    /// The rule `Attach` uses: an empty selector is the shell this channel is attached to, and
    /// anything else is an id prefix. Nothing is guessed — a close aimed at the wrong shell is not
    /// recoverable (`Bridge::resolve`).
    async fn resolve<H: MeshHost>(
        &mut self,
        shell: u8,
        selector: &str,
        host: &mut H,
    ) -> Option<String> {
        if selector.is_empty() {
            return self.attached.get(&shell).map(|a| a.id.clone());
        }
        let listing = host.listing().await?;
        listing
            .into_iter()
            .map(|row| row.id)
            .find(|id| id.starts_with(selector))
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
            // **A paste reads the mode now, not at attach.** bash sets `CSI ? 2004 h` with its
            // first prompt, so a mode captured at attach is the mode before the shell existed, and
            // a paste that beat the next paint ran every line (measured 2026-11-12: `echo ONE`
            // and `echo TWO` executed instead of being inserted). A typed run cannot carry a
            // newline -- the phone sends one as a key -- so only a paste needs the fresh read, and
            // pastes are rare enough that one screen costs nothing.
            let pane = if items
                .iter()
                .any(|item| matches!(item, ldmux_protocol::mesh_term::SendItem::Paste(_)))
            {
                match host.screen(id).await {
                    Ok(read) => PaneModes {
                        bracketed_paste: read.bracketed_paste,
                    },
                    // A pane that cannot be read is a pane that cannot be checked; the attach-time
                    // mode is what we know, and writing nothing is worse than writing unquoted.
                    Err(_) => pane,
                }
            } else {
                pane
            };
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

/// The widest grid a phone may state, in cells.
///
/// The keeper clamps a pane to its own limit as well, so this is the mesh route's stated bound
/// rather than the last word: a grid outside it is treated as "not stated" and the pane keeps the
/// size the attach read. It came from `mesh_bridge.rs` with `ClaimOutcome`, because the frame loop
/// goes and the bound does not.
pub const MAX_GRID_COLS: u8 = 200;
pub const MAX_GRID_ROWS: u8 = 120;

/// A phone's stated grid, or `None` when it stated nothing a pane could take.
#[must_use]
pub fn stated_grid(cols: u8, rows: u8) -> Option<(u8, u8)> {
    ((1..=MAX_GRID_COLS).contains(&cols) && (1..=MAX_GRID_ROWS).contains(&rows))
        .then_some((cols, rows))
}

/// Milliseconds from `from` to `now`, saturating to zero for a clock that went backwards.
fn elapsed(now: Instant, from: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

/// One row as the patches that carry it, splitting it if it is longer than a segment.
///
/// **A row is not one patch.** `MAX_SEGMENT` is 120 **bytes** and the encoder refuses a longer
/// segment, which fails the *whole* `Snapshot` — so a single wide row used to stop the screen
/// updating entirely, silently, because the daemon reported the failure only in its stats. That is
/// easy to reach and not an edge case: a box-drawing glyph is three bytes, so forty-one of them are
/// over the limit, and a 46-column pane has room for forty-six.
///
/// The first segment replaces the row and the rest append, which is what `Patch::append` is for and
/// what the phone already applies (`crates/session/src/client.rs`, `PageMirror::paint`).
fn row_patches(y: u8, text: &str) -> Vec<Patch> {
    let mut out = Vec::new();
    let mut append = false;
    let mut rest = text;
    loop {
        let mut cut = rest.len().min(MAX_SEGMENT);
        // **Bytes, never mid-character.** A cut inside a glyph would produce invalid UTF-8 that the
        // encoder refuses, which is the failure this function exists to prevent.
        while cut > 0 && !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == 0 {
            break;
        }
        out.push(Patch {
            y,
            append,
            text: rest[..cut].to_owned(),
        });
        rest = &rest[cut..];
        append = true;
        if rest.is_empty() {
            break;
        }
    }
    out
}

/// Every row, for a first paint.
fn full_rows(screen: &Screen) -> Vec<Patch> {
    let mut out = Vec::new();
    for (y, text) in screen.lines.iter().enumerate() {
        // A blank row needs no patch when the whole screen is being replaced: the phone clears
        // every row first, so an empty patch would cost three bytes to say nothing.
        if text.is_empty() {
            continue;
        }
        let Ok(y) = u8::try_from(y) else { continue };
        out.extend(row_patches(y, text));
    }
    out
}

/// Only the rows that differ, for a diff against the screen the phone holds.
///
/// A row that shrank must still be sent, even when the new text is a prefix of the old: the phone
/// replaces the row rather than editing it, so an unchanged prefix with a shorter tail needs the
/// patch to become shorter at all.
fn diff_rows(before: &Screen, after: &Screen) -> Vec<Patch> {
    let mut out = Vec::new();
    for (y, text) in after.lines.iter().enumerate() {
        let old = before.lines.get(y).map_or("", String::as_str);
        if old == text {
            continue;
        }
        let Ok(y) = u8::try_from(y) else { continue };
        // A row that emptied is still a patch: the phone replaces the row rather than editing it,
        // so "now blank" has to be said.
        if text.is_empty() {
            out.push(Patch {
                y,
                append: false,
                text: String::new(),
            });
            continue;
        }
        out.extend(row_patches(y, text));
    }
    out
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
    /// The one radio link's peer id, as the service sees it.
    const PEER_DEVICE: u32 = 0;
    use std::time::Duration;

    /// A host that records what it was asked to do and answers with a scripted screen.
    #[derive(Default)]
    struct FakeHost {
        screens: Vec<Screen>,
        written: Vec<String>,
        resized: Vec<(u8, u8)>,
        fail_write: bool,
        /// What `listing` answers. `None` is a host that could not be asked, which is a fact the
        /// service must not draw as "no shells".
        runs: Option<Vec<&'static str>>,
        marks: Vec<(String, u8)>,
        /// Every claim, in order, with the consent it carried.
        claims: Vec<(String, bool)>,
        released: Vec<String>,
        holder: Option<String>,
        /// Whether `claim` refuses with [`ClaimOutcome::Held`].
        refuse_claim: bool,
        created: Vec<(String, u8, u8)>,
        closed: Vec<String>,
        live: Vec<String>,
        history: Option<(u32, bool, Vec<String>)>,
    }

    impl FakeHost {
        fn with(screens: Vec<Screen>) -> Self {
            Self {
                screens,
                runs: Some(Vec::new()),
                ..Self::default()
            }
        }
        fn last(&self) -> Screen {
            self.screens
                .last()
                .cloned()
                .unwrap_or_else(|| screen(&[""], 20, 3))
        }
        fn running(mut self, ids: &[&'static str]) -> Self {
            self.runs = Some(ids.to_vec());
            self
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
        async fn listing(&mut self) -> Option<Vec<SessionSummary>> {
            self.runs.as_ref().map(|ids| {
                ids.iter()
                    .map(|id| SessionSummary {
                        id: (*id).to_owned(),
                        ..SessionSummary::default()
                    })
                    .collect()
            })
        }
        async fn marks(&mut self) -> Vec<(String, u8)> {
            self.marks.clone()
        }
        async fn holder(&mut self, _id: &str) -> Option<String> {
            self.holder.clone()
        }
        async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String> {
            self.claims.push((id.to_owned(), takeover));
            if self.refuse_claim {
                return Ok(ClaimOutcome::Held {
                    holder: "desktop".to_owned(),
                });
            }
            Ok(ClaimOutcome::Claimed)
        }
        async fn release(&mut self, id: &str) -> bool {
            self.released.push(id.to_owned());
            true
        }
        async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
            self.created.push((cwd.to_owned(), cols, rows));
            Ok("made-1".to_owned())
        }
        async fn close(&mut self, id: &str) -> Result<(), String> {
            self.closed.push(id.to_owned());
            Ok(())
        }
        async fn make_live(&mut self, id: &str) -> Result<(), String> {
            self.live.push(id.to_owned());
            Ok(())
        }
        async fn history(
            &mut self,
            _id: &str,
            _from: u32,
            _rows: u8,
        ) -> Option<(u32, bool, Vec<String>)> {
            self.history.clone()
        }
    }

    /// A screen at the given grid, built the way the keeper's text becomes one.
    fn screen(lines: &[&str], cols: u8, rows: u8) -> Screen {
        Screen::from_keeper(&lines.join("\n"), u64::from(cols), u64::from(rows), (0, 0), false, false, 0)
    }

    /// **A row longer than a segment is split, so the screen still encodes.**
    ///
    /// `MAX_SEGMENT` is 120 **bytes** and the encoder refuses a longer one, which fails the whole
    /// `Snapshot` rather than one row. So before this, a single wide row stopped the screen
    /// updating entirely and said so only in the daemon's stats — and it is not an edge case: a
    /// box-drawing glyph is three bytes, so forty-one of them are over the limit on a 46-column
    /// pane. Measured 2026-11-12 while auditing `RELEASE-AUDIT.md` 6.136.
    #[test]
    fn a_row_wider_than_a_segment_is_split_and_the_snapshot_still_encodes() {
        // Three bytes a glyph, so this is 150 bytes: over the limit by a wide margin and by the
        // route a real pane takes to get there.
        let wide = "─".repeat(50);
        assert!(wide.len() > MAX_SEGMENT, "the fixture must exceed one segment");
        let patches = full_rows(&screen(&[&wide], 46, 2));
        assert_eq!(patches.len(), 2, "one row became two segments: {patches:?}");
        assert!(!patches[0].append, "the first segment replaces the row");
        assert!(patches[1].append, "the second appends to it");
        assert_eq!(
            patches.iter().map(|p| p.text.clone()).collect::<String>(),
            wide,
            "the segments are the row, in order, with nothing lost"
        );
        for patch in &patches {
            assert!(patch.text.len() <= MAX_SEGMENT);
        }
        // The whole point: the message these patches go into can be encoded at all.
        let snapshot = Message::Snapshot {
            flags: 0,
            chunk_idx: 0,
            chunk_count: 1,
            snapshot_id: 1,
            base_id: 0,
            applied_op_id: 0,
            applied_status: 0,
            cols: 46,
            rows: 2,
            cursor_x: 0,
            cursor_y: 0,
            first_line: 0,
            shell: "s1".to_owned(),
            patches,
        };
        assert!(
            snapshot.encode().is_ok(),
            "a wide row still fails the frame: {:?}",
            snapshot.encode().err()
        );
    }

    /// A segment never ends inside a character.
    ///
    /// A cut mid-glyph produces invalid UTF-8, which the encoder refuses — the same failure the
    /// split exists to prevent, reached another way.
    #[test]
    fn a_segment_cut_never_splits_a_character() {
        // Two-byte and three-byte characters arranged so a naive 120-byte cut lands mid-glyph.
        let row = format!("{}{}", "─".repeat(39), "é".repeat(20));
        let patches = row_patches(0, &row);
        for patch in &patches {
            assert!(patch.text.len() <= MAX_SEGMENT, "a segment is over the limit");
        }
        assert_eq!(
            patches.iter().map(|p| p.text.clone()).collect::<String>(),
            row
        );
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

    // ── the shell-control messages, ported from the frame loop ──────────────────────────────

    fn attach_msg(version: u8, selector: &str, cols: u8, rows: u8) -> Message {
        Message::Attach {
            version,
            selector: selector.to_owned(),
            cols,
            rows,
        }
    }

    /// **The marks ride with the list, in one reply.** The launcher is the one screen that shows
    /// every shell, and two replies were two records on the air (the `Message::List` arm of
    /// `mesh_bridge.rs`): the answer is the session pages, then the marks pages, and a caller that
    /// split them would spend
    /// the record this test exists to save.
    #[tokio::test]
    async fn a_list_carries_the_marks_in_the_same_reply() {
        let mut host = FakeHost::with(vec![]).running(&["aaaa-1", "bbbb-2"]);
        host.marks = vec![("aaaa-1".to_owned(), 0), ("bbbb-2".to_owned(), 1)];
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc.list(&mut host).await else {
            panic!("a list is an answer");
        };
        let sessions: Vec<&Message> = reply
            .iter()
            .filter(|m| matches!(m, Message::Sessions { .. }))
            .collect();
        let marks: Vec<&Message> = reply
            .iter()
            .filter(|m| matches!(m, Message::Marks { .. }))
            .collect();
        assert_eq!(sessions.len(), 1, "two shells are one page");
        assert_eq!(marks.len(), 1, "the marks are in the same reply");
        let Message::Marks { entries, last, .. } = marks[0] else {
            unreachable!()
        };
        assert!(last, "a single marks page is the last one");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].kind, 1);
    }

    /// **Every field, unconditionally.** The four flags existed because one path served version 5,
    /// 6, 7 and 10 phones; a version-15 phone reads all four, and a flag left `false` here would
    /// silently drop the folder and the holder from every row.
    #[tokio::test]
    async fn a_list_numbers_every_field_for_a_v15_phone() {
        let mut host = FakeHost::with(vec![]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc.list(&mut host).await else {
            panic!("a list is an answer");
        };
        let Message::Sessions {
            cwd,
            origin,
            owner,
            shell,
            ..
        } = &reply[0]
        else {
            panic!("the first page is the sessions");
        };
        assert!(
            *cwd && *origin && *owner && *shell,
            "a v15 phone was sent the old form: cwd={cwd} origin={origin} owner={owner} shell={shell}"
        );
    }

    /// **A host that could not be asked is not a host with no shells.** Drawing the failure as an
    /// empty page emptied the launcher while every shell was alive
    /// (`mesh_bridge.rs`, `Bridge::handle`, the `Message::List` arm), so the service says nothing and
    /// the phone asks again.
    #[tokio::test]
    async fn a_list_the_host_could_not_answer_says_nothing() {
        let mut host = FakeHost::with(vec![]);
        host.runs = None;
        let mut rpc = MeshRpc::new();
        assert_eq!(rpc.list(&mut host).await, Outcome::Pending);
    }

    /// **A shell another device holds is not this phone's to end.** Without the holder rule the
    /// same button that the network route refuses destroyed a shell a desk window was working in
    /// (`Bridge::handle`, the `Message::Close` arm).
    #[tokio::test]
    async fn a_close_of_a_shell_a_desk_window_holds_is_refused() {
        let mut host = FakeHost::with(vec![]).running(&["aaaa-1"]);
        host.holder = Some("desktop".to_owned());
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .close(0, "aaaa-1", &mut host)
            .await
        else {
            panic!("a refusal is an answer");
        };
        assert_eq!(
            reply,
            vec![Message::Closed {
                reason: closed::HELD
            }]
        );
        assert!(host.closed.is_empty(), "the held shell was ended anyway");
    }

    /// **A release that names another shell leaves the new claim alone.** A channel re-attached
    /// elsewhere must not have its own claim released by a close aimed at the old one
    /// (the same arm of `Bridge::handle`).
    #[tokio::test]
    async fn a_close_ends_the_shell_and_answers_the_new_list() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 20, 3)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (20, 3), &mut host)
            .await;
        let Outcome::Answer(reply) = rpc.close(0, "aaaa-1", &mut host).await else {
            panic!("a close is answered with the list");
        };
        assert_eq!(host.closed, vec!["aaaa-1"]);
        assert_eq!(host.released, vec!["aaaa-1"]);
        assert!(
            matches!(reply[0], Message::Sessions { .. }),
            "the phone is shown the list the close changed"
        );
        assert_eq!(rpc.attached_to(0), None, "the ended shell is still attached");
    }

    /// **A shell is born at the phone's grid.** A pty started at the host's 80x24 and reflowed on
    /// the first attach is a screen of rewrapped output the operator never asked for
    /// (the `Message::Create` arm of `Bridge::handle`).
    #[tokio::test]
    async fn a_create_is_born_at_the_grid_the_phone_stated() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        let Outcome::Answer(reply) = rpc.create(0, "/home/me", &mut host).await else {
            panic!("a create is answered");
        };
        assert_eq!(host.created, vec![("/home/me".to_owned(), 46, 17)]);
        assert_eq!(reply, vec![Message::Created { id: "made-1".to_owned() }]);
    }

    /// **A channel that has stated nothing gets the host's default**, which is what the launcher
    /// does: it creates before it attaches, so there is no grid to honour yet.
    #[tokio::test]
    async fn a_create_with_no_stated_grid_uses_the_hosts_default() {
        let mut host = FakeHost::with(vec![]);
        let mut rpc = MeshRpc::new();
        rpc.create(0, "", &mut host).await;
        assert_eq!(host.created, vec![(String::new(), 80, 24)]);
    }

    /// **A takeover keeps the grid the phone already stated.** It is not a place to renegotiate the
    /// protocol, and a grid read from the message that does not carry one would fit the pane to 0x0
    /// (the `Message::Takeover` arm of `Bridge::handle`).
    #[tokio::test]
    async fn a_takeover_keeps_the_grid_the_phone_stated_and_consents() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        let Outcome::Answer(reply) = rpc
            .attach(0, VERSION, "aaaa-1", true, (46, 17), &mut host)
            .await
        else {
            panic!("a takeover is answered with a screen");
        };
        assert_eq!(
            host.claims,
            vec![("aaaa-1".to_owned(), false), ("aaaa-1".to_owned(), true)],
            "the second claim did not carry the consent"
        );
        assert!(
            host.resized.iter().all(|g| *g == (46, 17)),
            "the takeover refitted the pane to a grid nobody stated: {:?}",
            host.resized
        );
        assert!(
            matches!(reply[0], Message::Snapshot { .. }),
            "a takeover is answered with the whole screen"
        );
    }

    /// **The host decides whether the holder is in use**, and its refusal reaches the phone as one
    /// `Closed { HELD }` (`Bridge::attach_with`).
    #[tokio::test]
    async fn an_attach_refuses_a_shell_another_viewer_holds() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        host.refuse_claim = true;
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await
        else {
            panic!("a refusal is an answer");
        };
        assert_eq!(
            reply,
            vec![Message::Closed {
                reason: closed::HELD
            }]
        );
        assert_eq!(rpc.attached_to(0), None, "a refused attach left an attachment");
    }

    /// **A build that speaks version 14 gets one message saying so**, not half a screen it cannot
    /// read (`docs/RADIO-PLAN.md` §10a). Exactly one, and no rows — and through the dispatch, so
    /// the arm the transport will call is the one under test.
    #[tokio::test]
    async fn an_attach_from_an_older_build_gets_one_closed() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .on_message(
                0,
                PEER_DEVICE,
                &attach_msg(14, "aaaa-1", 46, 17),
                &mut host,
                Instant::now(),
            )
            .await
            .expect("a version refusal is an answer")
        else {
            panic!("a version refusal is an answer");
        };
        assert_eq!(
            reply,
            vec![Message::Closed {
                reason: closed::VERSION
            }]
        );
        assert!(host.claims.is_empty(), "an older build was claimed a shell");
    }

    /// A selector that names nothing running is `NOT_FOUND`, which the phone draws as a row that
    /// has gone rather than as a shell it cannot open.
    #[tokio::test]
    async fn an_attach_of_an_unknown_selector_is_not_found() {
        let mut host = FakeHost::with(vec![]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .attach(0, VERSION, "zzzz", false, (46, 17), &mut host)
            .await
        else {
            panic!("a missing shell is an answer");
        };
        assert_eq!(
            reply,
            vec![Message::Closed {
                reason: closed::NOT_FOUND
            }]
        );
    }

    /// **A command for a shell this link is not attached to is refused, not guessed.** Nothing is
    /// written, and the refusal names the op so the phone's draft stays spent exactly once
    /// (`Bridge::v15_send`).
    #[tokio::test]
    async fn a_send_with_no_attachment_is_refused_and_writes_nothing() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 20, 3)]);
        let mut rpc = MeshRpc::new();
        let Outcome::Answer(reply) = rpc
            .on_message(
                0,
                PEER_DEVICE,
                &command(9, vec![SendItem::Text(b"ls".to_vec())]),
                &mut host,
                Instant::now(),
            )
            .await
            .expect("a refusal is an answer")
        else {
            panic!("a refusal is an answer");
        };
        let Message::Snapshot {
            applied_op_id,
            applied_status,
            patches,
            ..
        } = &reply[0]
        else {
            panic!("the refusal is a Snapshot");
        };
        assert_eq!(*applied_op_id, 9);
        assert_eq!(*applied_status, applied::NO_SHELL);
        assert!(patches.is_empty(), "a refusal carried a screen");
        assert!(host.written.is_empty(), "a refusal wrote to a pty");
    }

    /// **The write path obeys the holder rule too.** A desk window that took the shell displaces
    /// the phone, and typing into the same pty anyway is what this refuses; the claim and the
    /// pane's size are given back exactly as `Detach` gives them back
    /// (`Bridge::refuse_held_write`).
    #[tokio::test]
    async fn a_send_after_the_desk_took_the_shell_lets_go_of_it() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 200, 50)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        host.holder = Some("desktop".to_owned());
        let Outcome::Answer(reply) = rpc
            .on_message(
                0,
                PEER_DEVICE,
                &command(10, vec![SendItem::Text(b"ls".to_vec())]),
                &mut host,
                Instant::now(),
            )
            .await
            .expect("answered")
        else {
            panic!("a refusal is an answer");
        };
        assert_eq!(
            reply,
            vec![Message::Closed {
                reason: closed::HELD
            }]
        );
        assert!(host.written.is_empty(), "a displaced phone wrote to the pty");
        assert_eq!(host.released, vec!["aaaa-1"], "the claim was left behind");
        assert_eq!(
            rpc.attached_to(0),
            None,
            "a displaced attachment stayed in the table"
        );
        assert_eq!(
            host.resized.last(),
            Some(&(200, 50)),
            "the desk's pane was left at the phone's size"
        );
    }

    /// **`Detach` gives the pane back its own size.** Only after a fit of ours: a shell the phone
    /// merely looked at keeps the desk's geometry, and a call that changes nothing is still a
    /// keeper round trip (`Bridge::restore_grid`).
    #[tokio::test]
    async fn a_detach_gives_the_claim_back_and_restores_the_pane_size() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 200, 50)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        rpc.detach(0, &mut host).await;
        assert_eq!(host.released, vec!["aaaa-1"]);
        assert_eq!(host.resized, vec![(46, 17), (200, 50)]);
        assert_eq!(rpc.attached_to(0), None);
    }

    /// **A link that dies ends every attach.** A claim left behind locks a desk window out of a
    /// shell nobody is watching (`Bridge::drop_all`).
    #[tokio::test]
    async fn a_dropped_link_gives_every_claim_back() {
        let mut host =
            FakeHost::with(vec![screen(&["$ "], 200, 50)]).running(&["aaaa-1", "bbbb-2"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        rpc.attach(1, VERSION, "bbbb-2", false, (46, 17), &mut host)
            .await;
        rpc.drop_all(&mut host).await;
        // The order is not a fact: the attachments are a map, and releasing one claim before
        // another says nothing. That both were released is the whole claim of this test.
        let mut released = host.released.clone();
        released.sort();
        assert_eq!(released, vec!["aaaa-1", "bbbb-2"]);
        assert_eq!(rpc.attached_to(0), None);
        assert_eq!(rpc.attached_to(1), None);
    }

    /// **A history page states the absolute line of its first row**, and advances by its line
    /// count rather than its byte count: the phone places it exactly instead of assuming it joins
    /// what it holds (`Bridge::handle`'s `Message::WantHistory` arm).
    #[tokio::test]
    async fn history_pages_advance_by_line_count_and_state_their_first_line() {
        let line = "x".repeat(60);
        let lines: Vec<String> = (0..20).map(|_| line.clone()).collect();
        // 20 rows of 61 bytes is two pages against the 560-byte budget, so the second page's
        // `from` is a fact the arithmetic produced and not the start of the answer.
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        host.history = Some((100, true, lines));
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        let Outcome::Answer(pages) = rpc.want_history(0, 100, 30, &mut host).await else {
            panic!("history is an answer");
        };
        assert!(pages.len() > 1, "twenty rows fitted one page against the budget");
        let (mut from, mut count) = (None, 0u32);
        for (index, page) in pages.iter().enumerate() {
            let Message::HistoryPage {
                from: at,
                last,
                older,
                lines,
            } = page
            else {
                panic!("a history answer is pages");
            };
            assert!(*older, "older rides every page");
            assert_eq!(*last, index + 1 == pages.len(), "only the final page is last");
            if index == 0 {
                assert_eq!(*at, 100, "the first page does not start where it was asked");
            }
            from.get_or_insert(*at);
            count += u32::try_from(lines.len()).expect("a page of lines");
        }
        assert_eq!(count, 20, "the pages lost or duplicated a line");
        let second = &pages[1];
        let Message::HistoryPage { from: at, .. } = second else {
            unreachable!()
        };
        let Message::HistoryPage { lines: first, .. } = &pages[0] else {
            unreachable!()
        };
        assert_eq!(
            *at,
            100 + u32::try_from(first.len()).expect("lines"),
            "the second page's line number is not the first page's start plus its line count"
        );
    }

    /// A host that cannot answer is not "nothing above this", so silence is the whole effect
    /// (the same arm).
    #[tokio::test]
    async fn history_is_silent_when_the_host_cannot_answer() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        assert_eq!(rpc.want_history(0, 0, 5, &mut host).await, Outcome::Pending);
    }

    /// `MakeLive` opens a desk window and says nothing: the list the phone already has shows the
    /// window, and a second record to say so is airtime bought with nothing (`Bridge::handle`'s
    /// `Message::MakeLive` arm).
    #[tokio::test]
    async fn make_live_opens_the_window_and_says_nothing() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        rpc.attach(0, VERSION, "aaaa-1", false, (46, 17), &mut host)
            .await;
        let outcome = rpc
            .on_message(
                0,
                PEER_DEVICE,
                &Message::MakeLive {
                    selector: "aaaa-1".to_owned(),
                },
                &mut host,
                Instant::now(),
            )
            .await
            .expect("answered");
        assert_eq!(outcome, Outcome::Pending);
        assert_eq!(host.live, vec!["aaaa-1"]);
    }

    /// **One entry point, every message.** A message the version-15 model does not use (the frame
    /// loop's `Input`) is ignored rather than half-served, and the caller has one reply path.
    #[tokio::test]
    async fn a_frame_loop_message_has_no_effect_on_the_v15_path() {
        let mut host = FakeHost::with(vec![screen(&["$ "], 46, 17)]).running(&["aaaa-1"]);
        let mut rpc = MeshRpc::new();
        let outcome = rpc
            .on_message(
                0,
                PEER_DEVICE,
                &Message::Input(b"ls\r".to_vec()),
                &mut host,
                Instant::now(),
            )
            .await
            .expect("ignored, not refused");
        assert_eq!(outcome, Outcome::Pending);
        assert!(host.written.is_empty(), "a retired message reached the pty");
    }
}
