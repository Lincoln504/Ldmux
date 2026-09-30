//! Deriving "what changed on screen" without a terminal emulator.
//!
//! Port of `packages/daemon/src/tmux/screen-diff.ts` (the source of record).
//!
//! The polling fallback (used only when a control-mode stream cannot be
//! established) has no VT model, so it compares successive `capture-pane -e`
//! outputs. Four cases, in order of preference:
//!
//! - **none** — nothing changed;
//! - **append** — `next` extends `previous` verbatim, so the tail can be
//!   written straight out;
//! - **rows** — same number of rows, so the changed rows are rewritten in place
//!   with absolute cursor addressing (`CSI <row>;1H` + `CSI K`). This is the
//!   common case for an interactive shell whose prompt line is rewritten on
//!   every keystroke, and — unlike a full repaint — it never pushes duplicate
//!   screens into the client's scrollback;
//! - **repaint** — anything else (different row count, scroll, resize): clear
//!   and re-send the screen and let the previous one scroll into history.
//!
//! Comparison is on decoded strings, so a multi-byte character can never be
//! split: the unit of comparison is a Unicode code point, not a byte.

/// The update the polling fallback must produce from two successive captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    /// Nothing changed.
    None,
    /// `next` extends `previous` verbatim; the tail is written straight out.
    Append,
    /// Same row count; the changed rows are rewritten in place.
    Rows,
    /// Anything else: clear and re-send the whole screen.
    Repaint,
}

/// Clear the screen and home the cursor: the repaint prefix.
pub const CLEAR_AND_HOME: &str = "\x1b[2J\x1b[H";

/// The update, or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenUpdate {
    pub kind: UpdateKind,
    pub text: String,
}

/// Absolute cursor addressing to the start of a 0-based row, erasing the row.
fn rewrite_row(index: usize, text: &str) -> String {
    format!("\x1b[{line};1H\x1b[K{text}", line = index + 1)
}

/// The update from one capture to the next.
pub fn screen_update(previous: &str, next: &str) -> ScreenUpdate {
    if next == previous {
        return ScreenUpdate {
            kind: UpdateKind::None,
            text: String::new(),
        };
    }
    // `starts_with` on "" is always true, so the first capture is a plain
    // append.
    if let Some(tail) = next.strip_prefix(previous) {
        return ScreenUpdate {
            kind: UpdateKind::Append,
            text: tail.to_string(),
        };
    }

    let prev_rows: Vec<&str> = previous.split('\n').collect();
    let next_rows: Vec<&str> = next.split('\n').collect();
    if prev_rows.len() != next_rows.len() {
        return ScreenUpdate {
            kind: UpdateKind::Repaint,
            text: format!("{CLEAR_AND_HOME}{next}"),
        };
    }

    // Longest common prefix and suffix of rows: only the middle is rewritten.
    let mut prefix = 0;
    while prefix < prev_rows.len() && prev_rows[prefix] == next_rows[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < prev_rows.len() - prefix
        && prev_rows[prev_rows.len() - 1 - suffix] == next_rows[next_rows.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut text = String::new();
    for i in prefix..next_rows.len() - suffix {
        text.push_str(&rewrite_row(i, next_rows[i]));
    }
    if text.is_empty() {
        ScreenUpdate {
            kind: UpdateKind::None,
            text: String::new(),
        }
    } else {
        ScreenUpdate {
            kind: UpdateKind::Rows,
            text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_screens_produce_no_output() {
        let u = screen_update("abc", "abc");
        assert_eq!(u, ScreenUpdate {
            kind: UpdateKind::None,
            text: "".into()
        });
    }

    #[test]
    fn first_capture_is_a_plain_append() {
        let u = screen_update("", "hello");
        assert_eq!(u.kind, UpdateKind::Append);
        assert_eq!(u.text, "hello");
    }

    #[test]
    fn appended_output_is_forwarded_verbatim() {
        let u = screen_update("$ ls\n", "$ ls\nfile\n");
        assert_eq!(u.kind, UpdateKind::Append);
        assert_eq!(u.text, "file\n");
    }

    #[test]
    fn a_rewritten_row_is_addressed_in_place_never_repainted() {
        // The interactive-shell case: the prompt line changes, the rest does
        // not.
        let u = screen_update("line one\n$ \nline three\n", "line one\n$ ls -a\nline three\n");
        assert_eq!(u.kind, UpdateKind::Rows);
        assert_eq!(u.text, "\x1b[2;1H\x1b[K$ ls -a");
    }

    #[test]
    fn only_the_changed_middle_rows_are_rewritten() {
        let u = screen_update("a\nb\nc\nd\ne\n", "a\nB\nc\nd\ne\n");
        assert_eq!(u.kind, UpdateKind::Rows);
        assert_eq!(u.text, "\x1b[2;1H\x1b[KB");
    }

    #[test]
    fn a_different_row_count_repaints_instead_of_guessing() {
        let u = screen_update("x\ny\n", "a\nb\nc\n");
        assert_eq!(u.kind, UpdateKind::Repaint);
        assert_eq!(u.text, format!("{CLEAR_AND_HOME}a\nb\nc\n"));
    }

    #[test]
    fn a_scrolled_screen_equal_rows_shifted_content_is_rewritten_row_wise() {
        let u = screen_update("1\n2\n3\n", "2\n3\n4\n");
        assert_eq!(u.kind, UpdateKind::Rows);
        // Nothing in common at the same index, so every row is rewritten.
        assert_eq!(u.text, "\x1b[1;1H\x1b[K2\x1b[2;1H\x1b[K3\x1b[3;1H\x1b[K4");
    }

    #[test]
    fn multi_byte_characters_are_compared_whole_never_split() {
        let u = screen_update("héllo 😀", "héllo 😀!");
        assert_eq!(u, ScreenUpdate {
            kind: UpdateKind::Append,
            text: "!".into()
        });
    }

    #[test]
    fn an_escape_sequence_appended_mid_screen_is_preserved() {
        let u = screen_update("\x1b[31mred", "\x1b[31mred\x1b[0m");
        assert_eq!(u, ScreenUpdate {
            kind: UpdateKind::Append,
            text: "\x1b[0m".into()
        });
    }

    #[test]
    fn empty_previous_row_set_cannot_cause_an_empty_rows_update() {
        assert_eq!(screen_update("", "").kind, UpdateKind::None);
    }
}
