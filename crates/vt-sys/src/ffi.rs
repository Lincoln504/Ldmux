//! The raw `extern "C"` surface of the vendored `libghostty-vt`.
//!
//! Bound by hand against `packages/mobile/native/prebuilt/host/include/ghostty/vt/terminal.h`
//! (the "one generic accessor" decision, plan D2). Everything here is `unsafe`
//! at the call site; use [`crate::safe`] instead.

#![allow(non_camel_case_types, non_snake_case)]

use std::os::raw::c_int;
use std::os::raw::c_void;

/// Opaque handle to a terminal instance (`typedef struct ...* GhosttyTerminal`).
pub type GhosttyTerminal = *mut c_void;

/// A borrowed byte string (pointer + length). The memory is not owned by this
/// struct; the pointer is valid only for the lifetime documented by the call
/// that produced it (for a `get`, until the next mutating call).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyString {
    pub ptr: *const u8,
    pub len: usize,
}

/// The allocator interface. We always pass NULL to mean "use the default"
/// (libc malloc/free), so only the shape is needed here.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyAllocator {
    pub ctx: *mut c_void,
    pub vtable: *const c_void,
}

// ── Result codes (GhosttyResult, an `int`-backed enum) ───────────────────
pub const SUCCESS: c_int = 0;
pub const OUT_OF_MEMORY: c_int = -1;
pub const INVALID_VALUE: c_int = -2;
pub const OUT_OF_SPACE: c_int = -3;
pub const NO_VALUE: c_int = -4;
pub const IO_ERROR: c_int = -5;
pub const LIMIT_EXCEEDED: c_int = -6;
pub const REJECTED: c_int = -7;

// ── GhosttyTerminalData: keys of the one generic accessor ──────────────────
pub const DATA_INVALID: c_int = 0;
pub const DATA_COLS: c_int = 1;
pub const DATA_ROWS: c_int = 2;
pub const DATA_CURSOR_X: c_int = 3;
pub const DATA_CURSOR_Y: c_int = 4;
pub const DATA_CURSOR_PENDING_WRAP: c_int = 5;
pub const DATA_ACTIVE_SCREEN: c_int = 6;
pub const DATA_CURSOR_VISIBLE: c_int = 7;
pub const DATA_KITTY_KEYBOARD_FLAGS: c_int = 8;
pub const DATA_SCROLLBAR: c_int = 9;
pub const DATA_CURSOR_STYLE: c_int = 10;
pub const DATA_MOUSE_TRACKING: c_int = 11;
pub const DATA_TITLE: c_int = 12;
pub const DATA_PWD: c_int = 13;
pub const DATA_TOTAL_ROWS: c_int = 14;
pub const DATA_SCROLLBACK_ROWS: c_int = 15;
pub const DATA_MODE: c_int = 37;
pub const DATA_WIDTH_PX: c_int = 16;
pub const DATA_HEIGHT_PX: c_int = 17;
pub const DATA_VIEWPORT_ACTIVE: c_int = 32;
pub const DATA_VT_GROUND: c_int = 38;
pub const DATA_CURSOR_AT_PROMPT: c_int = 39;

// ── GhosttyTerminalOption: keys of ghostty_terminal_set ────────────────────
pub const OPT_USERDATA: c_int = 0;
pub const OPT_WRITE_PTY: c_int = 1;
pub const OPT_BELL: c_int = 2;
pub const OPT_TITLE: c_int = 9;
pub const OPT_PWD: c_int = 10;
pub const OPT_SCROLLBACK_MAX_BYTES: c_int = 27;
pub const OPT_SCROLLBACK_MAX_LINES: c_int = 28;

