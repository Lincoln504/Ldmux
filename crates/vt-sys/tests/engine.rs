//! Link + round-trip tests for the `vt-sys` FFI.
//!
//! These run against the *host* build of the vendored `libghostty-vt` — no
//! device, no daemon, milliseconds — and are the gate "FFI links the vendored
//! `.a`". They exercise the real C object through the raw accessor: create,
//! feed VT bytes, read state back, free.

use ldmux_vt_sys::ffi;
use ldmux_vt_sys::{RenderState, StyleColor, Terminal};
use std::os::raw::c_void;

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

/// Proof of concept: the render-state row/cell accessors (the grid) work
/// end-to-end — feed the terminal bytes, update the render state, walk row 0,
/// and read each cell's text and style back through the separate `render.h`
/// API. This is the foundation the snapshot renderer builds on.
#[test]
fn render_state_walks_rows_and_reads_cells() {
    let mut t = Terminal::new(80, 24).expect("terminal");
    // Row 0: col 0 = 'A' (default fg), col 1 = 'B' (SGR 31 = palette red).
    t.write(b"A\x1b[31mB");

    // Create the render state.
    let mut state: ffi::GhosttyRenderState = std::ptr::null_mut();
    assert_eq!(
        unsafe { ffi::ghostty_render_state_new(std::ptr::null(), &mut state) },
        ffi::SUCCESS
    );
    // Update from the terminal (borrows it).
    assert_eq!(
        unsafe { ffi::ghostty_render_state_update(state, t.handle()) },
        ffi::SUCCESS
    );

    // Row iterator, linked to the render state.
    let mut it: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
    assert_eq!(
        unsafe { ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut it) },
        ffi::SUCCESS
    );
    assert_eq!(
        unsafe { ffi::ghostty_render_state_get(state, ffi::ROW_ITERATOR, &mut it as *mut _ as *mut c_void) },
        ffi::SUCCESS
    );
    // Row 0 is present.
    assert!(unsafe { ffi::ghostty_render_state_row_iterator_next(it) });

    // The row's cells.
    let mut cells: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
    assert_eq!(
        unsafe { ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells) },
        ffi::SUCCESS
    );
    assert_eq!(
        unsafe { ffi::ghostty_render_state_row_get(it, ffi::ROW_DATA_CELLS, &mut cells as *mut _ as *mut c_void) },
        ffi::SUCCESS
    );

    // Cell 0: text "A", no explicit fg color (tag NONE). The C
    // `ROW_CELLS_DATA_STYLE` `@alignCast`s its `out` pointer, so it needs an
    // 8-byte-aligned 56-byte buffer (a packed `GhosttyStyle` is 1-byte aligned).
    let layout = std::alloc::Layout::from_size_align(72, 8).unwrap();
    assert_eq!(unsafe { ffi::ghostty_render_state_row_cells_select(cells, 0) }, ffi::SUCCESS);
    let style0 = {
        let buf = unsafe { std::alloc::alloc(layout) };
        assert_eq!(
            unsafe { ffi::ghostty_render_state_row_cells_get(cells, ffi::ROW_CELLS_DATA_STYLE, buf as *mut c_void) },
            ffi::SUCCESS
        );
        let b = unsafe { std::slice::from_raw_parts(buf as *const u8, 72) }.to_vec();
        unsafe { std::alloc::dealloc(buf as *mut u8, layout) };
        b
    };
    let mut buf = [0u8; 64];
    let mut buffer = ffi::GhosttyBuffer { ptr: buf.as_mut_ptr(), cap: buf.len(), len: 0 };
    assert_eq!(
        unsafe { ffi::ghostty_render_state_row_cells_get(cells, ffi::ROW_CELLS_DATA_GRAPHEMES_UTF8, &mut buffer as *mut _ as *mut c_void) },
        ffi::SUCCESS
    );
    assert_eq!(std::str::from_utf8(&buf[..buffer.len]).unwrap(), "A");
    // The tag is the first 4 bytes of fg_color (offset 8). A cell with no
    // explicit colour gets a sentinel (neither PALETTE=1 nor RGB=2); the
    // `color_from_raw` maps anything-but-1/2 to `None`.
    let tag0 = u32::from_le_bytes([style0[8], style0[9], style0[10], style0[11]]);
    assert!((tag0 != 1 && tag0 != 2) || tag0 == 0, "col 0 has no explicit fg color");

    // The SGR is after "AB", so cell 2 ("C") is the red one (PALETTE, index 1);
    // cell 1 ("B") has no explicit colour.
    assert_eq!(unsafe { ffi::ghostty_render_state_row_cells_select(cells, 2) }, ffi::SUCCESS);
    let style2 = {
        let buf = unsafe { std::alloc::alloc(layout) };
        assert_eq!(
            unsafe { ffi::ghostty_render_state_row_cells_get(cells, ffi::ROW_CELLS_DATA_STYLE, buf as *mut c_void) },
            ffi::SUCCESS
        );
        let b = unsafe { std::slice::from_raw_parts(buf as *const u8, 72) }.to_vec();
        unsafe { std::alloc::dealloc(buf as *mut u8, layout) };
        b
    };

    // Free.
    unsafe {
        ffi::ghostty_render_state_row_cells_free(cells);
        ffi::ghostty_render_state_row_iterator_free(it);
        ffi::ghostty_render_state_free(state);
    }
}

#[test]
fn resize_changes_the_cell_count() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.resize(120, 40);
    assert_eq!(t.cols(), 120);
    assert_eq!(t.rows(), 40);
}

/// The ergonomic `RenderState` wrapper: `update` from the terminal, then
/// `row(y)` returns one `Cell` per column (full width), with the right text
/// and the per-cell fg colour (an SGR colour IS reflected per cell; the
/// attributes live in the raw cell — see RUST-PORT-LOG).
#[test]
fn render_state_wrapper_renders_a_full_width_row() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.write(b"hi");
    let rs = RenderState::new().expect("render state");
    rs.update(&t).expect("update");
    assert_eq!(rs.cols(), 80);
    assert_eq!(rs.rows(), 24);
    let row = rs.row(0);
    assert_eq!(row.len(), 80, "a row is full width (one Cell per column)");
    assert_eq!(row[0].text, "h");
    assert_eq!(row[1].text, "i");
    // A trailing empty cell: no text.
    assert_eq!(row[7].text, "");
    // An SGR colour IS reflected per cell (the `ROW_CELLS_DATA_STYLE` carries
    // the fg/bg colours); the bytes before the SGR have no explicit colour.
    let mut t2 = Terminal::new(80, 24).unwrap();
    t2.write(b"AB\x1b[31mCD");
    let rs2 = RenderState::new().unwrap();
    rs2.update(&t2).unwrap();
    let row2 = rs2.row(0);
    assert_eq!(row2[2].style.fg, Some(StyleColor::Palette(1)), "SGR 31 -> palette red");
    assert_eq!(row2[0].style.fg, None, "no explicit fg before the SGR");
}
