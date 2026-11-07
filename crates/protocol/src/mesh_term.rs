//! The mesh terminal profile: what goes *inside* the L3 session (ADR-0025).
//!
//! The mesh link carries at most 255 records an hour of 143 application bytes each — the
//! one-byte budget field's ceiling, which the deployed SF7 tuple reaches well before any legal
//! limit does (`docs/ARCHITECTURE.md` section 9 has the arithmetic) — so a VT byte stream cannot
//! cross it. This module is the alternative: the host
//! sends the screen as **plain text rows**, and after the first full screen only the rows that
//! changed. Everything here is pure: no I/O, no clock, no terminal crate. The host feeds it a
//! [`Screen`] it got from the keeper's snapshot; the phone feeds the decoded messages to a
//! [`Mirror`].
//!
//! # Framing
//!
//! Messages are written one after another into the session's ordered byte stream, which the
//! ARQ may split anywhere. Each message is self-delimiting:
//!
//! ```text
//! tag (1) | len (1 or 2: LEB128, 7 bits per byte, low first) | body (len bytes)
//! ```
//!
//! A decoder skips a tag it does not know (the length says how far), so the set can grow without
//! a flag day. A length above [`MAX_BODY`] is malformed.
//!
//! # Messages
//!
//! | tag | direction | name | body |
//! |---|---|---|---|
//! | 0x01 | phone→host | `Attach` | `version u8`, `selector` (0..=40 bytes, ASCII session-id prefix, may be empty) |
//! | 0x02 | phone→host | `Input` | 1..=[`MAX_INPUT`] raw bytes for the shell |
//! | 0x03 | phone→host | `Resync` | empty: "send me the whole screen again" |
//! | 0x04 | phone→host | `Detach` | empty |
//! | 0x05 | phone→host | `List` | empty: "which shells are running?" (ADR-0025, 2026-10-29 addition) |
//! | 0x06 | phone→host | `Key` | 1..=[`MAX_KEYS`] key names (`Up`, `S-Tab`, `C-c`), single-space separated, at most [`MAX_INPUT`] bytes (version 2) |
//! | 0x07 | phone→host | `Resize` | `cols u8`, `rows u8`, both 1..=255: fit the pty to the phone, then a fresh `Profile` and a full screen (version 3) |
//! | 0x08 | phone→host | `Create` | `version u8`, `cwd` (0..=[`MAX_CWD`] bytes, empty = the host's default): start a new shell on the host (version 7) |
//! | 0x09 | phone→host | `Close` | `selector` (0..=[`MAX_SELECTOR`] bytes): end that shell, or let go of it when a desk window holds it (version 7) |
//! | 0x0a | phone→host | `MakeLive` | `selector`: open it in a desk window on the host (version 7) |
//! | 0x0b | phone→host | `Takeover` | `selector`: attach, displacing whoever holds it (version 8) |
//! | 0x81 | host→phone | `Profile` | `version u8`, `cols u8`, `rows u8`, `unsupported u16 LE`, and from version 2 `shell_len u8 \| shell id` |
//! | 0x82 | host→phone | `Rows` | `epoch u16 LE`, `flags u8` (bit 0 = full), then patches |
//! | 0x83 | host→phone | `Commit` | `epoch u16 LE`, `cursor_x u8`, `cursor_y u8`, `flags u8` |
//! | 0x84 | host→phone | `Closed` | `reason u8` |
//! | 0x86 | host→phone | `Created` | `id_len u8 \| id`: the shell `Create` just made (version 7) |
//! | 0x85 | host→phone | `Sessions` | `flags u8` (bit 0 = last page, bit 1 = entries carry a `cwd`, bit 2 = `created_by`, bit 3 = the current holder), then entries `id_len u8 \| id \| label_len u8 \| label`, and after each: `cwd`, `origin`, `owner` when their bits are set |
//!
//! A patch is `y u8 | flags u8 (bit 0 = append) | n u8 | n bytes of UTF-8`. A row longer than
//! [`MAX_SEGMENT`] bytes is sent as a first segment (replace) and further segments (append).
//!
//! # What this is not
//!
//! Every host→phone message encodes to at most [`MAX_MESSAGE`] bytes, so each one fits one
//! 144-byte record. The phone paints on `Commit` only: `Rows` for an epoch are held until its
//! `Commit` arrives, so a screen is never half-drawn.

use std::collections::VecDeque;

/// Profile version carried in `Attach` and `Profile`.
///
/// Version 2 adds the `Key` message and names the attached shell in `Profile`. A host answers
/// in the version the phone attached with (it accepts [`MIN_VERSION`]..=[`VERSION`]), so a
/// version-1 phone still gets a version-1 `Profile` it can decode, and a phone that attaches
/// with 2 against a version-1 host is told `closed::VERSION` rather than left sending a message
/// that host would skip.
///
/// Version 3 adds `Resize`, so the phone can ask the host to fit its pty to the phone's own
/// grid, as every tailnet client does. A host answers a resize with a fresh `Profile` and a full
/// screen. The `Profile` body is unchanged from version 2.
///
/// Version 6 adds a shell's working directory to each `Sessions` entry (see [`CWD_VERSION`]). A
/// `Sessions` page is answered **before** an attach can succeed (the phone learns which shell to
/// attach to from it), so it cannot be gated on a `Profile`'s version: the page carries a flag of
/// its own and the host sets it only for a phone that has attached with version 6 or later.
pub const VERSION: u8 = 9;
/// The oldest profile version a host still speaks.
pub const MIN_VERSION: u8 = 1;
/// The first version that has `Key` and the shell id in `Profile`.
pub const KEY_VERSION: u8 = 2;
/// The first version that has `Resize`.
pub const RESIZE_VERSION: u8 = 3;
/// First version whose phone decodes **several messages in one record** (`TermDecoder` is a
/// stream decoder). From here the host packs a `Rows` and the `Commit` that belongs to it into
/// one record when they fit, which is one frame instead of two for an ordinary echo.
pub const STREAM_VERSION: u8 = 4;

/// The version whose `Commit` names the absolute line at the top of the screen (version 5).
///
/// The field is what makes a phone-side scrollback possible at all: a mirror of screen positions
/// cannot tell "these rows scrolled up into history" from "these rows were rewritten in place",
/// and guessing by matching content is what makes mosh's history unreliable. Four bytes once per
/// commit answer it exactly.
pub const STREAM_LINE_VERSION: u8 = 5;

/// The version whose `Sessions` entries carry the shell's working directory (version 6).
///
/// The phone's picker shows where a shell is, as the tailnet launcher does, instead of the id it
/// would send. The field is gated on the flag bit in each page rather than on the profile version
/// because a list is answered before the first successful attach; a version-5 phone keeps
/// decoding the version-5 form.
pub const CWD_VERSION: u8 = 6;

/// The version that lets a phone start and end shells on the host, not only view them (version 7).
///
/// Version 6 and older gave the phone `List`, `Attach`, `Input`, `Key`, `Resize` and `Detach`, so a
/// phone could type into a shell the host already had but could not make one. That is the whole of
/// the gap between the mesh route and the tailnet launcher: over the tailnet a phone opens and
/// closes its own shells, and over the radio it could only watch the desk's. A host on version 7
/// answers `Create`, `Close` and `MakeLive`; a version-6 host is not asked, because a message it
/// would skip leaves the phone waiting for an answer that cannot come.
pub const SHELL_CONTROL_VERSION: u8 = 7;

/// The version that says **who holds a shell now**, and lets a phone take one from the desk
/// (version 8).
///
/// A phone that attaches to a shell a desk window is watching used to take it silently: the claim
/// displaced the window, the pty was fitted to the phone, and the person at the desk saw their
/// shell go narrow and stop responding to their typing with no way back. That is the same event
/// the tailnet route has always handled by asking — `HeldBy`, then a takeover the user chooses.
/// Version 8 carries the holder on each `Sessions` entry and refuses a plain `Attach` on a
/// shell somebody else holds; `Takeover` is the phone saying yes.
pub const OWNER_VERSION: u8 = 8;
/// The version whose `Sessions` entries carry the `created_by` origin (version 7).
///
/// The mesh launcher needs the same fact the tailnet one has: a shell this phone opened stays the
/// phone's after the app restarts, so the row keeps its close and open-on-desktop actions instead
/// of passing for a desk shell. Carried as a flag bit in each page for the same reason `cwd` is —
/// a list is answered before the first successful attach, so there is no profile version to gate on.
pub const ORIGIN_VERSION: u8 = 7;

/// The version whose host sends the [`Message::Marks`] table (version 9).
///
/// The radio has no way to reach a phone that is not connected, and keeping a session established
/// just to be reachable would cost about twelve records an hour of a budget that is a few hundred —
/// so the host sends the table when the phone attaches instead of pushing. A version-8 phone is
/// never sent the tag, and a version-9 phone against a version-8 host simply never sees one, which
/// the app must treat as "no marks" rather than as a broken link.
/// `Marks` gained a `first`/`last` flags byte after this version number was chosen, and the number was
/// deliberately **not** bumped: nothing has been released, so version 9 is the first and only layout
/// the tag has ever had. A bump would claim a version 9 that existed and does not. The fixture under
/// `test/fixtures/mesh-term-vectors.json` is the definition of the layout, and the app's codec test
/// reads it.
pub const MARKS_VERSION: u8 = 9;

/// Most key names in one `Key` message.
pub const MAX_KEYS: usize = 16;

/// A message body is at most this long.
pub const MAX_BODY: usize = 300;
/// A `Rows` body is packed up to this many bytes, so a whole message (tag + len + body) is at
/// most [`MAX_MESSAGE`] and fits one record's 144-byte payload.
///
/// **134, not 130, because a version-7 entry carries a third field.** The worst case is
/// `2 + 40 (id) + 24 (label) + 61 (cwd) + 7 (origin)` = 134, and a page must hold at least one
/// entry: the packer puts an oversized entry on a page of its own, and an entry that cannot fit
/// even alone made `Sessions` fail to encode, which is a list the phone never receives. The
/// message is still 137 bytes, inside one record's 144, so the invariant the number exists for
/// holds.
pub const MAX_ROWS_BODY: usize = 134;
/// The largest encoded host→phone message.
pub const MAX_MESSAGE: usize = 1 + 2 + MAX_ROWS_BODY;
/// The payload of one record after the L3 headers: 172 − 24 (ADR-0020) − 4 (ARQ).
pub const RECORD_PAYLOAD: usize = 143;
/// Most bytes in one `Input` message.
pub const MAX_INPUT: usize = 96;
/// Most bytes of a shell label in a `Sessions` entry (cut at a character boundary by the host).
pub const MAX_LABEL: usize = 24;
/// Most bytes of a shell's working directory in a `Sessions` entry (cut at a character boundary
/// by the host). One entry's worst case, with the version-8 bytes, is
/// `2 + 40 (id) + 24 (label) + 61 (cwd) + 1 (origin) + 1 (owner) = 129` bytes, under
/// [`MAX_ROWS_BODY`]'s 134, so an entry always fits one record however long its path is.
pub const MAX_CWD: usize = 60;
/// Most shells a host lists. A host with more lists the first this many and says nothing else:
/// a picker of more than this is not usable at one record a minute anyway.
pub const MAX_SESSIONS: usize = 16;
/// Most bytes of text in one row segment.
pub const MAX_SEGMENT: usize = 120;
/// Longest `Attach` selector.
pub const MAX_SELECTOR: usize = 40;
/// The longest origin word kept in memory: `desktop` is the longest of `phone`, `desktop`, `local`.
pub const MAX_ORIGIN: usize = 7;

/// Who started a shell, or who holds it, as one byte on the wire.
///
/// A length-prefixed word cost eight bytes where one says it. Two fields per entry made the
/// worst case `2 + id 40 + label 24 + cwd 61 + origin 8 + owner 8` = 143, one byte past the record
/// a page has to fit, so a full-length shell could not be listed at all. The vocabulary is five
/// words and it changes with the protocol, not with the user's data, so a code is the right
/// representation: 0 nobody, 1 the phone, 2 the desk, 3 the host itself, 4 the mesh link.
pub mod holder {
    pub const NONE: u8 = 0;
    pub const PHONE: u8 = 1;
    pub const DESKTOP: u8 = 2;
    pub const LOCAL: u8 = 3;
    pub const MESH: u8 = 4;
}

/// The byte for a holder/origin word. An unknown word is [`holder::LOCAL`], which is what the
/// daemon writes when a caller names nothing.
fn holder_code(word: &str) -> u8 {
    match word {
        "phone" => holder::PHONE,
        "desktop" => holder::DESKTOP,
        "mesh" => holder::MESH,
        "" => holder::NONE,
        _ => holder::LOCAL,
    }
}

/// The word for a code, for the fields the app reads.
fn holder_word(code: u8) -> String {
    match code {
        holder::PHONE => "phone",
        holder::DESKTOP => "desktop",
        holder::MESH => "mesh",
        holder::LOCAL => "local",
        _ => "",
    }
    .to_owned()
}
/// A decoder holds at most this much unparsed input.
pub const MAX_BUFFER: usize = 4096;

