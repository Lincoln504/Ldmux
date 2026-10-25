//! The Kitty keyboard protocol, end to end: a program that asks for it, and a
//! modified key that only exists under it.
//!
//! This is the path that makes `Shift+Enter` a newline instead of a submit in a
//! TUI such as `pi`. It crosses three components, which is why it is one test:
//! the keeper's emulator answers the program's capability query, its state then
//! reports the flags, and the daemon encodes the key the client sent as `CSI u`.
//! Any of the three missing and the key silently degrades to `Enter` — which is
//! exactly the bug that was reported ("Shift+Enter submits instead of inserting a
//! line").

mod common;

use std::time::Duration;

use common::rig::Rig;

/// A program asks the terminal whether it speaks the protocol; the answer turns
/// the protocol on for the session; then `S-Enter` from the client arrives as
/// `CSI 13 ; 2 u` rather than `CR`.
#[tokio::test]
async fn shift_enter_reaches_a_program_as_a_distinct_kitty_key() {
    let rig = Rig::start().await;
    let (mut ws, _) = rig.attach_with(&[], false).await;

    // What `pi` does at startup: push the disambiguate flag and ask what the
    // terminal has. The shell printing the push is enough for the keeper's
    // emulator to parse it, exactly as a program's stdout would be. Then read raw
    // input, so what the daemon writes is what the test observes rather than a line
    // editor's idea of it.
    //
    // **The marker is the synchronisation, not a sleep.** Everything after it in the
    // same stream happened after the push, so a client that has read "READY" is
    // looking at a keeper whose state already carries the flag. A fixed sleep was
    // enough alone (600 ms) and not under `cargo test --workspace`: the key then
    // arrived while the flags were still 0, the daemon encoded a plain `CR`, and the
    // test waited the full 60 s for a sequence that was never due.
    // **The program prints the query on stdout** — that is how a program asks a
    // terminal — then reads the answer. `cat -v` because the keeper drops a *reply*
    // from the pane's output (a reply is not content, and an echoed one would be
    // answered for ever, `without_kitty_replies`); `-v` renders what the program read
    // so this test can see it.
    ws.send_json(&serde_json::json!({
        "type":"input",
        "data":"printf '\\033[>1u\\033[?u'; stty raw -echo; exec cat -v\n",
    }))
    .await;
    // **The answer to the query is the synchronisation.** `pi` pushes the flag and
    // asks what the terminal has, and it treats keys as protocol keys only after the
    // answer arrives — so this is the program's own handshake, not a proxy for it.
    // Seeing `CSI ? 1 u` proves the keeper's emulator parsed the push *and* is the
    // instance answering, which is exactly the state a later key batch reads.
    //
    // A sleep was the first version of this and it was wrong in both directions: 600
    // ms was enough alone and not under load (the key then arrived with flags 0 and
    // the daemon wrote a plain `CR`), and "READY" is not a sync either — output is
    // forwarded to clients slightly ahead of the state a `Read` answers with, which
    // is why the flag can be visible and not yet in effect.
    let mut out = Vec::new();
    ws.until_output(&mut out, "^[[?1u", Duration::from_millis(300))
        .await;

    ws.send_json(&serde_json::json!({"type":"key","keys":["S-Enter"]}))
        .await;
    // `^[[13;2u` is what the program read, rendered: the daemon wrote
    // `ESC [ 1 3 ; 2 u` into the pane.
    ws.until_output(&mut out, "^[[13;2u", Duration::from_millis(300))
        .await;

    // And a plain Enter is still `CR`: a protocol parser accepts it, and programs
    // that never asked for the protocol require it.
    ws.send_json(&serde_json::json!({"type":"key","keys":["Enter"]}))
        .await;
    // A fresh buffer: `out` already holds what was read before this key, and a
    // needle found in that proves nothing. `^M` is a `CR` as `cat -v` renders it.
    let mut after = Vec::new();
    ws.until_output(&mut after, "^M", Duration::from_millis(300))
        .await;
}
