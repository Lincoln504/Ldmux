//! Drives the client C ABI the way C does: raw pointers, status codes, caller-owned buffers.
//!
//! Nothing here uses the safe `ldmux_session::client` API. The point is the boundary: a null handle,
//! a length that lies, a buffer too small, a call after `free`, and a panic behind the wall.

use std::ptr::{null, null_mut, NonNull};

use ldmux_session::handshake::Keypair;
use ldmux_session_ffi::*;

const NOW: u64 = 1_800_000_000_000;

/// A config for a phone talking to one host, with real keys.
fn config() -> LdmuxClientConfig {
    let host = Keypair::generate().expect("a keypair");
    let phone = Keypair::generate().expect("a keypair");
    LdmuxClientConfig {
        peer_static: host.public,
        psk: [0x42; 32],
        local_private: phone.private,
        local_public: phone.public,
        path_width: 1,
        sf: 7,
        flood_reach_hops: 4,
        max_open_pages: 5,
        airtime_ms: 602.6,
        scope: 0,
    }
}

/// A live handle, freed when it goes out of scope.
struct Handle(*mut LdmuxClient);

impl Handle {
    fn new(cfg: &LdmuxClientConfig) -> Self {
        let mut out: *mut LdmuxClient = null_mut();
        // SAFETY: `cfg` and `out` are both live for the call.
        let rc = unsafe { ldmux_client_new(cfg, &mut out) };
        assert_eq!(rc, 0, "ldmux_client_new refused a valid config: {rc}");
        assert!(!out.is_null(), "a successful create returned a null handle");
        Self(out)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `ldmux_client_new` and is freed once here.
        unsafe { ldmux_client_free(&mut self.0) };
    }
}

#[test]
fn a_null_handle_is_refused_rather_than_dereferenced() {
    let mut mirror = LdmuxMirror::default();
    // SAFETY: every call below takes a null handle on purpose; the ABI promises to refuse it.
    unsafe {
        assert_eq!(ldmux_client_established(null_mut()), LDMUX_ERR_NULL);
        assert_eq!(ldmux_client_connect(null_mut(), NOW, NOW), LDMUX_ERR_NULL);
        assert_eq!(
            ldmux_client_on_ble_rx(null_mut(), [0x88, 0, 0].as_ptr(), 3, NOW),
            LDMUX_ERR_NULL
        );
        assert_eq!(
            ldmux_client_mirror(null_mut(), b"x".as_ptr(), 1, &mut mirror),
            LDMUX_ERR_NULL
        );
        // Freeing a null slot is a no-op rather than a crash.
        ldmux_client_free(null_mut());
        // Freeing a live handle through a null-typed slot twice: the second is a no-op.
        let mut slot: *mut LdmuxClient = null_mut();
        ldmux_client_free(&mut slot);
    }
}

#[test]
fn a_length_that_lies_is_refused() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let mut op = 0u32;
    // SAFETY: the pointers are live; the *lengths* are the lie under test.
    unsafe {
        assert_eq!(
            ldmux_client_open(h.0, null(), 4, 46, 20, NOW),
            LDMUX_ERR_ARG,
            "a null pointer with a non-zero length must be refused"
        );
        assert_eq!(
            ldmux_client_open(h.0, b"shell".as_ptr(), 0, 46, 20, NOW),
            LDMUX_ERR_ARG,
            "an empty shell id must be refused"
        );
        assert_eq!(
            ldmux_client_open(h.0, b"shell".as_ptr(), LDMUX_SHELL_MAX + 1, 46, 20, NOW),
            LDMUX_ERR_ARG,
            "a shell id past the wire's own limit must be refused"
        );
        assert_eq!(
            ldmux_client_send(
                h.0,
                b"shell".as_ptr(),
                5,
                LDMUX_ITEM_TEXT,
                null(),
                0,
                80,
                24,
                NOW,
                &mut op
            ),
            LDMUX_ERR_ARG,
            "a send with no bytes must be refused"
        );
        assert_eq!(
            ldmux_client_send(
                h.0,
                b"shell".as_ptr(),
                5,
                LDMUX_ITEM_TEXT,
                b"ls".as_ptr(),
                2,
                80,
                24,
                NOW,
                &mut op
            ),
            LDMUX_ERR_NO_PAGE,
            "a send to a shell that was never opened has no page to go to"
        );
        assert_eq!(
            ldmux_client_on_ble_rx(h.0, null(), 9, NOW),
            LDMUX_ERR_ARG,
            "a null frame with a length must be refused"
        );
    }
}