/// Features the mesh profile does not carry (`Profile.unsupported`). The client shows these as
/// absent rather than pretending: a set bit means "not available on this link".
pub mod unsupported {
    pub const SCROLLBACK: u16 = 1 << 0;
    /// Colours and text attributes: rows are plain text.
    pub const STYLE: u16 = 1 << 1;
    pub const HYPERLINKS: u16 = 1 << 2;
    pub const MOUSE: u16 = 1 << 3;
    pub const IMAGES: u16 = 1 << 4;
    /// A version-1 or version-2 phone does not resize the pane; it renders the host's geometry.
    /// A version-3 phone sends `Resize` and this bit is cleared in its `Profile`.
    pub const RESIZE: u16 = 1 << 5;
    pub const CLIPBOARD: u16 = 1 << 6;
    /// Everything a version-1 host leaves out.
    pub const ALL_V1: u16 = SCROLLBACK | STYLE | HYPERLINKS | MOUSE | IMAGES | RESIZE | CLIPBOARD;
}

/// Why the host ended the attach (`Closed.reason`).
pub mod closed {
    /// The selector matched no running shell.
    pub const NOT_FOUND: u8 = 1;
    /// The selector matched more than one shell, or was empty and more than one is running.
    pub const AMBIGUOUS: u8 = 2;
    /// The shell exited.
    pub const EXITED: u8 = 3;
    /// The host could not read the screen.
    pub const UNAVAILABLE: u8 = 4;
    /// `Attach.version` is not one the host speaks.
    pub const VERSION: u8 = 5;
    /// Another viewer holds the shell and could not be displaced.
    pub const HELD: u8 = 6;
}

/// `Commit.flags`.
pub mod commit {
    /// The program is on the alternate screen (a full-screen program: rows are rewritten
    /// wholesale, so updates will be larger).
    pub const ALTERNATE: u8 = 1 << 0;
    /// The shell has exited.
    pub const DEAD: u8 = 1 << 1;
    /// **The pane has stopped changing; this commit carries no rows.**
    ///
    /// The host is the only end that knows an answer has finished arriving, and the phone used to
    /// guess: it ended its wait for an answer after [`QUIET_MS`](crate::mesh_term) of silence, so
    /// the bar said "waiting for the answer" for seconds after the answer was already drawn. One
    /// extra record per burst says it exactly, and the phone's timer stays as the fallback for a
    /// host that does not send it. Unknown flag bits are ignored by every decoder, so this is
    /// additive on the wire.
    pub const IDLE: u8 = 1 << 2;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub y: u8,
    /// Add to the row instead of replacing it.
    pub append: bool,
    pub text: String,
}

/// One running shell in a `Sessions` reply: its id (usable as an `Attach` selector), a label, and
/// the directory it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    pub label: String,
    /// Where the shell is (version 6). Empty in a page built for a version-5 phone, where the
    /// field is not on the wire at all.
    pub cwd: String,
    /// Who holds it now: `desktop`, `phone`, `mesh`, or empty for nobody (version 8). A shell with
    /// a holder is one the phone must ask to take; the desk's own window is told when it loses it.
    pub owner: String,
    /// Who started the shell: `phone`, `desktop`, `local` (version 7). Empty in a page built for
    /// a version-6 phone. The mesh launcher reads this to keep a phone-opened shell marked as the
    /// phone's, so its close and open-on-desktop actions survive an app restart.
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Attach {
        version: u8,
        selector: String,
    },
    Input(Vec<u8>),
    Resync,
    Detach,
    Profile {
        version: u8,
        cols: u8,
        rows: u8,
        unsupported: u16,
        /// The id of the attached shell (version 2; empty in version 1). After an `Attach` with
        /// an empty selector the host chose the shell, and this is how the phone learns which.
        shell: String,
    },
    Rows {
        epoch: u16,
        full: bool,
        patches: Vec<Patch>,
    },
    Commit {
        epoch: u16,
        cursor_x: u8,
        cursor_y: u8,
        flags: u8,
        /// The absolute line number of row 0 (version 5). A screen that scrolls by N advances
        /// this by N, which is how the phone learns that the N rows leaving the top are history
        /// it already holds rather than rows overwritten in place - the one fact a mirror of
        /// screen positions cannot express.
        first_line: u32,
    },
    Closed {
        reason: u8,
    },
    /// Phone asks which shells are running.
    List,
    /// Named keys for the shell (version 2). The host encodes them for the pane's own cursor-key
    /// and keyboard mode, exactly as it does for a tailnet client, so the phone carries no key
    /// table and cannot send an arrow the program will not read.
    Key(Vec<String>),
    /// The phone's grid, in cells (version 3). The host resizes the attached pty to it and starts
    /// the screen over: a fresh `Profile` and a full screen. Both are 1..=255; the host clamps
    /// them again to the range a pane may have.
    Resize {
        cols: u8,
        rows: u8,
    },
    /// One page of the answer. `last` is set on the final page; an empty list is one page with
    /// `last` set and no entries. `cwd` says the entries carry their working directory (version
    /// 6): the host sets it only for a phone that attached with [`CWD_VERSION`] or later, and the
    /// flag is what lets one decoder read both forms. `origin` says they carry `created_by`
    /// (version 7), set only for a phone on [`ORIGIN_VERSION`] or later.
    Sessions {
        last: bool,
        cwd: bool,
        origin: bool,
        owner: bool,
        entries: Vec<SessionEntry>,
    },
    /// The marks that are true right now: one per shell, `kind` 0 = done, 1 = attention.
    ///
    /// **The radio route replays, it does not push.** There is no way to reach a phone that is not
    /// connected, and keeping a radio session up to be reachable costs airtime for something the
    /// list already says, so the host sends this table when the phone attaches and the app draws it
    /// beside the rows. Version 9 and later only; the note a `ldmux buzz` may carry is not sent,
    /// because the radio's budget is records and a row label is enough.
    ///
    /// **Paged, and the phone merges the pages.** A message body is capped at
    /// [`MAX_ROWS_BODY`] and an entry is its id plus two bytes, so a dozen marked shells need more
    /// than one message. `first` opens a run and `last` closes it: a page that is not `last` is
    /// appended to what has arrived, and the page that is `last` replaces the table. Without the
    /// flags the phone would replace the table per page and show only the last one.
    Marks {
        first: bool,
        last: bool,
        entries: Vec<MarkEntry>,
    },
    /// Phone asks the host to start a shell in `cwd` (empty = the host's default directory).
    /// Version 7 and later only.
    Create {
        version: u8,
        cwd: String,
    },
    /// Phone asks the host to end that shell. A shell a desk window holds is put down instead of
    /// destroyed, exactly as the tailnet's `close` does (ADR-0010). Version 7 and later only.
    Close {
        selector: String,
    },
    /// Phone asks to attach to a shell somebody else holds, displacing them. The consent the
    /// tailnet's takeover is, and the second half of `Closed { HELD }`: a plain `Attach` is
    /// refused while a desk window holds the shell, so taking it is always something the user
    /// chose. Version 8 and later only.
    Takeover {
        selector: String,
    },
    /// Phone asks the host to open that shell in a desk window. Version 7 and later only.
    MakeLive {
        selector: String,
    },
    /// Host: the shell `Create` just made. The phone learns which id to attach to without
    /// guessing it from a list that another client may have changed in between.
    Created {
        id: String,
    },
}

/// One row of [`Message::Marks`]: a shell and the word about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkEntry {
    pub id: String,
    /// 0 = done (an agent finished a turn), 1 = attention (it wants a look).
    pub kind: u8,
}

const T_LIST: u8 = 0x05;
const T_KEY: u8 = 0x06;
const T_RESIZE: u8 = 0x07;
const T_SESSIONS: u8 = 0x85;
const T_CREATE: u8 = 0x08;
const T_CLOSE: u8 = 0x09;
const T_MAKE_LIVE: u8 = 0x0a;
const T_TAKEOVER: u8 = 0x0b;
const T_CREATED: u8 = 0x86;
const T_ATTACH: u8 = 0x01;
const T_INPUT: u8 = 0x02;
const T_RESYNC: u8 = 0x03;
const T_DETACH: u8 = 0x04;
const T_PROFILE: u8 = 0x81;
const T_ROWS: u8 = 0x82;
const T_COMMIT: u8 = 0x83;
const T_CLOSED: u8 = 0x84;
/// The marks table (version 9). Host to phone only.
const T_MARKS: u8 = 0x87;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// A field is over its limit (selector, input, a patch segment, a `Rows` body).
    TooLong,
    /// An empty `Input`.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A length above [`MAX_BODY`].
    BodyTooLong,
    /// The unparsed input grew past [`MAX_BUFFER`].
    Overflow,
    /// A known tag whose body does not parse. The message is dropped, the stream continues.
    Malformed(u8),
}

