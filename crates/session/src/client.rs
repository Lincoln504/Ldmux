//! The phone's client: the whole radio-side app protocol in one pure value.
//!
//! Everything the Android app needs in order to be a terminal over the radio lives here, so the app
//! no longer reimplements the protocol in Kotlin (`docs/RADIO-PLAN.md` §1 `A2`, §8.1). The app owns
//! a [`Client`] behind the C ABI, hands it BLE bytes and user actions, and receives
//! [`ClientEvent`]s and per-shell [`PageMirror`]s. There is no second copy of the frame format, the
//! handshake, the transaction layer or the screen diffing on the phone.
//!
//! # What it composes, and what it does not
//!
//! [`RadioEndpoint`] already owns the link: the bootstrap, the route, the carriage for each attempt
//! and the reliable message layer. This type adds the two things the app is about:
//!
//! * **pages** — up to [`MAX_OPEN_PAGES`] shells open at once, each with its own screen, cursor and
//!   answer, keyed by the shell id the message itself carries. There is deliberately **no page byte
//!   in the transport**: `Link`'s page index was a fact about the old frame loop, and every v15
//!   message names its own shell (`docs/RADIO-PLAN.md` §12.7b), so multi-shell survives by message
//!   field. That is gate `B11`, and [`Client::on_message`] is where it is decided.
//! * **the screen** — a `Snapshot` is one atomic screen: geometry, cursor, rows and the answer to the
//!   command that produced it. [`PageMirror`] applies it whole or not at all, which is what makes
//!   `RC-11` (a screen drawn from three messages that could arrive apart) impossible.
//!
//! # Purity
//!
//! No clock, no socket, no thread: every method takes `now_ms`, and the caller's loop is
//! [`Client::poll`] plus [`Client::next_deadline_ms`]. That is what lets the whole client be tested
//! against a simulated mesh in `cargo test`, and what keeps `ldmux-protocol`'s host half — `tokio`,
//! `nix`, Unix sockets — out of the phone's library.

use std::collections::BTreeMap;
use std::fmt;

use ldmux_mesh::error::MeshError;
use ldmux_protocol::mesh_term::{
    Decoder, EncodeError, MAX_CHUNKS, MAX_SEND_ITEMS, MarkEntry, Message, Patch, SendItem,
    SessionEntry, SnapshotAssembly, VERSION as TERMINAL_VERSION, applied, snapshot_flag,
};

use crate::endpoint::{
    ArrivalInfo, CarriageSink, EndpointConfig, Outbound, RadioEndpoint, Role, SentKind, Stats,
};
use crate::handshake::{HandshakeError, Keypair};
use crate::transaction::BeginError;

/// The plan's page cap: five shells open at once, the same number the tailnet launcher allows.
///
/// The app has its own copy in `PageOrder.kt` because it was written first. The engine's is the one
/// that refuses the sixth page, so an app that disagrees can only be more permissive than the link it
/// is on, never less.
pub const MAX_OPEN_PAGES: usize = 5;

/// How long a part-way screen is kept before it is dropped.
///
/// The assembler joins chunks by `snapshot_id` and has no clock of its own. Without a deadline, one
/// lost chunk would leave the assembly open for ever and every later screen of that page would be
/// refused as "another screen's chunk" — a page that silently stops updating. Thirty seconds is
/// longer than any screen takes to arrive at the public preset and far shorter than a session.
const ASSEMBLY_DEADLINE_MS: u64 = 30_000;

/// What the host said it did with one command, carried by the snapshot that answers it.
///
/// **It is a value, not an inference.** The phone used to decide "the command ran" from the screen
/// changing, so a command that printed nothing was indistinguishable from one that never reached the
/// pty. `status` is [`applied`]'s vocabulary, verbatim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Applied {
    /// Which command this answers.
    pub op_id: u32,
    /// What happened to it, from [`applied`].
    pub status: u8,
}

impl Applied {
    /// The bytes reached the pty.
    #[must_use]
    pub fn was_written(self) -> bool {
        self.status == applied::WRITTEN
    }

    /// This command had already been applied, so nothing was written a second time: the reason a lost
    /// answer is safe to retry, from the operator's side.
    #[must_use]
    pub fn was_duplicate(self) -> bool {
        self.status == applied::DUPLICATE
    }

    /// Another client holds the shell, so nothing was written.
    #[must_use]
    pub fn is_held(self) -> bool {
        self.status == applied::HELD
    }

    /// A word for a log or a status line, so no caller has to spell the codes.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self.status {
            applied::WRITTEN => "written",
            applied::DUPLICATE => "already applied",
            applied::NO_SHELL => "no shell",
            applied::HELD => "held elsewhere",
            applied::TOO_LONG => "too long",
            applied::FAILED => "the host refused it",
            _ => "no verdict",
        }
    }
}

/// One snapshot, as the fields a mirror needs.
struct Screen<'a> {
    flags: u8,
    snapshot_id: u32,
    base_id: u32,
    applied: Option<Applied>,
    cols: u8,
    rows: u8,
    cursor: (u8, u8),
    first_line: u32,
    patches: &'a [Patch],
}

