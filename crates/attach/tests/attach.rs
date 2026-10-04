//! The attach client's local-terminal helpers. The full daemon↔attach loop is
//! tested in `crates/daemon/tests/attach_client.rs` (which can start a daemon;
//! the attach crate cannot, without a dependency cycle).

/// The terminal size falls back to 80x24 when fd 0 is not a tty.
#[test]
fn tty_size_is_sane() {
    let (c, r) = attach::tty_size_for_test();
    assert!(c > 0 && r > 0, "size must be positive, got {c}x{r}");
}
