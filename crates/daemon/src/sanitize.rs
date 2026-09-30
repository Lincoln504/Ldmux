//! Stateful output sanitizer for pane bytes (ADR-0001, docs/TRANSPORT.md §5.6).
//!
//! Port of `packages/daemon/src/sanitize.ts` (the source of record).
//!
//! Why this exists: pane output is replayed to a client that *answers* terminal
//! queries. An unfiltered OSC 52 lets a program write the user's clipboard, and
//! unfiltered device-status queries make xterm.js emit replies that ldmux sends
//! back into the shell (`term.onData` → `input` → `send-keys`), injecting bytes
//! that were never typed. This is the dooboxes "Patch 1" lesson.
//!
//! Policy (one instance per PaneStream, so sequences may straddle chunks):
//! drop OSC 52 (clipboard), all DCS/SOS/PM/APC strings, and device-status
//! queries (DA/DSR: CSI c, CSI >c, CSI 5n, CSI 6n, CSI ?0n). Keep everything
//! else, including SGR, cursor movement, OSC 8 hyperlinks, OSC 0/2 titles.
//!
//! OSC 52 is the one exception that can be asked for, and it is asked for in
//! `clipboard_write` rather than assumed. Dropping it is a security decision
//! with a real cost — a program inside the shell cannot put anything on the
//! phone's clipboard, so `pi`'s copy reports success and does nothing — but
//! honouring it is load-bearing: the pane is arbitrary code, and a clipboard is
//! shared with every other app on the device. A terminal that silently lets any
//! program overwrite what the user copies elsewhere is a terminal nobody asked
//! for. So the default stays closed, and the user may open it deliberately.
//!
//! The read direction has no exception at all. `GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ`
//! is deliberately unset in the shim, so a program cannot ask what is on the
//! clipboard even when writes are allowed: pasting your passwords somewhere else
//! is a far worse outcome than a copy that does not arrive.

const ESC: char = '\x1b';
const BEL: char = '\x07';
/// Guard against a malformed stream pinning an unbounded partial sequence.
const MAX_PENDING: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    String,
    StringEsc,
    Overflow,
}

/// One instance per PaneStream, so sequences may straddle chunks.
pub struct Sanitizer {
    /// Pass OSC 52 through instead of dropping it. Off unless the host says
    /// otherwise (`ldmux clipboard-write`), because it widens a security
    /// boundary to a program the user did not write.
    clipboard_write: bool,
    state: State,
    pending: String,
}

impl Sanitizer {
    pub fn new(clipboard_write: bool) -> Self {
        Sanitizer {
            clipboard_write,
            state: State::Ground,
            pending: String::new(),
        }
    }

    /// Drop any partial sequence and return to the ground state (a resync
    /// point).
    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.pending.clear();
    }

    /// Feed a chunk of decoded pane text; returns the chunk with policy
    /// applied.
    ///
    /// The input is compared in `char`s (Unicode scalars); the TS source of
    /// record walks UTF-16 code units. The two agree on every input that
    /// matters here — the state machine inspects ASCII control characters only
    /// — and pass-through bytes are appended whole either way.
    pub fn push(&mut self, text: &str) -> String {
        let mut out = String::new();
        for c in text.chars() {
            match self.state {
                State::Ground => {
                    if c == ESC {
                        self.state = State::Esc;
                        self.pending.push(ESC);
                    } else {
                        out.push(c);
                    }
                }
                State::Esc => {
                    self.pending.push(c);
                    if c == '[' {
                        self.state = State::Csi;
                    } else if c == ']' {
                        self.state = State::Osc;
                    } else if c == 'P' || c == 'X' || c == '^' || c == '_' {
                        self.state = State::String;
                    } else {
                        // Two-character escape (e.g. ESC 7): out and resync.
                        out.push_str(&self.pending);
                        self.reset();
                    }
                }
                State::Csi => {
                    self.pending.push(c);
                    let code = c as u32;
                    if (0x40..=0x7e).contains(&code) {
                        // Device queries never reach the client: a real
                        // terminal *answers* them, and the answer would be sent
                        // back through the pane as input. Keyed on the final
                        // byte alone — `c` (DA) and `n` (DSR) are queries in
                        // every parameter form (`CSI c`, `CSI >c`, `CSI ?6n`,
                        // …), so an allowlist of parameter strings can never be
                        // exhaustive.
                        if c != 'c' && c != 'n' {
                            out.push_str(&self.pending);
                        }
                        self.reset();
                    }
                }
                State::Osc => {
                    if c == BEL {
                        self.pending.push(c);
                        self.finish_osc(&mut out);
                    } else if c == ESC {
                        self.pending.push(c);
                        self.state = State::OscEsc;
                    } else {
                        self.pending.push(c);
                    }
                }
                State::OscEsc => {
                    if c == '\\' {
                        self.pending.push(c);
                        self.finish_osc(&mut out);
                    } else {
                        // Not a string terminator: the ESC stays as body
                        // content and the OSC resumes.
                        self.pending.push(c);
                        self.state = State::Osc;
                    }
                }
                State::String => {
                    // DCS/SOS/PM/APC strings are dropped whole: nothing from
                    // this state reaches the client.
                    if c == BEL {
                        self.reset();
                    } else if c == ESC {
                        self.state = State::StringEsc;
                    }
                }
                State::StringEsc => {
                    if c == '\\' {
                        self.reset();
                    } else {
                        self.state = State::String;
                    }
                }
                State::Overflow => {
                    // A malformed stream must not pin an unbounded partial
                    // sequence, and must not dump the tail of it as visible
                    // text: drop until the next ESC resyncs us.
                    if c == ESC {
                        self.state = State::Esc;
                        self.pending.clear();
                        self.pending.push(ESC);
                    }
                }
            }
            // A malformed stream must not pin an unbounded partial sequence.
            if self.pending.len() > MAX_PENDING {
                self.pending.clear();
                self.state = State::Overflow;
            }
        }
        out
    }
}

