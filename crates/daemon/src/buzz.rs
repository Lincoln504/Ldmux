//! End-of-turn marks: one word about a shell, for the device the operator is holding.
//!
//! **A buzz is not a notification and not a write.** It touches no claim, no pty and no output; it
//! says "this shell finished what you asked" (`done`) or "this shell wants a look" (`attention`),
//! and the app draws it beside that shell's row in its list. Opening the shell clears it.
//!
//! Why this shape rather than Android notifications:
//!
//! - **A phone that is not connected cannot be pushed to.** There is no push service here and the
//!   offline mode is exactly "no internet at all", so the marks live on the host as a table and are
//!   delivered when a device connects. That is the whole delivery model, and it is why a mark
//!   survives the app being closed for an hour.
//! - **A row mark costs nothing and reads better than a notification.** A notification for a shell the operator
//!   is already looking at is noise, and a stack of them says less than the grid of rows that says
//!   which shells are waiting.
//! - The tailnet route can promote a mark to a real notification (opt-in); the radio route cannot,
//!   because promoting one there would mean keeping a radio session alive to receive it, and that
//!   costs airtime for something the list already says.
//!
//! The producer is `ldmux buzz`, which names its own shell from `$LDMUX_SESSION_ID`, so an agent
//! runs it with no arguments and cannot mark the wrong shell.

use std::collections::BTreeMap;

/// How long a mark stays in the table after it was made.
///
/// Ten minutes is the span of "I stepped away and came back": a mark older than that is history the
/// shell's own screen already tells. It also bounds the replay a phone receives when it reconnects,
/// so a device that was away overnight is not handed a wall of stale marks.
pub const MARK_TTL_MS: u64 = 10 * 60 * 1000;

/// The most shells that can carry a mark at once.
pub const MARKS_MAX: usize = 64;

/// The longest note kept, in bytes. It is displayed in a row and, on the radio, sent as one record.
pub const NOTE_MAX: usize = 80;

/// The shortest gap between two marks for the same shell.
///
/// A shell that finishes a burst of work can end several turns in a row, and a build loop can fire
/// far more often than a person wants to see. Within this window the *newest* mark replaces the old
/// one, so the row still ends up saying the right thing.
pub const COALESCE_MS: u64 = 5 * 1000;

/// What a mark says about a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An agent finished a turn.
    Done,
    /// The shell wants attention: a question, an error, a prompt waiting.
    Attention,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Attention => "attention",
        }
    }

    /// The wire byte, for the radio route (`mesh_term::Message::Marks`).
    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Attention => 1,
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Self::Done),
            "attention" => Some(Self::Attention),
            _ => None,
        }
    }
}

/// One mark: which shell, what it says, and when it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub session: String,
    pub kind: Kind,
    /// The operator's own line, already sanitized and bounded. Empty when none was given.
    pub note: String,
    /// Epoch ms.
    pub at: u64,
}

impl Mark {
    /// The JSON one mark, as every route sends it.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "session": self.session,
            "kind": self.kind.as_str(),
            "note": self.note,
            "at": self.at,
        })
    }
}

/// Why a buzz was refused, in the sentence the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    UnknownSession,
    BadKind,
    TooSoon { since_last_ms: u64 },
}

impl Refused {
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::UnknownSession => "no such running shell".to_string(),
            Self::BadKind => "kind is `done` or `attention`".to_string(),
            Self::TooSoon { since_last_ms } => format!(
                "that shell was marked {} s ago; a mark is not a progress bar",
                since_last_ms / 1000
            ),
        }
    }
}

/// The mark table: one entry per shell, newest first out of the queue.
#[derive(Default)]
pub struct Marks {
    by_session: BTreeMap<String, Mark>,
    out: Vec<Mark>,
}

impl Marks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a mark, replacing any earlier one for that shell.
    ///
    /// Refuses a repeat inside [`COALESCE_MS`] so a loop cannot become a notification stream, and
    /// drops the oldest entries past [`MARKS_MAX`] and [`MARK_TTL_MS`].
    pub fn mark(&mut self, session: &str, kind: Kind, note: &str, now: u64) -> Result<Mark, Refused> {
        if let Some(previous) = self.by_session.get(session) {
            let since = now.saturating_sub(previous.at);
            if since < COALESCE_MS {
                return Err(Refused::TooSoon {
                    since_last_ms: since,
                });
            }
        }
        let mark = Mark {
            session: session.to_string(),
            kind,
            note: sanitize(note),
            at: now,
        };
        self.by_session.insert(session.to_string(), mark.clone());
        self.out.push(mark.clone());
        self.prune(now);
        Ok(mark)
    }

    /// Forget a shell's mark: the operator opened it, so it has been read.
    pub fn clear(&mut self, session: &str) {
        self.by_session.remove(session);
    }

    /// The current table, in session-id order, for a device that has just connected.
    ///
    /// **Session-id order, not age.** `by_session` is a `BTreeMap` keyed by the id, so this is
    /// deterministic and stable between calls — which is what the radio's paging needs, since it
    /// cuts this list into pages. Age would be nicer to read and would let the same table page
    /// differently on two consecutive reads.
    #[must_use]
    pub fn snapshot(&self, now: u64) -> Vec<Mark> {
        self.by_session
            .values()
            .filter(|m| now.saturating_sub(m.at) <= MARK_TTL_MS)
            .cloned()
            .collect()
    }

    /// Take the marks made since the last call. The broadcaster owns delivery; this is the queue.
    pub fn drain_out(&mut self) -> Vec<Mark> {
        std::mem::take(&mut self.out)
    }

    /// Drop what has expired, and the oldest entries past the cap.
    pub fn prune(&mut self, now: u64) {
        self.by_session
            .retain(|_, m| now.saturating_sub(m.at) <= MARK_TTL_MS);
        let mut live: Vec<(String, u64)> = self
            .by_session
            .iter()
            .map(|(k, m)| (k.clone(), m.at))
            .collect();
        if live.len() <= MARKS_MAX {
            return;
        }
        live.sort_by_key(|(_, at)| *at);
        let drop_n = live.len() - MARKS_MAX;
        for (session, _) in live.into_iter().take(drop_n) {
            self.by_session.remove(&session);
        }
    }
}

