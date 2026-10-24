//! The ergonomic named-accessor wrapper over the raw FFI — the layer the plan
//! calls "the ~20 named accessors" (plan §3.4 / D2). One `Terminal` value owns
//! one C terminal; it is `!Send`/`!Sync` (a raw handle), so it is owned by one
//! task per session, exactly as the C object is meant to be used.

use crate::ffi;
use std::os::raw::c_void;

/// An FFI call failed (a non-`SUCCESS` `GhosttyResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiError {
    pub code: i32,
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "libghostty-vt returned code {}", self.code)
    }
}

impl std::error::Error for FfiError {}

/// The DEC private mode identifiers we query (plan §3.4 state fields). A DEC
/// mode packs to its bare value in `GhosttyMode` (bit 15 = 0), so these are
/// the numbers straight from `modes.h`.
///
/// `MODE_APP_CURSOR` is DECCKM (mode 1), *not* the plan's "DECTCEM" (mode 25):
/// the plan's label was wrong, and the empirical gate caught it — mode 25 is
/// "cursor *visible*" (on by default, stays on), which is not application
/// cursor-key mode (off by default, tracks `?1h`/`?1l`). Mode 1 is the faithful
/// source.
pub const MODE_APP_CURSOR: u16 = 1; // `appCursor` (DECCKM, application cursor keys)
pub const MODE_SGR_MOUSE: u16 = 1006; // `mouseSgr`
pub const MODE_BRACKETED_PASTE: u16 = 2004; // `bracketedPaste`

/// One terminal instance: the per-session emulator handle.
///
/// Owns the C `GhosttyTerminal`; freed on drop. `!Send`/`!Sync` (it wraps a raw
/// pointer), so it must live in one task — the keeper's per-session task.
pub struct Terminal {
    ptr: ffi::GhosttyTerminal,
    /// Pixel size of one cell, applied on every resize. Arbitrary headless:
    /// there is no renderer to size cells for; it only feeds image protocols
    /// and size reports, which the skeleton does not exercise.
    cell: (u32, u32),
}

impl Terminal {
    /// Create a `cols`×`rows` terminal on the default allocator.
    pub fn new(cols: u16, rows: u16) -> Result<Self, FfiError> {
        let mut ptr: ffi::GhosttyTerminal = std::ptr::null_mut();
        // SAFETY: a NULL allocator selects the default one; `ptr` is a writable
        // slot the library fills with a new terminal (checked non-null below).
        let r = unsafe { ffi::ghostty_terminal_new(std::ptr::null(), &mut ptr, cols, rows) };
        if r != ffi::SUCCESS || ptr.is_null() {
            return Err(FfiError { code: r });
        }
        Ok(Self {
            ptr,
            cell: (10, 20),
        })
    }

