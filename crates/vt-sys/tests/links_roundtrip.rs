//! OSC 8 hyperlinks through the snapshot path: engine -> `links::row_links` ->
//! `snapshot::render_with_links` -> a second engine, which must see the same
//! links on the same cells.

use ldmux_vt_sys::links::{self, URI_MAX};
use ldmux_vt_sys::snapshot::{self, LINK_DISTINCT_MAX};
use ldmux_vt_sys::{Cell, RenderState, Terminal};

const COLS: u16 = 40;
const ROWS: u16 = 4;

struct Shot {
    cells: Vec<Vec<Cell>>,
    links: Vec<Vec<Option<String>>>,
}

fn capture(bytes: &[u8]) -> Shot {
    let mut t = Terminal::new(COLS, ROWS).unwrap();
    t.write(bytes);
    let rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();
    Shot {
        cells: (0..rs.rows()).map(|y| rs.row(y)).collect(),
        links: (0..rs.rows())
            .map(|y| links::row_links(&t, rs.cols(), y))
            .collect(),
    }
}

fn render(s: &Shot) -> String {
    let refs: Vec<&[Cell]> = s.cells.iter().map(|r| r.as_slice()).collect();
    snapshot::render_with_links(&refs, &s.links, COLS as usize)
}

/// Link of cell (x, y), flattening "no links in this row" to None.
fn link_at(s: &Shot, x: usize, y: usize) -> Option<&str> {
    s.links.get(y)?.get(x)?.as_deref()
}

/// A snapshot replays into a screen, so rows are CRLF-joined.
fn replay(text: &str) -> Shot {
    capture(text.replace('\n', "\r\n").as_bytes())
}

#[test]
fn links_survive_the_round_trip_on_the_same_cells() {
    let src = b"plain \x1b]8;;https://a.example/x\x1b\\link-a\x1b]8;;\x1b\\ mid \
\x1b]8;;https://b.example/y\x07link-b\x1b]8;;\x07 tail\r\nsecond row";
    let first = capture(src);
    assert_eq!(link_at(&first, 6, 0), Some("https://a.example/x"));
    assert_eq!(link_at(&first, 5, 0), None);

    let text = render(&first);
    assert!(text.contains("\x1b]8;;https://a.example/x\x1b\\link-a\x1b]8;;\x1b\\"));
    let second = replay(&text);
    assert_eq!(first.links, second.links, "same links on the same cells");
    let texts = |s: &Shot| -> Vec<String> {
        s.cells
            .iter()
            .map(|r| r.iter().map(|c| c.text.as_str()).collect())
            .collect()
    };
    assert_eq!(texts(&first), texts(&second));
}

#[test]
fn a_link_is_closed_at_the_end_of_its_row() {
    // The link covers row 0 to its last column and is closed by the program only
    // afterwards, so row 0 ends inside the link.
    let mut src = b"\x1b]8;;https://a.example/\x1b\\".to_vec();
    src.extend(std::iter::repeat_n(b'a', COLS as usize));
    src.extend_from_slice(b"\x1b]8;;\x1b\\\r\nnext");
    let first = capture(&src);
    let text = render(&first);
    let row0 = text.split('\n').next().unwrap();
    assert!(
        row0.ends_with("\x1b]8;;\x1b\\"),
        "row 0 closes its link: {row0:?}"
    );
    let second = replay(&text);
    assert_eq!(
        link_at(&second, COLS as usize - 1, 0),
        Some("https://a.example/")
    );
    for x in 0..4 {
        assert_eq!(link_at(&second, x, 1), None, "row 1 is not linked");
    }
    assert_eq!(first.links, second.links);
}

#[test]
fn a_styled_linked_run_keeps_both() {
    let first = capture(b"\x1b[1;4:3;31m\x1b]8;;https://a.example/\x1b\\hi\x1b]8;;\x1b\\\x1b[0m");
    let second = replay(&render(&first));
    assert_eq!(link_at(&second, 0, 0), Some("https://a.example/"));
    assert_eq!(second.cells[0][0].style, first.cells[0][0].style);
    assert!(second.cells[0][0].style.bold);
    assert_eq!(second.cells[0][0].style.underline_kind, 3);
}

