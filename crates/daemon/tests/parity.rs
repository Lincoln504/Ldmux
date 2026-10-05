//! The §3.11 snapshot-parity gate (the plan's #1 risk, R1).
//!
//! **The gate is on the cells, not the SGR bytes.** Plan §3.11(3) is explicit:
//! "the cells are the source of truth; the format is ours." So this harness
//! pins two halves separately, and **neither needs tmux (or any other external
//! program) at test time**:
//!
//! 1. `golden_corpus_cells_match_the_reference_grids` — the *engine* half: for
//!    every `parity/NN-name.bin` corpus file, the same input bytes fed to our
//!    engine (the `RenderState` walk) must produce the grid recorded in the
//!    committed golden `parity/NN-name.capture`, compared cell by cell. A
//!    divergence is a real engine defect (R1: a wrong screen with no error).
//! 2. `round_trip_render_then_parse_is_identity` — the *format* half: our SGR
//!    spelling (`sgr.rs`, a reset-then-re-issue encoding) is faithful to the
//!    cells — parsing what `snapshot::render` emits recovers the cells.
//!
//! **Provenance of the goldens** (`parity/README.md`): each `.capture` is what
//! an independent terminal emulator (tmux 3.5a) showed for the matching
//! `.bin`, captured once on 2026-10-05 with `capture-pane -p -e -N` by
//! `scripts/regen_parity_golden.sh`. Regenerating is deliberate and manual;
//! `cargo test` never runs it. The goldens are decoded by `sgrep`, a narrow
//! SGR-dump → cells decoder.
//!
//! The corpus: basic SGR, every attribute (bold/faint/italic/underline/blink/
//! inverse/strike), the 16 ANSI + 256 + 24-bit colours, wide (CJK/emoji) and
//! combining characters, line wrap, OSC 8 hyperlinks, and scroll (40 lines
//! into a 24-row grid, so only the visible tail is compared). Overline (SGR 53)
//! is deliberately absent: tmux 3.5a mis-serialises it (`ESC[5:3m`), so it has
//! no trustworthy reference; it is pinned by the render round-trip instead.

mod sgrep;

use ldmux_vt_sys::{Cell, RenderState, Terminal};

/// The viewport geometry, pinned (the reference pane and the engine match).
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Our engine's grid for a corpus: feed the bytes, walk the rows.
fn our_cells(corpus: &[u8]) -> Vec<Vec<Cell>> {
    let mut term = Terminal::new(COLS, ROWS).expect("the engine allocates");
    term.write(corpus);
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
    use ldmux_vt_sys::sgr::BLANK;
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

#[test]
fn golden_corpus_cells_match_the_reference_grids() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity");
    let mut corpora: Vec<_> = std::fs::read_dir(&dir)
        .expect("the parity directory exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "bin"))
        .collect();
    corpora.sort();
    assert!(corpora.len() >= 7, "the corpus is missing files: {corpora:?}");
    for bin in corpora {
        let name = bin.file_stem().unwrap().to_string_lossy().into_owned();
        let golden = std::fs::read_to_string(bin.with_extension("capture"))
            .unwrap_or_else(|e| panic!("{name}: golden missing ({e}); run scripts/regen_parity_golden.sh"));
        let corpus = std::fs::read(&bin).unwrap();
        let want = normalize(sgrep::parse(&golden, COLS as usize), COLS as usize);
        let got = normalize(our_cells(&corpus), COLS as usize);
        assert_eq!(want.len(), got.len(), "{name}: row count");
        for (y, (wr, gr)) in want.iter().zip(&got).enumerate() {
            assert_eq!(wr.len(), gr.len(), "{name}: row {y} width");
            for (x, (wc, gc)) in wr.iter().zip(gr).enumerate() {
                assert_eq!(
                    wc, gc,
                    "§3.11 engine divergence in {name} at row {y} col {x}.\n  golden: {wc:?}\n  ours:   {gc:?}\n(a divergence means the engines parsed the same bytes differently — an R1 defect)"
                );
            }
        }
    }
}

#[test]
fn round_trip_render_then_parse_is_identity() {
    use ldmux_vt_sys::{sgr::BLANK, StyleColor};

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
    let rendered = ldmux_vt_sys::snapshot::render(&[row.as_slice()], width);
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
