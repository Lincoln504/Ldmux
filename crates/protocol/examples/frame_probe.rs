use ldmux_protocol::mesh_term::{Message, Patch};

struct Row {
    y: u8,
    text: &'static str,
}

fn main() {
    let rows = [
        Row { y: 0, text: r##"$ cargo build --release"## },
        Row { y: 0, text: r##"   Compiling ldmux-protocol v0.1.0 (/home/ldeen/Documents/ldmux/crates/protocol)"## },
        Row { y: 0, text: r##"   Compiling ldmux-session v0.1.0 (/home/ldeen/Documents/ldmux/crates/session)"## },
        Row { y: 0, text: r##"    Finished `release` profile [optimized] target(s) in 41.28s"## },
        Row { y: 0, text: r##"$ ldmux mesh status"## },
        Row { y: 0, text: r##"state    ready"## },
        Row { y: 0, text: r##"preset   910.525 MHz  62.5 kHz  SF7  CR5  path 1 B  22 dBm"## },
        Row { y: 0, text: r##"scope    none (floods are unscoped, which most meshes forward)"## },
        Row { y: 0, text: r##"capacity 171 B per carrier record"## },
        Row { y: 0, text: r##"route    direct (no repeater in the path)"## },
        Row { y: 0, text: r##"signal   -76 dBm, SNR 9.75 dB over a -88 dBm noise floor"## },
        Row { y: 0, text: r##"airtime  24.1 s this hour"## },
        Row { y: 0, text: r##"session  established"## },
        Row { y: 0, text: r##"  records      21 sent, 6 retransmitted, 0 dropped, 0 pending"## },
        Row { y: 0, text: r##"  received     7 bytes"## },
        Row { y: 0, text: r##"$ "## },
        Row { y: 0, text: r##"ldmux "## },
    ];
    let mut inner = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        inner.extend_from_slice(
            &Message::Rows {
                epoch: 1,
                full: i == 0,
                patches: vec![Patch {
                    y: r.y,
                    append: false,
                    text: r.text.to_owned(),
                }],
            }
            .encode()
            .unwrap(),
        );
    }
    inner.extend_from_slice(
        &Message::Commit {
            epoch: 1,
            cursor_x: 0,
            cursor_y: 16,
            flags: 0,
            first_line: 0,
        }
        .encode()
        .unwrap(),
    );
    let n = inner.len();
    println!("inner {n} B");
    match (Message::Frame { inner }).encode() {
        Ok(b) => println!("encoded message {} bytes (body {}), MAX_BODY 300: {}", b.len(), b.len() - 3, b.len() - 3 <= 300),
        Err(e) => println!("ENCODE FAILED: {e:?}"),
    }
}
