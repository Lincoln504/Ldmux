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
}
