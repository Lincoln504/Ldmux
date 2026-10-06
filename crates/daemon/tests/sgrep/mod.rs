//! A test-only decoder for a captured, SGR-annotated screen dump (the
//! goldens in `tests/parity/*.capture`, and the output of
//! `ldmux_vt_sys::snapshot::render`): a row stream → a grid of `Cell`s, the
//! same shape the engine's `RenderState` walk returns.
//!
//! It exists so the §3.11 gate compares *cells*, not SGR bytes (plan
//! §3.11(3): "the cells are the source of truth; the format is ours"). It is
//! deliberately narrow — SGR (`m`) sequences, plus OSC strings which are
//! skipped (a hyperlink is not part of a `Cell`) — and panics on any other
//! escape: an unsupported escape is a harness defect, not data to guess at.
//!
//! Column width: a wide character (CJK, emoji) occupies two columns and
//! leaves a spacer cell with no text of its own; a zero-width combining mark
//! joins the previous cell's text.

use ldmux_vt_sys::sgr::BLANK;
use ldmux_vt_sys::{Cell, CellStyle, StyleColor};

/// Parse a full `capture-pane -p -e` output into rows of `Cell`s, each row
/// padded to `width` with empty cells.
///
/// - one character = one column, except wide characters (two) and combining
///   marks (zero) — see the module note;
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
        if c == '\x1b' && chars.peek() == Some(&']') {
            // An OSC string (e.g. an OSC 8 hyperlink): terminated by ST
            // (`ESC \`) or BEL. It carries no cell content.
            chars.next();
            while let Some(oc) = chars.next() {
                if oc == '\x07' {
                    break;
                }
                if oc == '\x1b' {
                    chars.next(); // the `\` of ST
                    break;
                }
            }
        } else if c == '\x1b' {
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
        } else if width_of(c) == 0 {
            // A combining mark joins the cell before it.
            if let Some(last) = row.iter_mut().rev().find(|cell| !cell.text.is_empty()) {
                last.text.push(c);
            }
        } else if width_of(c) == 2 {
            row.push(Cell {
                text: c.to_string(),
                style,
            });
            row.push(Cell {
                text: String::new(),
                style,
            });
            col += 2;
        } else {
            row.push(Cell {
                text: c.to_string(),
                style,
            });
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

/// Terminal column width of a character, for the ranges the corpus uses
/// (combining marks 0, CJK / emoji 2, everything else 1).
fn width_of(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Pad the current row to `width` with empty cells.
fn pad_row(row: &mut Vec<Cell>, col: &mut usize, width: usize) {
    while *col < width {
        row.push(Cell {
            text: String::new(),
            style: BLANK,
        });
        *col += 1;
    }
}

/// Apply one SGR parameter string to a style (the standard SGR table). Unknown codes are ignored (forward-compat;
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
            // The 16 ANSI colours are palette entries 0-15.
            30..=37 => s.fg = Some(StyleColor::Palette((v - 30) as u8)),
            90..=97 => s.fg = Some(StyleColor::Palette((v - 90 + 8) as u8)),
            40..=47 => s.bg = Some(StyleColor::Palette((v - 40) as u8)),
            100..=107 => s.bg = Some(StyleColor::Palette((v - 100 + 8) as u8)),
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