unsafe extern "C" {
    /// Create a terminal. `allocator` NULL = default (libc malloc/free).
    pub fn ghostty_terminal_new(
        allocator: *const GhosttyAllocator,
        terminal: *mut GhosttyTerminal,
        cols: u16,
        rows: u16,
    ) -> c_int;

    /// Free a terminal (may be passed NULL; then a no-op).
    pub fn ghostty_terminal_free(terminal: GhosttyTerminal);

    /// Configure a callback / option. `value` is typed per option; NULL per
    /// the option's doc to clear.
    pub fn ghostty_terminal_set(
        terminal: GhosttyTerminal,
        option: c_int,
        value: *const c_void,
    ) -> c_int;

    /// Feed raw VT bytes through the parser. Never fails (input is untrusted;
    /// the goal is to stay consistent, not to reject).
    pub fn ghostty_terminal_vt_write(
        terminal: GhosttyTerminal,
        data: *const u8,
        len: usize,
    );

    /// Change the cell count. `cell_*_px` are the pixel size of one cell
    /// (drives image protocols and size reports; arbitrary headless).
    pub fn ghostty_terminal_resize(
        terminal: GhosttyTerminal,
        cols: u16,
        rows: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> c_int;

    /// The one generic accessor: extract `data` into `out` (typed per the
    /// `GhosttyTerminalData` doc).
    pub fn ghostty_terminal_get(
        terminal: GhosttyTerminal,
        data: c_int,
        out: *mut c_void,
    ) -> c_int;

    /// Batch accessor (one call, many keys). Bound for completeness of "that
    /// one function" — the skeleton uses the single `get`.
    pub fn ghostty_terminal_get_multi(
        terminal: GhosttyTerminal,
        count: usize,
        keys: *const c_int,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> c_int;

    /// Free a library-allocated buffer (e.g. a `continuation_alloc` result).
    pub fn ghostty_free(
        allocator: *const GhosttyAllocator,
        ptr: *mut u8,
        len: usize,
    );

    /// Scroll the terminal viewport (TOP / BOTTOM / DELTA / ROW). Returns
    /// void (may be passed NULL for the terminal, a no-op).
    pub fn ghostty_terminal_scroll_viewport(
        terminal: GhosttyTerminal,
        behavior: GhosttyTerminalScrollViewport,
    );
}

// ── terminal modes + active screen (terminal.h / modes.h) ────────────────

/// `GhosttyTerminalScreen` (the `DATA_ACTIVE_SCREEN` out type):
/// which screen the terminal is on.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GhosttyTerminalScreen {
    Primary = 0,
    Alternate = 1,
}

/// `GhosttyTerminalModeConfig` — the in/out struct of `DATA_MODE`. `mode`
/// is the mode identifier to configure or query; `value` is the value to
/// set, or the current value on a query. (A `u16` + a `bool`: 4-byte layout,
/// matching C — no padding needed.)
#[derive(Clone, Copy)]
#[repr(C)]
pub struct GhosttyTerminalModeConfig {
    pub mode: u16,
    pub value: bool,
}

// ── scroll_viewport tagged union (terminal.h) ─────────────────────────────
// `GhosttyTerminalScrollViewport` is a struct { tag, value-union }, passed by
// value. The union is 16 bytes (the `_padding[2]` widens it past the 8-byte
// `delta`/`row`), so the struct is tag(4)+pad(4)+value(16) = 24 bytes.
/// The scroll-viewport value union (8-byte `delta`/`row`, padded to 16).
#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyTerminalScrollViewportValue {
    pub delta: isize,
    pub row: usize,
    pub _padding: [u64; 2],
}

/// The tagged union passed to `ghostty_terminal_scroll_viewport`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalScrollViewport {
    pub tag: c_int,
    pub value: GhosttyTerminalScrollViewportValue,
}

// Scroll-viewport tags (GhosttyTerminalScrollViewportTag: `int`).
pub const SCROLL_VIEWPORT_TOP: c_int = 0;
pub const SCROLL_VIEWPORT_BOTTOM: c_int = 1;
pub const SCROLL_VIEWPORT_DELTA: c_int = 2;
pub const SCROLL_VIEWPORT_ROW: c_int = 3;

// ── render.h: the render-state row/cell accessors (the grid) ─────────────
// The grid is not a `GhosttyTerminalData` key (plan D2 under-bounded it); it
// is a separate API over opaque handles. The cells are the source of truth for
// the snapshot renderer (plan §3.4 / §3.11.3).

/// Opaque render-state handle (`struct GhosttyRenderStateImpl*`).
pub type GhosttyRenderState = *mut c_void;
/// Opaque row-iterator handle.
pub type GhosttyRenderStateRowIterator = *mut c_void;
/// Opaque row-cells handle.
pub type GhosttyRenderStateRowCells = *mut c_void;

/// An RGB color (0-255 each).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

