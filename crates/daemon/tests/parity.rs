//! The §3.11 snapshot-parity gate (the plan's #1 risk, R1).
//!
//! **The gate is on the cells, not the SGR bytes.** Plan §3.11(3) is explicit:
//! "the cells are the source of truth; the format is ours." So this harness
//! pins the two halves separately:
//!
//! 1. `parity_corpus_cells_match_tmux` — the *engine* half: the same input
//!    bytes → (a) a real tmux pane's grid, decoded from
//!    `capture-pane -p -e -N` by `sgrep`, and (b) our engine's cells (the
//!    `RenderState` walk). Compared cell by cell. A divergence here is a real
//!    engine defect (R1: a wrong screen with no error).
//! 2. `round_trip_render_then_parse_is_identity` — the *format* half: our SGR
//!    spelling (`sgr.rs`, a reset-then-re-issue encoding) is faithful to the
//!    cells — parsing what `snapshot::render` emits recovers the cells.
//!
//! (Decision recorded in `docs/RUST-PORT-LOG.md`: the earlier byte-identical
//! reading of §3.11(2) is superseded by §3.11(3)'s own words — tmux's SGR
//! spelling (a transition encoder) is a *moving* reference across tmux
//! versions, while the cells are the stable truth both engines must match.
//! `sgr.rs` keeps its re-issue encoding: it is a valid SGR, and its
//! fidelity is pinned by the round-trip.)
//!
//! **The pane design** (resolves the open design question): a tmux pane runs
//! a *shell*, so feeding the corpus as the pane's *input* would put the
//! shell's echo + prompt in the grid, not just the corpus. So the corpus is
//! fed as the pane's **output**: the pane runs `clear; cat <corpus-file>;
//! exec sleep 9999`, where `clear` erases the echoed command text, `cat`
//! emits the corpus as the pane's *output* (so the **emulator** parses it,
//! not the shell), and `exec sleep` suppresses the prompt. The grid is then
//! *just* the corpus — the same scope as our engine fed the same bytes
//! directly.
//!
//! This drives a **scratch** tmux server (a unique `-L` socket), never the
//! live box. The tmux binary is resolved `LDMUX_TMUX_BIN` before PATH (the
//! same order as `packages/daemon/src/tmux/control-plane.ts:44`).
//!
//! Known version quirk (observed 2026-09-28, tmux 3.5a on this box):
//! `capture-pane -N` pads short rows to 20 columns and long rows to the
//! window width (80) — not a uniform 80. The cell gate does not depend on it:
//! `normalize` pads every row to the pinned width on both sides.

mod sgrep;

use ldmux_daemon::exec::{run, ExecOpts};
use ldmux_vt_sys::{Cell, RenderState, Terminal};

/// The pinned corpus (plan §3.11). A set of SGR sequences + text:
/// bold "B", underline "U", 256-colour "C", 24-bit "D", then a newline.
///
/// Pinned: any change here changes the golden. Extend deliberately (the plan
/// names the full corpus — all attributes, wide/emoji, wrap, OSC 8, scroll —
/// but the first pin is this minimal set).
const CORPUS: &[u8] = b"\x1b[1mB\x1b[0m\x1b[4mU\x1b[0m\x1b[38;5;123mC\x1b[0m\x1b[38;2;255;128;0mD\x1b[0m\n";

