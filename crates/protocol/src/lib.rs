#![forbid(unsafe_code)]
//! The ldmux wire contract: frames, messages, keeper, rpc, pairing, net,
//! paths, socket.
//!
//! Port of `packages/protocol` (the TS original). Every module names
//! its TS source in its own doc comment. **Those files no longer exist** — the
//! TypeScript tree was deleted at D4; read them at git commit `6b3efb8`, the
//! last commit that carried the tree. The keeper messages (`keeper`) are new
//! and have no TS source.
//!
//! See `docs/ARCHITECTURE.md` for the module map
//! (phase state).

/// The build that produced this binary: `<git short sha>`, `<sha>-dirty`, a name a release
/// pipeline supplied in `LDMUX_BUILD_ID`, or `unknown` (a nix sandbox has no `.git`).
///
/// Every binary in the tree is built from its own checkout and the daemon, the CLI, the keeper
/// and a desk window share a machine for weeks; without this there is no way to ask which of them
/// is running what, which is RELEASE-AUDIT 14.2 in one line. Set by `crates/protocol/build.rs`.
pub const BUILD_ID: &str = env!("LDMUX_BUILD_ID");

/// What a version line says: the wire protocol this build speaks, and the build itself.
pub fn version_line() -> String {
    format!(
        "ldmux {} (protocol {})",
        BUILD_ID,
        rpc::DAEMON_PROTOCOL_VERSION
    )
}

/// Bounds for a client-requested pane size. The floor keeps a pane usable (a
/// client that bugs out at 1x1 leaves a shell nobody can read); the ceiling stops a
/// bad client from asking for a grid whose capture would blow the stream's byte budget.
///
/// **2000 columns, because 500 was not "well above any real display"** — it was below
/// one. A wide monitor at a small font asks for more: a 4K window at a 7 px cell is about
/// 620 columns, and the clamp then pinned the pane at 500 while the window kept growing,
/// so the terminal stopped filling the window and the columns right of 500 stayed blank.
/// That reads as "the font got smaller and then the view shrank instead", measured
/// 2026-11-03. Two thousand is 510k cells at the row ceiling (half a megabyte, one grid),
/// and the frame's column field is 16 bits, so this is a deliberate bound rather than a
/// format limit — but a bound that a real display can reach has to be above it.
pub const MIN_PANE_COLS: u64 = 20;
pub const MAX_PANE_COLS: u64 = 2000;
pub const MIN_PANE_ROWS: u64 = 5;
/// 255, not 300, because the client's wire format is the binding constraint.
///
/// A frame carries a selection's rows in **one byte each** (header bytes 36 and
/// 39 — see `packages/mobile/native/.../ldmux_terminal.c`), while its columns
/// already get two. Above 255 a pane wraps its selection rows modulo 256: row 256
/// draws as row 0, row 300 as row 44, so both handles jump to the top of the
/// screen and the highlighted range is wrong. Nothing reports it, because nothing
/// is inconsistent — the number is simply truncated. No real terminal is 255 rows
/// tall (the phone reaches roughly 100 at its smallest font).
/// `packages/mobile/test/architecture.test.ts` asserts the two ends agree, because
/// this is a number that only makes sense next to the format that reads it.
pub const MAX_PANE_ROWS: u64 = 255;

/// Clamp a client-requested pane size into the range the wire format can carry.
/// Every path that sizes a PTY goes through this, so the grid the emulator holds
/// and the grid the client is told about are the same grid.
pub fn clamp_pane_size(cols: u64, rows: u64) -> (u64, u64) {
    (
        cols.clamp(MIN_PANE_COLS, MAX_PANE_COLS),
        rows.clamp(MIN_PANE_ROWS, MAX_PANE_ROWS),
    )
}

pub mod base64;
pub mod frames;
pub mod keeper;
pub mod keeper_call;
pub mod mesh_term;
pub mod messages;
pub mod net;
pub mod pairing;
pub mod paths;
pub mod rpc;
pub mod socket;
pub mod utf8;

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn clamp_keeps_a_usable_size() {
        assert_eq!(clamp_pane_size(80, 24), (80, 24));
        assert_eq!(clamp_pane_size(1, 1), (MIN_PANE_COLS, MIN_PANE_ROWS));
        assert_eq!(clamp_pane_size(0, 0), (MIN_PANE_COLS, MIN_PANE_ROWS));
    }

    #[test]
    fn clamp_holds_the_one_byte_row_ceiling() {
        // The frame carries a selection's rows in one byte; above 255 they wrap.
        assert_eq!(clamp_pane_size(80, 300).1, MAX_PANE_ROWS);
        assert_eq!(clamp_pane_size(80, u64::MAX).1, MAX_PANE_ROWS);
        assert_eq!(clamp_pane_size(9999, 24).0, MAX_PANE_COLS);
        // A wide window at a small font is inside the bounds, and must stay there: this is the
        // size that used to be clamped to 500 and left the right of the window unpainted.
        assert_eq!(clamp_pane_size(600, 60), (600, 60));
        assert_eq!(clamp_pane_size(1600, 40), (1600, 40));
        // Compile-time, not run-time: raising the constant past the frame's
        // one-byte row field is a build error, not a failing test.
        const { assert!(MAX_PANE_ROWS <= 255) };
    }
}
