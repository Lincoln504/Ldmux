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
        // SAFETY: `state` is a writable handle slot and the allocator is NULL (default); the
        // handle is freed once at the end of the test.
        unsafe { ffi::ghostty_render_state_new(std::ptr::null(), &mut state) },
        ffi::SUCCESS
    );
    // Update from the terminal (borrows it).
    assert_eq!(
        // SAFETY: `state` was created above and `t.handle()` is the live terminal `t`, which
        // outlives the call.
        unsafe { ffi::ghostty_render_state_update(state, t.handle()) },
        ffi::SUCCESS
    );

    // Row iterator, linked to the render state.
    let mut it: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
    assert_eq!(
        // SAFETY: `it` is a writable handle slot and the allocator is NULL.
        unsafe { ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut it) },
        ffi::SUCCESS
    );
    assert_eq!(
        // SAFETY: `state` is live; ROW_ITERATOR's out type is a row-iterator handle and `it`
        // is exactly one (a valid, writable slot), with a matching pointer cast.
        unsafe {
            ffi::ghostty_render_state_get(
                state,
                ffi::ROW_ITERATOR,
                &mut it as *mut _ as *mut c_void,
            )
        },
        ffi::SUCCESS
    );
    // Row 0 is present.
    // SAFETY: `it` is a live iterator linked to `state` above.
    assert!(unsafe { ffi::ghostty_render_state_row_iterator_next(it) });

    // The row's cells.
    let mut cells: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
    assert_eq!(
        // SAFETY: `cells` is a writable handle slot and the allocator is NULL.
        unsafe { ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells) },
        ffi::SUCCESS
    );
    assert_eq!(
        // SAFETY: `it` is live and positioned on row 0; ROW_DATA_CELLS's out type is a
        // row-cells handle and `cells` is exactly one (a valid, writable slot).
        unsafe {
            ffi::ghostty_render_state_row_get(
                it,
                ffi::ROW_DATA_CELLS,
                &mut cells as *mut _ as *mut c_void,
            )
        },
        ffi::SUCCESS
    );

    // Cell 0: text "A", no explicit fg color (tag NONE). The C
    // `ROW_CELLS_DATA_STYLE` `@alignCast`s its `out` pointer, so it needs an
    // 8-byte-aligned 56-byte buffer (a packed `GhosttyStyle` is 1-byte aligned).
    let layout = std::alloc::Layout::from_size_align(72, 8).unwrap();
    assert_eq!(
        // SAFETY: `cells` is the live handle created above; column 0 exists in an 80-column row.
        unsafe { ffi::ghostty_render_state_row_cells_select(cells, 0) },
        ffi::SUCCESS
    );
    let style0 = {
        // SAFETY: `layout` has non-zero size (72, align 8). Zeroed, so every one of
        // the 72 bytes is initialised before the slice below is formed even if
        // the library leaves some of them (padding) unwritten.
        let buf = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!buf.is_null(), "allocation failed");
        // SAFETY: `cells` is selected on a live cell; `buf` is 72 bytes, 8-aligned
        // (the alignment `ROW_CELLS_DATA_STYLE` casts its out pointer to) and
        // writable, which is the size of `GhosttyStyle`.
        let got = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                cells,
                ffi::ROW_CELLS_DATA_STYLE,
                buf as *mut c_void,
            )
        };
        assert_eq!(got, ffi::SUCCESS);
        // SAFETY: `buf` is a live 72-byte allocation, fully initialised (zeroed,
        // then written by C), and any bytes are valid `u8`s; copied out before the
        // dealloc below.
        let b = unsafe { std::slice::from_raw_parts(buf as *const u8, 72) }.to_vec();
        // SAFETY: `buf` was allocated above with this same `layout` and is not
        // used again.
        unsafe { std::alloc::dealloc(buf, layout) };
        b
    };
    let mut buf = [0u8; 64];
    let mut buffer = ffi::GhosttyBuffer {
        ptr: buf.as_mut_ptr(),
        cap: buf.len(),
        len: 0,
    };
    assert_eq!(
        // SAFETY: `cells` is selected on a live cell; GRAPHEMES_UTF8's out type is a
        // `GhosttyBuffer`, and `buffer` is one whose `ptr`/`cap` describe the live,
        // writable 64-byte `buf`.
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                cells,
                ffi::ROW_CELLS_DATA_GRAPHEMES_UTF8,
                &mut buffer as *mut _ as *mut c_void,
            )
        },
        ffi::SUCCESS
    );
    assert_eq!(std::str::from_utf8(&buf[..buffer.len]).unwrap(), "A");
    // The tag is the first 4 bytes of fg_color (offset 8). A cell with no
    // explicit colour gets a sentinel (neither PALETTE=1 nor RGB=2); the
    // `color_from_raw` maps anything-but-1/2 to `None`.
    let tag0 = u32::from_le_bytes([style0[8], style0[9], style0[10], style0[11]]);
    // Neither PALETTE (1) nor RGB (2): no explicit colour. (A "no colour"
    // cell carries a sentinel tag, e.g. 0xfffffffe — anything-but-1/2.)
    assert!(
        tag0 != 1 && tag0 != 2,
        "col 0 has no explicit fg color (tag {tag0:#x})"
    );

    // The SGR is after "A", so cell 1 ("B") is the red one (PALETTE, index 1).
    // SAFETY: `cells` is the live handle created above; column 1 exists in an 80-column row.
    let rc = unsafe { ffi::ghostty_render_state_row_cells_select(cells, 1) };
    assert_eq!(rc, ffi::SUCCESS);
    let style1 = {
        // SAFETY: `layout` has non-zero size (72, align 8). Zeroed, so every one of
        // the 72 bytes is initialised before the slice below is formed even if
        // the library leaves some of them (padding) unwritten.
        let buf = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!buf.is_null(), "allocation failed");
        // SAFETY: `cells` is selected on a live cell; `buf` is 72 bytes, 8-aligned
        // (the alignment `ROW_CELLS_DATA_STYLE` casts its out pointer to) and
        // writable, which is the size of `GhosttyStyle`.
        let got = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                cells,
                ffi::ROW_CELLS_DATA_STYLE,
                buf as *mut c_void,
            )
        };
        assert_eq!(got, ffi::SUCCESS);
        // SAFETY: `buf` is a live 72-byte allocation, fully initialised (zeroed,
        // then written by C), and any bytes are valid `u8`s; copied out before the
        // dealloc below.
        let b = unsafe { std::slice::from_raw_parts(buf as *const u8, 72) }.to_vec();
        // SAFETY: `buf` was allocated above with this same `layout` and is not
        // used again.
        unsafe { std::alloc::dealloc(buf, layout) };
        b
    };
    // fg_color is at offset 8 (16 bytes: tag @8, value @16).
    let tag1 = u32::from_le_bytes([style1[8], style1[9], style1[10], style1[11]]);
    let val1 = u32::from_le_bytes([style1[16], style1[17], style1[18], style1[19]]);
    assert_eq!(tag1, 1, "cell 1 fg is PALETTE");
    assert_eq!(val1, 1, "cell 1 fg palette index is 1");

    // Free.
    // SAFETY: each handle was created in this test, is freed exactly once, and
    // nothing uses them afterwards (`cells` and `it` before `state`, which they
    // were linked to).
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
/// attributes live in the raw cell).
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
    // A cell the program never wrote is a blank *column* (a space), not "no text":
    // only the second half of a wide character has no text, and dropping an
    // unwritten cell slides everything to its right left.
    assert_eq!(row[7].text, " ");
    // An SGR colour IS reflected per cell (the `ROW_CELLS_DATA_STYLE` carries
    // the fg/bg colours); the bytes before the SGR have no explicit colour.
    let mut t2 = Terminal::new(80, 24).unwrap();
    t2.write(b"AB\x1b[31mCD");
    let rs2 = RenderState::new().unwrap();
    rs2.update(&t2).unwrap();
    let row2 = rs2.row(0);
    assert_eq!(
        row2[2].style.fg,
        Some(StyleColor::Palette(1)),
        "SGR 31 -> palette red"
    );
    assert_eq!(row2[0].style.fg, None, "no explicit fg before the SGR");
}