    /// Feed raw bytes through the VT parser (the Write direction into the
    /// emulator). Never fails: the parser keeps state consistent on any input.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // SAFETY: `self.ptr` is the live terminal this value owns (non-null from
        // `new`, freed only in `drop`); `bytes` is a valid slice read for its length
        // only; `&mut self` plus `!Send` means no concurrent use of the handle.
        unsafe { ffi::ghostty_terminal_vt_write(self.ptr, bytes.as_ptr(), bytes.len()) };
    }

    /// The raw C handle (the render state updates from it).
    pub fn handle(&self) -> ffi::GhosttyTerminal {
        self.ptr
    }

    /// Change the cell count (the Resize direction into the emulator).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cw, ch) = self.cell;
        // SAFETY: `self.ptr` is the live terminal this value owns; integer args.
        let _ = unsafe { ffi::ghostty_terminal_resize(self.ptr, cols, rows, cw, ch) };
    }

    /// Cap the retained scrollback to `lines` physical lines (the
    /// `history-limit` analogue, plan §3.4/§3.7). Set once after creation,
    /// before any input, so the grid history is bounded from the start.
    /// `lines == 0` disables scrollback (erases retained history).
    pub fn set_max_scrollback(&self, lines: usize) {
        let v = lines;
        // SAFETY: `self.ptr` is the live terminal this value owns. This option's
        // value type is a `usize` read through the pointer, and `v` is a live
        // `usize` local that the library copies and does not retain.
        let r = unsafe {
            ffi::ghostty_terminal_set(
                self.ptr,
                ffi::OPT_SCROLLBACK_MAX_LINES,
                &v as *const _ as *const c_void,
            )
        };
        debug_assert_eq!(r, ffi::SUCCESS, "set_max_scrollback");
    }

    /// Cap the retained scrollback to `bytes` (the `scrollback-limit` byte
    /// budget, the other half of the pair above).
    ///
    /// **Both are needed.** The line cap alone does not decide what is retained:
    /// the byte budget binds first — measured 2026-10-05, a terminal created with
    /// the C API's own defaults retained 995 rows after 12,000 lines, and one with
    /// `set_max_scrollback(10_000)` alone retained 913 of 1,500 (a megabyte or so
    /// either way). Raising the line cap without this one changes nothing.
    /// `bytes == 0` disables scrollback.
    pub fn set_max_scrollback_bytes(&self, bytes: usize) {
        let v = bytes;
        // SAFETY: `self.ptr` is the live terminal this value owns. This option's
        // value type is a `usize` read through the pointer, and `v` is a live
        // `usize` local that the library copies and does not retain.
        let r = unsafe {
            ffi::ghostty_terminal_set(
                self.ptr,
                ffi::OPT_SCROLLBACK_MAX_BYTES,
                &v as *const _ as *const c_void,
            )
        };
        debug_assert_eq!(r, ffi::SUCCESS, "set_max_scrollback_bytes");
    }

    /// Scroll the terminal viewport (the history-page mechanism, plan §3.4).
    /// The live pane always sits at `Bottom`; a snapshot with history
    /// scrolls up, renders, and scrolls back — all on the owning thread.
    pub fn scroll_viewport(&mut self, how: ScrollViewport) {
        let b = match how {
            ScrollViewport::Top => ffi::GhosttyTerminalScrollViewport {
                tag: ffi::SCROLL_VIEWPORT_TOP,
                value: ffi::GhosttyTerminalScrollViewportValue { _padding: [0; 2] },
            },
            ScrollViewport::Bottom => ffi::GhosttyTerminalScrollViewport {
                tag: ffi::SCROLL_VIEWPORT_BOTTOM,
                value: ffi::GhosttyTerminalScrollViewportValue { _padding: [0; 2] },
            },
            ScrollViewport::Delta(d) => ffi::GhosttyTerminalScrollViewport {
                tag: ffi::SCROLL_VIEWPORT_DELTA,
                value: ffi::GhosttyTerminalScrollViewportValue { delta: d as isize },
            },
            ScrollViewport::Row(r) => ffi::GhosttyTerminalScrollViewport {
                tag: ffi::SCROLL_VIEWPORT_ROW,
                value: ffi::GhosttyTerminalScrollViewportValue { row: r as usize },
            },
        };
        // SAFETY: `self.ptr` is the live terminal this value owns; `b` is a fully
        // initialised tagged union passed by value, whose active field matches its
        // tag (every variant writes the whole 16-byte union or the field the
        // tag selects, over a zero-initialised padding member for Top/Bottom).
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.ptr, b) };
    }

    // ── Scalar accessors (through the one generic `get`) ─────────────────

    /// Terminal width in cells.
    pub fn cols(&self) -> u16 {
        self.get_u16(ffi::DATA_COLS)
    }

    /// Terminal height in cells.
    pub fn rows(&self) -> u16 {
        self.get_u16(ffi::DATA_ROWS)
    }

    /// Cursor column (0-indexed).
    pub fn cursor_x(&self) -> u16 {
        self.get_u16(ffi::DATA_CURSOR_X)
    }

    /// Cursor row (0-indexed, within the active area).
    pub fn cursor_y(&self) -> u16 {
        self.get_u16(ffi::DATA_CURSOR_Y)
    }

    /// Whether the cursor has a pending soft-wrap.
    pub fn cursor_pending_wrap(&self) -> bool {
        self.get_bool(ffi::DATA_CURSOR_PENDING_WRAP)
    }

    /// Whether the cursor is visible (DEC mode 25).
    pub fn cursor_visible(&self) -> bool {
        self.get_bool(ffi::DATA_CURSOR_VISIBLE)
    }

    /// Whether any mouse-tracking mode is active.
    pub fn mouse_tracking(&self) -> bool {
        self.get_bool(ffi::DATA_MOUSE_TRACKING)
    }

    /// Give the terminal somewhere to write its own replies.
    ///
    /// A terminal answers what it is asked: device attributes (DA1), cursor
    /// position and status reports (DSR/CPR), mode reports (DECRQM), its version
    /// (XTVERSION), the Kitty keyboard query, mouse/alternate-screen reports. The
    /// emulator parses those queries perfectly and then, with no write-back
    /// registered, drops the answer on the floor — and the daemon's sanitizer
    /// stops them reaching a client (a client's answer would arrive as typed
    /// input). So a program inside a session that asked "where is the cursor"
    /// waited forever, and `pi` could not learn that the terminal distinguishes
    /// `Shift+Enter` from `Enter`.
    ///
    /// `userdata` is handed back to every call; the keeper passes its pty master.
    ///
    /// # Safety
    ///
    /// `f` must be safe to call synchronously from inside `write`, must not
    /// re-enter the terminal, and must not retain `data` after returning.
    /// `userdata` must stay valid for the terminal's lifetime.
    pub unsafe fn set_write_pty(&mut self, f: ffi::WritePtyFn, userdata: *mut c_void) {
        // SAFETY: `self.ptr` is the live terminal this value owns; `userdata`'s
        // contract is the caller's (see above); this option's value type is a
        // plain `void*` the library only stores and hands back.
        let _ = unsafe {
            ffi::ghostty_terminal_set(self.ptr, ffi::OPT_USERDATA, userdata as *const c_void)
        };
        // SAFETY: as above, and this option's value type is a function pointer
        // with the `GhosttyTerminalWritePtyFn` shape.
        let _ = unsafe {
            ffi::ghostty_terminal_set(self.ptr, ffi::OPT_WRITE_PTY, f as *const c_void)
        };
    }

    /// The Kitty keyboard protocol flags the program has set (`CSI > Ps u`),
    /// as a bitset: 1 disambiguate, 2 event types, 4 alternate keys, 8 all keys
    /// as escape codes, 16 associated text.
    ///
    /// The daemon needs this to encode a modified Enter, Escape or Space: those
    /// keys have no legacy byte sequence that carries the modifier, so a
    /// terminal without the protocol cannot tell `Shift+Enter` from `Enter`.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        let mut v: u8 = 0;
        // SAFETY: `self.ptr` is the live terminal this value owns;
        // `DATA_KITTY_KEYBOARD_FLAGS` writes one `GhosttyKittyKeyFlags` (the
        // header says `uint8_t *`), and `v` is a live, aligned `u8`.
        let _ = unsafe {
            ffi::ghostty_terminal_get(
                self.ptr,
                ffi::DATA_KITTY_KEYBOARD_FLAGS,
                &mut v as *mut _ as *mut c_void,
            )
        };
        v
    }

    /// The current value of a terminal mode, queried through `DATA_MODE`.
    /// `mode` is the packed mode identifier (a bare value for DEC private
    /// modes: 25 = cursor visible, 1006 = SGR mouse, 2004 = bracketed
    /// paste). An unknown mode leaves the value false.
    pub fn mode(&self, mode: u16) -> bool {
        let mut cfg = ffi::GhosttyTerminalModeConfig { mode, value: false };
        // SAFETY: `self.ptr` is the live terminal this value owns; DATA_MODE's out
        // type is a `GhosttyTerminalModeConfig`, and `cfg` is exactly that (same
        // `repr(C)` layout, `mode` set as the key) on this stack frame.
        let r = unsafe {
            ffi::ghostty_terminal_get(self.ptr, ffi::DATA_MODE, &mut cfg as *mut _ as *mut c_void)
        };
        // A valid DEC mode queries clean; an unknown one reports INVALID_VALUE
        // and leaves the value false (which is the answer we want either way).
        debug_assert!(r == ffi::SUCCESS || r == ffi::INVALID_VALUE);
        cfg.value
    }

    /// Whether the terminal is on the alternate screen (DEC 1047/47).
    pub fn alternate_on(&self) -> bool {
        // Read as a plain `u32`, not as the Rust enum: C writing a discriminant
        // outside the enum's variants into an enum-typed slot would be UB, and
        // the header does not promise the set of values stays closed.
        let mut screen: u32 = ffi::GhosttyTerminalScreen::Primary as u32;
        // SAFETY: `self.ptr` is the live terminal this value owns; DATA_ACTIVE_SCREEN
        // writes a `GhosttyTerminalScreen`, a `repr(u32)` enum, i.e. one `u32`, and
        // `screen` is a live, aligned `u32`.
        let r = unsafe {
            ffi::ghostty_terminal_get(
                self.ptr,
                ffi::DATA_ACTIVE_SCREEN,
                &mut screen as *mut _ as *mut c_void,
            )
        };
        debug_assert_eq!(r, ffi::SUCCESS);
        screen == ffi::GhosttyTerminalScreen::Alternate as u32
    }

    /// Whether the viewport is pinned to the active area (vs scrolled into
    /// history).
    pub fn viewport_active(&self) -> bool {
        self.get_bool(ffi::DATA_VIEWPORT_ACTIVE)
    }

    /// Whether VT processing is at ground (safe to splice out-of-band bytes).
    pub fn vt_ground(&self) -> bool {
        self.get_bool(ffi::DATA_VT_GROUND)
    }

    /// Whether the cursor is at a semantic prompt.
    pub fn cursor_at_prompt(&self) -> bool {
        self.get_bool(ffi::DATA_CURSOR_AT_PROMPT)
    }

    /// Total rows in the active screen including scrollback.
    pub fn total_rows(&self) -> usize {
        self.get_usize(ffi::DATA_TOTAL_ROWS)
    }

    /// Number of scrollback rows (total minus viewport).
    pub fn scrollback_rows(&self) -> usize {
        self.get_usize(ffi::DATA_SCROLLBACK_ROWS)
    }

    /// The terminal title as set by escape sequences (OSC 0/2). A copy.
    pub fn title(&self) -> String {
        self.get_string(ffi::DATA_TITLE)
    }

    /// The terminal working directory as set by escape sequences (OSC 7). A
    /// copy (the stored bytes, undecoded).
    pub fn pwd(&self) -> String {
        self.get_string(ffi::DATA_PWD)
    }

    // ── The typed `get` helpers ───────────────────────────────────────────

    fn get_u16(&self, data: i32) -> u16 {
        let mut v: u16 = 0;
        // SAFETY: `self.ptr` is the live terminal this value owns; every `data`
        // key passed to this helper writes a `u16` (see the callers), and `v` is
        // a live, aligned `u16`.
        let _ =
            unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn get_bool(&self, data: i32) -> bool {
        let mut v: bool = false;
        // SAFETY: `self.ptr` is the live terminal this value owns; every `data`
        // key passed to this helper writes a `bool` (see the callers), and `v` is
        // a live, aligned `bool`.
        let _ =
            unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn get_usize(&self, data: i32) -> usize {
        let mut v: usize = 0;
        // SAFETY: `self.ptr` is the live terminal this value owns; every `data`
        // key passed to this helper writes a `usize` (see the callers), and `v` is
        // a live, aligned `usize`.
        let _ =
            unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn get_string(&self, data: i32) -> String {
        // SAFETY: `GhosttyString` is a raw pointer and a length; all-zero (null, 0)
        // is a valid value.
        let mut s: ffi::GhosttyString = unsafe { std::mem::zeroed() };
        // SAFETY: `self.ptr` is the live terminal this value owns; the title/pwd
        // keys write a `GhosttyString`, and `s` is exactly that.
        let r =
            unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut s as *mut _ as *mut c_void) };
        if r != ffi::SUCCESS || s.ptr.is_null() || s.len == 0 {
            return String::new();
        }
        // SAFETY: the call succeeded and `ptr` is non-null with `len > 0`, so the
        // library is pointing at `len` initialised bytes it owns. They stay valid
        // until the terminal is next mutated, and the slice is copied into a
        // `String` before `&self` returns; `&self` and `!Sync` rule out a
        // mutation in between.
        let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
        String::from_utf8_lossy(bytes).into_owned()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` is the live terminal created in `new`, freed here
        // exactly once (`Terminal` is not `Clone`, and drop runs once).
        unsafe { ffi::ghostty_terminal_free(self.ptr) };
    }
}

// ── The render state (the grid accessor, plan §3.4 / §3.11) ────────────────

/// A resolved style color: a palette index or a direct RGB. `Option::None`
/// at the call site means "no explicit color" (the terminal default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleColor {
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// A cell's complete visual style (plain Rust, produced by the render state).
/// The SGR encoder (`crate::sgr`) consumes this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: Option<StyleColor>,
    pub bg: Option<StyleColor>,
    pub underline: Option<StyleColor>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    /// One of the `GHOSTTY_SGR_UNDERLINE_*` values (0 = none).
    pub underline_kind: u32,
}

/// One rendered cell: its text (the grapheme cluster, UTF-8) and its style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub style: CellStyle,
}

/// A scroll-viewport behavior (plan §3.4: history pages). The live pane sits
/// at `Bottom`; a history snapshot scrolls up, renders, and scrolls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollViewport {
    /// Top of the scrollback (the oldest retained line).
    Top,
    /// Bottom: the active area (the live screen).
    Bottom,
    /// A relative scroll in rows (up is negative, down positive).
    Delta(i32),
    /// An absolute row offset from the top of the scrollback.
    Row(u32),
}

/// One terminal's render state: the per-session renderer's state.
///
/// Owns the C `GhosttyRenderState`; freed on drop. `!Send`/`!Sync` (a raw
/// pointer), so it lives in one task per session (the keeper's), exactly as the
/// `Terminal` beside it. `update` borrows the terminal; it must be called from
/// the same task that owns both.
pub struct RenderState {
    ptr: ffi::GhosttyRenderState,
}

impl RenderState {
    /// Create an empty render state.
    pub fn new() -> Result<Self, FfiError> {
        let mut ptr: ffi::GhosttyRenderState = std::ptr::null_mut();
        // SAFETY: a NULL allocator selects the default one; `ptr` is a writable
        // slot the library fills (checked non-null below).
        let r = unsafe { ffi::ghostty_render_state_new(std::ptr::null(), &mut ptr) };
        if r != ffi::SUCCESS || ptr.is_null() {
            return Err(FfiError { code: r });
        }
        Ok(Self { ptr })
    }

    /// The raw C handle (for the raw FFI probes and tests).
    pub fn handle(&self) -> ffi::GhosttyRenderState {
        self.ptr
    }

    /// Update from a terminal (the snapshot's source of truth). Borrows it.
    pub fn update(&self, terminal: &Terminal) -> Result<(), FfiError> {
        // SAFETY: `self.ptr` is the live render state this value owns, and
        // `terminal.handle()` is a live terminal borrowed for the call (not
        // retained, and not mutated: both are single-threaded handles).
        let r = unsafe { ffi::ghostty_render_state_update(self.ptr, terminal.handle()) };
        if r != ffi::SUCCESS {
            return Err(FfiError { code: r });
        }
        Ok(())
    }

    /// The viewport width in cells.
    pub fn cols(&self) -> u16 {
        self.get_u16(ffi::RS_COLS)
    }

    /// The viewport height in cells.
    pub fn rows(&self) -> u16 {
        self.get_u16(ffi::RS_ROWS)
    }

    fn get_u16(&self, data: u32) -> u16 {
        let mut v: u16 = 0;
        // SAFETY: `self.ptr` is the live render state this value owns; the only
        // callers pass RS_COLS/RS_ROWS, which write a `u16`, and `v` is one.
        let _ = unsafe {
            ffi::ghostty_render_state_get(self.ptr, data, &mut v as *mut _ as *mut c_void)
        };
        v
    }

    /// Render one viewport row (y): one `Cell` per column, full width (0..cols).
    /// A row beyond the viewport is empty.
    pub fn row(&self, y: u16) -> Vec<Cell> {
        let mut it: ffi::GhosttyRenderStateRowIterator = std::ptr::null_mut();
        // SAFETY: a NULL allocator selects the default one; `it` is a writable
        // handle slot. From here on `it` is freed on every return path below
        // (and only once).
        if unsafe { ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut it) }
            != ffi::SUCCESS
        {
            return Vec::new();
        }
        // SAFETY: `self.ptr` is the live render state this value owns; ROW_ITERATOR's
        // out type is a row-iterator handle, and `it` is exactly one (a valid
        // iterator from `_new`, which this call links to the state).
        if unsafe {
            ffi::ghostty_render_state_get(
                self.ptr,
                ffi::ROW_ITERATOR,
                &mut it as *mut _ as *mut c_void,
            )
        } != ffi::SUCCESS
        {
            // SAFETY: `it` was created by `_new` above and is freed here, once,
            // on this return path.
            unsafe { ffi::ghostty_render_state_row_iterator_free(it) };
            return Vec::new();
        }
        // Advance to row y (the iterator walks from 0).
        for _ in 0..=y {
            // SAFETY: `it` is the live iterator linked to the state above.
            if !unsafe { ffi::ghostty_render_state_row_iterator_next(it) } {
                // SAFETY: freed once, on this return path.
                unsafe { ffi::ghostty_render_state_row_iterator_free(it) };
                return Vec::new();
            }
        }
        let mut cells: ffi::GhosttyRenderStateRowCells = std::ptr::null_mut();
        // SAFETY: a NULL allocator selects the default one; `cells` is a writable
        // handle slot. From here on `cells` and `it` are each freed once on every
        // return path.
        if unsafe { ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells) }
            != ffi::SUCCESS
        {
            // SAFETY: `it` is live and freed once, on this return path.
            unsafe { ffi::ghostty_render_state_row_iterator_free(it) };
            return Vec::new();
        }
        // SAFETY: `it` is a live iterator positioned on a row (the `next` loop
        // above); ROW_DATA_CELLS's out type is a row-cells handle and `cells` is
        // exactly one (a valid handle from `_new`).
        if unsafe {
            ffi::ghostty_render_state_row_get(
                it,
                ffi::ROW_DATA_CELLS,
                &mut cells as *mut _ as *mut c_void,
            )
        } != ffi::SUCCESS
        {
            // SAFETY: `cells` and `it` are live handles created above, each freed
            // once, here, on this return path.
            unsafe {
                ffi::ghostty_render_state_row_cells_free(cells);
                ffi::ghostty_render_state_row_iterator_free(it);
            };
            return Vec::new();
        }
        let cols = self.cols();
        let mut out = Vec::new();
        let mut text_buf = [0u8; 4096];
        // The C `GhosttyStyle` is 72 bytes (unpacked, 8-byte aligned — the
        // colour value is an 8-aligned union), and `ROW_CELLS_DATA_STYLE`
        // `@alignCast`s its `out` pointer. Our Rust mirror is narrower, so we
        // use the C size (72), not `size_of` (56), and read raw bytes.
        const STYLE_C_SIZE: usize = STYLE_C_SIZE_BYTES;
        // A `[u64; 9]` is 72 bytes, 8-aligned, and zeroed: no allocation, and
        // nothing is ever read that the C side did not write (a failed `get`
        // leaves the zeros, which decode as "no colour, no attributes").
        for x in 0..cols {
            // SAFETY: `cells` is the live row-cells handle; `x < cols`, and an
            // out-of-range column would be reported as an error, not UB.
            if unsafe { ffi::ghostty_render_state_row_cells_select(cells, x) } != ffi::SUCCESS {
                // Keep the column count honest: a blank, not a shifted row.
                out.push(Cell {
                    text: " ".to_string(),
                    style: style_to_plain(&[0u8; STYLE_C_SIZE]),
                });
                continue;
            }
            let mut style_buf = new_style_buf();
            // SAFETY: `cells` is selected on a cell; `style_buf` is 72 bytes
            // (the size of `GhosttyStyle`), 8-aligned (it is a `[u64; 9]`, the
            // alignment ROW_CELLS_DATA_STYLE casts its out pointer to) and writable.
            let got_style = unsafe {
                ffi::ghostty_render_state_row_cells_get(
                    cells,
                    ffi::ROW_CELLS_DATA_STYLE,
                    style_buf.as_mut_ptr() as *mut c_void,
                )
            };
            if got_style != ffi::SUCCESS {
                style_buf = new_style_buf();
            }
            let mut buffer = ffi::GhosttyBuffer {
                ptr: text_buf.as_mut_ptr(),
                cap: text_buf.len(),
                len: 0,
            };
            // SAFETY: `cells` is selected on a cell; GRAPHEMES_UTF8's out type is a
            // `GhosttyBuffer`, and `buffer` is one whose `ptr`/`cap` describe the
            // live, writable `text_buf` (the library writes at most `cap` bytes
            // and sets `len`, which is clamped with `.min` before use anyway).
            let got_text = unsafe {
                ffi::ghostty_render_state_row_cells_get(
                    cells,
                    ffi::ROW_CELLS_DATA_GRAPHEMES_UTF8,
                    &mut buffer as *mut _ as *mut c_void,
                )
            };
            if got_text != ffi::SUCCESS {
                buffer.len = 0;
            }
            let buffer_len = buffer.len.min(text_buf.len());
            // SAFETY: `style_buf` is initialised (zeroed, then written by C), and
            // any byte pattern is a valid `u8`.
            let style_bytes = unsafe {
                std::slice::from_raw_parts(style_buf.as_ptr() as *const u8, STYLE_C_SIZE)
            };
            // A cell with no text is either the second half of a wide character
            // (it must render as nothing: the wide glyph already covers the
            // column) or a blank the program never wrote (it MUST render as a
            // column, or everything to its right slides left). The cell's
            // `wide` attribute tells them apart. Only an empty cell needs the
            // question asked, so a cell with text skips two FFI calls: on a
            // 5,000-line history walk that is most of the cells.
            let spacer = buffer_len == 0 && {
                let mut raw: u64 = 0;
                let mut wide: u32 = 0;
                // SAFETY: `cells` is selected on a cell; ROW_CELLS_DATA_RAW writes a
                // `u64` (the raw cell) into `raw`, and `ghostty_cell_get` with
                // CELL_DATA_WIDE then writes a `u32` into `wide`; `&&` short-circuits
                // so `raw` is only used after it was written successfully.
                let have_wide = unsafe {
                    ffi::ghostty_render_state_row_cells_get(
                        cells,
                        ffi::ROW_CELLS_DATA_RAW,
                        &mut raw as *mut _ as *mut c_void,
                    ) == ffi::SUCCESS
                        && ffi::ghostty_cell_get(
                            raw,
                            ffi::CELL_DATA_WIDE,
                            &mut wide as *mut _ as *mut c_void,
                        ) == ffi::SUCCESS
                };
                have_wide
                    && (wide == ffi::CELL_WIDE_SPACER_TAIL || wide == ffi::CELL_WIDE_SPACER_HEAD)
            };
            let text = if buffer_len == 0 && !spacer {
                " ".to_string()
            } else {
                String::from_utf8_lossy(&text_buf[..buffer_len]).into_owned()
            };
            out.push(Cell {
                text,
                style: style_to_plain(style_bytes),
            });
        }
        // SAFETY: `cells` and `it` are live handles created above, each freed
        // once, here, on the normal return path (no `Cell` borrows from them: the
        // text was copied out).
        unsafe {
            ffi::ghostty_render_state_row_cells_free(cells);
            ffi::ghostty_render_state_row_iterator_free(it);
        };
        out
    }
}

/// `sizeof(GhosttyStyle)` in the vendored header.
const STYLE_C_SIZE_BYTES: usize = 72;

// The layout `style_to_plain` reads at hand-measured offsets, checked at compile
// time against the `#[repr(C)]` mirror in `ffi` (which is declared from the header's
// field order). The header itself is checked by the same numbers in
// `packages/mobile/native/ldmux_terminal.c` (`_Static_assert`), and the live engine
// by `tests/style_layout.rs`, which decodes every attribute out of a real cell.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<ffi::GhosttyStyle>() == STYLE_C_SIZE_BYTES);
    assert!(offset_of!(ffi::GhosttyStyle, fg_color) == 8);
    assert!(offset_of!(ffi::GhosttyStyle, bg_color) == 24);
    assert!(offset_of!(ffi::GhosttyStyle, underline_color) == 40);
    assert!(offset_of!(ffi::GhosttyStyle, bold) == 56);
    assert!(offset_of!(ffi::GhosttyStyle, italic) == 57);
    assert!(offset_of!(ffi::GhosttyStyle, faint) == 58);
    assert!(offset_of!(ffi::GhosttyStyle, blink) == 59);
    assert!(offset_of!(ffi::GhosttyStyle, inverse) == 60);
    assert!(offset_of!(ffi::GhosttyStyle, invisible) == 61);
    assert!(offset_of!(ffi::GhosttyStyle, strikethrough) == 62);
    assert!(offset_of!(ffi::GhosttyStyle, overline) == 63);
    assert!(offset_of!(ffi::GhosttyStyle, underline) == 64);
    // Inside a colour: tag at 0, value at 8.
    assert!(offset_of!(ffi::GhosttyStyleColor, value) == 8);
    assert!(size_of::<ffi::GhosttyStyleColor>() == 16);
};

/// A zeroed, 8-aligned style image with its `size` field set. The C struct is a
/// *sized* struct (`GHOSTTY_INIT_SIZED`): the first word is the caller's
/// `sizeof`, which lets the library grow the struct compatibly. It is ignored on
/// `get` today, which is why leaving it zero worked; it is set so that stops being
/// load-bearing luck.
fn new_style_buf() -> [u64; STYLE_C_SIZE_BYTES / 8] {
    let mut b = [0u64; STYLE_C_SIZE_BYTES / 8];
    b[0] = STYLE_C_SIZE_BYTES as u64;
    b
}

/// Read one tagged color from a raw `GhosttyStyle` byte image. `base` is the
/// offset of the 16-byte `GhosttyStyleColor` (fg at 8, bg at 24, underline at
/// 40); the tag is its first 4 bytes and the colour value lives at offset 8
/// within it (measured against the vendored library, plan §3.4, and now asserted
/// at compile time above).
fn color_from_raw(b: &[u8], base: usize) -> Option<StyleColor> {
    let tag = u32::from_le_bytes([b[base], b[base + 1], b[base + 2], b[base + 3]]);
    match tag {
        0 => None, // NONE
        1 => Some(StyleColor::Palette(b[base + 8])),
        2 => Some(StyleColor::Rgb(b[base + 8], b[base + 9], b[base + 10])),
        _ => None,
    }
}

/// Interpret the 72-byte `GhosttyStyle` image (read into an aligned buffer by
/// the caller) at the measured offsets. `b` is the 72 raw bytes:
///   fg_color @8, bg_color @24, underline_color @40 (each 16 bytes: tag @0,
///   value @8), then the eight 1-byte attributes @56..64 and `underline` @64.
fn style_to_plain(b: &[u8]) -> CellStyle {
    CellStyle {
        fg: color_from_raw(b, 8),
        bg: color_from_raw(b, 24),
        underline: color_from_raw(b, 40),
        bold: b[56] != 0,
        italic: b[57] != 0,
        faint: b[58] != 0,
        blink: b[59] != 0,
        inverse: b[60] != 0,
        invisible: b[61] != 0,
        strikethrough: b[62] != 0,
        overline: b[63] != 0,
        underline_kind: u32::from_le_bytes([b[64], b[65], b[66], b[67]]),
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` is the live render state created in `new`, freed
        // here exactly once (`RenderState` is not `Clone`).
        unsafe { ffi::ghostty_render_state_free(self.ptr) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_style_image_is_sized_like_the_c_struct() {
        let b = new_style_buf();
        assert_eq!(b[0], 72, "the size word is sizeof(GhosttyStyle)");
        assert_eq!(std::mem::size_of_val(&b), 72);
        assert!(
            b[1..].iter().all(|&w| w == 0),
            "everything else starts zeroed"
        );
    }
}