#[test]
fn an_oversize_uri_is_dropped_but_its_text_stays() {
    let mut src = b"\x1b]8;;https://a.example/".to_vec();
    src.extend(std::iter::repeat_n(b'a', URI_MAX));
    src.extend_from_slice(b"\x1b\\long\x1b]8;;\x1b\\ ok");
    let first = capture(&src);
    assert_eq!(link_at(&first, 0, 0), None);
    let text = render(&first);
    assert!(
        !text.contains("\x1b]8"),
        "no OSC 8 at all: {} bytes",
        text.len()
    );
    assert!(text.starts_with("long ok"));
}

/// The engine's own OSC buffer is a little under `URI_MAX` (a 2046-byte target is
/// delivered, 2047 is not), so the engine is the tighter bound in practice and
/// `URI_MAX` is the backstop for anything built by hand.
#[test]
fn the_longest_uri_the_engine_delivers_is_carried() {
    let prefix = "https://a.example/";
    let uri = format!("{prefix}{}", "a".repeat(2046 - prefix.len()));
    let first = capture(format!("\x1b]8;;{uri}\x1b\\x\x1b]8;;\x1b\\").as_bytes());
    assert_eq!(link_at(&first, 0, 0), Some(uri.as_str()));
    let second = replay(&render(&first));
    assert_eq!(link_at(&second, 0, 0), Some(uri.as_str()));
}

#[test]
fn a_hand_built_uri_over_the_cap_is_refused_by_the_renderer() {
    let cells = [Cell {
        text: "x".into(),
        style: ldmux_vt_sys::sgr::BLANK,
    }];
    let big = format!("https://a/{}", "a".repeat(URI_MAX));
    let links = vec![vec![Some(big)]];
    assert_eq!(snapshot::render_with_links(&[&cells], &links, 1), "x");
}

#[test]
fn a_hostile_uri_cannot_inject_into_the_replay() {
    // Controls in a hand-built link table are refused by the renderer itself,
    // whatever produced them.
    let cells = [Cell {
        text: "x".into(),
        style: ldmux_vt_sys::sgr::BLANK,
    }];
    let links = vec![vec![Some("https://a/\x1b[2J".to_string())]];
    let text = snapshot::render_with_links(&[&cells], &links, 1);
    assert_eq!(text, "x");
}

#[test]
fn distinct_links_are_capped() {
    // More distinct links than the cap, one cell each, across several rows.
    let mut t = Terminal::new(COLS, ROWS).unwrap();
    let mut s = String::new();
    for i in 0..(LINK_DISTINCT_MAX + 20) {
        s.push_str(&format!("\x1b]8;;https://h/{i}\x1b\\x\x1b]8;;\x1b\\"));
        if (i + 1) % COLS as usize == 0 {
            s.push_str("\r\n");
        }
    }
    t.write(s.as_bytes());
    let rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();
    // The screen holds only COLS*ROWS cells, so feed the renderer a synthetic
    // table instead: one distinct URI per cell over many rows.
    let n = LINK_DISTINCT_MAX + 20;
    let cells: Vec<Vec<Cell>> = (0..n)
        .map(|_| {
            vec![Cell {
                text: "x".into(),
                style: ldmux_vt_sys::sgr::BLANK,
            }]
        })
        .collect();
    let links: Vec<Vec<Option<String>>> = (0..n)
        .map(|i| vec![Some(format!("https://h/{i}"))])
        .collect();
    let refs: Vec<&[Cell]> = cells.iter().map(|r| r.as_slice()).collect();
    let text = snapshot::render_with_links(&refs, &links, 1);
    assert_eq!(text.matches("\x1b]8;;https").count(), LINK_DISTINCT_MAX);
    assert_eq!(
        text.split('\n').count(),
        n,
        "every row's text is still there"
    );
}

#[test]
fn no_links_is_byte_identical_to_render() {
    let first = capture(b"\x1b[1mbold\x1b[0m and plain\r\nrow two");
    let refs: Vec<&[Cell]> = first.cells.iter().map(|r| r.as_slice()).collect();
    assert_eq!(
        snapshot::render(&refs, COLS as usize),
        snapshot::render_with_links(&refs, &first.links, COLS as usize)
    );
}