/// What a screen did to a mirror.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Paint {
    /// The mirror now holds this screen.
    Painted,
    /// The same screen again: the answer to a retransmitted command.
    Duplicate,
    /// A diff cut against a screen this mirror never held, so applying it would paint one screen's
    /// patches onto another's rows.
    WrongBase,
}

/// One shell's screen, as the host last painted it.
///
/// A mirror is replaced whole by each `Snapshot`, never edited by a message that arrives apart from
/// its geometry, so there is no state here in which the rows and the cursor disagree about the screen
/// they describe.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PageMirror {
    /// The shell this screen belongs to, as the host named it.
    pub shell: String,
    /// The identity of this screen. A `base_id` diff names the identity it was cut against.
    pub snapshot_id: u32,
    /// The grid the host painted, in cells.
    pub cols: u8,
    /// Rows in the grid.
    pub rows: u8,
    /// The screen's text, one entry per row, `rows` entries.
    pub lines: Vec<String>,
    /// The cursor cell.
    pub cursor: (u8, u8),
    /// The absolute line number of row 0, so a screen that scrolls can be told from one rewritten in
    /// place.
    pub first_line: u32,
    /// A full-screen program owns the pane.
    pub alternate: bool,
    /// The shell's process exited.
    pub dead: bool,
    /// The pane had stopped changing when this was captured. Clear means the answer is a mid-state,
    /// which is the difference between "up to date" and "still running".
    pub settled: bool,
    /// What the host did with the command this screen answers, when this screen answers one.
    pub applied: Option<Applied>,
}

impl PageMirror {
    fn blank(shell: &str) -> Self {
        Self {
            shell: shell.to_owned(),
            snapshot_id: 0,
            cols: 0,
            rows: 0,
            lines: Vec::new(),
            cursor: (0, 0),
            first_line: 0,
            alternate: false,
            dead: false,
            settled: false,
            applied: None,
        }
    }

    /// Whether there is a screen to draw at all.
    #[must_use]
    pub fn painted(&self) -> bool {
        self.snapshot_id != 0
    }

    /// Apply one whole screen.
    ///
    /// **A diff naming a screen this mirror never held is refused**, not applied: a `Snapshot` that
    /// was lost leaves the mirror holding an older screen, and a diff against the newer one would put
    /// its patches on the wrong rows — text from one screen appearing inside another, which reads as
    /// corruption rather than as a missing frame.
    fn apply(&mut self, screen: &Screen<'_>) -> Paint {
        if self.snapshot_id != 0 && screen.snapshot_id == self.snapshot_id {
            return Paint::Duplicate;
        }
        let full = screen.flags & snapshot_flag::FULL != 0;
        if !full && screen.base_id != self.snapshot_id {
            return Paint::WrongBase;
        }
        if full || self.lines.len() != usize::from(screen.rows) {
            self.lines = vec![String::new(); usize::from(screen.rows)];
        }
        for patch in screen.patches {
            let Some(line) = self.lines.get_mut(usize::from(patch.y)) else {
                continue;
            };
            if patch.append {
                line.push_str(&patch.text);
            } else {
                line.clone_from(&patch.text);
            }
        }
        self.snapshot_id = screen.snapshot_id;
        self.cols = screen.cols;
        self.rows = screen.rows;
        self.cursor = screen.cursor;
        self.first_line = screen.first_line;
        self.alternate = screen.flags & snapshot_flag::ALTERNATE != 0;
        self.dead = screen.flags & snapshot_flag::DEAD != 0;
        self.settled = screen.flags & snapshot_flag::SETTLED != 0;
        self.applied = screen.applied;
        Paint::Painted
    }
}

/// One page: the open shell, its screen, and whether the host has answered it at all.
struct Page {
    mirror: PageMirror,
    /// Whether the host has answered this page at all, which is the only acceptance v15 has: a
    /// `Snapshot` is the attach's answer, so there is no separate acknowledgement to wait for.
    attached: bool,
    /// The command submitted last on this page, so its answer can be recognised.
    pending: Option<u32>,
}

/// Something the app has to draw or say, as a value.
#[derive(Clone, PartialEq, Debug)]
pub enum ClientEvent {
    /// The host answered a page for the first time: which shell, and the grid it painted.
    Attached {
        /// The shell the host named.
        shell: String,
        /// The grid it painted, in cells.
        cols: u8,
        /// Rows in the grid.
        rows: u8,
    },
    /// A screen arrived for a page.
    Settled {
        /// Which shell.
        shell: String,
        /// The identity of the screen now held.
        snapshot_id: u32,
        /// What happened to the command this screen answers, or `None` when it answers a refresh.
        applied: Option<Applied>,
        /// Whether the pane had stopped changing. Clear means the answer is a mid-state, and the
        /// operator should be offered a refresh rather than told the command finished.
        settled: bool,
    },
    /// Another client holds the shell, so the command was not applied.
    ///
    /// The same fact as a [`ClientEvent::Settled`] carrying an [`Applied`] whose `is_held` is true,
    /// repeated as its own value because a caller showing "in use on desktop · take it back" should
    /// not have to know the applied vocabulary to say one sentence.
    Held {
        /// Which shell.
        shell: String,
    },
    /// The host ended this client's view of the shell, and why (`mesh_term::closed`).
    Closed {
        /// The reason, from `mesh_term::closed`.
        reason: u8,
    },
    /// One page of the shell list. `last` closes the answer.
    Sessions {
        /// The final page of this answer.
        last: bool,
        /// The shells on this page.
        entries: Vec<SessionEntry>,
    },
    /// The host made a shell this client asked for, and this is its id.
    ///
    /// Without it the phone would attach by guessing from a list another client may have changed in
    /// between, which is how a shell created from a phone could end up attaching to somebody else's.
    Created {
        /// The id `Create` produced, usable as an `open` selector.
        id: String,
    },
    /// One page of the marks the host holds, so the launcher can draw a finished shell.
    Marks {
        /// The first page of a run.
        first: bool,
        /// The final page of the run.
        last: bool,
        /// The marks on this page.
        entries: Vec<MarkEntry>,
    },
    /// One page of scrollback, for the pager.
    History {
        /// The absolute line number of `lines[0]`.
        from: u32,
        /// The final page of this answer.
        last: bool,
        /// The pane holds history above this page.
        older: bool,
        /// The lines.
        lines: Vec<String>,
    },
}