/// A key name is printable ASCII with no space: the space is the separator, and anything else
/// is not a name the host's key table could know.
fn valid_key_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn put_len(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

impl Message {
    /// The wire bytes of this message.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut body = Vec::new();
        let tag = match self {
            Self::Attach { version, selector } => {
                if selector.len() > MAX_SELECTOR {
                    return Err(EncodeError::TooLong);
                }
                body.push(*version);
                body.extend_from_slice(selector.as_bytes());
                T_ATTACH
            }
            Self::Input(bytes) => {
                if bytes.is_empty() {
                    return Err(EncodeError::Empty);
                }
                if bytes.len() > MAX_INPUT {
                    return Err(EncodeError::TooLong);
                }
                body.extend_from_slice(bytes);
                T_INPUT
            }
            Self::Resync => T_RESYNC,
            Self::Detach => T_DETACH,
            Self::Profile {
                version,
                cols,
                rows,
                unsupported,
                shell,
            } => {
                body.extend_from_slice(&[*version, *cols, *rows]);
                body.extend_from_slice(&unsupported.to_le_bytes());
                if *version >= KEY_VERSION {
                    if shell.len() > MAX_SELECTOR {
                        return Err(EncodeError::TooLong);
                    }
                    body.push(shell.len() as u8);
                    body.extend_from_slice(shell.as_bytes());
                }
                T_PROFILE
            }
            Self::Rows {
                epoch,
                full,
                patches,
            } => {
                body.extend_from_slice(&epoch.to_le_bytes());
                body.push(u8::from(*full));
                for p in patches {
                    if p.text.len() > MAX_SEGMENT {
                        return Err(EncodeError::TooLong);
                    }
                    body.push(p.y);
                    body.push(u8::from(p.append));
                    body.push(p.text.len() as u8);
                    body.extend_from_slice(p.text.as_bytes());
                }
                if body.len() > MAX_ROWS_BODY {
                    return Err(EncodeError::TooLong);
                }
                T_ROWS
            }
            Self::Commit {
                epoch,
                cursor_x,
                cursor_y,
                flags,
                first_line,
            } => {
                body.extend_from_slice(&epoch.to_le_bytes());
                body.extend_from_slice(&[*cursor_x, *cursor_y, *flags]);
                if *first_line > 0 {
                    body.extend_from_slice(&first_line.to_le_bytes());
                }
                T_COMMIT
            }
            Self::Closed { reason } => {
                body.push(*reason);
                T_CLOSED
            }
            Self::List => T_LIST,
            Self::Key(names) => {
                if names.is_empty() {
                    return Err(EncodeError::Empty);
                }
                if names.len() > MAX_KEYS || names.iter().any(|n| !valid_key_name(n)) {
                    return Err(EncodeError::TooLong);
                }
                body.extend_from_slice(names.join(" ").as_bytes());
                if body.len() > MAX_INPUT {
                    return Err(EncodeError::TooLong);
                }
                T_KEY
            }
            Self::Resize { cols, rows } => {
                if *cols == 0 || *rows == 0 {
                    return Err(EncodeError::Empty);
                }
                body.extend_from_slice(&[*cols, *rows]);
                T_RESIZE
            }
            Self::Create { version, cwd } => {
                if cwd.len() > MAX_CWD {
                    return Err(EncodeError::TooLong);
                }
                body.push(*version);
                body.extend_from_slice(cwd.as_bytes());
                T_CREATE
            }
            Self::Close { selector } | Self::MakeLive { selector } | Self::Takeover { selector } => {
                if selector.len() > MAX_SELECTOR {
                    return Err(EncodeError::TooLong);
                }
                body.extend_from_slice(selector.as_bytes());
                match self {
                    Self::Close { .. } => T_CLOSE,
                    Self::MakeLive { .. } => T_MAKE_LIVE,
                    _ => T_TAKEOVER,
                }
            }
            Self::Created { id } => {
                if id.len() > MAX_SELECTOR {
                    return Err(EncodeError::TooLong);
                }
                body.push(id.len() as u8);
                body.extend_from_slice(id.as_bytes());
                T_CREATED
            }
            Self::Marks {
                first,
                last,
                entries,
            } => {
                body.push(u8::from(*first) | if *last { 2 } else { 0 });
                for e in entries {
                    if e.id.len() > MAX_SELECTOR {
                        return Err(EncodeError::TooLong);
                    }
                    body.push(e.kind);
                    body.push(e.id.len() as u8);
                    body.extend_from_slice(e.id.as_bytes());
                }
                if body.len() > MAX_ROWS_BODY {
                    return Err(EncodeError::TooLong);
                }
                T_MARKS
            }
            Self::Sessions {
                last,
                cwd,
                origin,
                owner,
                entries,
            } => {
                body.push(
                    u8::from(*last)
                        | if *cwd { 2 } else { 0 }
                        | if *origin { 4 } else { 0 }
                        | if *owner { 8 } else { 0 },
                );
                for e in entries {
                    if e.id.len() > MAX_SELECTOR || e.label.len() > MAX_LABEL {
                        return Err(EncodeError::TooLong);
                    }
                    if *cwd && e.cwd.len() > MAX_CWD {
                        return Err(EncodeError::TooLong);
                    }

                    body.push(e.id.len() as u8);
                    body.extend_from_slice(e.id.as_bytes());
                    body.push(e.label.len() as u8);
                    body.extend_from_slice(e.label.as_bytes());
                    if *cwd {
                        body.push(e.cwd.len() as u8);
                        body.extend_from_slice(e.cwd.as_bytes());
                    }
                    if *origin {
                        body.push(holder_code(&e.origin));
                    }
                    if *owner {
                        body.push(holder_code(&e.owner));
                    }
                }
                if body.len() > MAX_ROWS_BODY {
                    return Err(EncodeError::TooLong);
                }
                T_SESSIONS
            }
        };
        let mut out = Vec::with_capacity(body.len() + 3);
        out.push(tag);
        put_len(&mut out, body.len());
        out.extend_from_slice(&body);
        Ok(out)
    }

    fn parse(tag: u8, body: &[u8]) -> Result<Option<Self>, DecodeError> {
        let bad = DecodeError::Malformed(tag);
        Ok(Some(match tag {
            T_ATTACH => {
                let (&version, rest) = body.split_first().ok_or(bad.clone())?;
                if rest.len() > MAX_SELECTOR {
                    return Err(bad);
                }
                let selector = std::str::from_utf8(rest)
                    .map_err(|_| bad.clone())?
                    .to_owned();
                Self::Attach { version, selector }
            }
            T_INPUT => {
                if body.is_empty() || body.len() > MAX_INPUT {
                    return Err(bad);
                }
                Self::Input(body.to_vec())
            }
            T_RESYNC if body.is_empty() => Self::Resync,
            T_DETACH if body.is_empty() => Self::Detach,
            // Version 1 is exactly five bytes. A longer body is a version-2 body and must carry its
            // shell id: an old five-byte `Profile` is never an error.
            T_PROFILE if body.len() == 5 => Self::Profile {
                version: body[0],
                cols: body[1],
                rows: body[2],
                unsupported: u16::from_le_bytes([body[3], body[4]]),
                shell: String::new(),
            },
            T_PROFILE if body.len() > 5 && body[0] >= KEY_VERSION => {
                let n = usize::from(body[5]);
                if n > MAX_SELECTOR || body.len() != 6 + n {
                    return Err(bad);
                }
                let shell = std::str::from_utf8(&body[6..])
                    .map_err(|_| bad.clone())?
                    .to_owned();
                Self::Profile {
                    version: body[0],
                    cols: body[1],
                    rows: body[2],
                    unsupported: u16::from_le_bytes([body[3], body[4]]),
                    shell,
                }
            }
            // Neither shape: a `Profile` the phone cannot place. Skipping it left the phone with no
            // profile and no error, waiting for a screen it would never accept.
            T_PROFILE => return Err(bad),
            T_ROWS => {
                if body.len() < 3 {
                    return Err(bad);
                }
                let epoch = u16::from_le_bytes([body[0], body[1]]);
                let full = body[2] & 1 == 1;
                let mut patches = Vec::new();
                let mut rest = &body[3..];
                while !rest.is_empty() {
                    if rest.len() < 3 {
                        return Err(bad);
                    }
                    let n = usize::from(rest[2]);
                    if rest.len() < 3 + n || n > MAX_SEGMENT {
                        return Err(bad);
                    }
                    let text = std::str::from_utf8(&rest[3..3 + n])
                        .map_err(|_| bad.clone())?
                        .to_owned();
                    patches.push(Patch {
                        y: rest[0],
                        append: rest[1] & 1 == 1,
                        text,
                    });
                    rest = &rest[3 + n..];
                }
                Self::Rows {
                    epoch,
                    full,
                    patches,
                }
            }
            T_COMMIT if body.len() == 5 => Self::Commit {
                epoch: u16::from_le_bytes([body[0], body[1]]),
                cursor_x: body[2],
                cursor_y: body[3],
                flags: body[4],
                // Version 4 and older: the message carries no line number, and a phone on it is
                // told `Scrollback` is absent anyway.
                first_line: 0,
            },
            T_COMMIT if body.len() == 9 => Self::Commit {
                epoch: u16::from_le_bytes([body[0], body[1]]),
                cursor_x: body[2],
                cursor_y: body[3],
                flags: body[4],
                first_line: u32::from_le_bytes([body[5], body[6], body[7], body[8]]),
            },
            T_CLOSED if body.len() == 1 => Self::Closed { reason: body[0] },
            T_LIST if body.is_empty() => Self::List,
            T_KEY => {
                if body.is_empty() || body.len() > MAX_INPUT {
                    return Err(bad);
                }
                let text = std::str::from_utf8(body).map_err(|_| bad.clone())?;
                let names: Vec<String> = text.split(' ').map(str::to_owned).collect();
                if names.len() > MAX_KEYS || names.iter().any(|n| !valid_key_name(n)) {
                    return Err(bad);
                }
                Self::Key(names)
            }
            T_RESIZE if body.len() == 2 && body[0] != 0 && body[1] != 0 => Self::Resize {
                cols: body[0],
                rows: body[1],
            },
            T_CREATE => {
                let (&version, rest) = body.split_first().ok_or(bad.clone())?;
                if rest.len() > MAX_CWD {
                    return Err(bad);
                }
                let cwd = std::str::from_utf8(rest)
                    .map_err(|_| bad.clone())?
                    .to_owned();
                Self::Create { version, cwd }
            }
            T_CLOSE | T_MAKE_LIVE | T_TAKEOVER => {
                if body.len() > MAX_SELECTOR {
                    return Err(bad);
                }
                let selector = std::str::from_utf8(body)
                    .map_err(|_| bad.clone())?
                    .to_owned();
                match tag {
                    T_CLOSE => Self::Close { selector },
                    T_MAKE_LIVE => Self::MakeLive { selector },
                    _ => Self::Takeover { selector },
                }
            }
            T_CREATED => {
                let (&n, rest) = body.split_first().ok_or(bad.clone())?;
                if usize::from(n) != rest.len() || rest.len() > MAX_SELECTOR {
                    return Err(bad);
                }
                Self::Created {
                    id: std::str::from_utf8(rest)
                        .map_err(|_| bad.clone())?
                        .to_owned(),
                }
            }
            T_MARKS => {
                // The flags byte opens the body: `first` starts a run of pages, `last` ends it.
                let (&flags, rest) = body.split_first().ok_or_else(|| bad.clone())?;
                let first = flags & 1 != 0;
                let last = flags & 2 != 0;
                let mut rest = rest;
                let mut entries = Vec::new();
                while !rest.is_empty() {
                    let (&kind, tail) = rest.split_first().ok_or(bad.clone())?;
                    let (&n, tail) = tail.split_first().ok_or(bad.clone())?;
                    let n = usize::from(n);
                    if n > MAX_SELECTOR || tail.len() < n {
                        return Err(bad.clone());
                    }
                    let id = std::str::from_utf8(&tail[..n])
                        .map_err(|_| bad.clone())?
                        .to_owned();
                    rest = &tail[n..];
                    entries.push(MarkEntry { id, kind });
                }
                Message::Marks {
                    first,
                    last,
                    entries,
                }
            }
            T_SESSIONS => {
                let (&flags, mut rest) = body.split_first().ok_or(bad.clone())?;
                let with_cwd = flags & 2 != 0;
                let with_origin = flags & 4 != 0;
                let with_owner = flags & 8 != 0;
                let mut entries = Vec::new();
                while !rest.is_empty() {
                    let mut field = |max: usize| -> Result<String, DecodeError> {
                        let (&n, tail) = rest.split_first().ok_or(bad.clone())?;
                        let n = usize::from(n);
                        if n > max || tail.len() < n {
                            return Err(bad.clone());
                        }
                        let text = std::str::from_utf8(&tail[..n])
                            .map_err(|_| bad.clone())?
                            .to_owned();
                        rest = &tail[n..];
                        Ok(text)
                    };
                    let id = field(MAX_SELECTOR)?;
                    let label = field(MAX_LABEL)?;
                    let cwd = if with_cwd {
                        field(MAX_CWD)?
                    } else {
                        String::new()
                    };
                    let origin = if with_origin {
                        let (&c, tail) = rest.split_first().ok_or(bad.clone())?;
                        rest = tail;
                        holder_word(c)
                    } else {
                        String::new()
                    };
                    let owner = if with_owner {
                        let (&c, tail) = rest.split_first().ok_or(bad.clone())?;
                        rest = tail;
                        holder_word(c)
                    } else {
                        String::new()
                    };
                    entries.push(SessionEntry {
                        id,
                        label,
                        cwd,
                        origin,
                        owner,
                    });
                }
                Self::Sessions {
                    last: flags & 1 == 1,
                    cwd: with_cwd,
                    origin: with_origin,
                    owner: with_owner,
                    entries,
                }
            }
            T_RESYNC | T_DETACH | T_COMMIT | T_CLOSED | T_LIST | T_RESIZE => {
                return Err(bad);
            }
            _ => return Ok(None), // unknown tag: skipped, not fatal
        }))
    }
}

/// Reassembles messages from an arbitrarily chunked byte stream.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: VecDeque<u8>,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add bytes; return every complete message. A malformed body yields an `Err` entry for that
    /// message only and decoding continues after it. `Err(BodyTooLong | Overflow)` as the whole
    /// result means the stream is unusable and the caller should drop the attach.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Result<Message, DecodeError>>, DecodeError> {
        if self.buf.len() + bytes.len() > MAX_BUFFER {
            self.buf.clear();
            return Err(DecodeError::Overflow);
        }
        self.buf.extend(bytes.iter().copied());
        let mut out = Vec::new();
        // tag, then a one- or two-byte length
        while let Some(&tag) = self.buf.front() {
            let Some(&b1) = self.buf.get(1) else { break };
            let (len, head) = if b1 & 0x80 == 0 {
                (usize::from(b1), 2)
            } else {
                let Some(&b2) = self.buf.get(2) else { break };
                if b2 & 0x80 != 0 {
                    self.buf.clear();
                    return Err(DecodeError::BodyTooLong);
                }
                (usize::from(b1 & 0x7f) | (usize::from(b2) << 7), 3)
            };
            if len > MAX_BODY {
                self.buf.clear();
                return Err(DecodeError::BodyTooLong);
            }
            if self.buf.len() < head + len {
                break;
            }
            let body: Vec<u8> = self.buf.range(head..head + len).copied().collect();
            self.buf.drain(..head + len);
            match Message::parse(tag, &body) {
                Ok(Some(m)) => out.push(Ok(m)),
                Ok(None) => {}
                Err(e) => out.push(Err(e)),
            }
        }
        Ok(out)
    }
}

/// Cut `text` to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The `Sessions` pages for a list of `(id, label, cwd)`: at most [`MAX_SESSIONS`] entries, ids,
/// labels and paths cut to their limits, each page packed up to [`MAX_ROWS_BODY`] so it fits one
/// record, the final page flagged `last`. `cwd` says the entries carry the working directory
/// (version 6); a page built for an older phone leaves it off the wire and its entries carry an
/// empty `cwd`. Always at least one page, so "no shells" is an answer.
#[must_use]
pub fn session_pages(
    sessions: &[(String, String, String, String, String)],
    cwd: bool,
    origin: bool,
    owner: bool,
) -> Vec<Message> {
    let mut pages: Vec<Vec<SessionEntry>> = vec![Vec::new()];
    let mut size = 1; // the flags byte
    for (id, label, path, by, held) in sessions.iter().take(MAX_SESSIONS) {
        let e = SessionEntry {
            id: cut(id, MAX_SELECTOR),
            label: cut(label, MAX_LABEL),
            cwd: if cwd { cut(path, MAX_CWD) } else { String::new() },
            origin: if origin { cut(by, MAX_ORIGIN) } else { String::new() },
            owner: if owner { cut(held, MAX_ORIGIN) } else { String::new() },
        };
        let cost = 2
            + e.id.len()
            + e.label.len()
            + if cwd { 1 + e.cwd.len() } else { 0 }
            + if origin { 1 } else { 0 }
            + if owner { 1 } else { 0 };
        if size + cost > MAX_ROWS_BODY {
            pages.push(Vec::new());
            size = 1;
        }
        size += cost;
        pages.last_mut().expect("never empty").push(e);
    }
    let n = pages.len();
    pages
        .into_iter()
        .enumerate()
        .map(|(i, entries)| Message::Sessions {
            last: i + 1 == n,
            cwd,
            origin,
            owner,
            entries,
        })
        .collect()
}

// ───────────────────────────── the screen ─────────────────────────────

/// What the host reads from the keeper: plain text rows and the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    pub cols: u8,
    pub rows: u8,
    pub cursor_x: u8,
    pub cursor_y: u8,
    pub alternate: bool,
    pub dead: bool,
    /// The absolute line number of row 0, from the keeper's own line numbering. Zero when the
    /// source cannot say, which a phone treats as "no line identity": it accumulates nothing
    /// rather than guessing at a scroll.
    pub first_line: u32,
    /// Exactly `rows` entries, trailing spaces removed.
    pub lines: Vec<String>,
}

