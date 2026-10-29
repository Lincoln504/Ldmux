//! The mesh terminal profile: what goes *inside* the L3 session (ADR-0025).
//!
//! The mesh link carries about 58 records an hour of 144 payload bytes each (ADR-0025 has the
//! arithmetic), so a VT byte stream cannot cross it. This module is the alternative: the host
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
//! | 0x81 | host→phone | `Profile` | `version u8`, `cols u8`, `rows u8`, `unsupported u16 LE` |
//! | 0x82 | host→phone | `Rows` | `epoch u16 LE`, `flags u8` (bit 0 = full), then patches |
//! | 0x83 | host→phone | `Commit` | `epoch u16 LE`, `cursor_x u8`, `cursor_y u8`, `flags u8` |
//! | 0x84 | host→phone | `Closed` | `reason u8` |
//! | 0x85 | host→phone | `Sessions` | `flags u8` (bit 0 = last page), then entries `id_len u8 \| id \| label_len u8 \| label` |
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
pub const VERSION: u8 = 1;

/// A message body is at most this long.
pub const MAX_BODY: usize = 300;
/// A `Rows` body is packed up to this many bytes, so a whole message (tag + len + body) is at
/// most [`MAX_MESSAGE`] and fits one record's 144-byte payload.
pub const MAX_ROWS_BODY: usize = 130;
/// The largest encoded host→phone message.
pub const MAX_MESSAGE: usize = 1 + 2 + MAX_ROWS_BODY;
/// The payload of one record after the L3 headers: 172 − 24 (ADR-0020) − 4 (ARQ).
pub const RECORD_PAYLOAD: usize = 144;
/// Most bytes in one `Input` message.
pub const MAX_INPUT: usize = 96;
/// Most bytes of a shell label in a `Sessions` entry (cut at a character boundary by the host).
pub const MAX_LABEL: usize = 24;
/// Most shells a host lists. A host with more lists the first this many and says nothing else:
/// a picker of more than this is not usable at one record a minute anyway.
pub const MAX_SESSIONS: usize = 16;
/// Most bytes of text in one row segment.
pub const MAX_SEGMENT: usize = 120;
/// Longest `Attach` selector.
pub const MAX_SELECTOR: usize = 40;
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
    /// The phone does not resize the pane; it renders the host's geometry.
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
}

/// `Commit.flags`.
pub mod commit {
    /// The program is on the alternate screen (a full-screen program: rows are rewritten
    /// wholesale, so updates will be larger).
    pub const ALTERNATE: u8 = 1 << 0;
    /// The shell has exited.
    pub const DEAD: u8 = 1 << 1;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub y: u8,
    /// Add to the row instead of replacing it.
    pub append: bool,
    pub text: String,
}

/// One running shell in a `Sessions` reply: its id (usable as an `Attach` selector) and a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    pub label: String,
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
    },
    Closed {
        reason: u8,
    },
    /// Phone asks which shells are running.
    List,
    /// One page of the answer. `last` is set on the final page; an empty list is one page with
    /// `last` set and no entries.
    Sessions {
        last: bool,
        entries: Vec<SessionEntry>,
    },
}