/// Why an action could not be taken.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClientError {
    /// The page cap is reached. The sixth page is refused rather than silently dropping one: a page
    /// the operator opened and cannot see is a shell they believe they closed.
    TooManyPages {
        /// The cap.
        max: usize,
    },
    /// That shell is already open.
    AlreadyOpen(String),
    /// No page is open for that shell, so there is nothing to send to or refresh.
    NoPage(String),
    /// The message could not be built.
    Encode(EncodeError),
    /// The transaction layer refused the message.
    Begin(BeginError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPages { max } => write!(f, "{max} shells are open · close one first"),
            Self::AlreadyOpen(shell) => write!(f, "{shell} is already open"),
            Self::NoPage(shell) => write!(f, "{shell} is not open"),
            Self::Encode(e) => write!(f, "the message is not legal on this wire: {e:?}"),
            Self::Begin(e) => write!(f, "the link refused the message: {e:?}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// What the client needs that is not key material.
///
/// The phone's own keypair is passed to [`Client::new`], because a [`Keypair`] wipes its private half
/// on drop and so cannot live in a value the caller clones.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// The host radio's static public key: what an initiation is addressed to.
    pub peer_static: [u8; 32],
    /// The pre-shared key the Noise suite mixes in.
    pub psk: [u8; 32],
    /// The path hash width this mesh uses, in bytes (1..=3).
    pub path_width: u8,
    /// One full record's time on the air at this radio's tuple, in milliseconds.
    pub airtime_ms: f64,
    /// The spreading factor, which decides the demodulation floor a route is ranked against.
    pub sf: u8,
    /// The region scope this radio's floods carry, or `None` when they are unscoped.
    pub scope: Option<u16>,
    /// How far this mesh reaches, in hops, for sizing a flood before a path is known.
    pub flood_reach_hops: u8,
    /// How many pages may be open at once.
    pub max_open_pages: usize,
}

impl ClientConfig {
    /// A configuration with the public preset's tuple and the plan's page cap.
    ///
    /// `airtime_ms` is the measured full-record figure at 62.5 kHz/SF7, because it is what the pacing
    /// and the burst tail are derived from; a caller on another tuple sets its own.
    #[must_use]
    pub fn new(peer_static: [u8; 32], psk: [u8; 32]) -> Self {
        Self {
            peer_static,
            psk,
            path_width: 1,
            airtime_ms: 602.6,
            sf: 7,
            scope: None,
            flood_reach_hops: 32,
            max_open_pages: MAX_OPEN_PAGES,
        }
    }

    /// Sets the path hash width the mesh uses.
    #[must_use]
    pub fn with_path_width(mut self, path_width: u8) -> Self {
        self.path_width = path_width;
        self
    }

    /// Sets one full record's airtime.
    #[must_use]
    pub fn with_airtime_ms(mut self, airtime_ms: f64) -> Self {
        self.airtime_ms = airtime_ms;
        self
    }

    /// Sets the spreading factor a route is ranked against.
    #[must_use]
    pub fn with_sf(mut self, sf: u8) -> Self {
        self.sf = sf;
        self
    }

    /// Sets the region scope this radio's floods carry.
    #[must_use]
    pub fn with_scope(mut self, scope: Option<u16>) -> Self {
        self.scope = scope;
        self
    }

    /// Sets how far this mesh reaches, in hops.
    #[must_use]
    pub fn with_flood_reach_hops(mut self, hops: u8) -> Self {
        self.flood_reach_hops = hops;
        self
    }

    /// Sets the page cap. The default is [`MAX_OPEN_PAGES`].
    #[must_use]
    pub fn with_max_open_pages(mut self, max: usize) -> Self {
        self.max_open_pages = max;
        self
    }

    fn endpoint(&self) -> EndpointConfig {
        EndpointConfig::new(Role::Phone, self.peer_static, self.psk)
            .with_path_width(self.path_width)
            .with_airtime_ms(self.airtime_ms)
            .with_sf(self.sf)
            .with_scope(self.scope)
            .with_flood_reach_hops(self.flood_reach_hops)
    }
}

/// Counters, so a field failure is read off a log instead of guessed at.
#[derive(Clone, Default, Debug)]
pub struct ClientStats {
    /// `Send` messages handed to the transport.
    pub commands_sent: u64,
    /// `Fetch` messages handed to the transport.
    pub fetches_sent: u64,
    /// `Attach` messages handed to the transport.
    pub attaches_sent: u64,
    /// `List` messages handed to the transport.
    pub lists_sent: u64,
    /// Parts handed over, retransmissions included.
    pub parts_sent: u64,
    /// Parts that had been on the air before.
    pub retransmissions: u64,
    /// Standalone acknowledgements handed over.
    pub acks_sent: u64,
    /// Initiations handed over, one per bootstrap rung.
    pub initiations_sent: u64,
    /// Messages the host acknowledged whole.
    pub completed: u64,
    /// Messages given up on. The session is untouched, which is why the operator may retry.
    pub timed_out: u64,
    /// Messages a newer one of the same kind replaced.
    pub superseded: u64,
    /// Bootstrap attempts that ran out of rungs.
    pub bootstrap_failures: u64,
    /// Screens applied to a mirror.
    pub snapshots_applied: u64,
    /// Screens that were the answer to a retransmission and changed nothing.
    pub snapshots_duplicate: u64,
    /// Diffs cut against a screen the mirror never held, and so dropped.
    pub snapshots_wrong_base: u64,
    /// Screens for a shell this client has no page for.
    pub snapshots_unknown_page: u64,
    /// Answers by [`applied`] status, indexed by the status byte (`0..=6`).
    by_status: [u64; 7],
    /// Answers that said another client holds the shell.
    pub held: u64,
    /// Downlink messages this client does not act on.
    pub unknown_downlink: u64,
    /// A screen dropped because its chunks never all arrived.
    pub snapshots_refused: u64,
}

impl ClientStats {
    /// How many answers carried this [`applied`] status.
    #[must_use]
    pub fn applied_count(&self, status: u8) -> u64 {
        self.by_status.get(usize::from(status)).copied().unwrap_or(0)
    }

    fn note_applied(&mut self, applied: Applied) {
        if let Some(slot) = self.by_status.get_mut(usize::from(applied.status)) {
            *slot += 1;
        }
        if applied.is_held() {
            self.held += 1;
        }
    }
}

/// The phone's client: pages, screens and the link underneath them.
pub struct Client {
    endpoint: RadioEndpoint,
    config: ClientConfig,
    /// One page per open shell, keyed by the id the messages carry.
    pages: BTreeMap<String, Page>,
    /// The pages in the order they were opened, which is the order the pager shows them.
    order: Vec<String>,
    /// The screen being joined from its chunks, if one is part-way through.
    assembling: SnapshotAssembly,
    /// When the part-way screen started arriving, so a lost chunk cannot hold the page hostage.
    assembling_since: Option<u64>,
    /// The next command id. Monotonic per client, which is what makes a retransmission idempotent.
    next_op: u32,
    /// Events produced since the last [`Client::take_events`].
    events: Vec<ClientEvent>,
    stats: ClientStats,
}

impl Client {
    /// A client for one host, holding one keypair.
    #[must_use]
    pub fn new(config: ClientConfig, local: Keypair) -> Self {
        let endpoint = RadioEndpoint::phone(config.endpoint(), local);
        Self {
            endpoint,
            config,
            pages: BTreeMap::new(),
            order: Vec::new(),
            assembling: SnapshotAssembly::default(),
            assembling_since: None,
            next_op: 1,
            events: Vec::new(),
            stats: ClientStats::default(),
        }
    }

    /// Start the bootstrap: one initiation, held across every rung until an answer arrives.
    ///
    /// # Errors
    ///
    /// [`HandshakeError`] when the initiation cannot be built, which is a bad key or a bad pairing
    /// rather than anything the air did.
    pub fn connect(&mut self, now_ms: u64, wall_ms: u64) -> Result<(), HandshakeError> {
        self.endpoint.connect(now_ms, wall_ms)
    }

    /// Whether a session exists, so records can be sealed and opened.
    #[must_use]
    pub fn established(&self) -> bool {
        self.endpoint.established()
    }

    /// Open a page for `shell`, without asking for its screen yet.
    ///
    /// The page is opened by an `Attach`, which names the shell and nothing else: the grid belongs to
    /// the `Fetch` or `Send` that follows, because the phone's grid is the one the operator just
    /// measured and an `Attach` cannot know it. The host's answer to the `Attach` is the first
    /// `Snapshot`; v15 has no separate acknowledgement, so waiting for one would be waiting for a
    /// message that does not exist.
    ///
    /// # Errors
    ///
    /// [`ClientError::TooManyPages`], [`ClientError::AlreadyOpen`], or a transport refusal.
    pub fn open(&mut self, shell: &str, cols: u8, rows: u8, now_ms: u64) -> Result<(), ClientError> {
        if self.pages.contains_key(shell) {
            return Err(ClientError::AlreadyOpen(shell.to_owned()));
        }
        if self.order.len() >= self.config.max_open_pages {
            return Err(ClientError::TooManyPages {
                max: self.config.max_open_pages,
            });
        }
        let message = Message::Attach {
            version: TERMINAL_VERSION,
            selector: shell.to_owned(),
            // **The phone's grid, not the host's default.** A pty left at the host's own size paints
            // a screen the phone has to clip, and the clipped rows are the top of the picture — the
            // prompt and the command. Attaching without a grid is a defect this route removed once.
            cols,
            rows,
        };
        self.submit(&message, now_ms)?;
        self.stats.attaches_sent += 1;
        self.pages.insert(
            shell.to_owned(),
            Page {
                mirror: PageMirror::blank(shell),
                attached: false,
                pending: None,
            },
        );
        self.order.push(shell.to_owned());
        Ok(())
    }

    /// Close a page. **Nothing goes on the air.**
    ///
    /// The plan's radio route has no named detach, and a `Detach` with no selector cannot say which of
    /// several pages it means; adding one would be a wire change, not a client change. What the phone
    /// does instead is stop reading the shell, which is the whole of what closing a page means to the
    /// operator: the shell keeps running on the host, and the host's claim on it lapses by its own
    /// engagement timeout rather than by a record spent saying goodbye.
    ///
    /// # Errors
    ///
    /// [`ClientError::NoPage`] when that shell is not open.
    pub fn close(&mut self, shell: &str) -> Result<(), ClientError> {
        if self.pages.remove(shell).is_none() {
            return Err(ClientError::NoPage(shell.to_owned()));
        }
        self.order.retain(|open| open != shell);
        if self.pages.is_empty() {
            // A screen part-way through belonged to a page that is gone; its remaining chunks would
            // otherwise be refused by the next page's `snapshot_id` and counted as a fault.
            self.assembling.clear();
            self.assembling_since = None;
        }
        Ok(())
    }

    /// Ask the host to start a shell in `cwd` (empty means the host's default directory).
    ///
    /// The answer is a [`ClientEvent::Created`] carrying the id to open. A `Create` does not open a
    /// page: the phone attaches to the id it is given, so a shell created from a phone is one the
    /// phone knows the name of rather than one it has to guess out of a list another client may have
    /// changed in between.
    ///
    /// # Errors
    ///
    /// A transport refusal, or [`ClientError::Encode`] when the directory is too long to say.
    pub fn create(&mut self, cwd: &str, now_ms: u64) -> Result<(), ClientError> {
        let message = Message::Create {
            version: TERMINAL_VERSION,
            cwd: cwd.to_owned(),
        };
        self.submit(&message, now_ms)
    }

    /// Take `shell` from whoever holds it, displacing them.
    ///
    /// A plain [`Client::open`] is refused while a desk window holds the shell, so taking it is
    /// always something the operator chose — which is what the tailnet's takeover is, and the second
    /// half of saying "in use on desktop · take it back".
    ///
    /// # Errors
    ///
    /// A transport refusal.
    pub fn takeover(&mut self, shell: &str, now_ms: u64) -> Result<(), ClientError> {
        let message = Message::Takeover {
            selector: shell.to_owned(),
        };
        self.submit(&message, now_ms)?;
        // The page exists locally as well: the host answers a takeover with a screen, and a screen
        // with no page to land on would be refused as a screen for a shell nobody opened.
        if !self.pages.contains_key(shell) {
            self.pages.insert(
                shell.to_owned(),
                Page {
                    mirror: PageMirror::blank(shell),
                    attached: false,
                    pending: None,
                },
            );
            self.order.push(shell.to_owned());
        }
        self.stats.attaches_sent += 1;
        Ok(())
    }

    /// Ask the host to open `shell` in a desk window: **send to desktop**.
    ///
    /// # Errors
    ///
    /// A transport refusal.
    pub fn make_live(&mut self, shell: &str, now_ms: u64) -> Result<(), ClientError> {
        let message = Message::MakeLive {
            selector: shell.to_owned(),
        };
        self.submit(&message, now_ms)
    }

    /// Ask the host for a page of `shell`'s own scrollback, starting at absolute line `from`.
    ///
    /// **The keeper has the history; the link never sends it.** A screen the phone never saw — a
    /// shell joined late, or one that printed while the view was frozen — is history it cannot have,
    /// and the pad's up key on such a screen used to answer "no history yet on this link", which is
    /// true and useless. The answer is one or more [`ClientEvent::History`] pages.
    ///
    /// # Errors
    ///
    /// A transport refusal, or [`ClientError::Encode`] when `rows` is past what one page may carry.
    pub fn want_history(
        &mut self,
        shell: &str,
        from: u32,
        rows: u8,
        now_ms: u64,
    ) -> Result<(), ClientError> {
        let _ = shell;
        let message = Message::WantHistory { from, rows };
        self.submit(&message, now_ms)
    }

    /// Ask the host to end `shell`, or to put it down when a desk window holds it.
    ///
    /// The local page goes too, because a page for a shell that no longer exists would draw its last
    /// screen for ever and refuse every command.
    ///
    /// # Errors
    ///
    /// A transport refusal.
    pub fn end(&mut self, shell: &str, now_ms: u64) -> Result<(), ClientError> {
        let message = Message::Close {
            selector: shell.to_owned(),
        };
        self.submit(&message, now_ms)?;
        let _ = self.close(shell);
        Ok(())
    }

    /// Send one command to `shell`: every typed piece, paste and key, as one message.
    ///
    /// A command that does not fit one message is split into chunks that share its `op_id`, and the
    /// host applies the joined items **once**, when the last chunk arrives — so a paste is inserted
    /// whole or not at all, never half of one.
    ///
    /// # Returns
    ///
    /// The `op_id`, which the answer carries, so a caller can tell the answer to *this* command from
    /// the answer to an earlier one.
    ///
    /// # Errors
    ///
    /// [`ClientError::NoPage`] when the shell is not open; a transport refusal otherwise.
    pub fn send(
        &mut self,
        shell: &str,
        items: &[SendItem],
        cols: u8,
        rows: u8,
        now_ms: u64,
    ) -> Result<u32, ClientError> {
        let have_snapshot_id = self
            .pages
            .get(shell)
            .ok_or_else(|| ClientError::NoPage(shell.to_owned()))?
            .mirror
            .snapshot_id;
        let op_id = self.next_op.max(1);
        self.next_op = self.next_op.wrapping_add(1).max(1);
        let chunks = chunk_items(shell, items, op_id, have_snapshot_id, cols, rows)?;
        // `chunk_count` is one byte, and the decoder refuses zero or a count past `MAX_CHUNKS`: a
        // command that would need more than 255 messages is beyond what one command may be.
        if chunks.len() > MAX_CHUNKS {
            return Err(ClientError::Encode(EncodeError::TooLong));
        }
        let count = u8::try_from(chunks.len()).map_err(|_| ClientError::Encode(EncodeError::TooLong))?;
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let message = Message::Send {
                op_id,
                chunk_idx: u8::try_from(idx).unwrap_or(0),
                chunk_count: count,
                have_snapshot_id,
                cols,
                rows,
                flags: 0,
                shell: shell.to_owned(),
                items: chunk,
            };
            self.submit(&message, now_ms)?;
        }
        self.stats.commands_sent += 1;
        if let Some(page) = self.pages.get_mut(shell) {
            page.pending = Some(op_id);
        }
        Ok(op_id)
    }

    /// Ask for `shell`'s screen again, at the grid the phone draws with. **Spends no draft.**
    ///
    /// # Errors
    ///
    /// [`ClientError::NoPage`] when the shell is not open.
    pub fn fetch(
        &mut self,
        shell: &str,
        cols: u8,
        rows: u8,
        now_ms: u64,
    ) -> Result<(), ClientError> {
        let have_snapshot_id = self
            .pages
            .get(shell)
            .ok_or_else(|| ClientError::NoPage(shell.to_owned()))?
            .mirror
            .snapshot_id;
        let message = Message::Fetch {
            have_snapshot_id,
            cols,
            rows,
            shell: shell.to_owned(),
        };
        self.submit(&message, now_ms)?;
        self.stats.fetches_sent += 1;
        Ok(())
    }

    /// Ask which shells the host is running. The answer is one or more [`ClientEvent::Sessions`].
    ///
    /// # Errors
    ///
    /// A transport refusal.
    pub fn list(&mut self, now_ms: u64) -> Result<(), ClientError> {
        self.submit(&Message::List, now_ms)?;
        self.stats.lists_sent += 1;
        Ok(())
    }

    /// Take one radio record: what the carrier handed up, before anything has been decided about it.
    ///
    /// This is the only entry for what arrives from the air, so the endpoint's order of checks — the
    /// direction filter, the transit rule, then the tag — is the order that runs. A record that
    /// decrypts becomes a [`ClientEvent`]; nothing else does.
    pub fn on_record(
        &mut self,
        record: &[u8],
        info: &ArrivalInfo,
        now_ms: u64,
        wall_ms: u64,
    ) -> Vec<ClientEvent> {
        self.endpoint.on_record(record, info, now_ms, wall_ms);
        for message in self.endpoint.take_inbound() {
            self.absorb(&message, now_ms);
        }
        self.take_events()
    }

    /// Take one already-decrypted application message.
    ///
    /// The app's BLE pipe hands records to [`Client::on_record`], so this exists for the callers that
    /// already hold a message: a test, and a transport that decrypted elsewhere.
    pub fn on_message(&mut self, message: &[u8], now_ms: u64) -> Vec<ClientEvent> {
        self.absorb(message, now_ms);
        self.take_events()
    }

    /// Let the link decide what it owes the air, and hand it to the carrier.
    ///
    /// # Errors
    ///
    /// [`MeshError`] when the carrier refuses a record or closes.
    pub async fn poll<C: CarriageSink>(
        &mut self,
        now_ms: u64,
        carrier: &mut C,
    ) -> Result<Vec<Outbound>, MeshError> {
        let report = self.endpoint.poll(now_ms, carrier).await?;
        for item in &report {
            self.note(item);
        }
        for message in self.endpoint.take_inbound() {
            self.absorb(&message, now_ms);
        }
        Ok(report)
    }

    /// When the caller should look again even if nothing arrives.
    #[must_use]
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.endpoint.next_deadline_ms()
    }

    /// The screen for one open shell.
    #[must_use]
    pub fn mirror(&self, shell: &str) -> Option<&PageMirror> {
        self.pages.get(shell).map(|page| &page.mirror)
    }

    /// The open pages, in the order they were opened.
    #[must_use]
    pub fn pages(&self) -> &[String] {
        &self.order
    }

    /// How many records may still be sent this hour, as the radio reported.
    pub fn set_records_left(&mut self, records_left: u8) {
        self.endpoint.set_records_left(records_left);
    }

    /// Every event since the last call.
    pub fn take_events(&mut self) -> Vec<ClientEvent> {
        core::mem::take(&mut self.events)
    }

    /// The client's own counters.
    #[must_use]
    pub fn stats(&self) -> &ClientStats {
        &self.stats
    }

    /// The link's counters, for a log that has to say what the air did.
    #[must_use]
    pub fn link_stats(&self) -> &Stats {
        self.endpoint.stats()
    }

    fn submit(&mut self, message: &Message, now_ms: u64) -> Result<(), ClientError> {
        let bytes = message.encode().map_err(ClientError::Encode)?;
        self.endpoint
            .submit(&bytes, now_ms)
            .map(|_tx_id| ())
            .map_err(ClientError::Begin)
    }

    /// Decode one application message and act on it.
    fn absorb(&mut self, message: &[u8], now_ms: u64) {
        let decoded = match Decoder::new().push(message) {
            Ok(mut messages) => messages.pop(),
            Err(_) => None,
        };
        let Some(Ok(message)) = decoded else {
            self.stats.unknown_downlink += 1;
            return;
        };
        match message {
            Message::Snapshot { .. } => self.absorb_snapshot(&message, now_ms),
            Message::Closed { reason } => self.events.push(ClientEvent::Closed { reason }),
            Message::Created { id } => self.events.push(ClientEvent::Created { id }),
            Message::Sessions { last, entries, .. } => {
                self.events.push(ClientEvent::Sessions { last, entries });
            }
            Message::Marks {
                first,
                last,
                entries,
            } => self.events.push(ClientEvent::Marks {
                first,
                last,
                entries,
            }),
            Message::HistoryPage {
                from,
                last,
                older,
                lines,
            } => self.events.push(ClientEvent::History {
                from,
                last,
                older,
                lines,
            }),
            _ => self.stats.unknown_downlink += 1,
        }
    }

    /// Join one snapshot chunk, and paint the screen when it is whole.
    fn absorb_snapshot(&mut self, chunk: &Message, now_ms: u64) {
        if self.assembling.is_open() {
            let started = self.assembling_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(*started) > ASSEMBLY_DEADLINE_MS {
                // A chunk never arrived. Drop the assembly rather than refuse every later screen as
                // "another screen's chunk".
                self.assembling.clear();
                self.assembling_since = None;
                self.stats.snapshots_refused += 1;
            }
        } else {
            self.assembling_since = None;
        }
        let joined = match self.assembling.accept(chunk) {
            Ok(Some(whole)) => whole,
            // More chunks are needed: nothing to paint yet, which is what keeps a chunked screen
            // atomic rather than half-drawn.
            Ok(None) => {
                self.assembling_since.get_or_insert(now_ms);
                return;
            }
            Err(_) => {
                // A chunk of another screen, or a count that disagrees: drop the assembly rather than
                // join two screens into one.
                self.assembling.clear();
                self.assembling_since = None;
                self.stats.snapshots_refused += 1;
                return;
            }
        };
        self.assembling_since = None;
        let Message::Snapshot {
            flags,
            snapshot_id,
            base_id,
            applied_op_id,
            applied_status,
            cols,
            rows,
            cursor_x,
            cursor_y,
            first_line,
            shell,
            patches,
            ..
        } = &joined
        else {
            return;
        };
        let screen = Screen {
            flags: *flags,
            snapshot_id: *snapshot_id,
            base_id: *base_id,
            applied: if *flags & snapshot_flag::HAS_APPLIED != 0 {
                Some(Applied {
                    op_id: *applied_op_id,
                    status: *applied_status,
                })
            } else {
                None
            },
            cols: *cols,
            rows: *rows,
            cursor: (*cursor_x, *cursor_y),
            first_line: *first_line,
            patches,
        };
        let Some(page) = self.pages.get_mut(shell) else {
            // A screen for a shell with no page: the host still thinks it is attached, or this is a
            // page the operator closed. Painting it into some other page would be the cross-shell
            // bleed `S7` was.
            self.stats.snapshots_unknown_page += 1;
            return;
        };
        match page.mirror.apply(&screen) {
            Paint::Duplicate => {
                self.stats.snapshots_duplicate += 1;
                return;
            }
            Paint::WrongBase => {
                self.stats.snapshots_wrong_base += 1;
                return;
            }
            Paint::Painted => {}
        }
        let just_attached = !page.attached;
        page.attached = true;
        page.pending = None;
        let (applied, settled, painted_cols, painted_rows) = (
            page.mirror.applied,
            page.mirror.settled,
            page.mirror.cols,
            page.mirror.rows,
        );
        self.stats.snapshots_applied += 1;
        if let Some(applied) = applied {
            self.stats.note_applied(applied);
        }
        if just_attached {
            self.events.push(ClientEvent::Attached {
                shell: shell.clone(),
                cols: painted_cols,
                rows: painted_rows,
            });
        }
        self.events.push(ClientEvent::Settled {
            shell: shell.clone(),
            snapshot_id: *snapshot_id,
            applied,
            settled,
        });
        if applied.is_some_and(Applied::is_held) {
            self.events.push(ClientEvent::Held {
                shell: shell.clone(),
            });
        }
    }

    fn note(&mut self, outbound: &Outbound) {
        match outbound {
            Outbound::Sent { what, .. } => match what {
                SentKind::Initiation => self.stats.initiations_sent += 1,
                SentKind::Part {
                    retransmit: true, ..
                } => {
                    self.stats.parts_sent += 1;
                    self.stats.retransmissions += 1;
                }
                SentKind::Part {
                    retransmit: false, ..
                } => self.stats.parts_sent += 1,
                SentKind::Ack => self.stats.acks_sent += 1,
                // The phone answers nothing and asks nothing to handshake again: those are the
                // host's records, and counting them here would report a thing that did not happen.
                SentKind::Answer | SentKind::AnswerTeaching | SentKind::NeedHandshake => {}
            },
            Outbound::Completed { .. } => self.stats.completed += 1,
            Outbound::TimedOut { .. } => self.stats.timed_out += 1,
            Outbound::Superseded { .. } => self.stats.superseded += 1,
            Outbound::BootstrapFailed { .. } => self.stats.bootstrap_failures += 1,
        }
    }
}