// The C style structs are UNPACKED (the colour value sits at offset 8 of each
// 16-byte `GhosttyStyleColor`, with 4 bytes of padding after the 4-byte tag).
// We mirror them `#[repr(C)]` (72-byte `GhosttyStyle`) so `size_of` matches the
// C, and read the fields as raw bytes (`style_to_plain`) at the measured
// offsets, not via the field accessors.
/// A color value: the raw 8 bytes of the C union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyStyleColorValue {
    pub bytes: [u8; 8],
}

/// A tagged color: unset (tag 0), a palette index (tag 1), or RGB (tag 2).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyStyleColor {
    pub tag: u32,
    pub value: GhosttyStyleColorValue,
}

/// A terminal cell's complete visual style (a sized struct; 72 bytes in C).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyStyle {
    pub size: u64,
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: u32,
}

/// A caller-provided byte buffer (the `GRAPHEMES_UTF8` out).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyBuffer {
    pub ptr: *mut u8,
    pub cap: usize,
    pub len: usize,
}

// ── render.h enums (the `: int` underlying type → u32) ────────────────────
// `GhosttyRenderStateData` keys (the `ghostty_render_state_get` selector).
pub const RS_COLS: u32 = 1;
pub const RS_ROWS: u32 = 2;
// The row-iterator data kind (populates a `GhosttyRenderStateRowIterator`).
pub const ROW_ITERATOR: u32 = 4;

pub const ROW_DATA_CELLS: u32 = 3;

pub const ROW_CELLS_DATA_STYLE: u32 = 2;
pub const ROW_CELLS_DATA_GRAPHEMES_UTF8: u32 = 9;

unsafe extern "C" {
    /// Create an empty render state (the per-session renderer's state).
    pub fn ghostty_render_state_new(
        allocator: *const GhosttyAllocator,
        state: *mut GhosttyRenderState,
    ) -> c_int;

    /// Free a render state (may be passed NULL; then a no-op).
    pub fn ghostty_render_state_free(state: GhosttyRenderState);

    /// Update a render state from a terminal (one convenience call).
    pub fn ghostty_render_state_update(state: GhosttyRenderState, terminal: GhosttyTerminal)
        -> c_int;

    /// Get a value from a render state (e.g. ROW_ITERATOR).
    pub fn ghostty_render_state_get(state: GhosttyRenderState, data: u32, out: *mut c_void) -> c_int;

    /// Create a row-iterator instance (populated via `get` ROW_ITERATOR).
    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const GhosttyAllocator,
        out_iterator: *mut GhosttyRenderStateRowIterator,
    ) -> c_int;

    /// Free a row iterator.
    pub fn ghostty_render_state_row_iterator_free(iterator: GhosttyRenderStateRowIterator);

    /// Advance to the next row (y = 0, ascending). Returns true if a row is
    /// now available to read.
    pub fn ghostty_render_state_row_iterator_next(
        iterator: GhosttyRenderStateRowIterator,
    ) -> bool;

    /// Read a value of the current row (e.g. ROW_DATA_CELLS).
    pub fn ghostty_render_state_row_get(
        iterator: GhosttyRenderStateRowIterator,
        data: u32,
        out: *mut c_void,
    ) -> c_int;

    /// Create a row-cells instance (the cells of the current row).
    pub fn ghostty_render_state_row_cells_new(
        allocator: *const GhosttyAllocator,
        out_cells: *mut GhosttyRenderStateRowCells,
    ) -> c_int;

    /// Free a row-cells instance.
    pub fn ghostty_render_state_row_cells_free(cells: GhosttyRenderStateRowCells);

    /// Advance to the next cell. Returns true if a cell is now available.
    pub fn ghostty_render_state_row_cells_next(cells: GhosttyRenderStateRowCells) -> bool;

    /// Position the cell iterator at column `x`.
    pub fn ghostty_render_state_row_cells_select(
        cells: GhosttyRenderStateRowCells,
        x: u16,
    ) -> c_int;

    /// Read a value of the current cell (e.g. STYLE, GRAPHEMES_UTF8).
    pub fn ghostty_render_state_row_cells_get(
        cells: GhosttyRenderStateRowCells,
        data: u32,
        out: *mut c_void,
    ) -> c_int;
}
