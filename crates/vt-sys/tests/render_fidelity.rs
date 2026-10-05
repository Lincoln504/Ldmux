//! What the snapshot renderer does with the awkward cells: gaps, wide
//! characters, combining marks, hyperlinks. The render is parsed back to a
//! plain grid and compared with what a terminal would show.

use ldmux_vt_sys::{snapshot, Cell, RenderState, Terminal};

fn rows_of(bytes: &[u8], cols: u16, rows: u16) -> (Vec<Vec<Cell>>, String) {
    let mut t = Terminal::new(cols, rows).unwrap();
    t.write(bytes);
    let rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();
    let cells: Vec<Vec<Cell>> = (0..rs.rows()).map(|y| rs.row(y)).collect();
    let refs: Vec<&[Cell]> = cells.iter().map(|r| r.as_slice()).collect();
    let text = snapshot::render(&refs, rs.cols() as usize);
    (cells, text)
}

/// Strip SGR (`ESC [ ... m`) from a rendered line, leaving what a terminal
/// would lay out column by column.
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for n in it.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn width(s: &str) -> usize {
    // Wide characters in these tests are all East Asian wide / emoji = 2 columns.
    s.chars()
        .map(|c| match c as u32 {
            0x0300..=0x036F => 0, // combining
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0x1F300..=0x1FAFF => 2,
            _ => 1,
        })
        .sum()
}

/// Cells the program left unwritten between two writes are still columns. If
/// the render drops them, everything to the right of the gap slides left and
/// the replayed screen is wrong with no error anywhere.
#[test]
fn unwritten_cells_between_writes_keep_their_columns() {
    let (_, text) = rows_of(b"a\x1b[5Cb", 20, 2);
    let line0 = text.split('\n').next().unwrap();
    assert_eq!(plain(line0).chars().take(7).collect::<String>(), "a     b", "line: {line0:?}");
}

#[test]
fn a_cleared_region_then_drawn_text_keeps_position() {
    // The shape of every full-screen program: erase, then position and draw.
    let (_, text) = rows_of(b"\x1b[2J\x1b[3;10Hhello\x1b[5;1Hx", 30, 6);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(plain(lines[2]).trim_end().to_string(), "         hello");
    assert_eq!(plain(lines[4]).trim_end(), "x");
}

#[test]
fn every_row_keeps_its_columns_and_never_exceeds_the_grid() {
    let (_, text) = rows_of("a\x1b[5Cb\r\n\u{4e2d}\u{6587}x\r\ne\u{301}y".as_bytes(), 20, 4);
    let lines: Vec<String> = text.split('\n').map(plain).collect();
    assert_eq!(lines[0], "a     b");
    assert_eq!(lines[1], "\u{4e2d}\u{6587}x");
    assert_eq!(lines[2], "e\u{301}y");
    assert_eq!(lines[3], "");
    assert_eq!(text.split('\n').count(), 4, "one line per grid row");
    for (i, l) in lines.iter().enumerate() {
        assert!(width(l) <= 20, "row {i}: {l:?}");
    }
}

#[test]
fn wide_characters_take_two_columns_and_no_spacer_text() {
    let (cells, text) = rows_of("\u{4e2d}\u{6587}x".as_bytes(), 10, 1);
    let line = plain(text.split('\n').next().unwrap());
    assert_eq!(line, "\u{4e2d}\u{6587}x", "no stray cell text for the spacer halves");
    assert_eq!(width(&line), 5);
    // the cell list is one per column; the spacer after a wide char carries no text
    assert_eq!(cells[0].len(), 10);
    assert_eq!(cells[0][0].text, "\u{4e2d}");
    assert_eq!(cells[0][1].text, "");
    assert_eq!(cells[0][5].text, " ", "an unwritten cell is a blank column");
}

#[test]
fn a_combining_mark_stays_with_its_base() {
    let (_, text) = rows_of("e\u{301}x".as_bytes(), 5, 1);
    assert!(plain(text.split('\n').next().unwrap()).starts_with("e\u{301}x"));
}

/// A row whose last cell is styled must not leave that style active for the
/// next row: the rows are one string, and the style is stream state.
#[test]
fn a_style_ending_a_row_does_not_bleed_into_the_next() {
    // A full-width red-background bar, then a plain row.
    let mut input = b"\x1b[41m".to_vec();
    input.extend(std::iter::repeat_n(b' ', 10));
    input.extend_from_slice(b"\x1b[0m\r\nx");
    let (_, text) = rows_of(&input, 10, 2);
    let lines: Vec<&str> = text.split('\n').collect();
    // Interpret the stream: the second line starts from a clean state iff the
    // first line leaves SGR reset.
    let first_end_reset = lines[0].rfind("\x1b[0m").is_some_and(|i| lines[0][i..].starts_with("\x1b[0m") && !lines[0][i + 4..].contains("\x1b["));
    assert!(first_end_reset, "row 0 must end reset, got {:?}", lines[0]);
}

/// A styled blank (a status bar's background, an inverse space) is content;
/// only default-style trailing blanks may be dropped.
#[test]
fn styled_blanks_are_kept_and_plain_trailing_blanks_are_not_emitted() {
    let (_, text) = rows_of(b"hi\x1b[44m   \x1b[0m", 10, 1);
    let line = text.split('\n').next().unwrap();
    assert_eq!(plain(line), "hi   ", "the styled blanks stay: {line:?}");
    let (_, text) = rows_of(b"hi", 10, 1);
    assert_eq!(text, "hi", "trailing default blanks are not emitted");
}