/// Split one command into chunks that each fit one `T_SEND`.
///
/// The limit is the encoder's, not a second copy of its arithmetic: every candidate is encoded, and a
/// chunk the encoder refuses is a chunk that is too large. A single item that does not fit is split —
/// a paste of forty lines is one item and several messages — and the host joins adjacent same-kind
/// items into one write, so a split inside a text run reassembles byte for byte.
fn chunk_items(
    shell: &str,
    items: &[SendItem],
    op_id: u32,
    have_snapshot_id: u32,
    cols: u8,
    rows: u8,
) -> Result<Vec<Vec<SendItem>>, ClientError> {
    let fits = |candidate: &[SendItem]| {
        if candidate.is_empty() || candidate.len() > MAX_SEND_ITEMS {
            return false;
        }
        Message::Send {
            op_id,
            chunk_idx: 0,
            chunk_count: 1,
            have_snapshot_id,
            cols,
            rows,
            flags: 0,
            shell: shell.to_owned(),
            items: candidate.to_vec(),
        }
        .encode()
        .is_ok()
    };

    // Pieces: items small enough to sit in a chunk on their own, with an oversized one cut in two
    // until it is.
    let mut pieces: Vec<SendItem> = Vec::new();
    let mut waiting: Vec<SendItem> = items.to_vec();
    waiting.reverse();
    while let Some(item) = waiting.pop() {
        if fits(std::slice::from_ref(&item)) {
            pieces.push(item);
            continue;
        }
        let Some((left, right)) = split_item(item) else {
            // One item that cannot be halved and does not fit: a name longer than a message, which no
            // legal message can carry.
            return Err(ClientError::Encode(EncodeError::TooLong));
        };
        waiting.push(right);
        waiting.push(left);
    }

    let mut chunks: Vec<Vec<SendItem>> = Vec::new();
    let mut current: Vec<SendItem> = Vec::new();
    for piece in pieces {
        let mut candidate = current.clone();
        candidate.push(piece.clone());
        if fits(&candidate) {
            current = candidate;
        } else {
            if current.is_empty() {
                return Err(ClientError::Encode(EncodeError::TooLong));
            }
            chunks.push(core::mem::take(&mut current));
            current.push(piece);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        return Err(ClientError::Encode(EncodeError::Empty));
    }
    Ok(chunks)
}

/// Cut one item in two, or `None` when it cannot be cut.
fn split_item(item: SendItem) -> Option<(SendItem, SendItem)> {
    match item {
        SendItem::Text(bytes) => {
            let (left, right) = split_bytes(&bytes);
            Some((SendItem::Text(left), SendItem::Text(right)))
        }
        SendItem::Paste(bytes) => {
            let (left, right) = split_bytes(&bytes);
            Some((SendItem::Paste(left), SendItem::Paste(right)))
        }
        SendItem::Keys(names) => {
            if names.len() < 2 {
                return None;
            }
            let mid = names.len() / 2;
            let mut left = names;
            let right = left.split_off(mid);
            Some((SendItem::Keys(left), SendItem::Keys(right)))
        }
    }
}

/// Half of a byte run, cut at a character boundary when the run is valid UTF-8.
///
/// A mid-character cut would still be legal on the wire and would still reassemble at the host, but a
/// boundary costs nothing and keeps a capture of one message readable.
fn split_bytes(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if bytes.len() < 2 {
        return (bytes.to_vec(), Vec::new());
    }
    let mut mid = bytes.len() / 2;
    // A UTF-8 continuation byte is `10xxxxxx`; stepping off one keeps the cut between characters.
    while mid < bytes.len() && bytes[mid] & 0xC0 == 0x80 {
        mid += 1;
    }
    (bytes[..mid].to_vec(), bytes[mid..].to_vec())
}