impl Sanitizer {
    /// OSC 52 = clipboard write/read. Everything else passes.
    fn finish_osc(&mut self, out: &mut String) {
        // pending is `ESC ] <body>`; the body is what follows the `]`.
        let body = &self.pending[2..];
        let num = body.split(';').next().unwrap_or("");
        if num != "52" || self.clipboard_write {
            out.push_str(&self.pending);
        }
        self.reset();
    }
}

/// One-shot sanitization, for snapshot replays, which are standalone renders.
///
/// Snapshots never need the clipboard exception: a snapshot is a rendered
/// capture, which carries characters and no escape sequences at all.
pub fn sanitize_once(text: &str) -> String {
    Sanitizer::new(false).push(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text_and_sgr_through() {
        let t = "hello \x1b[31mred\x1b[0m world";
        assert_eq!(sanitize_once(t), t);
    }

    #[test]
    fn drops_osc_52_clipboard_writes_bel_and_st_terminated() {
        assert_eq!(sanitize_once("a\x1b]52;c;SGVsbG8=\x07b"), "ab");
        assert_eq!(sanitize_once("a\x1b]52;c;SGVsbG8=\x1b\\b"), "ab");
    }

    #[test]
    fn osc_52_passes_only_when_the_host_asked_for_it() {
        // The default is the security control, so it is asserted first and
        // separately: a change that silently flips the default has to fail a
        // test, not a review.
        let mut off = Sanitizer::new(false);
        assert_eq!(off.push("a\x1b]52;c;SGVsbG8=\x07b"), "ab");

        let mut on = Sanitizer::new(true);
        assert_eq!(on.push("a\x1b]52;c;SGVsbG8=\x07b"), "a\x1b]52;c;SGVsbG8=\x07b");
        assert_eq!(on.push("a\x1b]52;c;SGVsbG8=\x1b\\b"), "a\x1b]52;c;SGVsbG8=\x1b\\b");
    }

    #[test]
    fn allowing_a_clipboard_write_relaxes_nothing_else() {
        // The boundary is one sequence wide. Everything else the sanitizer
        // drops — the device queries a client would answer, and the DCS/APC
        // strings — stays dropped, because the option says what it says and
        // nothing more.
        let mut on = Sanitizer::new(true);
        assert_eq!(on.push("a\x1bPq#0;2;0;0;0\x1b\\b"), "ab");
        assert_eq!(on.push("a\x1b_Ga=T\x1b\\b"), "ab");
        for query in ["\x1b[c", "\x1b[>c", "\x1b[5n", "\x1b[6n", "\x1b[?0n"].iter() {
            assert_eq!(on.push(&format!("a{query}b")), "ab");
        }
        // Other OSC still passes, which is what makes the exception narrow
        // rather than a general loosening of the escape-sequence policy.
        assert_eq!(on.push("\x1b]0;title\x07"), "\x1b]0;title\x07");
    }

    #[test]
    fn an_osc_52_split_across_chunks_is_still_one_write() {
        // The sequence can straddle two output chunks, so the decision has to
        // be made by the same state machine that tracks the partial sequence.
        // Checking the option only on the first chunk would let the tail of a
        // sequence through unexamined.
        let mut off = Sanitizer::new(false);
        assert_eq!(off.push("a\x1b]52;c;SGVs"), "a");
        assert_eq!(off.push("bG8=\x07b"), "b");

        let mut on = Sanitizer::new(true);
        assert_eq!(on.push("a\x1b]52;c;SGVs"), "a");
        assert_eq!(on.push("bG8=\x07b"), "\x1b]52;c;SGVsbG8=\x07b");
    }

    #[test]
    fn keeps_other_osc_titles_and_hyperlinks() {
        assert_eq!(sanitize_once("\x1b]0;title\x07"), "\x1b]0;title\x07");
        let t = "x\x1b]8;;https://e\x1b\\link\x1b]8;;\x1b\\y";
        assert_eq!(sanitize_once(t), t);
    }

    #[test]
    fn drops_dcs_sos_pm_apc_strings() {
        assert_eq!(sanitize_once("a\x1bPq#0;2;0;0;0\x1b\\b"), "ab");
        assert_eq!(sanitize_once("a\x1b_Ga=T\x1b\\b"), "ab");
    }

    #[test]
    fn drops_device_status_queries_the_reason_xterm_js_would_auto_reply() {
        for query in ["\x1b[c", "\x1b[>c", "\x1b[0c", "\x1b[>0c", "\x1b[5n", "\x1b[6n", "\x1b[?0n"].iter() {
            assert_eq!(sanitize_once(&format!("a{query}b")), "ab");
        }
    }

    #[test]
    fn keeps_ordinary_csi_sequences() {
        let t = "\x1b[2J\x1b[H\x1b[1;31m!\x1b[0m";
        assert_eq!(sanitize_once(t), t);
    }

    #[test]
    fn keeps_two_character_escapes() {
        assert_eq!(sanitize_once("a\x1b7b"), "a\x1b7b");
    }

    #[test]
    fn reassembles_a_filtered_sequence_split_across_chunks() {
        let mut s = Sanitizer::new(false);
        let mut out = String::new();
        out.push_str(&s.push("a\x1b"));
        out.push_str(&s.push("]52;c;QQ=="));
        out.push_str(&s.push("\x07"));
        out.push_str(&s.push("b"));
        assert_eq!(out, "ab");
    }

    #[test]
    fn reassembles_an_allowed_sequence_split_across_chunks() {
        let mut s = Sanitizer::new(false);
        let mut out = String::new();
        out.push_str(&s.push("a\x1b[3"));
        out.push_str(&s.push("1m"));
        out.push_str(&s.push("b"));
        assert_eq!(out, "a\x1b[31mb");
    }

    #[test]
    fn a_malformed_stream_is_bounded_and_never_dumps_its_tail_as_text() {
        let mut s = Sanitizer::new(false);
        let big = format!("\x1b[{}m", "1;".repeat(9000));
        assert_eq!(s.push(&big), "");
        // … and it resyncs at the next ESC rather than staying poisoned.
        assert_eq!(s.push("\x1b[32mok"), "\x1b[32mok");
    }

    #[test]
    fn drops_device_status_queries_in_any_parameter_form_not_just_an_allowlist() {
        // The policy keys on the final byte: a real terminal answers all of
        // these, and the answer would be sent back into the pane as input.
        for query in ["\x1b[?6n", "\x1b[1;2c", "\x1b[?62;1;2c", "\x1b[=c", "\x1b[?15n", "\x1b[999n"].iter() {
            assert_eq!(sanitize_once(&format!("a{query}b")), "ab");
        }
    }

    #[test]
    fn still_keeps_csi_sequences_whose_final_byte_is_not_a_device_query() {
        // c/n are queries; p/q/J/m/H and friends are not and must survive
        // intact.
        for keep in ["\x1b[0m", "\x1b[2J", "\x1b[3;7H", "\x1b[?25l", "\x1b[?25h", "\x1b[4h"].iter() {
            assert_eq!(sanitize_once(&format!("a{keep}b")), format!("a{keep}b"));
        }
    }

    #[test]
    fn reset_drops_a_partial_sequence_so_a_mode_change_cannot_splice_bytes() {
        // PaneStream resets the sanitizer on the control<->polling transition:
        // a pending partial sequence left by one source must not be joined to
        // the generated repaint bytes of the other.
        let mut s = Sanitizer::new(false);
        assert_eq!(s.push("a\x1b[3"), "a");
        s.reset();
        assert_eq!(s.push("1m\x1b[2J"), "1m\x1b[2J");
    }
}