const T_LIST: u8 = 0x05;
const T_SESSIONS: u8 = 0x85;
const T_ATTACH: u8 = 0x01;
const T_INPUT: u8 = 0x02;
const T_RESYNC: u8 = 0x03;
const T_DETACH: u8 = 0x04;
const T_PROFILE: u8 = 0x81;
const T_ROWS: u8 = 0x82;
const T_COMMIT: u8 = 0x83;
const T_CLOSED: u8 = 0x84;

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
            } => {
                body.extend_from_slice(&[*version, *cols, *rows]);
                body.extend_from_slice(&unsupported.to_le_bytes());
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
            } => {
                body.extend_from_slice(&epoch.to_le_bytes());
                body.extend_from_slice(&[*cursor_x, *cursor_y, *flags]);
                T_COMMIT
            }
            Self::Closed { reason } => {
                body.push(*reason);
                T_CLOSED
            }
            Self::List => T_LIST,
            Self::Sessions { last, entries } => {
                body.push(u8::from(*last));
                for e in entries {
                    if e.id.len() > MAX_SELECTOR || e.label.len() > MAX_LABEL {
                        return Err(EncodeError::TooLong);
                    }
                    body.push(e.id.len() as u8);
                    body.extend_from_slice(e.id.as_bytes());
                    body.push(e.label.len() as u8);
                    body.extend_from_slice(e.label.as_bytes());
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
            T_PROFILE if body.len() == 5 => Self::Profile {
                version: body[0],
                cols: body[1],
                rows: body[2],
                unsupported: u16::from_le_bytes([body[3], body[4]]),
            },
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
            },
            T_CLOSED if body.len() == 1 => Self::Closed { reason: body[0] },
            T_LIST if body.is_empty() => Self::List,
            T_SESSIONS => {
                let (&flags, mut rest) = body.split_first().ok_or(bad.clone())?;
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
                    entries.push(SessionEntry { id, label });
                }
                Self::Sessions {
                    last: flags & 1 == 1,
                    entries,
                }
            }
            T_RESYNC | T_DETACH | T_PROFILE | T_COMMIT | T_CLOSED | T_LIST => {
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

/// The `Sessions` pages for a list of `(id, label)`: at most [`MAX_SESSIONS`] entries, ids and
/// labels cut to their limits, each page packed up to [`MAX_ROWS_BODY`] so it fits one record,
/// the final page flagged `last`. Always at least one page, so "no shells" is an answer.
#[must_use]
pub fn session_pages(sessions: &[(String, String)]) -> Vec<Message> {
    let mut pages: Vec<Vec<SessionEntry>> = vec![Vec::new()];
    let mut size = 1; // the flags byte
    for (id, label) in sessions.iter().take(MAX_SESSIONS) {
        let e = SessionEntry {
            id: cut(id, MAX_SELECTOR),
            label: cut(label, MAX_LABEL),
        };
        let cost = 2 + e.id.len() + e.label.len();
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
#[derive(Debug, Default)]
pub struct Differ {
    sent: Option<Screen>,
    epoch: u16,
}

impl Differ {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget what the phone holds, so the next update is a full screen (`Resync`, or a new
    /// attach).
    pub fn reset(&mut self) {
        self.sent = None;
    }

    /// The messages that bring the phone from what it holds to `screen`; empty when nothing
    /// differs. The first call after `new` or `reset` is a full screen, preceded by a `Profile`.
    pub fn update(&mut self, screen: &Screen) -> Vec<Message> {
        let mut out = Vec::new();
        let geometry_changed = self
            .sent
            .as_ref()
            .is_none_or(|s| s.cols != screen.cols || s.rows != screen.rows);
        let full = geometry_changed;
        if geometry_changed {
            out.push(Message::Profile {
                version: VERSION,
                cols: screen.cols,
                rows: screen.rows,
                unsupported: unsupported::ALL_V1,
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
        let flags = (u8::from(screen.alternate) * commit::ALTERNATE)
            | (u8::from(screen.dead) * commit::DEAD);
        let cursor_or_flags_changed = self.sent.as_ref().is_none_or(|s| {
            s.cursor_x != screen.cursor_x
                || s.cursor_y != screen.cursor_y
                || s.alternate != screen.alternate
                || s.dead != screen.dead
        });
        if flat.is_empty() && !full && !cursor_or_flags_changed {
            return Vec::new();
        }
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        // Pack patches into `Rows` messages of at most MAX_ROWS_BODY bytes. A full screen with
        // no text still sends one empty `Rows(full)`, so the phone clears.
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
                flush(&mut cur, &mut first, &mut out);
                size = 3;
            }
            size += need;
            cur.push(p);
        }
        flush(&mut cur, &mut first, &mut out);
        out.push(Message::Commit {
            epoch,
            cursor_x: screen.cursor_x,
            cursor_y: screen.cursor_y,
            flags,
        });
        self.sent = Some(screen.clone());
        out
    }
}

/// The phone's side, as a reference implementation: what a client does with the messages. The
/// Kotlin port is checked against the same vectors.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mirror {
    pub profile: Option<(u8, u8, u16)>,
    pub lines: Vec<String>,
    pub cursor: (u8, u8),
    pub flags: u8,
    pub epoch: u16,
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
                ..
            } => {
                self.profile = Some((*cols, *rows, *unsupported));
                self.lines = vec![String::new(); usize::from(*rows)];
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
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
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
        roundtrip(&Message::Resync);
        roundtrip(&Message::Detach);
        roundtrip(&Message::Profile {
            version: 1,
            cols: 80,
            rows: 24,
            unsupported: unsupported::ALL_V1,
        });
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
            epoch: 7,
            cursor_x: 3,
            cursor_y: 4,
            flags: 3,
        });
        roundtrip(&Message::Closed {
            reason: closed::EXITED,
        });
    }

    fn sh(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| {
                (
                    format!("{i:02x}{}", "f".repeat(30)),
                    format!("shell number {i} long label"),
                )
            })
            .collect()
    }

    #[test]
    fn list_and_sessions_have_exact_bytes() {
        assert_eq!(Message::List.encode().unwrap(), [0x05, 0x00]);
        // no shells: one page, `last`, no entries
        assert_eq!(
            session_pages(&[]),
            vec![Message::Sessions {
                last: true,
                entries: vec![]
            }]
        );
        assert_eq!(
            session_pages(&[]).remove(0).encode().unwrap(),
            [0x85, 0x01, 0x01]
        );
        let one = Message::Sessions {
            last: true,
            entries: vec![SessionEntry {
                id: "ab".into(),
                label: "x".into(),
            }],
        };
        // flags 1, id_len 2 "ab", label_len 1 "x"
        assert_eq!(
            one.encode().unwrap(),
            [0x85, 0x06, 0x01, 0x02, b'a', b'b', 0x01, b'x']
        );
        roundtrip(&Message::List);
        roundtrip(&one);
    }

    #[test]
    fn sessions_pages_fit_one_record_and_say_which_is_last() {
        for n in [0usize, 1, 2, 3, 7, MAX_SESSIONS, MAX_SESSIONS + 5] {
            let pages = session_pages(&sh(n));
            let mut total = 0;
            for (i, m) in pages.iter().enumerate() {
                let bytes = m.encode().unwrap();
                assert!(
                    bytes.len() <= MAX_MESSAGE,
                    "n={n} page {i} is {} bytes",
                    bytes.len()
                );
                assert!(bytes.len() <= RECORD_PAYLOAD);
                let Message::Sessions { last, entries } = m else {
                    panic!()
                };
                assert_eq!(*last, i + 1 == pages.len(), "only the final page is last");
                total += entries.len();
            }
            assert_eq!(
                total,
                n.min(MAX_SESSIONS),
                "n={n}: nothing lost, nothing past the cap"
            );
        }
        assert!(
            session_pages(&sh(7)).len() > 1,
            "a long list really does span records"
        );
    }

    #[test]
    fn sessions_survive_any_chunking_and_cut_at_char_boundaries() {
        let list = vec![("é".repeat(30), "漢".repeat(20))]; // 60-byte id, 60-byte label
        let pages = session_pages(&list);
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
        assert!(
            entries[0].id.chars().all(|c| c == 'é'),
            "never half a character"
        );
    }

    #[test]
    fn oversize_or_broken_sessions_are_refused() {
        let bad = Message::Sessions {
            last: true,
            entries: vec![SessionEntry {
                id: "x".repeat(MAX_SELECTOR + 1),
                label: String::new(),
            }],
        };
        assert_eq!(bad.encode(), Err(EncodeError::TooLong));
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

    #[test]
    fn exact_bytes_are_pinned() {
        // These are the vectors the Kotlin port must reproduce.
        assert_eq!(Message::Resync.encode().unwrap(), [0x03, 0x00]);
        assert_eq!(
            Message::Profile {
                version: 1,
                cols: 80,
                rows: 24,
                unsupported: 0x007F
            }
            .encode()
            .unwrap(),
            [0x81, 0x05, 0x01, 0x50, 0x18, 0x7F, 0x00]
        );
        assert_eq!(
            Message::Commit {
                epoch: 0x0102,
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
        let s = Screen::from_keeper(text, 80, 2, (0, 1), false, false);
        assert_eq!(s.lines, ["$ ls", "file"]);
        let s = Screen::from_keeper("only", 80, 3, (0, 0), false, false);
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
            } => json!({
                "kind":"profile","version":version,"cols":cols,"rows":rows,"unsupported":unsupported}),
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
            } => json!({
                "kind":"commit","epoch":epoch,"cursorX":cursor_x,"cursorY":cursor_y,"flags":flags}),
            Message::Closed { reason } => json!({"kind":"closed","reason":reason}),
            Message::List => json!({"kind":"list"}),
            Message::Sessions { last, entries } => json!({
                "kind":"sessions","last":last,
                "entries": entries.iter().map(|e| json!({"id":e.id,"label":e.label})).collect::<Vec<_>>()}),
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
            Message::Input(b"\x03".to_vec()),
            Message::Input(b"ls -la\r".to_vec()),
            Message::Resync,
            Message::Detach,
            Message::Profile {
                version: 1,
                cols: 80,
                rows: 24,
                unsupported: unsupported::ALL_V1,
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
                entries: vec![],
            },
            Message::Sessions {
                last: false,
                entries: vec![
                    SessionEntry {
                        id: "ab12cd34".into(),
                        label: "RCNT".into(),
                    },
                    SessionEntry {
                        id: "ef56".into(),
                        label: "héllo 漢".into(),
                    },
                ],
            },
        ];
        let many: Vec<(String, String)> = (0..7)
            .map(|i| {
                (
                    format!("{i:02x}{}", "f".repeat(30)),
                    format!("shell number {i} long label"),
                )
            })
            .collect();
        let page_cases: Vec<_> = session_pages(&many)
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
                       "maxRowsBody":MAX_ROWS_BODY,"maxLabel":MAX_LABEL,"maxSessions":MAX_SESSIONS,"maxMessage":MAX_MESSAGE,"recordPayload":RECORD_PAYLOAD,"maxBuffer":MAX_BUFFER,
                       "version":VERSION,"unsupportedAllV1":unsupported::ALL_V1},
            "messages": msg_cases,
            "sessionPages": {"input": many.iter().map(|(i,l)| json!({"id":i,"label":l})).collect::<Vec<_>>(), "pages": page_cases},
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
