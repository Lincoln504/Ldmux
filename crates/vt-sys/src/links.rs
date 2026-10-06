//! OSC 8 hyperlinks on the viewport, for the snapshot path.
//!
//! The render state (see `safe.rs`) carries style and text but not link targets, so
//! they are read per viewport cell through the terminal's grid references, the same
//! call the mobile shim uses (`note_hyperlink` in `ldmux_terminal.c`). The result is
//! a parallel row of `Option<String>` that [`crate::snapshot::render_with_links`]
//! wraps in `OSC 8 ; ; uri ST` runs.
//!
//! A link target is attacker-controlled program output, and it is about to be
//! written back into a byte stream that terminals interpret, so it is bounded and
//! cleaned here rather than trusted: a URI over [`URI_MAX`] bytes, with a control
//! character in it, or empty is dropped (the cell renders unlinked).

use std::os::raw::{c_int, c_void};

use crate::Terminal;
use crate::ffi;

/// The longest link target carried, in bytes (the same cap as the mobile frame,
/// `LDMUX_URI_MAX`).
pub const URI_MAX: usize = 2048;

const POINT_TAG_VIEWPORT: u32 = 1;
/// `GhosttyCellData`: the cell has an OSC 8 hyperlink.
const CELL_DATA_HAS_HYPERLINK: u32 = 7;

/// `GhosttyPointCoordinate`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PointCoordinate {
    x: u16,
    y: u32,
}

/// `GhosttyPointValue`: a coordinate inside the header's 16-byte padding union.
#[repr(C)]
#[derive(Clone, Copy)]
union PointValue {
    coordinate: PointCoordinate,
    _padding: [u64; 2],
}

/// `GhosttyPoint`, passed by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    tag: u32,
    value: PointValue,
}

/// `GhosttyGridRef`, a sized struct (`size` first).
#[repr(C)]
struct GridRef {
    size: usize,
    node: *mut c_void,
    x: u16,
    y: u16,
}

unsafe extern "C" {
    fn ghostty_terminal_grid_ref(
        terminal: ffi::GhosttyTerminal,
        point: Point,
        out_ref: *mut GridRef,
    ) -> c_int;
    fn ghostty_grid_ref_cell(grid_ref: *const GridRef, out_cell: *mut u64) -> c_int;
    fn ghostty_grid_ref_hyperlink_uri(
        grid_ref: *const GridRef,
        buf: *mut u8,
        buf_len: usize,
        out_len: *mut usize,
    ) -> c_int;
}

/// Whether `uri` may be written into an OSC 8 sequence: non-empty, within
/// [`URI_MAX`], and free of C0/C1 controls and DEL, which would end the OSC early
/// or smuggle a second sequence into the replay.
pub fn uri_is_safe(uri: &str) -> bool {
    !uri.is_empty()
        && uri.len() <= URI_MAX
        && !uri
            .chars()
            .any(|c| c.is_control() || c == '\u{7f}' || c == '\u{9c}')
}

/// The link target of each cell of viewport row `y`, `cols` wide. Empty when no
/// cell in the row is linked, which is the common case and costs the caller
/// nothing downstream.
///
/// Call it for the same viewport the render state was last updated from.
pub fn row_links(term: &Terminal, cols: u16, y: u16) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    let mut buf = [0u8; URI_MAX];
    for x in 0..cols {
        let point = Point {
            tag: POINT_TAG_VIEWPORT,
            value: PointValue {
                coordinate: PointCoordinate { x, y: y as u32 },
            },
        };
        let mut r = GridRef {
            size: std::mem::size_of::<GridRef>(),
            node: std::ptr::null_mut(),
            x: 0,
            y: 0,
        };
        // SAFETY: `term.handle()` is the live terminal `term` owns, `point` is a plain
        // by-value struct matching the header's layout, and `r` is a writable sized
        // struct the library fills; its node is checked by the calls that follow.
        if unsafe { ghostty_terminal_grid_ref(term.handle(), point, &mut r) } != ffi::SUCCESS {
            continue;
        }
        let mut cell: u64 = 0;
        let mut linked: u8 = 0;
        // SAFETY: `r` was just filled and no terminal call has intervened; `cell` and
        // `linked` are writable slots of the sizes the library writes (u64, bool).
        let has = unsafe {
            ghostty_grid_ref_cell(&r, &mut cell) == ffi::SUCCESS
                && ffi::ghostty_cell_get(
                    cell,
                    CELL_DATA_HAS_HYPERLINK,
                    &mut linked as *mut u8 as *mut c_void,
                ) == ffi::SUCCESS
                && linked != 0
        };
        if !has {
            continue;
        }
        let mut n: usize = 0;
        // SAFETY: `buf` is writable for `URI_MAX` bytes and `n` is a writable length;
        // a URI that does not fit returns OUT_OF_SPACE and writes nothing.
        let rc = unsafe { ghostty_grid_ref_hyperlink_uri(&r, buf.as_mut_ptr(), buf.len(), &mut n) };
        if rc != ffi::SUCCESS || n == 0 || n > buf.len() {
            continue;
        }
        let Ok(uri) = std::str::from_utf8(&buf[..n]) else {
            continue;
        };
        if !uri_is_safe(uri) {
            continue;
        }
        if out.is_empty() {
            out.resize(cols as usize, None);
        }
        out[x as usize] = Some(uri.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_oversize_are_unsafe() {
        assert!(uri_is_safe("https://example.com/a?b=c"));
        assert!(uri_is_safe("https://例え.jp/"));
        assert!(!uri_is_safe(""));
        assert!(!uri_is_safe("https://a/\x1b]0;x\x07"));
        assert!(!uri_is_safe("https://a/\x07"));
        assert!(!uri_is_safe("https://a/\u{9c}"));
        assert!(!uri_is_safe(&"a".repeat(URI_MAX + 1)));
        assert!(uri_is_safe(&"a".repeat(URI_MAX)));
    }
}
