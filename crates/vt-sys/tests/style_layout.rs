//! The hand-measured `GhosttyStyle` offsets, checked against the live engine: every
//! attribute is set through real SGR and must come back out of a real cell. The
//! compile-time asserts in `safe.rs` pin the Rust mirror; this pins the *library*,
//! so an engine bump that moves a field fails here, loudly, instead of painting the
//! wrong colour.

use ldmux_vt_sys::{Cell, RenderState, StyleColor, Terminal};

fn cells(bytes: &[u8], cols: u16) -> Vec<Cell> {
    let mut t = Terminal::new(cols, 2).unwrap();
    t.write(bytes);
    let rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();
    rs.row(0)
}

#[test]
fn every_boolean_attribute_decodes_from_its_own_offset() {
    // One attribute per cell, so a misplaced offset shows as the wrong flag.
    let c = cells(
        b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[2mF\x1b[0m\x1b[5mK\x1b[0m\x1b[7mV\x1b[0m\
          \x1b[8mH\x1b[0m\x1b[9mS\x1b[0m\x1b[53mO\x1b[0m\x1b[4mU\x1b[0mn",
        20,
    );
    let s = |i: usize| c[i].style;
    assert!(s(0).bold && !s(0).italic);
    assert!(s(1).italic && !s(1).bold);
    assert!(s(2).faint);
    assert!(s(3).blink);
    assert!(s(4).inverse);
    assert!(s(5).invisible);
    assert!(s(6).strikethrough);
    assert!(s(7).overline);
    assert_eq!(s(8).underline_kind, 1);
    for (i, cell) in c.iter().take(9).enumerate() {
        let st = cell.style;
        let on = [
            st.bold,
            st.italic,
            st.faint,
            st.blink,
            st.inverse,
            st.invisible,
            st.strikethrough,
            st.overline,
        ]
        .iter()
        .filter(|b| **b)
        .count();
        assert!(
            on <= 1,
            "cell {i} has {on} attributes set; an offset is off"
        );
    }
    assert_eq!(c[9].style.underline_kind, 0);
}

#[test]
fn the_three_colours_and_the_underline_kind_decode() {
    let c = cells(b"\x1b[38;5;5;48;2;10;20;30;4:3;58;2;200;100;50mX", 4);
    let st = c[0].style;
    assert_eq!(st.fg, Some(StyleColor::Palette(5)));
    assert_eq!(st.bg, Some(StyleColor::Rgb(10, 20, 30)));
    assert_eq!(st.underline, Some(StyleColor::Rgb(200, 100, 50)));
    assert_eq!(st.underline_kind, 3);
    // And the palette form of the underline colour.
    let c = cells(b"\x1b[4:5;58;5;9mX", 4);
    assert_eq!(c[0].style.underline, Some(StyleColor::Palette(9)));
    assert_eq!(c[0].style.underline_kind, 5);
}