/// The note as it may be shown and sent: printable, one line, bounded.
///
/// A note reaches a notification shade and a row on a phone, so control bytes are removed rather
/// than rendered: an escape sequence in a notification is how a title lies about what it is. The
/// cut is on a character boundary, and the result is at most [`NOTE_MAX`] bytes.
#[must_use]
pub fn sanitize(note: &str) -> String {
    let mut out = String::new();
    for ch in note.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        // Whitespace collapses: a note is one line, and a tab or a run of spaces in a row reads as
        // a gap the user has to interpret.
        if ch == ' ' && out.ends_with(' ') {
            continue;
        }
        if out.len() + ch.len_utf8() > NOTE_MAX {
            break;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_is_one_per_shell_and_the_newest_wins() {
        let mut m = Marks::new();
        m.mark("s1", Kind::Attention, "", 1_000).unwrap();
        m.mark("s1", Kind::Done, "finished", 1_000 + COALESCE_MS).unwrap();
        let snap = m.snapshot(2_000 + COALESCE_MS);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].kind, Kind::Done);
        assert_eq!(snap[0].note, "finished");
    }

    #[test]
    fn a_second_mark_inside_the_coalesce_window_is_refused_with_its_age() {
        let mut m = Marks::new();
        m.mark("s1", Kind::Done, "", 1_000).unwrap();
        assert_eq!(
            m.mark("s1", Kind::Done, "", 1_500),
            Err(Refused::TooSoon { since_last_ms: 500 })
        );
    }

    #[test]
    fn an_old_mark_is_not_replayed() {
        let mut m = Marks::new();
        m.mark("s1", Kind::Done, "", 1_000).unwrap();
        assert!(m.snapshot(1_000 + MARK_TTL_MS + 1).is_empty(), "expired");
        assert_eq!(m.snapshot(1_000 + MARK_TTL_MS).len(), 1, "still inside");
    }

    #[test]
    fn the_table_is_capped_and_the_oldest_go_first() {
        let mut m = Marks::new();
        for i in 0..(MARKS_MAX + 3) {
            let at = 1_000 + i as u64 * (COALESCE_MS + 1);
            m.mark(&format!("s{i}"), Kind::Done, "", at).unwrap();
        }
        let snap = m.snapshot(10_000);
        assert_eq!(snap.len(), MARKS_MAX);
        assert!(snap.iter().all(|x| x.session != "s0"), "the oldest went");
    }

    #[test]
    fn clearing_a_mark_forgets_it_and_does_not_resend_it() {
        let mut m = Marks::new();
        m.mark("s1", Kind::Done, "", 1_000).unwrap();
        let _ = m.drain_out();
        m.clear("s1");
        assert!(m.snapshot(1_000).is_empty());
        assert!(m.drain_out().is_empty(), "a cleared mark is not new news");
    }

    #[test]
    fn a_note_is_one_printable_bounded_line() {
        assert_eq!(
            sanitize("  two   spaces\ttab\nnewline  "),
            "two spaces tab newline"
        );
        assert_eq!(sanitize("\u{1b}[31mred\u{1b}[0m"), "[31mred [0m");
        assert!(sanitize(&"x".repeat(500)).len() <= NOTE_MAX);
        // Multi-byte characters are never cut in half.
        let long = sanitize(&"é".repeat(500));
        assert!(long.len() <= NOTE_MAX);
        assert!(long.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_wire_byte_and_the_word_agree_with_the_phone() {
        assert_eq!(Kind::Done.as_str(), "done");
        assert_eq!(Kind::Done.as_byte(), 0);
        assert_eq!(Kind::Attention.as_byte(), 1);
        assert_eq!(Kind::parse("attention"), Some(Kind::Attention));
        assert_eq!(Kind::parse("nope"), None);
    }
}
