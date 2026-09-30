//! The SGR encoder: a cell's `CellStyle` → the minimal SGR sequence that
//! renders it, relative to a previous style.
//!
//! This is the pure half of the snapshot renderer (plan §3.4 / §3.11): the
//! cells (the source of truth, from the ghostty-vt render state) are turned
//! into tmux-`capture-pane -e`-identical bytes. The renderer
//! (`snapshot.rs`) walks the cells row by row and asks this module for the
//! escape to emit *between* a previous style and the current one, so the
//! output is a run-length stream: a style run is emitted once, then its text.
//!
//! The mapping is the standard SGR parameter table (the same codes tmux and
//! every xterm-family terminal understand):
//!
//! | attribute     | on            | off          |
//! |---------------|---------------|--------------|
//! | reset all     | `0`           | —           |
//! | bold          | `1`           | `21`         |
//! | faint         | `2`           | —           |
//! | italic        | `3`           | `23`         |
//! | underline     | `4`           | `24`         |
//! | blink         | `5`           | `25`         |
//! | inverse       | `7`           | `27`         |
//! | invisible     | `8`           | `28`         |
//! | strikethrough | `9`           | `29`         |
//! | overline      | `53`          | `55`         |
//! | fg colour     | `38;5;n` / `38;2;r;g;b` | `39` |
//! | bg colour     | `48;5;n` / `48;2;r;g;b` | `49` |
//!
//! A `capture-pane -e` row always *starts* from the default style (a `0`), so
//! the first transition is from a blank style.

use ldmux_vt_sys::{CellStyle, StyleColor};

/// The blank style: every attribute off, no explicit colour. This is the
/// "before" style for the first cell of a row (a `capture-pane -e` row
/// always starts from the default).
pub const BLANK: CellStyle = CellStyle {
    fg: None,
    bg: None,
    underline: None,
    bold: false,
    italic: false,
    faint: false,
    blink: false,
    inverse: false,
    invisible: false,
    strikethrough: false,
    overline: false,
    underline_kind: 0,
};

/// Encode the transition from `from` to `to` as the minimal SGR sequence.
///
/// The output is always terminated with `m` (or empty, if nothing changed).
/// The attributes are emitted in a fixed order (reset, bold, faint, italic,
/// underline, blink, inverse, invisible, strikethrough, overline, fg, bg) so
/// the output is deterministic for a given pair of styles.
pub fn transition(from: &CellStyle, to: &CellStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Reset-if-off: if `from` has an attribute `to` does not, emit its "off"
    // code (unless `to` is blank, in which case a single `0` covers all).
    // Colours: if `from` has an explicit colour `to` does not, emit the
    // default-colour code (39/49).
    // New attributes: emit the "on" codes.
    //
    // The simplest correct encoding: if the two styles differ, emit a full
    // re-description of `to` (a `0` then every "on" attribute of `to`). This
    // is what tmux does in practice for a cell whose style changed: it does
    // not diff the bits, it re-issues the whole style. It is also the only
    // encoding that is byte-stable across the corpus (a diff would depend on
    // the order of the changes, which the corpus does not pin).
    if from != to {
        let mut sgr = vec!["0".to_string()];
        if to.bold { sgr.push("1".into()); }
        if to.faint { sgr.push("2".into()); }
        if to.italic { sgr.push("3".into()); }
        if to.underline_kind != 0 { sgr.push("4".into()); }
        if to.blink { sgr.push("5".into()); }
        if to.inverse { sgr.push("7".into()); }
        if to.invisible { sgr.push("8".into()); }
        if to.strikethrough { sgr.push("9".into()); }
        if to.overline { sgr.push("53".into()); }
        if let Some(fg) = &to.fg {
            sgr.push(fg_code(fg));
        }
        if let Some(bg) = &to.bg {
            sgr.push(bg_code(bg));
        }
        parts.push(format!("\x1b[{}m", sgr.join(";")));
    }
    parts.join("")
}

/// The `38;…` code for a foreground colour.
fn fg_code(c: &StyleColor) -> String {
    match c {
        StyleColor::Palette(n) => format!("38;5;{n}"),
        StyleColor::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
    }
}

/// The `48;…` code for a background colour.
fn bg_code(c: &StyleColor) -> String {
    match c {
        StyleColor::Palette(n) => format!("48;5;{n}"),
        StyleColor::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> CellStyle {
        BLANK
    }

    #[test]
    fn blank_to_blank_is_empty() {
        assert_eq!(transition(&style(), &style()), "");
    }

    #[test]
    fn blank_to_bold_is_a_full_reissue() {
        let mut to = style();
        to.bold = true;
        assert_eq!(transition(&style(), &to), "\x1b[0;1m");
    }

    #[test]
    fn blank_to_palette_red_is_38() {
        let mut to = style();
        to.fg = Some(StyleColor::Palette(1));
        assert_eq!(transition(&style(), &to), "\x1b[0;38;5;1m");
    }

    #[test]
    fn truecolor_is_38_2() {
        let mut to = style();
        to.fg = Some(StyleColor::Rgb(200, 100, 50));
        assert_eq!(transition(&style(), &to), "\x1b[0;38;2;200;100;50m");
    }

    #[test]
    fn bold_plus_inverse_is_both() {
        let mut to = style();
        to.bold = true;
        to.inverse = true;
        assert_eq!(transition(&style(), &to), "\x1b[0;1;7m");
    }

    #[test]
    fn a_change_is_a_reissue_not_a_diff() {
        let mut a = style();
        a.bold = true;
        let mut b = style();
        b.inverse = true;
        // `a` (bold) → `b` (inverse): a full re-issue of `b` (a `0` then `7`).
        assert_eq!(transition(&a, &b), "\x1b[0;7m");
    }
}
