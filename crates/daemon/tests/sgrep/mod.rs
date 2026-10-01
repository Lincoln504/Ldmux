//! A test-only decoder for tmux `capture-pane -e` output: an SGR-annotated
//! row stream → a grid of `Cell`s, the same shape the engine's
//! `RenderState` walk returns.
//!
//! It exists so the §3.11 gate compares *cells*, not SGR bytes (plan
//! §3.11(3): "the cells are the source of truth; the format is ours"). It is
//! deliberately narrow — the corpus emits only SGR (`m`) sequences, one
//! character per column — and panics on anything else: an unsupported escape
//! is a harness defect (the corpus emits none), not data to guess at.
//!
//! Wide/emoji cells are deferred with the corpus: every character here is
//! one column, and the plan's full corpus (wide chars, emoji) is what will
//! extend that.

use ldmux_vt_sys::sgr::BLANK;
use ldmux_vt_sys::{Cell, CellStyle, StyleColor};

/// Parse a full `capture-pane -p -e` output into rows of `Cell`s, each row
/// padded to `width` with empty cells.
///
/// - one character = one column (single-width only — see the module note);
/// - an SGR sequence restyles from the *next* character on (the terminal
///   rule);
/// - a newline ends a row (padded to `width`);
/// - `38`/`48` consume their `5;n` or `2;r;g;b` parameters.
pub fn parse(out: &str, width: usize) -> Vec<Vec<Cell>> {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut col: usize = 0;
    let mut style: CellStyle = BLANK;

    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // A CSI sequence: expect '[' then parameters then a terminator.
            let bracket = chars.next();
            let mut params = String::new();
            let mut term: Option<char> = None;
            for pc in chars.by_ref() {
                if (0x40..=0x7e).contains(&(pc as u32)) {
                    term = Some(pc);
                    break;
                }
                params.push(pc);
            }
            if bracket != Some('[') || term != Some('m') {
                panic!("sgrep: unsupported escape in the capture (the corpus emits only SGR)");
            }
            apply(params.as_bytes(), &mut style);
        } else if c == '\n' {
            // Every newline ends a row — an empty line is an empty row
            // (the capture is `ROWS` lines, each `\n`-terminated).
            pad_row(&mut row, &mut col, width);
            rows.push(std::mem::take(&mut row));
            col = 0;
        } else {
            row.push(Cell { text: c.to_string(), style });
            col += 1;
        }
    }
    // A trailing row without a final newline (capture-pane -p ends with one,
    // but be total).
    if col > 0 {
        pad_row(&mut row, &mut col, width);
        rows.push(row);
    }
    rows
}

/// Pad the current row to `width` with empty cells.
fn pad_row(row: &mut Vec<Cell>, col: &mut usize, width: usize) {
    while *col < width {
        row.push(Cell { text: String::new(), style: BLANK });
        *col += 1;
    }
}

/// Apply one SGR parameter string to a style (the SGR table the codes tmux
/// `capture-pane -e` come from). Unknown codes are ignored (forward-compat;
/// the corpus emits none).
fn apply(params: &[u8], s: &mut CellStyle) {
    let toks: Vec<String> = params
        .split(|&c| c == b';')
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        i += 1;
        let v: i32 = t.parse().unwrap_or(0);
        match v {
            0 => *s = BLANK,
            1 => s.bold = true,
            21 => s.bold = false,
            2 => s.faint = true,
            // 22 is "normal intensity": it clears faint *and* underline (the
            // xterm table).
            22 => {
                s.faint = false;
                s.underline_kind = 0;
                s.underline = None;
            }
            3 => s.italic = true,
            23 => s.italic = false,
            4 => s.underline_kind = 1, // GHOSTTY_SGR_UNDERLINE_SINGLE
            24 => {
                s.underline_kind = 0;
                s.underline = None;
            }
            5 => s.blink = true,
            25 => s.blink = false,
            7 => s.inverse = true,
            27 => s.inverse = false,
            8 => s.invisible = true,
            28 => s.invisible = false,
            9 => s.strikethrough = true,
            29 => s.strikethrough = false,
            53 => s.overline = true,
            55 => s.overline = false,
            59 => s.underline = None, // default underline colour
            38 | 48 => {
                let which: &mut Option<StyleColor> = if v == 38 { &mut s.fg } else { &mut s.bg };
                let kind: Option<i32> = tok_at(&toks, i);
                i += 1;
                match kind {
                    Some(5) => {
                        let n = tok_at(&toks, i).unwrap_or(0) as u8;
                        i += 1;
                        *which = Some(StyleColor::Palette(n));
                    }
                    Some(2) => {
                        let r = tok_at(&toks, i).unwrap_or(0) as u8;
                        i += 1;
                        let g = tok_at(&toks, i).unwrap_or(0) as u8;
                        i += 1;
                        let b = tok_at(&toks, i).unwrap_or(0) as u8;
                        i += 1;
                        *which = Some(StyleColor::Rgb(r, g, b));
                    }
                    _ => {}
                }
            }
            39 => s.fg = None,
            49 => s.bg = None,
            _ => {}
        }
    }
}

/// The `i`-th token as an `i32` (out-of-range or unparseable → `None`).
fn tok_at(toks: &[String], i: usize) -> Option<i32> {
    toks.get(i).and_then(|t| t.parse::<i32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_vt_sys::StyleColor;

    #[test]
    fn parses_a_known_stream() {
        // B(bold), U(underline), C(palette 123), X(back to blank), width 5.
        let out = "\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[39mX\n";
        let grid = parse(out, 5);
        assert_eq!(grid.len(), 1);
        let row = &grid[0];
        assert_eq!(row.len(), 5);
        assert_eq!(row[0].text, "B");
        assert!(row[0].style.bold);
        assert_eq!(row[1].text, "U");
        assert_eq!(row[1].style.underline_kind, 1);
        assert_eq!(row[2].text, "C");
        assert_eq!(row[2].style.fg, Some(StyleColor::Palette(123)));
        assert_eq!(row[3].text, "X");
        assert_eq!(row[3].style, BLANK);
        assert_eq!(row[4].text, "");
        assert_eq!(row[4].style, BLANK);
    }

    #[test]
    fn a_truecolor_and_a_full_reset() {
        let out = "\x1b[38;2;255;128;0mD\x1b[0mE\n";
        let grid = parse(out, 2);
        assert_eq!(grid[0][0].text, "D");
        assert_eq!(grid[0][0].style.fg, Some(StyleColor::Rgb(255, 128, 0)));
        assert_eq!(grid[0][1].text, "E");
        assert_eq!(grid[0][1].style, BLANK);
    }

    #[test]
    fn a_bg_colour_and_its_default() {
        let out = "\x1b[48;5;5mZ\x1b[49mW\n";
        let grid = parse(out, 2);
        assert_eq!(grid[0][0].style.bg, Some(StyleColor::Palette(5)));
        assert_eq!(grid[0][1].style.bg, None);
    }

    #[test]
    fn short_rows_are_padded_to_the_width() {
        let out = "A\n\n\n";
        let grid = parse(out, 3);
        assert_eq!(grid.len(), 3);
        assert_eq!(grid[0][0].text, "A");
        assert_eq!(grid[0][1].text, "");
        assert_eq!(grid[0][2].text, "");
        assert_eq!(grid[1].len(), 3);
        assert_eq!(grid[2].len(), 3);
    }
}