impl Screen {
    /// Build from the keeper's rendered text (SGR lines joined by `\n`). Escape sequences are
    /// removed, trailing spaces trimmed, and the row count forced to `rows`.
    #[must_use]
    pub fn from_keeper(
        text: &str,
        cols: u64,
        rows: u64,
        cursor: (u64, u64),
        alternate: bool,
        dead: bool,
        first_line: u32,
    ) -> Self {
        let rows_u8 = rows.min(255) as u8;
        let mut lines: Vec<String> = text.split('\n').map(plain_row).collect();
        // The keeper may include history above the live rows; the live rows are the last `rows`.
        if lines.len() > usize::from(rows_u8) {
            lines.drain(..lines.len() - usize::from(rows_u8));
        }
        lines.resize(usize::from(rows_u8), String::new());
        Self {
            cols: cols.min(255) as u8,
            rows: rows_u8,
            cursor_x: cursor.0.min(255) as u8,
            cursor_y: cursor.1.min(255) as u8,
            alternate,
            dead,
            first_line,
            lines,
        }
    }
}

/// One row as plain text: escape sequences removed, trailing spaces trimmed.
#[must_use]
pub fn plain_row(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.peek().copied() {
                Some('[') => {
                    it.next();
                    for d in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(d) = it.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    it.next();
                }
                None => {}
            },
            // **A tab is layout, not noise.** Dropping it reflowed every indented or
            // column-aligned line on the phone: `ls -l`, `make`, a Python traceback, `git log
            // --stat`. Expanded to the next 8-column stop, which is what the shell's own output
            // assumed.
            '\t' => {
                let col = out.chars().count();
                let stop = 8 - (col % 8);
                for _ in 0..stop {
                    out.push(' ');
                }
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.truncate(out.trim_end_matches(' ').len());
    out
}

/// Split a row into segments of at most [`MAX_SEGMENT`] bytes on character boundaries.
fn segments(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + MAX_SEGMENT).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        out.push(&text[start..end]);
        start = end;
    }
    out
}

/// The host's record of what the phone holds, and the producer of updates.
///
/// The session underneath is reliable and ordered, so what was last *sent* is what the phone
/// will hold. An update is the difference from that, computed only when the caller asks (the
/// bridge asks only when nothing is queued), so a phone that was away converges by one diff
/// against the present, never by replaying the changes in between.
#[derive(Debug, Clone)]
pub struct Differ {
    sent: Option<Screen>,
    epoch: u16,
    /// The profile version the phone attached with: its `Profile` is written in that version.
    version: u8,
    /// The attached shell, named in a version-2 `Profile`.
    shell: String,
    /// A frame whose rows did not all fit in the window the last time it was offered.
    ///
    /// Cleared by `reset` and by a geometry change: the phone keeps the `pending` rows of an open
    /// epoch, so continuing that epoch after either would paint the abandoned frame's patches over
    /// the new screen.
    ///
    /// The phone drops rows from an older epoch when the next epoch's rows arrive, so a frame
    /// split across ticks must keep one epoch: otherwise the second batch looks like a new frame
    /// and the first batch is thrown away, which is how a long screen arrived as its last rows
    /// only.
    frame_open: bool,
}

impl Default for Differ {
    fn default() -> Self {
        Self {
            sent: None,
            epoch: 0,
            frame_open: false,
            version: VERSION,
            shell: String::new(),
        }
    }
}

impl Differ {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A differ for one attach: `version` is what the phone asked for and `shell` is the shell
    /// it is attached to, so the first update tells it which.
    #[must_use]
    pub fn for_attach(version: u8, shell: &str) -> Self {
        Self {
            version,
            shell: shell.to_owned(),
            ..Self::default()
        }
    }

    /// Forget what the phone holds, so the next update is a full screen (`Resync`, or a new
    /// attach).
    ///
    /// **The epoch moves and the frame closes.** Clearing only `sent` left a frame that was
    /// still open (a screen split across ticks, or one whose send was refused) on the *same*
    /// epoch as the resync that followed it. The phone still holds that frame's `Rows` as
    /// `pending`, so its `Commit` painted the abandoned patches over the fresh full screen: a row
    /// the new screen leaves empty gets no replace patch under `full`, so a fragment of the old
    /// text stayed on a row the host says is blank, permanently, and `up_to_date` then agreed with
    /// the phone about the wrong screen.
    pub fn reset(&mut self) {
        self.sent = None;
        self.frame_open = false;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// The "nothing more is coming" marker for a pane the phone already has, if one is due.
    ///
    /// A `Commit` with [`commit::IDLE`] and no `Rows`: one record, sent once per burst, which is
    /// what lets the phone end its wait for an answer at the moment the answer ends instead of a
    /// quiet-timer later. `None` when there is no settled frame to mark (nothing has been sent yet,
    /// or a frame is still being carried over several ticks).
    #[must_use]
    pub fn idle_marker(&self, screen: &Screen) -> Option<Message> {
        if self.frame_open || self.sent.as_ref() != Some(screen) {
            return None;
        }
        let flags = commit::IDLE
            | (u8::from(screen.alternate) * commit::ALTERNATE)
            | (u8::from(screen.dead) * commit::DEAD);
        Some(Message::Commit {
            epoch: self.epoch,
            cursor_x: screen.cursor_x,
            cursor_y: screen.cursor_y,
            flags,
            first_line: if self.version >= STREAM_LINE_VERSION {
                screen.first_line
            } else {
                0
            },
        })
    }

    /// Whether `screen` is exactly what the phone already holds.
    ///
    /// The bridge uses this to decide whether to spend the tick on a diff at all: a screen that
    /// keeps changing is coalesced, and the coalescing window only makes sense if the decision
    /// is "there is something new", not "a tick has passed". Conservative in one direction
    /// only - `false` can still yield no messages (a frame split across ticks), while `true`
    /// always means nothing to send.
    #[must_use]
    pub fn up_to_date(&self, screen: &Screen) -> bool {
        !self.frame_open && self.sent.as_ref() == Some(screen)
    }

    /// The messages that bring the phone from what it holds to `screen`; empty when nothing
    /// differs. The first call after `new` or `reset` is a full screen, preceded by a `Profile`.
    pub fn update(&mut self, screen: &Screen) -> Vec<Message> {
        self.update_within(screen, usize::MAX)
    }

    /// As [`Self::update`], sending at most `room` messages and recording only what it sent.
    ///
    /// A frame bigger than the window used to be enqueued whole, because the queue was only
    /// checked between ticks. One full repaint could then put eight records in front of a
    /// keystroke, and the phone waited seconds for each of them: on the device that reads as a
    /// freeze, keys that go missing, and a session that eventually gives up. Bounding at the
    /// source only works if the record of what the phone holds stays honest - a row that was not
    /// sent must not be marked as sent, or the next diff skips it and the phone keeps a hole. So
    /// the snapshot advances per row, over exactly the patches that went out, and the cursor
    /// moves only when its `Commit` does.
    ///
    /// A change of geometry is the one thing sent whole or not at all: a `Profile` without the
    /// `Rows` that clear the pane, or with the cursor left elsewhere, describes a screen the
    /// phone does not have. Geometry changes are rare; the pane can wait a tick for one.
    pub fn update_within(&mut self, screen: &Screen, room: usize) -> Vec<Message> {
        if room == 0 {
            return Vec::new();
        }
        let geometry_changed = self
            .sent
            .as_ref()
            .is_none_or(|s| s.cols != screen.cols || s.rows != screen.rows);
        let full = geometry_changed;
        // A geometry change ends an open frame the way a reset does: the `Profile` is the first
        // message of the new one, and reusing the abandoned frame's epoch unpacks its rows on the
        // phone under the new geometry. A closed frame needs nothing here (the epoch moves when the
        // next frame opens), which is what keeps a reset from counting twice.
        if geometry_changed {
            if self.frame_open {
                self.epoch = self.epoch.wrapping_add(1);
            }
            self.frame_open = false;
        }
        // Profile + one `Rows` + `Commit`, the least a geometry change can be said in.
        if full && room < 3 {
            return Vec::new();
        }
        let mut out = Vec::new();
        if geometry_changed {
            out.push(Message::Profile {
                version: self.version,
                cols: screen.cols,
                rows: screen.rows,
                // A version-3 phone sends `Resize`, so telling it that resize is unsupported
                // would contradict the message it is already sending.
                unsupported: if self.version >= RESIZE_VERSION {
                    unsupported::ALL_V1 & !unsupported::RESIZE
                } else {
                    unsupported::ALL_V1
                },
                shell: self.shell.clone(),
            });
        }
        let mut flat: Vec<Patch> = Vec::new();
        for (y, line) in screen.lines.iter().enumerate() {
            let unchanged = !full
                && self
                    .sent
                    .as_ref()
                    .is_some_and(|s| s.lines.get(y) == Some(line));
            if unchanged || (full && line.is_empty()) {
                continue;
            }
            for (i, seg) in segments(line).into_iter().enumerate() {
                flat.push(Patch {
                    y: y as u8,
                    append: i > 0,
                    text: seg.to_owned(),
                });
            }
        }
        // The row the cursor is on goes first.
        //
        // A frame that does not fit one window is carried over several ticks, and the last batch
        // is seconds behind the first on this link. In a shell the cursor sits on the line being
        // typed, so sending rows in screen order put the echo last: the cursor moved (its
        // `Commit` is cheap) and the letters arrived a batch later. The row the user is writing
        // on is the one worth the airtime first; every other row is equally urgent and equally
        // delayed.
        if flat.len() > 1 {
            let cursor_row = screen.cursor_y;
            flat.sort_by_key(|p| u8::from(p.y != cursor_row));
        }
        let flags = (u8::from(screen.alternate) * commit::ALTERNATE)
            | (u8::from(screen.dead) * commit::DEAD);
        // A scroll with identical content still has to reach the phone: the rows leaving the top
        // are history the phone already holds, and a commit that does not say so loses them.
        let line_changed = self
            .sent
            .as_ref()
            .is_some_and(|s| s.first_line != screen.first_line);
        let cursor_or_flags_changed = self.sent.as_ref().is_none_or(|s| {
            s.cursor_x != screen.cursor_x
                || s.cursor_y != screen.cursor_y
                || s.alternate != screen.alternate
                || s.dead != screen.dead
                || s.first_line != screen.first_line
        });
        if !self.frame_open && flat.is_empty() && !full && !cursor_or_flags_changed && !line_changed
        {
            return Vec::new();
        }
        if !self.frame_open {
            self.epoch = self.epoch.wrapping_add(1);
        }
        let epoch = self.epoch;
        // Pack patches into `Rows` messages of at most MAX_ROWS_BODY bytes. A full screen with
        // no text still sends one empty `Rows(full)`, so the phone clears.
        let mut packed: Vec<Message> = Vec::new();
        {
            let mut cur: Vec<Patch> = Vec::new();
            let mut size = 3usize;
            let mut first = true;
            let flush = |cur: &mut Vec<Patch>, first: &mut bool, out: &mut Vec<Message>| {
                if cur.is_empty() && !(*first && full) {
                    return;
                }
                out.push(Message::Rows {
                    epoch,
                    full: full && *first,
                    patches: std::mem::take(cur),
                });
                *first = false;
            };
            for p in flat {
                let need = 3 + p.text.len();
                if size + need > MAX_ROWS_BODY {
                    flush(&mut cur, &mut first, &mut packed);
                    size = 3;
                }
                size += need;
                cur.push(p);
            }
            flush(&mut cur, &mut first, &mut packed);
        }
        // One message is held back for the `Commit`: it carries the cursor and the flags, and a
        // frame that cannot include it must leave the old cursor in the record so the next tick
        // sends the change rather than forgetting it.
        let room_for_rows = room.saturating_sub(out.len() + 1);
        let packed_len = packed.len();
        let take = room_for_rows.min(packed_len);
        // The `Commit` goes with the rows, not after all of them: the phone applies a `Rows`
        // only when its `Commit` arrives, so rows sent without one are discarded when the next
        // epoch's rows arrive - which is how a long frame used to disappear row by row.
        //
        // It goes only when the frame is **whole**. A commit after a partial frame moves the
        // cursor over text the phone has not been given yet, which the user sees as the cursor
        // running ahead of their typing: the letters arrive a second later, in the next batch.
        // Rows without a commit wait on the phone, so holding the cursor back loses nothing.
        // One byte left for the commit message's own header.
        let commit = out.len() + take < room && take == packed_len;
        self.frame_open = !commit;
        // What the phone will now hold, row by row, from the patches that actually went out.
        let mut carried: Vec<Option<String>> = vec![None; screen.lines.len()];
        for m in packed.iter().take(take) {
            if let Message::Rows { patches, .. } = m {
                for p in patches {
                    let slot = carried
                        .get_mut(usize::from(p.y))
                        .expect("a patch names a row of this screen");
                    slot.get_or_insert_with(String::new).push_str(&p.text);
                }
            }
        }
        let mut snapshot = if full {
            screen.clone()
        } else {
            self.sent.clone().unwrap_or_else(|| screen.clone())
        };
        snapshot.cols = screen.cols;
        snapshot.rows = screen.rows;
        // A taller pane than the record has: the rows past the old end are blank on the phone
        // until their text is carried, and the record has to have them to say so.
        snapshot.lines.resize(screen.lines.len(), String::new());
        for y in 0..screen.lines.len() {
            match carried.get(y) {
                Some(Some(prefix)) => {
                    if let Some(slot) = snapshot.lines.get_mut(y) {
                        *slot = prefix.clone();
                    }
                }
                // A full screen clears the pane first, so a row that was not carried is blank on
                // the phone even where the host has text.
                _ if full => {
                    if let Some(slot) = snapshot.lines.get_mut(y) {
                        slot.clear();
                    }
                }
                _ => {}
            }
        }
        if commit {
            snapshot.cursor_x = screen.cursor_x;
            snapshot.cursor_y = screen.cursor_y;
            snapshot.alternate = screen.alternate;
            snapshot.dead = screen.dead;
            // **And the line number, which the `Commit` carries.** It was left out of the record,
            // so once the pane's first line differed from what the phone held, `up_to_date` was
            // false for ever and the host sent a `Commit` on every tick — measured 2026-11-06 as
            // "view 1 behind, 2 behind, 3 behind…" climbing about once a second on an idle shell,
            // with `show` clearing the count and it climbing again. A `Commit` that says the line
            // number has to be recorded as having said it.
            snapshot.first_line = screen.first_line;
        }
        out.extend(packed.into_iter().take(take));
        if commit {
            out.push(Message::Commit {
                epoch,
                cursor_x: screen.cursor_x,
                cursor_y: screen.cursor_y,
                flags,
                // Only a version 5 phone asked for line identity; an older one would read four
                // bytes it does not expect as the start of the next record.
                first_line: if self.version >= STREAM_LINE_VERSION {
                    screen.first_line
                } else {
                    0
                },
            });
        }
        // Nothing went out: the record must not move, or the change would be forgotten.
        if out.is_empty() {
            self.frame_open = packed_len > 0;
            return Vec::new();
        }
        self.sent = Some(snapshot);
        out
    }
}

/// The phone's side, as a reference implementation: what a client does with the messages. The
/// Kotlin port is checked against the same vectors.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mirror {
    pub profile: Option<(u8, u8, u16)>,
    /// The shell the host named in its `Profile` (empty from a version-1 host).
    pub shell: String,
    pub lines: Vec<String>,
    pub cursor: (u8, u8),
    pub flags: u8,
    pub epoch: u16,
    /// The absolute line number of row 0, from the last `Commit` (version 5; 0 when the host has
    /// no line identity, which is every host older than 5).
    pub first_line: u32,
    /// `Rows` held until their `Commit`.
    pending: Option<(u16, bool, Vec<Patch>)>,
    /// A `Closed` reason, once the host ended the attach.
    pub closed: Option<u8>,
}

