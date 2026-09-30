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
        let r = unsafe { ffi::ghostty_terminal_new(std::ptr::null(), &mut ptr, cols, rows) };
        if r != ffi::SUCCESS || ptr.is_null() {
            return Err(FfiError { code: r });
        }
        Ok(Self { ptr, cell: (10, 20) })
    }

    /// Feed raw bytes through the VT parser (the Write direction into the
    /// emulator). Never fails: the parser keeps state consistent on any input.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        unsafe { ffi::ghostty_terminal_vt_write(self.ptr, bytes.as_ptr(), bytes.len()) };
    }

    /// Change the cell count (the Resize direction into the emulator).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cw, ch) = self.cell;
        let _ = unsafe { ffi::ghostty_terminal_resize(self.ptr, cols, rows, cw, ch) };
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
        let _ = unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn get_bool(&self, data: i32) -> bool {
        let mut v: bool = false;
        let _ = unsafe {
            ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void)
        };
        v
    }

    fn get_usize(&self, data: i32) -> usize {
        let mut v: usize = 0;
        let _ = unsafe { ffi::ghostty_terminal_get(self.ptr, data, &mut v as *mut _ as *mut c_void) };
        v
    }

    fn get_string(&self, data: i32) -> String {
        let mut s: ffi::GhosttyString = unsafe { std::mem::zeroed() };
        let r = unsafe {
            ffi::ghostty_terminal_get(self.ptr, data, &mut s as *mut _ as *mut c_void)
        };
        if r != ffi::SUCCESS || s.ptr.is_null() || s.len == 0 {
            return String::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len) };
        String::from_utf8_lossy(bytes).into_owned()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_terminal_free(self.ptr) };
    }
}
