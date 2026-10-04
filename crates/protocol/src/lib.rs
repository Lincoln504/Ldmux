//! The ldmux wire contract: frames, messages, keeper, rpc, pairing, net,
//! paths, socket.
//!
//! Port of `packages/protocol` (the TS source of record). Every module names
//! its TS source in its own doc comment. **Those files no longer exist** — the
//! TypeScript tree was deleted at D4; read them at git commit `6b3efb8`, the
//! last commit that carried the tree. The keeper messages (`keeper`) are new
//! (plan §3.5) and have no TS source.
//!
//! See `docs/RUST-PORT-PLAN.md` (§4 module map) and `docs/RUST-PORT-LOG.md`
//! (phase state).

/// Bounds for a client-requested pane size. The floor keeps a pane usable (a
/// client that bugs out at 1x1 leaves a shell nobody can read); the ceiling is
/// well above any real display and stops a bad client from asking for a grid
/// whose capture would blow the stream's byte budget.
pub const MIN_PANE_COLS: u64 = 20;
pub const MAX_PANE_COLS: u64 = 500;
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
    (cols.clamp(MIN_PANE_COLS, MAX_PANE_COLS), rows.clamp(MIN_PANE_ROWS, MAX_PANE_ROWS))
}

pub mod frames;
pub mod keeper;
pub mod keeper_call;
pub mod messages;
pub mod net;
pub mod paths;
pub mod pairing;
pub mod rpc;
pub mod socket;

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
        assert!(MAX_PANE_ROWS <= 255);
    }
}