impl Mirror {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a host→phone message. Returns `true` when the visible screen changed (a `Commit`
    /// was applied).
    pub fn apply(&mut self, m: &Message) -> bool {
        match m {
            Message::Profile {
                cols,
                rows,
                unsupported,
                shell,
                ..
            } => {
                self.profile = Some((*cols, *rows, *unsupported));
                self.shell = shell.clone();
                self.lines = vec![String::new(); usize::from(*rows)];
                // **A profile ends whatever frame was open.** A `Rows` for an epoch that arrived
                // before it belongs to the geometry being replaced, and painting it over the new
                // pane is how stale text landed on a row the host says is blank.
                self.pending = None;
                false
            }
            Message::Rows {
                epoch,
                full,
                patches,
            } => match &mut self.pending {
                Some((e, f, p)) if *e == *epoch => {
                    *f |= *full;
                    p.extend(patches.iter().cloned());
                    false
                }
                _ => {
                    self.pending = Some((*epoch, *full, patches.clone()));
                    false
                }
            },
            Message::Commit {
                epoch,
                cursor_x,
                cursor_y,
                flags,
                first_line,
            } => {
                let (full, patches) = match self.pending.take() {
                    Some((e, f, p)) if e == *epoch => (f, p),
                    _ => (false, Vec::new()),
                };
                if full {
                    for l in &mut self.lines {
                        l.clear();
                    }
                }
                for p in patches {
                    let Some(line) = self.lines.get_mut(usize::from(p.y)) else {
                        continue;
                    };
                    if p.append {
                        line.push_str(&p.text);
                    } else {
                        line.clone_from(&p.text);
                    }
                }
                self.cursor = (*cursor_x, *cursor_y);
                self.flags = *flags;
                self.epoch = *epoch;
                self.first_line = *first_line;
                true
            }
            Message::Closed { reason } => {
                self.closed = Some(*reason);
                false
            }
            _ => false,
        }
    }
}

/// Records needed to carry `messages` at [`RECORD_PAYLOAD`] bytes each (the stream is split
/// across records by the ARQ, so this is ceil(total / 144)).
#[must_use]
pub fn records_for(messages: &[Message]) -> usize {
    let total: usize = messages
        .iter()
        .filter_map(|m| m.encode().ok())
        .map(|b| b.len())
        .sum();
    total.div_ceil(RECORD_PAYLOAD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str], cols: u8) -> Screen {
        Screen {
            cols,
            rows: lines.len() as u8,
            cursor_x: 2,
            cursor_y: 0,
            alternate: false,
            dead: false,
            first_line: 0,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// **A commit that says the line number must be recorded as having said it.**
    ///
    /// The record of what the phone holds left `first_line` out, so once the pane's top line moved
    /// (a scroll, a resize, history appearing) the differ believed the phone was out of date for
    /// ever and sent a `Commit` on every tick. Measured 2026-11-06 on an idle shell: the phone's bar
    /// counted "view 1 behind, 2 behind, 3 behind" about once a second, and `show` cleared it only
    /// for the next tick to add one.
    #[test]
    fn a_line_number_change_is_sent_once_and_then_remembered() {
        let mut d = Differ::for_attach(STREAM_LINE_VERSION, "abcd-1");
        let first = screen(&["$", ""], 10);
        assert!(!d.update(&first).is_empty(), "the first screen goes out");
        assert!(d.up_to_date(&first), "and is recorded");
        // The same text, but the pane now says its top row is line 7.
        let mut scrolled = first.clone();
        scrolled.first_line = 7;
        let sent = d.update(&scrolled);
        assert!(!sent.is_empty(), "the new line number has to reach the phone");
        assert!(
            d.up_to_date(&scrolled),
            "and must not be asked for again on the next tick"
        );
        assert!(
            d.update(&scrolled).is_empty(),
            "an unchanged screen produces no messages at all"
        );
    }

    /// A phone on version 5 is told the absolute line at the top of the screen, which is the one
    /// fact it needs to move rows into a local history. Guessing by matching content is what makes
    /// mosh's history unreliable: an in-place rewrite of identical text is indistinguishable from
    /// a scroll.
    #[test]
    fn a_current_phone_is_told_which_line_is_at_the_top() {
        fn deliver(d: &mut Differ, m: &mut Mirror, target: &Screen) {
            let mut dec = Decoder::new();
            for msg in d.update(target) {
                for item in dec.push(&msg.encode().unwrap()).unwrap() {
                    m.apply(&item.unwrap());
                }
            }
        }

        // Real rows, so the first update actually carries something and commits.
        let rows = ["$ one", "two", "", ""];
        let refs: Vec<&str> = rows.to_vec();
        let mut at_attach = screen(&refs, 20);
        at_attach.first_line = 7;

        let mut d = Differ::for_attach(VERSION, "ab12-1");
        let mut m = Mirror::new();
        deliver(&mut d, &mut m, &at_attach);
        assert_eq!(m.first_line, 7, "the phone knows where the screen starts");

        // The shell prints three lines: the screen scrolls by exactly three.
        let mut scrolled = screen(&refs, 20);
        scrolled.first_line = 10;
        deliver(&mut d, &mut m, &scrolled);
        assert_eq!(m.first_line, 10, "three rows left the top into history");

        // A version 4 phone is told nothing at all, and is offered no scrollback: reading four
        // bytes it does not expect would break the stream.
        let mut old = Differ::for_attach(4, "ab12-1");
        let first = old.update(&scrolled);
        assert!(
            first.iter().all(|msg| !matches!(msg, Message::Commit { first_line, .. } if *first_line != 0)),
            "a version 4 commit carries no line number"
        );
    }

    /// A frame spread over several calls is **one** frame: every batch carries the same epoch,
    /// and only the batch that completes it carries the `Commit`.
    ///
    /// A commit after a partial frame moves the cursor over text the phone has not been given,
    /// which reads as the cursor running ahead of the typing. A new epoch per batch is worse
    /// still: the phone drops rows from an older epoch when the next epoch's rows arrive, so the
    /// screen arrives as its last rows only.
    #[test]
    fn a_split_frame_keeps_one_epoch_and_commits_only_when_whole() {
        let lines: Vec<String> = (0..30).map(|i| format!("row {i} with some words")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let target = screen(&refs, 40);
        let mut d = Differ::for_attach(VERSION, "ab12-1");

        let mut epochs = std::collections::BTreeSet::new();
        let mut commits = 0;
        let mut rows_seen = 0;
        for turn in 0..40 {
            let out = d.update_within(&target, 3);
            if out.is_empty() {
                break;
            }
            for m in &out {
                match m {
                    Message::Rows { epoch, patches, .. } => {
                        epochs.insert(*epoch);
                        rows_seen += patches.len();
                    }
                    Message::Commit { .. } => commits += 1,
                    _ => {}
                }
            }
            assert!(turn < 39, "it must finish");
        }
        assert_eq!(commits, 1, "one frame, one commit");
        assert_eq!(epochs.len(), 1, "every batch belongs to the same frame");
        assert_eq!(rows_seen, 30, "every row is carried once");
    }

    /// **The idle marker is the host saying "nothing more is coming".** It exists for exactly one
    /// case: a pane the phone already holds, so it carries no rows and one flag. Before anything
    /// has been sent there is nothing to mark, and a frame still being carried over several ticks
    /// is not settled yet, so neither may produce one.
    #[test]
    fn the_idle_marker_exists_only_for_a_settled_frame() {
        let target = screen(&["one", "two"], 20);
        let mut d = Differ::for_attach(VERSION, "ab12-1");
        assert!(d.idle_marker(&target).is_none(), "nothing has been sent yet");

        let out = d.update_within(&target, 8);
        assert!(!out.is_empty(), "the first frame goes out");
        let marker = d.idle_marker(&target).expect("a settled frame can be marked");
        match marker {
            Message::Commit {
                epoch,
                flags,
                cursor_x,
                cursor_y,
                ..
            } => {
                assert_eq!(flags & commit::IDLE, commit::IDLE, "the flag is the message");
                assert_eq!(flags & commit::ALTERNATE, 0);
                let last_epoch = out.iter().find_map(|m| match m {
                    Message::Commit { epoch, .. } => Some(*epoch),
                    _ => None,
                });
                assert_eq!(Some(epoch), last_epoch, "the marker is for the frame already held");
                // The cursor is the screen's own, not a default: the marker must not move it.
                assert_eq!((cursor_x, cursor_y), (2, 0));
            }
            other => panic!("expected a commit, got {other:?}"),
        }

        // A bigger frame than one call can carry is not settled: no marker until its commit goes.
        let lines: Vec<String> = (0..20).map(|i| format!("row {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let big = screen(&refs, 40);
        let mut split = Differ::for_attach(VERSION, "ab12-1");
        let first = split.update_within(&big, 3);
        assert!(!first.is_empty());
        assert!(
            split.idle_marker(&big).is_none() || first.iter().any(|m| matches!(m, Message::Commit { .. })),
            "a frame still being carried is not marked idle"
        );
    }

    /// A frame bigger than the window is spread over calls and loses nothing: the record of
    /// what the phone holds advances per row, so the rows that did not fit come back.
    #[test]
    fn a_bounded_update_spreads_a_big_frame_and_loses_no_row() {
        fn pump(d: &mut Differ, target: &Screen, m: &mut Mirror) -> usize {
            let mut dec = Decoder::new();
            let mut turns = 0;
            loop {
                let msgs = d.update_within(target, 3);
                if msgs.is_empty() {
                    break;
                }
                turns += 1;
                assert!(msgs.len() <= 3, "never more than the window allows");
                assert!(turns < 60, "it must converge");
                for msg in msgs {
                    for item in dec.push(&msg.encode().unwrap()).unwrap() {
                        m.apply(&item.unwrap());
                    }
                }
            }
            turns
        }

        let mut d = Differ::for_attach(VERSION, "abcd");
        let mut m = Mirror::new();
        let blank: Vec<String> = (0..20).map(|_| String::new()).collect();
        let refs: Vec<&str> = blank.iter().map(String::as_str).collect();
        let empty = screen(&refs, 80);
        assert!(pump(&mut d, &empty, &mut m) >= 1);
        assert_eq!(m.lines, empty.lines, "a blank screen arrives whole");

        let lines: Vec<String> = (0..60)
            .map(|i| format!("row {i} with some text on it"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let target = screen(&refs, 80);
        let turns = pump(&mut d, &target, &mut m);
        assert!(turns > 1, "sixty rows do not fit three messages");
        assert_eq!(
            m.lines, target.lines,
            "every row arrives, none lost or stale"
        );
        assert_eq!(m.cursor, (target.cursor_x, target.cursor_y));
    }

    /// The cursor is only claimed when its `Commit` was sent; otherwise the change stays owed
    /// rather than being forgotten.
    #[test]
    fn a_frame_without_its_commit_leaves_the_cursor_pending() {
        let mut d = Differ::for_attach(VERSION, "abcd");
        let one = screen(&["$"], 80);
        let mut dec = Decoder::new();
        let mut m = Mirror::new();
        for _ in 0..10 {
            let msgs = d.update_within(&one, 8);
            if msgs.is_empty() {
                break;
            }
            for msg in msgs {
                for item in dec.push(&msg.encode().unwrap()).unwrap() {
                    m.apply(&item.unwrap());
                }
            }
        }
        let mut moved = one.clone();
        moved.cursor_x = 7;
        moved.lines[0] = "$ typed".to_owned();
        let tight = d.update_within(&moved, 1);
        assert!(
            !tight
                .iter()
                .any(|msg| matches!(msg, Message::Commit { .. })),
            "one message cannot be both the rows and the commit"
        );
        let again = d.update_within(&moved, 2);
        assert!(
            again
                .iter()
                .any(|msg| matches!(msg, Message::Commit { .. })),
            "the owed cursor is sent once there is room"
        );
    }

    fn roundtrip(m: &Message) {
        let bytes = m.encode().unwrap();
        let mut d = Decoder::new();
        let got = d.push(&bytes).unwrap();
        assert_eq!(got, vec![Ok(m.clone())]);
    }

    #[test]
    fn every_message_round_trips() {
        roundtrip(&Message::Attach {
            version: 1,
            selector: "ab12".into(),
        });
        roundtrip(&Message::Attach {
            version: 1,
            selector: String::new(),
        });
        roundtrip(&Message::Input(b"ls\r".to_vec()));
        // Version 7: the phone starts and ends shells, and learns the id of the one it started.
        roundtrip(&Message::Create {
            version: SHELL_CONTROL_VERSION,
            cwd: "~/Documents".into(),
        });
        roundtrip(&Message::Create {
            version: SHELL_CONTROL_VERSION,
            cwd: String::new(),
        });
        roundtrip(&Message::Close {
            selector: "ab12".into(),
        });
        roundtrip(&Message::Close {
            selector: String::new(),
        });
        roundtrip(&Message::MakeLive {
            selector: "ab12".into(),
        });
        roundtrip(&Message::Created {
            id: "ab12".into(),
        });
        roundtrip(&Message::Resync);
        roundtrip(&Message::Detach);
        roundtrip(&Message::Profile {
            version: 1,
            cols: 80,
            rows: 24,
            unsupported: unsupported::ALL_V1,
            shell: String::new(),
        });
        roundtrip(&Message::Profile {
            version: 2,
            cols: 80,
            rows: 24,
            unsupported: unsupported::ALL_V1,
            shell: "ab12-1".into(),
        });
        roundtrip(&Message::Key(vec!["Up".into()]));
        roundtrip(&Message::Key(vec![
            "C-c".into(),
            "M-C-S-Left".into(),
            "Enter".into(),
        ]));
        roundtrip(&Message::Rows {
            epoch: 0xBEEF,
            full: true,
            patches: vec![
                Patch {
                    y: 0,
                    append: false,
                    text: "héllo".into(),
                },
                Patch {
                    y: 255,
                    append: true,
                    text: String::new(),
                },
            ],
        });
        roundtrip(&Message::Commit {
            first_line: 0,
            epoch: 7,
            cursor_x: 3,
            cursor_y: 4,
            flags: 3,
        });
        roundtrip(&Message::Closed {
            reason: closed::EXITED,
        });
    }

    fn sh(n: usize) -> Vec<(String, String, String, String, String)> {
        (0..n)
            .map(|i| {
                (
                    format!("{i:02x}{}", "f".repeat(30)),
                    format!("shell number {i} long label"),
                    format!("~/projects/shell-{i}"),
                    "phone".to_string(),
                    String::new(),
                )
            })
            .collect()
    }

    #[test]
    fn list_and_sessions_have_exact_bytes() {
        assert_eq!(Message::List.encode().unwrap(), [0x05, 0x00]);
        // no shells: one page, `last`, no entries
        assert_eq!(
            session_pages(&[], false, false, false),
            vec![Message::Sessions {
                last: true,
                cwd: false,
                origin: false,
                owner: false,
                entries: vec![]
            }]
        );
        assert_eq!(
            session_pages(&[], false, false, false).remove(0).encode().unwrap(),
            [0x85, 0x01, 0x01]
        );
        // The same page for a version-6 phone: the cwd flag is set, and there are still no entries.
        assert_eq!(
            session_pages(&[], true, false, false).remove(0).encode().unwrap(),
            [0x85, 0x01, 0x03]
        );
        let one = Message::Sessions {
            last: true,
            cwd: false,
            origin: false,
            owner: false,
            entries: vec![SessionEntry {
                id: "ab".into(),
                label: "x".into(),
                cwd: String::new(),
                        origin: String::new(),
                owner: String::new(),
    }],
        };
        // flags 1, id_len 2 "ab", label_len 1 "x" (no cwd: version 5 form)
        assert_eq!(
            one.encode().unwrap(),
            [0x85, 0x06, 0x01, 0x02, b'a', b'b', 0x01, b'x']
        );
        let one_with_cwd = Message::Sessions {
            last: true,
            cwd: true,
            origin: false,
            owner: false,
            entries: vec![SessionEntry {
                id: "ab".into(),
                label: "x".into(),
                cwd: "~/".into(),
                        origin: String::new(),
                owner: String::new(),
    }],
        };
        // flags 3, id 2 "ab", label 1 "x", cwd 2 "~/"
        assert_eq!(
            one_with_cwd.encode().unwrap(),
            [0x85, 0x09, 0x03, 0x02, b'a', b'b', 0x01, b'x', 0x02, b'~', b'/']
        );
        // Version 7: the same entry with its origin, and the flags byte says which fields follow.
        let one_with_origin = Message::Sessions {
            last: true,
            cwd: true,
            origin: true,
            owner: false,
            entries: vec![SessionEntry {
                id: "ab".into(),
                label: "x".into(),
                cwd: "~/".into(),
                origin: "phone".into(),
                        owner: String::new(),
        }],
        };
        // flags 7 (last|cwd|origin), id 2 "ab", label 1 "x", cwd 2 "~/", origin one byte (1 = phone)
        assert_eq!(
            one_with_origin.encode().unwrap(),
            [0x85, 0x0a, 0x07, 0x02, b'a', b'b', 0x01, b'x', 0x02, b'~', b'/', 0x01]
        );
        // A version-7 page with no shells still carries the origin bit, so the phone knows the
        // entries would have said who started them.
        assert_eq!(
            session_pages(&[], false, true, false).remove(0).encode().unwrap(),
            [0x85, 0x01, 0x05]
        );
        assert_eq!(
            Message::Created { id: "ab".into() }.encode().unwrap(),
            [0x86, 0x03, 0x02, b'a', b'b']
        );
        assert_eq!(
            Message::Close { selector: "ab".into() }.encode().unwrap(),
            [0x09, 0x02, b'a', b'b']
        );
        roundtrip(&Message::List);
        roundtrip(&one);
        roundtrip(&one_with_cwd);
        roundtrip(&one_with_origin);
    }

    #[test]
    fn sessions_pages_fit_one_record_and_say_which_is_last() {
        for with_cwd in [false, true] {
            for n in [0usize, 1, 2, 3, 7, MAX_SESSIONS, MAX_SESSIONS + 5] {
                let pages = session_pages(&sh(n), with_cwd, with_cwd, with_cwd);
                let mut total = 0;
                for (i, m) in pages.iter().enumerate() {
                    let bytes = m.encode().unwrap();
                    assert!(
                        bytes.len() <= MAX_MESSAGE,
                        "cwd={with_cwd} n={n} page {i} is {} bytes",
                        bytes.len()
                    );
                    assert!(bytes.len() <= RECORD_PAYLOAD);
                    let Message::Sessions { last, cwd, entries, .. } = m else {
                        panic!()
                    };
                    assert_eq!(*last, i + 1 == pages.len(), "only the final page is last");
                    assert_eq!(*cwd, with_cwd, "the cwd flag is the page's own");
                    total += entries.len();
                }
                assert_eq!(
                    total,
                    n.min(MAX_SESSIONS),
                    "cwd={with_cwd} n={n}: nothing lost, nothing past the cap"
                );
            }
            assert!(
                session_pages(&sh(7), with_cwd, with_cwd, with_cwd).len() > 1,
                "a long list really does span records"
            );
        }
    }

    #[test]
    fn sessions_survive_any_chunking_and_cut_at_char_boundaries() {
        let list = vec![(
            "é".repeat(30),
            "漢".repeat(20),
            "~/漢".repeat(20),
            "phone".to_string(),
            String::new(),
        )]; // 60-byte id, 60-byte label, 120-byte path
        let pages = session_pages(&list, true, true, true);
        let mut stream = Vec::new();
        for m in &pages {
            stream.extend(m.encode().unwrap());
        }
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for b in &stream {
            for item in d.push(&[*b]).unwrap() {
                got.push(item.unwrap());
            }
        }
        assert_eq!(got, pages);
        let Message::Sessions { entries, .. } = &got[0] else {
            panic!()
        };
        assert!(entries[0].id.len() <= MAX_SELECTOR && entries[0].label.len() <= MAX_LABEL);
        assert!(entries[0].cwd.len() <= MAX_CWD);
        assert!(
            entries[0].id.chars().all(|c| c == 'é'),
            "never half a character"
        );
        assert!(
            entries[0].cwd.chars().all(|c| c == '~' || c == '/' || c == '漢'),
            "a path is cut at a character boundary too"
        );
    }

    #[test]
    fn oversize_or_broken_sessions_are_refused() {
        let bad = Message::Sessions {
            last: true,
            cwd: false,
            origin: false,
            owner: false,
            entries: vec![SessionEntry {
                id: "x".repeat(MAX_SELECTOR + 1),
                label: String::new(),
                cwd: String::new(),
                        origin: String::new(),
                owner: String::new(),
    }],
        };
        assert_eq!(bad.encode(), Err(EncodeError::TooLong));
        let bad_cwd = Message::Sessions {
            last: true,
            cwd: true,
            origin: false,
            owner: false,
            entries: vec![SessionEntry {
                id: "a".into(),
                label: String::new(),
                cwd: "p".repeat(MAX_CWD + 1),
                        origin: String::new(),
                owner: String::new(),
    }],
        };
        assert_eq!(bad_cwd.encode(), Err(EncodeError::TooLong));
        // entry claims 5 bytes of id but only 1 follows
        let mut d = Decoder::new();
        let out = d.push(&[0x85, 0x03, 0x01, 0x05, b'a']).unwrap();
        assert_eq!(out, vec![Err(DecodeError::Malformed(0x85))]);
        // a `List` with a body is malformed
        let mut d = Decoder::new();
        assert_eq!(
            d.push(&[0x05, 0x01, 0x00]).unwrap(),
            vec![Err(DecodeError::Malformed(0x05))]
        );
    }

    // ---- version 3: `Resize` -------------------------------------------------------------------

    #[test]
    fn resize_is_two_bytes_of_cells_and_zero_is_refused_both_ways() {
        assert_eq!(
            Message::Resize { cols: 45, rows: 30 }.encode().unwrap(),
            [0x07, 0x02, 45, 30]
        );
        assert_eq!(
            Message::Resize { cols: 0, rows: 30 }.encode(),
            Err(EncodeError::Empty)
        );
        assert_eq!(
            Message::Resize { cols: 45, rows: 0 }.encode(),
            Err(EncodeError::Empty)
        );
        for bad in [
            &[0x07, 0x02, 0, 30][..],
            &[0x07, 0x02, 45, 0],
            &[0x07, 0x01, 45],
            &[0x07, 0x03, 45, 30, 1],
        ] {
            let mut d = Decoder::new();
            assert_eq!(
                d.push(bad).unwrap(),
                vec![Err(DecodeError::Malformed(0x07))],
                "{bad:?}"
            );
        }
        let mut d = Decoder::new();
        assert_eq!(
            d.push(&[0x07, 0x02, 45, 30]).unwrap(),
            vec![Ok(Message::Resize { cols: 45, rows: 30 })]
        );
    }

    #[test]
    fn a_version_three_profile_names_its_shell_like_version_two() {
        let screen = Screen::from_keeper(&"\n".repeat(29), 45, 30, (0, 0), false, false, 0);
        let first = Differ::for_attach(3, "ab12-1").update(&screen);
        assert!(matches!(
            &first[0],
            Message::Profile { version: 3, cols: 45, rows: 30, shell, .. } if shell == "ab12-1"
        ));
        roundtrip(&first[0]);
    }

    // ---- version 2: `Key` and the shell id in `Profile` --------------------------------------

    #[test]
    fn key_names_are_one_space_separated_body() {
        assert_eq!(
            Message::Key(vec!["Up".into(), "C-c".into()])
                .encode()
                .unwrap(),
            [&[0x06, 0x06][..], b"Up C-c"].concat()
        );
    }

    #[test]
    fn a_key_with_a_space_too_many_names_or_too_long_a_body_is_refused() {
        // Encode refuses all three; a space inside a name would split into two names on the host.
        assert_eq!(
            Message::Key(vec!["C c".into()]).encode(),
            Err(EncodeError::TooLong)
        );
        assert_eq!(Message::Key(Vec::new()).encode(), Err(EncodeError::Empty));
        assert_eq!(
            Message::Key(vec!["Up".into(); MAX_KEYS + 1]).encode(),
            Err(EncodeError::TooLong)
        );
        assert_eq!(
            Message::Key(vec!["M-C-S-Left".into(); 10]).encode(),
            Err(EncodeError::TooLong),
            "ten of eleven bytes plus separators is over the 96-byte cap"
        );
        // Decode refuses the same, dropping only that message.
        let mut d = Decoder::new();
        for body in [&b"C c "[..], b"", b"Up  Down", b"Up\x01"] {
            let mut wire = vec![0x06, body.len() as u8];
            wire.extend_from_slice(body);
            let out = d.push(&wire).unwrap();
            assert_eq!(out, vec![Err(DecodeError::Malformed(0x06))], "{body:?}");
        }
        let many = vec!["a"; MAX_KEYS + 1].join(" ");
        let mut wire = vec![0x06, many.len() as u8];
        wire.extend_from_slice(many.as_bytes());
        assert_eq!(
            d.push(&wire).unwrap(),
            vec![Err(DecodeError::Malformed(0x06))]
        );
        let long = vec!["x"; 60].join(" ");
        let mut wire = vec![0x06];
        put_len(&mut wire, long.len());
        wire.extend_from_slice(long.as_bytes());
        assert_eq!(
            d.push(&wire).unwrap(),
            vec![Err(DecodeError::Malformed(0x06))]
        );
    }

    #[test]
    fn a_version_one_profile_decodes_with_no_shell_and_version_two_carries_it() {
        let mut d = Decoder::new();
        // The five-byte body a version-1 host writes: still decodes, never an error.
        let v1 = d.push(&[0x81, 0x05, 0x01, 0x50, 0x18, 0x7F, 0x00]).unwrap();
        assert_eq!(
            v1,
            vec![Ok(Message::Profile {
                version: 1,
                cols: 80,
                rows: 24,
                unsupported: 0x7F,
                shell: String::new(),
            })]
        );
        let v2 = Message::Profile {
            version: 2,
            cols: 80,
            rows: 24,
            unsupported: 0x7F,
            shell: "ab12-1".into(),
        };
        let wire = v2.encode().unwrap();
        assert_eq!(
            &wire[..8],
            &[0x81, 0x0C, 0x02, 0x50, 0x18, 0x7F, 0x00, 0x06]
        );
        assert_eq!(d.push(&wire).unwrap(), vec![Ok(v2)]);
        // A version-1 Profile never carries a shell on the wire, whatever the struct holds.
        let v1_with_shell = Message::Profile {
            version: 1,
            cols: 80,
            rows: 24,
            unsupported: 0x7F,
            shell: "ignored".into(),
        };
        assert_eq!(v1_with_shell.encode().unwrap().len(), 7);
        // A version-2 body whose shell length lies is malformed, not skipped silently.
        assert_eq!(
            d.push(&[0x81, 0x07, 0x02, 0x50, 0x18, 0x7F, 0x00, 0x09, b'a'])
                .unwrap(),
            vec![Err(DecodeError::Malformed(0x81))]
        );
    }

    #[test]
    fn a_differ_names_its_shell_in_the_attach_version() {
        let screen = Screen {
            cols: 80,
            rows: 2,
            lines: vec!["$".into(), String::new()],
            ..Screen::default()
        };
        let v2 = Differ::for_attach(2, "ab12-1").update(&screen);
        assert!(matches!(&v2[0], Message::Profile { version: 2, shell, .. } if shell == "ab12-1"));
        let v1 = Differ::for_attach(1, "ab12-1").update(&screen);
        assert!(matches!(&v1[0], Message::Profile { version: 1, .. }));
        // What a version-1 phone receives is exactly the five-byte body it can decode.
        assert_eq!(v1[0].encode().unwrap().len(), 7);
        let mut m = Mirror::new();
        for msg in &v2 {
            m.apply(msg);
        }
        assert_eq!(m.shell, "ab12-1");
    }

    #[test]
    fn exact_bytes_are_pinned() {
        // These are the vectors the Kotlin port must reproduce.
        assert_eq!(Message::Resync.encode().unwrap(), [0x03, 0x00]);
        assert_eq!(
            Message::Profile {
                version: 1,
                cols: 80,
                rows: 24,
                unsupported: 0x007F,
                shell: String::new(),
            }
            .encode()
            .unwrap(),
            [0x81, 0x05, 0x01, 0x50, 0x18, 0x7F, 0x00]
        );
        assert_eq!(
            Message::Commit {
                epoch: 0x0102,
                first_line: 0,
                cursor_x: 5,
                cursor_y: 6,
                flags: 1
            }
            .encode()
            .unwrap(),
            [0x83, 0x05, 0x02, 0x01, 0x05, 0x06, 0x01]
        );
        assert_eq!(
            Message::Rows {
                epoch: 1,
                full: true,
                patches: vec![Patch {
                    y: 2,
                    append: false,
                    text: "ok".into()
                }]
            }
            .encode()
            .unwrap(),
            [0x82, 0x08, 0x01, 0x00, 0x01, 0x02, 0x00, 0x02, b'o', b'k']
        );
        assert_eq!(
            Message::Input(b"\x03".to_vec()).encode().unwrap(),
            [0x02, 0x01, 0x03]
        );
    }

    #[test]
    fn a_stream_split_anywhere_decodes_the_same() {
        let msgs = vec![
            Message::Profile {
                version: 1,
                cols: 80,
                rows: 2,
                unsupported: 0x7f,
                shell: String::new(),
            },
            Message::Rows {
                epoch: 1,
                full: true,
                patches: vec![Patch {
                    y: 0,
                    append: false,
                    text: "$ ls".into(),
                }],
            },
            Message::Commit {
                epoch: 1,
                first_line: 0,
                cursor_x: 4,
                cursor_y: 0,
                flags: 0,
            },
        ];
        let stream: Vec<u8> = msgs.iter().flat_map(|m| m.encode().unwrap()).collect();
        for step in [1usize, 2, 3, 7, 144] {
            let mut d = Decoder::new();
            let mut got = Vec::new();
            for c in stream.chunks(step) {
                got.extend(d.push(c).unwrap().into_iter().map(Result::unwrap));
            }
            assert_eq!(got, msgs, "chunk size {step}");
        }
    }

    #[test]
    fn an_unknown_tag_is_skipped_and_a_bad_body_costs_one_message() {
        let mut stream = vec![0x7e, 0x02, 0xAA, 0xBB]; // unknown tag, 2 bytes
        stream.extend(Message::Resync.encode().unwrap());
        stream.extend([0x84, 0x02, 0x01, 0x01]); // Closed with a 2-byte body: malformed
        stream.extend(Message::Detach.encode().unwrap());
        let got = Decoder::new().push(&stream).unwrap();
        assert_eq!(
            got,
            vec![
                Ok(Message::Resync),
                Err(DecodeError::Malformed(0x84)),
                Ok(Message::Detach)
            ]
        );
    }

    #[test]
    fn oversize_and_overflow_are_refused() {
        // length 301 as LEB128: 0xAD 0x02
        assert_eq!(
            Decoder::new().push(&[0x02, 0xAD, 0x02]),
            Err(DecodeError::BodyTooLong)
        );
        // a three-byte length is not a length
        assert_eq!(
            Decoder::new().push(&[0x02, 0x80, 0x80, 0x01]),
            Err(DecodeError::BodyTooLong)
        );
        // A message is at most MAX_BODY + 3 bytes, so an unfinished one can never fill the
        // buffer; only a single oversized push can.
        assert_eq!(
            Decoder::new().push(&vec![0u8; MAX_BUFFER + 1]),
            Err(DecodeError::Overflow)
        );
    }

    #[test]
    fn encoders_refuse_what_the_wire_cannot_carry() {
        assert_eq!(Message::Input(Vec::new()).encode(), Err(EncodeError::Empty));
        assert_eq!(
            Message::Input(vec![0; MAX_INPUT + 1]).encode(),
            Err(EncodeError::TooLong)
        );
        assert_eq!(
            Message::Attach {
                version: 1,
                selector: "x".repeat(MAX_SELECTOR + 1)
            }
            .encode(),
            Err(EncodeError::TooLong)
        );
    }

    #[test]
    fn plain_row_strips_escapes_and_trailing_space() {
        assert_eq!(
            plain_row("\x1b[1;32muser@host\x1b[0m:~$ \x1b[K   "),
            "user@host:~$"
        );
        assert_eq!(plain_row("\x1b]0;title\x07text"), "text");
        assert_eq!(
            plain_row("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        assert_eq!(plain_row("   "), "");
    }

    #[test]
    fn from_keeper_takes_the_live_rows_when_history_is_above() {
        let text = "old1\nold2\n$ ls\nfile";
        let s = Screen::from_keeper(text, 80, 2, (0, 1), false, false, 0);
        assert_eq!(s.lines, ["$ ls", "file"]);
        let s = Screen::from_keeper("only", 80, 3, (0, 0), false, false, 0);
        assert_eq!(s.lines, ["only", "", ""]);
    }

    #[test]
    fn the_first_update_is_the_whole_screen_and_the_second_only_what_changed() {
        let mut d = Differ::new();
        let mut m = Mirror::new();
        let a = screen(&["$ ", "", "", "bottom"], 80);
        let first = d.update(&a);
        assert!(matches!(first[0], Message::Profile { .. }));
        for x in &first {
            m.apply(x);
        }
        assert_eq!(m.lines, a.lines);
        assert_eq!(m.cursor, (2, 0));

        let mut b = a.clone();
        b.lines[0] = "$ ls".into();
        b.cursor_x = 4;
        let second = d.update(&b);
        // Rows + Commit only: no Profile, no `full`, and not the unchanged "bottom" row.
        assert_eq!(second.len(), 2);
        let Message::Rows { full, patches, .. } = &second[0] else {
            panic!()
        };
        assert!(!full);
        assert_eq!(
            patches,
            &vec![Patch {
                y: 0,
                append: false,
                text: "$ ls".into()
            }]
        );
        for x in &second {
            m.apply(x);
        }
        assert_eq!(m.lines, b.lines);
        assert_eq!(m.cursor, (4, 0));

        assert!(d.update(&b).is_empty(), "nothing changed, nothing sent");
    }

    #[test]
    fn a_cursor_move_alone_is_a_commit_with_no_rows() {
        let mut d = Differ::new();
        let a = screen(&["x", "y"], 10);
        d.update(&a);
        let mut b = a.clone();
        b.cursor_x = 9;
        let u = d.update(&b);
        assert_eq!(u.len(), 1);
        assert!(matches!(u[0], Message::Commit { cursor_x: 9, .. }));
    }

    #[test]
    fn a_long_row_is_segmented_and_reassembled() {
        let long: String = "é".repeat(200); // 400 bytes
        let a = screen(&[&long, "z"], 255);
        let mut d = Differ::new();
        let msgs = d.update(&a);
        let mut m = Mirror::new();
        for x in &msgs {
            assert!(x.encode().unwrap().len() <= MAX_MESSAGE);
            m.apply(x);
        }
        assert_eq!(m.lines[0], long);
        assert_eq!(m.lines[1], "z");
    }

    #[test]
    fn a_resize_sends_a_new_profile_and_a_full_screen() {
        let mut d = Differ::new();
        d.update(&screen(&["a", "b"], 80));
        let u = d.update(&screen(&["a", "b", "c"], 80));
        assert!(matches!(u[0], Message::Profile { rows: 3, .. }));
        assert!(
            u.iter()
                .any(|m| matches!(m, Message::Rows { full: true, .. }))
        );
    }

    #[test]
    fn a_client_that_missed_many_changes_converges_by_one_diff_against_the_present() {
        let mut d = Differ::new();
        let mut m = Mirror::new();
        let mut s = screen(&["", "", "", ""], 40);
        for x in d.update(&s) {
            m.apply(&x);
        }
        // Fifty changes happen while nobody asks. Only the present is ever diffed.
        for i in 0..50 {
            s.lines[i % 4] = format!("line {i}");
        }
        let one = d.update(&s);
        for x in &one {
            m.apply(x);
        }
        assert_eq!(m.lines, s.lines);
        assert!(
            records_for(&one) <= 1,
            "four short rows are one record, not fifty updates"
        );
    }

    #[test]
    fn a_tall_screen_fits_the_record_rules() {
        // 200 rows of 100 characters: the worst case a u8 row count allows at a sane width.
        let lines: Vec<String> = (0..200)
            .map(|i| format!("{i:03} {}", "x".repeat(96)))
            .collect();
        let mut s = Screen {
            cols: 100,
            rows: 200,
            lines,
            ..Screen::default()
        };
        s.cursor_y = 199;
        let msgs = Differ::new().update(&s);
        let mut mirror = Mirror::new();
        for m in &msgs {
            assert!(
                m.encode().unwrap().len() <= RECORD_PAYLOAD,
                "every host message fits one record payload"
            );
            mirror.apply(m);
        }
        assert_eq!(mirror.lines, s.lines);
        // 200 × (3 + 100) = 20,600 bytes of patches: 150 records. That is the point: a screen
        // this full does not fit the hourly budget, and the budget (not this encoding) says so.
        let n = records_for(&msgs);
        assert!(
            n > 58,
            "a full 200x100 screen exceeds an hour of airtime ({n} records)"
        );
    }

    #[test]
    fn an_ordinary_screen_costs_a_handful_of_records() {
        let mut lines = vec![String::new(); 24];
        lines[0] = "user@host:~/proj$ git status".into();
        lines[1] = "On branch main".into();
        lines[2] = "nothing to commit, working tree clean".into();
        lines[3] = "user@host:~/proj$".into();
        let s = Screen {
            cols: 80,
            rows: 24,
            lines,
            cursor_x: 17,
            cursor_y: 3,
            ..Screen::default()
        };
        let n = records_for(&Differ::new().update(&s));
        assert!(n <= 2, "a mostly empty 80x24 screen is {n} records");
    }

    #[test]
    fn rows_for_a_stale_epoch_are_not_applied() {
        let mut m = Mirror::new();
        m.apply(&Message::Profile {
            version: 1,
            cols: 10,
            rows: 1,
            unsupported: 0,
            shell: String::new(),
        });
        m.apply(&Message::Rows {
            epoch: 5,
            full: false,
            patches: vec![Patch {
                y: 0,
                append: false,
                text: "stale".into(),
            }],
        });
        // The Commit is for another epoch: the held rows do not belong to it.
        m.apply(&Message::Commit {
            epoch: 6,
            first_line: 0,
            cursor_x: 0,
            cursor_y: 0,
            flags: 0,
        });
        assert_eq!(m.lines[0], "");
    }

    #[test]
    fn every_host_message_is_within_the_bound_for_any_row_content() {
        // Rows of every length from 0 to 400 bytes, multibyte and not.
        for n in (0..400).step_by(7) {
            for ch in ['a', 'é', '漢'] {
                let line: String = std::iter::repeat_n(ch, n / ch.len_utf8()).collect();
                let s = screen(&[&line, &line, "tail"], 255);
                let msgs = Differ::new().update(&s);
                let mut m = Mirror::new();
                for x in &msgs {
                    assert!(x.encode().unwrap().len() <= MAX_MESSAGE, "{n} {ch}");
                    m.apply(x);
                }
                assert_eq!(m.lines, s.lines);
            }
        }
    }

    // ── the shared fixture ─────────────────────────────────────────────────
    //
    // The Kotlin port in the mobile app reads `test/fixtures/mesh-term-vectors.json`. It is
    // generated here, from the Rust encoder, so the port is checked against bytes it did not
    // produce. This test fails when the file differs from what the encoder makes now; rerun with
    // `LDMUX_REGEN_FIXTURES=1 cargo test -p ldmux-protocol mesh_term_fixture` to rewrite it.

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn message_json(m: &Message) -> serde_json::Value {
        use serde_json::json;
        match m {
            Message::Attach { version, selector } => {
                json!({"kind":"attach","version":version,"selector":selector})
            }
            Message::Input(b) => json!({"kind":"input","bytes":hex(b)}),
            Message::Resync => json!({"kind":"resync"}),
            Message::Detach => json!({"kind":"detach"}),
            Message::Profile {
                version,
                cols,
                rows,
                unsupported,
                shell,
            } => json!({
                "kind":"profile","version":version,"cols":cols,"rows":rows,"unsupported":unsupported,"shell":shell}),
            Message::Key(names) => json!({"kind":"key","names":names}),
            Message::Resize { cols, rows } => json!({"kind":"resize","cols":cols,"rows":rows}),
            Message::Rows {
                epoch,
                full,
                patches,
            } => json!({
                "kind":"rows","epoch":epoch,"full":full,
                "patches": patches.iter().map(|p| json!({"y":p.y,"append":p.append,"text":p.text})).collect::<Vec<_>>()}),
            Message::Commit {
                epoch,
                cursor_x,
                cursor_y,
                flags,
                ..
            } => json!({
                "kind":"commit","epoch":epoch,"cursorX":cursor_x,"cursorY":cursor_y,"flags":flags}),
            Message::Closed { reason } => json!({"kind":"closed","reason":reason}),
            Message::List => json!({"kind":"list"}),
            Message::Sessions { last, cwd, origin, owner, entries } => json!({
                "kind":"sessions","last":last,"cwd":cwd,"origin":origin,"owner":owner,
                "entries": entries.iter().map(|e| json!({"id":e.id,"label":e.label,"cwd":e.cwd,"origin":e.origin,"owner":e.owner})).collect::<Vec<_>>()}),
            Message::Marks { first, last, entries } => json!({
                "kind":"marks","first":first,"last":last,
                "entries": entries.iter().map(|e| json!({"id":e.id,"kind":e.kind})).collect::<Vec<_>>()}),
            Message::Create { version, cwd } => json!({"kind":"create","version":version,"cwd":cwd}),
            Message::Close { selector } => json!({"kind":"close","selector":selector}),
            Message::MakeLive { selector } => json!({"kind":"make_live","selector":selector}),
            Message::Takeover { selector } => json!({"kind":"takeover","selector":selector}),
            Message::Created { id } => json!({"kind":"created","id":id}),
        }
    }

    fn fixture_value() -> serde_json::Value {
        use serde_json::json;
        let messages = vec![
            Message::Attach {
                version: 1,
                selector: "ab12".into(),
            },
            Message::Attach {
                version: 1,
                selector: String::new(),
            },
            Message::Marks {
                first: true,
                last: false,
                entries: vec![
                    MarkEntry {
                        id: "ab12".into(),
                        kind: 0,
                    },
                    MarkEntry {
                        id: "cd34".into(),
                        kind: 1,
                    },
                ],
            },
            Message::Marks {
                first: false,
                last: true,
                entries: vec![MarkEntry {
                    id: "ef56".into(),
                    kind: 0,
                }],
            },
            Message::Marks {
                first: true,
                last: true,
                entries: vec![],
            },
            Message::Input(b"\x03".to_vec()),
            Message::Input(b"ls -la\r".to_vec()),
            Message::Resync,
            Message::Detach,
            Message::Profile {
                version: 1,
                cols: 80,
                rows: 24,
                unsupported: unsupported::ALL_V1,
                shell: String::new(),
            },
            Message::Profile {
                version: 2,
                cols: 80,
                rows: 24,
                unsupported: unsupported::ALL_V1,
                shell: "ab12-1".into(),
            },
            Message::Key(vec!["Up".into()]),
            Message::Key(vec!["C-c".into(), "S-Tab".into()]),
            Message::Resize { cols: 45, rows: 30 },
            Message::Profile {
                version: 3,
                cols: 45,
                rows: 30,
                unsupported: unsupported::ALL_V1,
                shell: "ab12-1".into(),
            },
            Message::Rows {
                epoch: 1,
                full: true,
                patches: vec![Patch {
                    y: 2,
                    append: false,
                    text: "ok".into(),
                }],
            },
            Message::Rows {
                epoch: 0xBEEF,
                full: false,
                patches: vec![
                    Patch {
                        y: 0,
                        append: false,
                        text: "héllo 漢".into(),
                    },
                    Patch {
                        y: 255,
                        append: true,
                        text: String::new(),
                    },
                ],
            },
            Message::Commit {
                epoch: 0x0102,
                first_line: 0,
                cursor_x: 5,
                cursor_y: 6,
                flags: 1,
            },
            Message::Closed {
                reason: closed::EXITED,
            },
            Message::List,
            Message::Sessions {
                last: true,
                cwd: false,
                origin: false,
                owner: false,
                entries: vec![],
            },
            Message::Sessions {
                last: false,
                cwd: false,
                origin: false,
                owner: false,
                entries: vec![
                    SessionEntry {
                        id: "ab12cd34".into(),
                        label: "RCNT".into(),
                        cwd: String::new(),
                                        origin: String::new(),
                                owner: String::new(),
            },
                    SessionEntry {
                        id: "ef56".into(),
                        label: "héllo 漢".into(),
                        cwd: String::new(),
                                        origin: String::new(),
                                owner: String::new(),
            },
                ],
            },
            Message::Sessions {
                last: true,
                cwd: true,
                origin: false,
                owner: false,
                entries: vec![SessionEntry {
                    id: "ab12cd34".into(),
                    label: "RCNT".into(),
                    cwd: "~/Documents/RCNT".into(),
                                origin: String::new(),
                        owner: String::new(),
        }],
            },
        ];
        let many: Vec<(String, String, String, String, String)> = (0..7)
            .map(|i| {
                (
                    format!("{i:02x}{}", "f".repeat(30)),
                    format!("shell number {i} long label"),
                    format!("~/projects/shell-{i}"),
                    "phone".to_string(),
                    String::new(),
                )
            })
            .collect();
        let page_cases: Vec<_> = session_pages(&many, true, true, true)
            .iter()
            .map(|m| json!({"message": message_json(m), "hex": hex(&m.encode().unwrap())}))
            .collect();
        let msg_cases: Vec<_> = messages
            .iter()
            .map(|m| json!({"message": message_json(m), "hex": hex(&m.encode().unwrap())}))
            .collect();

        // A scripted session: each step is one screen the host reads, the bytes the Differ
        // produces for it, and what the reference Mirror holds after applying them.
        let long: String = "x".repeat(130);
        let steps_in: Vec<Screen> = vec![
            screen(&["$ ", "", ""], 20),
            screen(&["$ ls", "a.txt  b.txt", ""], 20),
            Screen {
                cursor_x: 9,
                cursor_y: 2,
                ..screen(&["$ ls", "a.txt  b.txt", "$"], 20)
            },
            screen(&["$ ls", &long, "héllo 漢"], 255),
            Screen {
                alternate: true,
                ..screen(&["$ ls", &long, "héllo 漢"], 255)
            },
            screen(&["one", "two"], 40),
        ];
        let mut differ = Differ::new();
        let mut mirror = Mirror::new();
        let mut steps = Vec::new();
        for sc in &steps_in {
            let msgs = differ.update(sc);
            let mut stream = Vec::new();
            for m in &msgs {
                stream.extend(m.encode().unwrap());
                mirror.apply(m);
            }
            steps.push(json!({
                "screen": {"cols":sc.cols,"rows":sc.rows,"cursorX":sc.cursor_x,"cursorY":sc.cursor_y,
                           "alternate":sc.alternate,"dead":sc.dead,"lines":sc.lines},
                "stream": hex(&stream),
                "messages": msgs.iter().map(message_json).collect::<Vec<_>>(),
                "mirror": {"lines":mirror.lines,"cursorX":mirror.cursor.0,"cursorY":mirror.cursor.1,
                           "flags":mirror.flags,"epoch":mirror.epoch,
                           "profile": mirror.profile.map(|(c,r,u)| json!({"cols":c,"rows":r,"unsupported":u}))},
            }));
        }

        // Streams that are not a clean run of messages.
        let unknown = [0x7e, 0x02, 0xaa, 0xbb, 0x04, 0x00];
        let mut d = Decoder::new();
        let unknown_out = d.push(&unknown).unwrap();
        let malformed = [0x83, 0x01, 0x00, 0x04, 0x00]; // Commit with a 1-byte body, then Detach
        let mut d = Decoder::new();
        let malformed_out = d.push(&malformed).unwrap();
        let oversize = [0x82, 0xad, 0x02]; // length 301
        let mut d = Decoder::new();
        let oversize_out = d.push(&oversize);
        let streams = json!({
            "unknownTagSkipped": {"hex": hex(&unknown), "messages": unknown_out.iter().map(|r| message_json(r.as_ref().unwrap())).collect::<Vec<_>>()},
            "malformedBodyCostsOneMessage": {"hex": hex(&malformed),
                "results": malformed_out.iter().map(|r| match r {
                    Ok(m) => message_json(m),
                    Err(e) => json!({"error": format!("{e:?}")}),
                }).collect::<Vec<_>>()},
            "oversizeBody": {"hex": hex(&oversize), "fatal": oversize_out.is_err()},
        });

        json!({
            "//": "Generated by crates/protocol/src/mesh_term.rs (fixture_value). Do not edit by hand: LDMUX_REGEN_FIXTURES=1 cargo test -p ldmux-protocol mesh_term_fixture. Read by the app's MeshTermCodecTest.",
            "limits": {"maxBody":MAX_BODY,"maxInput":MAX_INPUT,"maxSegment":MAX_SEGMENT,"maxSelector":MAX_SELECTOR,
                       "maxRowsBody":MAX_ROWS_BODY,"maxLabel":MAX_LABEL,"maxCwd":MAX_CWD,"maxSessions":MAX_SESSIONS,"maxMessage":MAX_MESSAGE,"recordPayload":RECORD_PAYLOAD,"maxBuffer":MAX_BUFFER,
                       "version":VERSION,"minVersion":MIN_VERSION,"cwdVersion":CWD_VERSION,"keyVersion":KEY_VERSION,
                       "resizeVersion":RESIZE_VERSION,"streamVersion":STREAM_VERSION,"lineVersion":STREAM_LINE_VERSION,
                       "originVersion":ORIGIN_VERSION,"ownerVersion":OWNER_VERSION,
                       "maxKeys":MAX_KEYS,"unsupportedAllV1":unsupported::ALL_V1,
                       "recordFloor":ldmux_mesh::constants::RECORD_FLOOR,"recordMax":ldmux_mesh::constants::RECORD_MAX,"creditWindow":ldmux_mesh::constants::CREDIT_WINDOW},
            "messages": msg_cases,
            "sessionPages": {"input": many.iter().map(|(i,l,p,by,held)| json!({"id":i,"label":l,"cwd":p,"origin":by,"owner":held})).collect::<Vec<_>>(), "pages": page_cases},
            "script": steps,
            "streams": streams,
        })
    }

    #[test]
    fn mesh_term_fixture_matches_the_encoder() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/fixtures/mesh-term-vectors.json"
        );
        let want = fixture_value();
        if std::env::var("LDMUX_REGEN_FIXTURES").is_ok() {
            let mut text = serde_json::to_string_pretty(&want).unwrap();
            text.push('\n');
            std::fs::write(path, text).unwrap();
        }
        let have: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path).expect("fixture missing: regenerate it"),
        )
        .unwrap();
        assert_eq!(have, want, "mesh-term-vectors.json is stale: regenerate it");
    }
}