/// The viewport geometry, pinned (the pane and the engine must match).
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Resolve the tmux binary: `LDMUX_TMUX_BIN` before PATH, bare `tmux` as the
/// last resort (the TS `resolveTmuxBin` order).
fn tmux_bin() -> String {
    let explicit = std::env::var("LDMUX_TMUX_BIN").unwrap_or_default();
    if !explicit.is_empty() && std::path::Path::new(&explicit).exists() {
        return explicit;
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = std::path::Path::new(&dir).join("tmux");
            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    "tmux".to_string()
}

/// Run a tmux command against the scratch server, capturing its output.
async fn tm(socket: &str, args: &[&str]) -> ldmux_daemon::exec::ExecResult {
    let mut argv: Vec<String> = vec![tmux_bin(), "-L".into(), socket.to_string()];
    for a in args {
        argv.push((*a).to_string());
    }
    run(argv, ExecOpts::default()).await
}

/// Our engine's grid for the corpus: feed the bytes, walk the rows.
fn our_cells() -> Vec<Vec<Cell>> {
    let mut term = Terminal::new(COLS, ROWS).expect("the engine allocates");
    term.write(CORPUS);
    let rs = RenderState::new().expect("the render state allocates");
    rs.update(&term).expect("the render state updates from the terminal");
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for y in 0..rs.rows() {
        rows.push(rs.row(y));
    }
    rows
}

/// The comparison normalization, applied identically to both sides:
///
/// 1. pad every row to `width` with empty cells (the full-width rule);
/// 2. a cell with text `" "` and the blank style is a *written space* or a
///    never-written position — the two are indistinguishable in a real grid
///    (a program that writes a space produces exactly the grid a blank
///    position produces), so both fold to empty.
fn normalize(rows: Vec<Vec<Cell>>, width: usize) -> Vec<Vec<Cell>> {
    use ldmux_daemon::sgr::BLANK;
    rows.into_iter()
        .map(|mut row| {
            while row.len() < width {
                row.push(Cell { text: String::new(), style: BLANK });
            }
            for c in row.iter_mut() {
                if c.text == " " && c.style == BLANK {
                    c.text.clear();
                }
            }
            row
        })
        .collect()
}

#[tokio::test]
async fn parity_corpus_cells_match_tmux() {
    // The corpus file (the pane `cat`s it as its *output*).
    let corpus_path = std::env::temp_dir().join(format!(
        "ldmux-parity-corpus-{}.bin",
        std::process::id()
    ));
    std::fs::write(&corpus_path, CORPUS).expect("write the corpus file");

    // A scratch socket (never the live box), unique per run.
    let socket = format!("ldmux-parity-{}", std::process::id());

    // Start a scratch server with a pane that runs `clear; cat <file>; exec
    // sleep 9999`: the `clear` erases the echoed command text, `cat` emits
    // the corpus as the pane's *output* (the emulator parses it, not the
    // shell), and `exec sleep` suppresses the prompt — so the grid is
    // *just* the corpus.
    let emitter = format!("clear; cat {}; exec sleep 9999", corpus_path.to_string_lossy());
    let r = tm(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            "s1",
            "-x",
            &COLS.to_string(),
            "-y",
            &ROWS.to_string(),
            &emitter,
        ],
    )
    .await;
    assert_eq!(r.code, 0, "new-session failed: {}", r.err);

    // Give the pane a moment to run `clear` + `cat` (the grid is then
    // stable: `exec sleep` means no further output).
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // (a) tmux: the grid, in SGR form, decoded to cells.
    let cap = tm(&socket, &["capture-pane", "-p", "-e", "-N", "-t", "s1"]).await;
    assert_eq!(cap.code, 0, "capture-pane failed: {}", cap.err);
    // Clean up the scratch server (reap the socket file).
    let _ = tm(&socket, &["kill-server"]).await;
    let _ = std::fs::remove_file(&corpus_path);
    let tmux_grid = sgrep::parse(&cap.out, COLS as usize);

    // (b) our engine: the cells, straight from the render state.
    let ours = our_cells();

    let (t, o) = (
        normalize(tmux_grid, COLS as usize),
        normalize(ours, COLS as usize),
    );
    assert_eq!(t.len(), o.len(), "row count: tmux {} vs ours {}", t.len(), o.len());
    let mut divergence: Option<(usize, usize, &Cell, &Cell)> = None;
    for (y, (tr, or_)) in t.iter().zip(&o).enumerate() {
        assert_eq!(tr.len(), or_.len(), "row {y} width");
        for (x, (tc, oc)) in tr.iter().zip(or_).enumerate() {
            if tc != oc {
                divergence = Some((y, x, tc, oc));
                break;
            }
        }
        if divergence.is_some() {
            break;
        }
    }
    assert!(
        divergence.is_none(),
        "§3.11 engine divergence at row {} col {}.\n  tmux cell: {:?}\n  ours cell: {:?}\n(corpus: the same bytes into both; a divergence means the engines parsed them differently — an R1 defect)",
        divergence.as_ref().map(|d| d.0).unwrap_or(0),
        divergence.as_ref().map(|d| d.1).unwrap_or(0),
        divergence.as_ref().map(|d| d.2).unwrap(),
        divergence.as_ref().map(|d| d.3).unwrap(),
    );
}

#[test]
fn round_trip_render_then_parse_is_identity() {
    use ldmux_vt_sys::StyleColor;
    use ldmux_daemon::sgr::BLANK;

    // A synthetic content row exercising every branch of both sides:
    // plain, bold, underline, 256-colour, 24-bit colour, then back to plain.
    let mut bold = Cell { text: "B".into(), style: BLANK };
    bold.style.bold = true;
    let mut ul = Cell { text: "U".into(), style: BLANK };
    ul.style.underline_kind = 1;
    let mut pal = Cell { text: "C".into(), style: BLANK };
    pal.style.fg = Some(StyleColor::Palette(123));
    let mut rgb = Cell { text: "D".into(), style: BLANK };
    rgb.style.fg = Some(StyleColor::Rgb(255, 128, 0));
    let plain = Cell { text: "E".into(), style: BLANK };
    let row = vec![plain.clone(), bold, ul, pal, rgb, plain];
    let width = 8;

    // Render our format, then parse it back: the content columns come back
    // exactly (the format is faithful to the cells).
    let rendered = ldmux_daemon::snapshot::render(&[row.as_slice()], width);
    let parsed = sgrep::parse(&rendered, width);
    assert_eq!(parsed.len(), 1, "one row");
    for x in 0..row.len() {
        assert_eq!(
            &parsed[0][x],
            &row[x],
            "cell {x} — rendered: {rendered:?}"
        );
    }
}