#[test]
fn a_normal_loop_drives_a_connect_and_gets_bytes_on_the_air() {
    let cfg = config();
    let h = Handle::new(&cfg);
    // SAFETY: the handle is live for every call.
    unsafe {
        assert_eq!(ldmux_client_connect(h.0, NOW, NOW), 0);
    }
    let mut out = vec![0u8; 512];
    let mut len = 0usize;
    let mut written = 0;
    for step in 0..20u64 {
        // SAFETY: `out` is `out.len()` writable bytes and `len` is writable.
        let rc = unsafe {
            ldmux_client_poll(
                h.0,
                NOW + step * 1_000,
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        assert!(rc >= 0, "poll failed: {rc}");
        if rc == 1 {
            written += 1;
            assert!(len > 3, "a frame is a header plus a payload");
            // The frame is the companion protocol's own: `<`, a length, then the command.
            assert_eq!(out[0], 0x3C, "host start byte");
            assert_eq!(out[3], 0x41, "CMD_SEND_RAW_PACKET");
        }
    }
    assert!(
        written > 0,
        "a phone that connects owes the air an initiation and never emitted one"
    );

    // A deadline the caller can set a timer from.
    let mut deadline = 0u64;
    // SAFETY: `deadline` is writable.
    let rc = unsafe { ldmux_client_next_deadline(h.0, &mut deadline) };
    assert!(rc == 0 || rc == 1);
    if rc == 1 {
        assert!(deadline > NOW, "a rung deadline is in the future");
    }
    // SAFETY: the handle is live and not freed.
    assert_eq!(unsafe { ldmux_client_established(h.0) }, 0);
}

#[test]
fn a_buffer_too_small_reports_the_size_and_keeps_the_frame() {
    let cfg = config();
    let h = Handle::new(&cfg);
    // SAFETY: the handle is live.
    unsafe { assert_eq!(ldmux_client_connect(h.0, NOW, NOW), 0) };

    // Poll with nowhere to write: the frame must be reported, not dropped.
    let mut len = 0usize;
    // SAFETY: a null buffer with capacity 0 is the documented way to ask how big it is.
    let rc = unsafe { ldmux_client_poll(h.0, NOW, null_mut(), 0, &mut len) };
    assert_eq!(rc, LDMUX_ERR_BUFFER, "a zero-capacity buffer is too small");
    assert!(len > 3, "the needed length must be reported");

    // The same frame is still there, and now it fits.
    let mut out = vec![0u8; len];
    let mut got = 0usize;
    // SAFETY: `out` is `len` writable bytes, which is what the ABI just asked for.
    let rc = unsafe {
        ldmux_client_poll(
            h.0,
            NOW,
            out.as_mut_ptr(),
            out.len(),
            &mut got,
        )
    };
    assert_eq!(rc, 1, "the frame a short buffer preserved must come back");
    assert_eq!(got, len);
    assert_eq!(out[0], 0x3C);
}

#[test]
fn a_shell_id_that_is_not_utf8_is_refused() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let bad = [0xFFu8, 0xFE];
    // SAFETY: the buffer is live; the bytes are the thing under test.
    let rc = unsafe { ldmux_client_open(h.0, bad.as_ptr(), bad.len(), 46, 20, NOW) };
    assert_eq!(rc, LDMUX_ERR_ARG);
}

#[test]
fn a_notification_that_is_not_a_session_record_is_quietly_ignored() {
    let cfg = config();
    let h = Handle::new(&cfg);
    // An advert, as the receive log would push it: `0x88`, SNR, RSSI, then an advert packet.
    let frame = [0x88u8, 0xF4, 0xA6, 0x01 | (0x04 << 2), 0x00];
    // SAFETY: `frame` is live for its length.
    let rc = unsafe { ldmux_client_on_ble_rx(h.0, frame.as_ptr(), frame.len(), NOW) };
    assert_eq!(rc, 0, "somebody else's traffic is not ours");
}

#[test]
fn using_a_freed_handle_is_refused_and_free_twice_is_a_no_op() {
    let cfg = config();
    let mut raw: *mut LdmuxClient = null_mut();
    // SAFETY: `cfg` and `raw` are live.
    assert_eq!(unsafe { ldmux_client_new(&cfg, &mut raw) }, 0);
    assert!(!raw.is_null());
    // SAFETY: the handle is live and freed once here.
    unsafe { ldmux_client_free(&mut raw) };
    assert!(raw.is_null(), "free nulls the caller's own slot");
    // SAFETY: the slot is null, so this is the documented no-op.
    unsafe { ldmux_client_free(&mut raw) };
    // SAFETY: a nulled handle is refused rather than dereferenced.
    assert_eq!(unsafe { ldmux_client_established(raw) }, LDMUX_ERR_NULL);
}

#[test]
fn the_event_drain_reports_nothing_rather_than_inventing_something() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let mut event = LdmuxClientEvent::default();
    let mut text = [0u8; 64];
    let mut len = 0usize;
    // SAFETY: all three buffers are live for their lengths.
    let rc = unsafe {
        ldmux_client_poll_event(
            h.0,
            &mut event,
            text.as_mut_ptr(),
            text.len(),
            &mut len,
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(len, 0);
    assert_eq!(event.kind, 0);
}

#[test]
fn a_config_with_an_impossible_path_width_is_refused() {
    let mut cfg = config();
    cfg.path_width = 4; // reserved by the firmware
    let mut out: *mut LdmuxClient = null_mut();
    // SAFETY: both arguments are live.
    assert_eq!(unsafe { ldmux_client_new(&cfg, &mut out) }, LDMUX_ERR_ARG);
    assert!(out.is_null(), "a refused create must not leave a handle");
}

#[test]
fn the_stats_struct_is_filled_from_the_engine() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let mut stats = LdmuxClientStats::default();
    // SAFETY: the handle and the struct are live.
    let rc = unsafe { ldmux_client_stats(h.0, &mut stats) };
    assert_eq!(rc, 0);
    // Nothing has happened yet, so every counter is zero — and the call is still honest about it.
    assert_eq!(stats.records_sent, 0);
    assert_eq!(stats.sessions_established, 0);
}

#[test]
fn the_handle_is_a_real_pointer_the_caller_can_hold() {
    // A C caller stores the handle in a struct field; make sure the ABI's own type is usable that
    // way rather than only as a local.
    struct Holder {
        client: NonNull<LdmuxClient>,
    }
    let cfg = config();
    let mut raw: *mut LdmuxClient = null_mut();
    // SAFETY: `cfg` and `raw` are live.
    assert_eq!(unsafe { ldmux_client_new(&cfg, &mut raw) }, 0);
    let holder = Holder {
        // SAFETY: `raw` is non-null after a successful create.
        client: unsafe { NonNull::new_unchecked(raw) },
    };
    // SAFETY: the handle is live and freed once.
    assert_eq!(unsafe { ldmux_client_established(holder.client.as_ptr()) }, 0);
    // SAFETY: the handle came from `ldmux_client_new`.
    unsafe { ldmux_client_free(&mut raw) };
}
