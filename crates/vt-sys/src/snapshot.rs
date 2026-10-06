//! The snapshot renderer: the ghostty-vt cells (the source of truth) → the
//! snapshot byte stream (plan §3.4 / §3.11).
//!
//! This is the pure half of the snapshot path. The keeper holds the C render
//! state, walks the cells (via `RenderState`), and hands the extracted cell
//! data here; this module turns it into the byte stream the client replays.
//!
//! The format (plan §3.4): one line per row, SGR-prefixed (a style run is
//! emitted once, then its text), **full-width** (the `-N` semantics — every
//! cell to the right edge with its attributes), `\n`-joined. The cells are
//! the source of truth; the format is ours to write (plan §3.11(3)). The
//! §3.11 cell-parity harness and the round-trip test
//! (`crates/daemon/tests/parity.rs`) make it safe.

use crate::{Cell, CellStyle, sgr};

/// A row of cells, padded to the full width with blank cells (the `-N`
/// semantics — every cell to the right edge). A cell past the row's real
/// content is a blank cell (the default style, empty text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlankCell;

/// The full-width row: the real cells, then blank cells to `width`.
pub struct FullWidthRow<'a> {
    row: &'a [Cell],
    width: usize,
}

impl<'a> FullWidthRow<'a> {
    /// Build a full-width view over a row of `len(row)` cells, padded to
    /// `width` with blank cells.
    pub fn new(row: &'a [Cell], width: usize) -> Self {
        Self { row, width }
    }

    /// The cell at column `x` (a blank cell if past the row's real content).
    pub fn cell(&self, x: usize) -> CellView<'a> {
        if x < self.row.len() {
            CellView::Real(&self.row[x])
        } else {
            CellView::Blank
        }
    }

    /// The number of columns (the full width).
    pub fn len(&self) -> usize {
        self.width
    }

    /// Whether the row has no columns (always false — a full-width row is at
    /// least as wide as `width`; present to satisfy the `len`/`is_empty` lint).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One column of a full-width row: a real cell or a blank cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellView<'a> {
    Real(&'a Cell),
    Blank,
}

impl CellView<'_> {
    /// The cell's text (empty for a blank cell).
    pub fn text(&self) -> &str {
        match self {
            CellView::Real(c) => &c.text,
            CellView::Blank => "",
        }
    }

    /// The cell's style (the blank style for a blank cell).
    pub fn style(&self) -> CellStyle {
        match self {
            CellView::Real(c) => c.style,
            CellView::Blank => sgr::BLANK,
        }
    }
}

/// Render a set of rows to the `capture-pane -e` format: one line per row,
/// SGR-prefixed (a style run is emitted once, then its text), full-width
/// (the `-N` semantics), `\n`-joined.
///
/// `rows` is a slice of rows, each a slice of cells (the row's real content,
/// padded to `width` by `FullWidthRow`). The output is *our* format for those
/// cells (plan §3.11(3): the cells are the source of truth; the format is
/// ours) — the §3.11 round-trip test pins the fidelity.
pub fn render(rows: &[&[Cell]], width: usize) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let fwr = FullWidthRow::new(row, width);
        // Default-style blanks at the right edge are the same on screen as
        // nothing, and a replaying client starts each repaint from a clear
        // screen, so they are not emitted. A *styled* blank is content (a status
        // bar's background, an inverse space) and is kept, as is every blank
        // that has something to its right.
        let end = (0..fwr.len())
            .rev()
            .find(|&x| {
                let cv = fwr.cell(x);
                !(cv.style() == sgr::BLANK && (cv.text().is_empty() || cv.text() == " "))
            })
            .map_or(0, |x| x + 1);
        let mut current = sgr::BLANK;
        for x in 0..end {
            let cv = fwr.cell(x);
            let style = cv.style();
            if style != current {
                out.push_str(&sgr::transition(&current, &style));
                current = style;
            }
            out.push_str(cv.text());
        }
        // Style is stream state, not row state: leave it reset so the next row
        // (which starts from the default) is not painted with this one's tail.
        if current != sgr::BLANK {
            out.push_str(&sgr::transition(&current, &sgr::BLANK));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellStyle, StyleColor};

    fn cell(text: &str, mut f: impl FnMut(&mut CellStyle)) -> Cell {
        let mut c = Cell {
            text: text.into(),
            style: sgr::BLANK,
        };
        f(&mut c.style);
        c
    }

    fn plain(text: &str) -> Cell {
        cell(text, |_| {})
    }

    #[test]
    fn a_single_plain_row_is_just_the_text() {
        let row = [plain("hello")];
        assert_eq!(render(&[&row], 5), "hello");
    }

    #[test]
    fn two_rows_are_newline_joined() {
        let a = [plain("ab")];
        let b = [plain("cd")];
        assert_eq!(render(&[&a, &b], 2), "ab\ncd");
    }

    #[test]
    fn a_bold_run_is_emitted_once_then_its_text() {
        let mut bold = Cell {
            text: "B".into(),
            style: sgr::BLANK,
        };
        bold.style.bold = true;
        let row = [plain("A"), bold, plain("C")];
        // "A" blank, then a transition to bold (a `0;1` re-issue), "B", then a
        // transition back to blank (a `0`), "C".
        assert_eq!(render(&[&row], 3), "A\x1b[0;1mB\x1b[0mC");
    }

    #[test]
    fn a_short_row_is_padded_to_full_width() {
        // A row of one cell ("A"), width 3: "A" then two blank cells (no SGR,
        // the blank style is the default).
        let row = [plain("A")];
        assert_eq!(render(&[&row], 3), "A");
    }

    #[test]
    fn a_fg_colour_run_is_38() {
        let mut red = Cell {
            text: "B".into(),
            style: sgr::BLANK,
        };
        red.style.fg = Some(StyleColor::Palette(1));
        let row = [plain("A"), red];
        assert_eq!(render(&[&row], 2), "A\x1b[0;38;5;1mB\x1b[0m");
    }
}
