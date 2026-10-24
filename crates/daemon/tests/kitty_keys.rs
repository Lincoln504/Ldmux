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
    // emulator to parse it, exactly as a program's stdout would be.
    ws.send_json(&serde_json::json!({"type":"input","data":"printf '\\033[>1u'\n"}))
        .await;
    // Then read raw input, so what the daemon writes is what the test observes
    // rather than a line editor's idea of it.
    ws.send_json(&serde_json::json!({"type":"input","data":"stty raw -echo; exec cat\n"}))
        .await;
    // Long enough for the shell to run both commands on a loaded machine.
    tokio::time::sleep(Duration::from_millis(600)).await;

    ws.send_json(&serde_json::json!({"type":"key","keys":["S-Enter"]}))
        .await;
    let mut out = Vec::new();
    ws.until_output(&mut out, "\u{1b}[13;2u", Duration::from_millis(300))
        .await;

    // And a plain Enter is still `CR`: a protocol parser accepts it, and programs
    // that never asked for the protocol require it.
    ws.send_json(&serde_json::json!({"type":"key","keys":["Enter"]}))
        .await;
    ws.until_output(&mut out, "\r", Duration::from_millis(300))
        .await;
}
