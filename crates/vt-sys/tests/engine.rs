//! Link + round-trip tests for the `vt-sys` FFI.
//!
//! These run against the *host* build of the vendored `libghostty-vt` — no
//! device, no daemon, milliseconds — and are the gate "FFI links the vendored
//! `.a`". They exercise the real C object through the raw accessor: create,
//! feed VT bytes, read state back, free.

use ldmux_vt_sys::Terminal;

#[test]
fn new_reports_its_own_dimensions() {
    let t = Terminal::new(80, 24).expect("terminal creates");
    assert_eq!(t.cols(), 80);
    assert_eq!(t.rows(), 24);
    // A fresh terminal's cursor is home: row 0, col 0.
    assert_eq!(t.cursor_x(), 0);
    assert_eq!(t.cursor_y(), 0);
}

#[test]
fn cursor_position_tracks_cup() {
    let mut t = Terminal::new(80, 24).unwrap();
    // CUP: \x1b[5;10H -> row 5, col 10 (1-indexed) -> (9, 4) 0-indexed.
    t.write(b"\x1b[5;10H");
    assert_eq!(t.cursor_x(), 9);
    assert_eq!(t.cursor_y(), 4);
    // CCH: \x1b[7G -> column 7 (1-indexed) -> col 6.
    t.write(b"\x1b[7G");
    assert_eq!(t.cursor_x(), 6);
}

#[test]
fn title_and_pwd_come_back_through_the_accessor() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.write(b"\x1b]2;hello title\x07");
    assert_eq!(t.title(), "hello title");
    // OSC 7 stores the raw bytes undecoded (the embedder decodes the URI).
    t.write(b"\x1b]7;file:///tmp\x07");
    assert_eq!(t.pwd(), "file:///tmp");
}

#[test]
fn scrollback_grows_past_the_viewport() {
    let mut t = Terminal::new(80, 24).unwrap();
    assert_eq!(t.scrollback_rows(), 0);
    // 30 lines in a 24-row screen -> 6 rows of history.
    t.write(&b"\n".repeat(30));
    assert!(t.scrollback_rows() > 0);
    assert!(t.total_rows() > 24);
}

#[test]
fn ground_is_reached_only_when_the_sequence_completes() {
    let mut t = Terminal::new(80, 24).unwrap();
    assert!(t.vt_ground());
    // An unterminated CUP: mid-sequence, not at ground.
    t.write(b"\x1b[1");
    assert!(!t.vt_ground());
    // Complete it: CUP row 1 col 2 -> back at ground.
    t.write(b";2H");
    assert!(t.vt_ground());
}

#[test]
fn resize_changes_the_cell_count() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.resize(120, 40);
    assert_eq!(t.cols(), 120);
    assert_eq!(t.rows(), 40);
}
