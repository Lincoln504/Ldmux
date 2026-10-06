//! The scrollback mechanism (plan §3.4: history pages) — an empirical gate on
//! the C-API behaviour the emulator's history snapshot will rely on:
//! scrollback accumulates, `scroll_viewport` slides the viewport, the render
//! state reflects the scrolled position, and `Bottom` restores the live
//! screen. These are the invariants the `Snapshot { history_lines }` render
//! is built on, so they are pinned here, not assumed.
//!
//! Layout (measured, 80x10, `set_max_scrollback(100)`, 30 lines `L0000`..
//! `L0029` each `X\r\n`): the last line lands on row 8, the cursor sits on
//! the empty row 9, `scrollback_rows` = 21 (L0000..L0020), `total_rows` = 31.
//! `Delta(-5)` puts the viewport top on L0016 (rows 0..4 = L0016..L0020);
//! `Bottom` restores the live screen (row 8 = L0029).

use ldmux_vt_sys::{RenderState, ScrollViewport, Terminal};

/// Join one rendered row's cells into its text (the grapheme per column).
fn row_text(rs: &RenderState, y: u16) -> String {
    rs.row(y)
        .iter()
        .map(|c| c.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Write `n` numbered lines (`L0000` .. `L{n-1}`, each `X\r\n`).
fn feed(t: &mut Terminal, n: u32) {
    for i in 0..n {
        let line = format!("L{i:04}\r\n");
        t.write(line.as_bytes());
    }
}

#[test]
fn scrollback_accumulates_and_pages() {
    let mut t = Terminal::new(80, 10).expect("terminal creates");
    t.set_max_scrollback(100);
    feed(&mut t, 30);

    // Scrollback accumulated (the grid retained the lines that scrolled off).
    let sb = t.scrollback_rows();
    assert!(sb >= 20, "expected ~20 history rows, got {sb}");
    assert_eq!(t.total_rows(), 10 + sb, "total = live + history");

    let rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();

    // The live screen: the last written line is on row 8; row 9 is the empty
    // cursor row.
    assert_eq!(row_text(&rs, 8), "L0029", "live last written line");
    assert_eq!(row_text(&rs, 9), "", "cursor row is empty");

    // Scroll up 5: the viewport slides into history; the top row is L0016
    // (the 5th history line above the live screen).
    t.scroll_viewport(ScrollViewport::Delta(-5));
    rs.update(&t).unwrap();
    assert!(!t.viewport_active(), "scrolled into history");
    assert_eq!(row_text(&rs, 0), "L0016", "scrolled top row");

    // Restore: the live screen comes back.
    t.scroll_viewport(ScrollViewport::Bottom);
    rs.update(&t).unwrap();
    assert!(t.viewport_active(), "back at the bottom");
    assert_eq!(row_text(&rs, 8), "L0029", "restored live");
}

/// A full history capture: N history lines + the full live screen, in the
/// order the client replays (history first, then the visible screen).
/// DEC private modes (SGR mouse, bracketed paste, application cursor keys,
/// cursor visibility) and the alternate screen are tracked by the emulator
/// from the fed stream, queryable through `DATA_MODE` / `DATA_ACTIVE_SCREEN`
/// — the sources of the state fields `mouseSgr`, `bracketedPaste`,
/// `appCursor`, `alternateOn`.
#[test]
fn terminal_modes_and_alternate_screen_are_tracked() {
    use ldmux_vt_sys::{MODE_APP_CURSOR, MODE_BRACKETED_PASTE, MODE_SGR_MOUSE};
    let mut t = Terminal::new(80, 10).unwrap();
    // Start clean: the modes off, cursor visible, on the primary screen.
    // (Mode 1 — DECCKM, `appCursor` — is off by default; mode 25 — cursor
    // visible — is on. The two are distinct; see the `MODE_APP_CURSOR` note.)
    assert!(!t.mode(MODE_SGR_MOUSE));
    assert!(!t.mode(MODE_BRACKETED_PASTE));
    assert!(!t.mode(MODE_APP_CURSOR));
    assert!(t.cursor_visible());
    assert!(!t.alternate_on());

    // The program enables SGR mouse (?1006), bracketed paste (?2004),
    // application cursor keys (?1), hides the cursor (?25l), and enters the
    // alternate screen (?1047).
    t.write(b"\x1b[?1006h\x1b[?2004h\x1b[?1h\x1b[?25l\x1b[?1047h");
    assert!(t.mode(MODE_SGR_MOUSE), "SGR mouse on after ?1006h");
    assert!(
        t.mode(MODE_BRACKETED_PASTE),
        "bracketed paste on after ?2004h"
    );
    assert!(
        t.mode(MODE_APP_CURSOR),
        "application cursor keys on after ?1h"
    );
    assert!(!t.cursor_visible(), "cursor hidden after ?25l");
    assert!(t.alternate_on(), "alternate screen on after ?1047h");

    // Restore: the modes off, cursor shown, primary screen.
    t.write(b"\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[?25h\x1b[?1047l");
    assert!(!t.mode(MODE_SGR_MOUSE), "SGR mouse off after ?1006l");
    assert!(
        !t.mode(MODE_BRACKETED_PASTE),
        "bracketed paste off after ?2004l"
    );
    assert!(
        !t.mode(MODE_APP_CURSOR),
        "application cursor keys off after ?1l"
    );
    assert!(t.cursor_visible(), "cursor shown after ?25h");
    assert!(!t.alternate_on(), "primary screen after ?1047l");
}

#[test]
fn history_capture_is_history_plus_live() {
    let mut t = Terminal::new(80, 10).expect("terminal creates");
    t.set_max_scrollback(100);
    feed(&mut t, 30);
    let rs = RenderState::new().unwrap();

    let n = 5usize;
    // 1. The live screen, at the bottom (10 rows).
    t.scroll_viewport(ScrollViewport::Bottom);
    rs.update(&t).unwrap();
    let live: Vec<String> = (0..10).map(|y| row_text(&rs, y as u16)).collect();
    assert_eq!(live[8], "L0029", "live last written line");

    // 2. The last N history lines: scroll up N, read the top N rows.
    t.scroll_viewport(ScrollViewport::Delta(-(n as i32)));
    rs.update(&t).unwrap();
    let hist: Vec<String> = (0..n).map(|y| row_text(&rs, y as u16)).collect();
    // 3. Restore the live screen.
    t.scroll_viewport(ScrollViewport::Bottom);
    rs.update(&t).unwrap();

    // The capture is the last N history lines, in order (oldest first):
    // L0016..L0020 — the run immediately above the live screen.
    let expect = ["L0016", "L0017", "L0018", "L0019", "L0020"];
    assert_eq!(hist.len(), expect.len());
    for (i, e) in expect.iter().enumerate() {
        assert_eq!(hist[i].as_str(), *e, "history line {i}");
    }
    // The live screen is intact after the capture (the restore worked).
    assert_eq!(row_text(&rs, 8), "L0029", "live intact after capture");
}
