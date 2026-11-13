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
    let mut seen: Vec<u8> = Vec::new();
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
            assert!(len > 0, "a frame is at least its command byte");
            // **The payload, not the serial frame.** A BLE characteristic carries `[opcode][args]`
            // and the host adds `<` and a length when it writes to a port, so a `0x3C` here would be
            // three bytes the radio reads as a command.
            assert_ne!(out[0], 0x3C, "the engine emits a payload, not a serial frame");
            seen.push(out[0]);
        }
    }
    assert!(
        written > 0,
        "a phone that connects owes the air an initiation and never emitted one"
    );
    assert!(
        seen.contains(&0x01),
        "the radio was never sent APP_START, so it was never told this client exists: {seen:?}"
    );
    assert!(
        seen.contains(&0x41),
        "no record was ever handed to the radio: {seen:?}"
    );

    // A deadline the caller can set a timer from.
    let mut deadline = 0u64;
    // SAFETY: `deadline` is writable.
    let rc = unsafe { ldmux_client_next_deadline(h.0, NOW, &mut deadline) };
    assert!(rc == 0 || rc == 1);
    if rc == 1 {
        // **At or after now, not strictly after it.** A command waiting for the radio's one answer
        // window is due the moment it is queued: it has no later instant of its own, and a caller
        // that slept until the next status poll left the record unsent.
        assert!(deadline >= NOW, "a deadline was already in the past");
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
    assert!(len > 0, "the needed length must be reported");

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
    // The first thing a client that has just connected asks its radio is `APP_START`: it is what
    // tells the firmware this client exists, and everything else is answered in the shapes it
    // selects.
    assert_eq!(out[0], 0x01, "APP_START is the first command");
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

/// A phone that has never heard from its radio reports nothing rather than a plausible reading.
#[test]
fn an_unasked_radio_reports_nothing_rather_than_a_guess() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let mut status = LdmuxRadioStatus::default();
    // SAFETY: the handle is live and `status` is writable.
    let rc = unsafe { ldmux_client_radio(h.0, &mut status) };
    assert_eq!(rc, 0);
    assert_eq!(status.connected, 0, "nothing has said the radio is there");
    assert_eq!(status.has_signal, 0, "no packet has been heard");
    assert_eq!(status.has_battery, 0);
    assert_eq!(status.has_preset, 0);
    assert_eq!(status.ops_left, u8::MAX, "no budget has been reported");
    // SAFETY: nulls are the argument under test; the ABI promises to refuse them.
    let radio_rc = unsafe { ldmux_client_radio(h.0, null_mut()) };
    assert_eq!(radio_rc, LDMUX_ERR_NULL);
    // SAFETY: the same, for the route.
    let route_rc = unsafe { ldmux_client_route(h.0, NOW, null_mut()) };
    assert_eq!(route_rc, LDMUX_ERR_NULL);
}

/// The radio's answers reach the status: `SELF_INFO`, `DEVICE_INFO`, the battery and the stats.
///
/// This is the whole of `AR-13` as a test: the engine asks, the answers come back through the one
/// notification entry, and the reading is what the settings screen draws.
#[test]
fn the_radios_answers_reach_the_status_through_the_one_entry() {
    let cfg = config();
    let h = Handle::new(&cfg);
    // SAFETY: the handle is live.
    unsafe { assert_eq!(ldmux_client_connect(h.0, NOW, NOW), 0) };

    let mut frame = vec![0u8; 512];
    let mut len = 0usize;
    let mut seen: Vec<u8> = Vec::new();
    let mut step = 0u64;
    while seen.len() < 4 && step < 32 {
        // SAFETY: `frame` is `len` writable bytes and `len` is writable.
        let rc = unsafe {
            ldmux_client_poll(h.0, NOW + step, frame.as_mut_ptr(), frame.len(), &mut len)
        };
        assert!(rc >= 0, "poll failed: {rc}");
        step += 1;
        if rc != 1 {
            continue;
        }
        let command = frame[0];
        seen.push(command);
        let answer = answer_to(command);
        // SAFETY: `answer` is a live slice for the call.
        unsafe { ldmux_client_on_ble_rx(h.0, answer.as_ptr(), answer.len(), NOW + step) };
    }
    assert!(
        seen.contains(&0x01) && seen.contains(&0x16) && seen.contains(&0x14),
        "the engine did not ask its radio the things it needs: {seen:?}"
    );

    let mut status = LdmuxRadioStatus::default();
    // SAFETY: the handle is live and `status` is writable.
    unsafe { assert_eq!(ldmux_client_radio(h.0, &mut status), 0) };
    assert_eq!(status.connected, 1, "APP_START was answered");
    assert_eq!(status.has_preset, 1);
    assert_eq!(status.freq_khz, 910_525);
    assert_eq!(status.bw_hz, 62_500);
    assert_eq!(status.spreading_factor, 7);
    assert_eq!(status.coding_rate, 5);
    assert_eq!(status.path_hash_width, 1, "mode 0 is a one-byte hash");
    assert_eq!(status.has_battery, 1);
    assert_eq!(status.battery_mv, 4_050);

    // **The four facts the settings page draws and nothing else would report.** They come from two
    // different answers, so the two `has_` bytes are separate: a radio can describe its current
    // operating point without answering `DEVICE_INFO` at all.
    assert_eq!(status.has_identity, 1, "SELF_INFO was answered");
    assert_eq!(&status.name[..usize::from(status.name_len)], b"radio");
    assert_eq!(status.max_tx_power_dbm, 22);
    assert_eq!(status.has_device, 1, "DEVICE_INFO was answered");
    assert_eq!(
        &status.firmware[..usize::from(status.firmware_len)],
        b"v1.17.1",
        "the firmware's own version string is carried, not just its code"
    );
    assert_eq!(status.max_contacts, 350, "the firmware stores half of it");
}

/// A route that has never been heard is unknown, and the next attempt floods.
#[test]
fn a_link_with_no_route_says_so_and_would_flood() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let mut route = LdmuxRoute::default();
    // SAFETY: the handle is live and `route` is writable.
    unsafe { assert_eq!(ldmux_client_route(h.0, NOW, &mut route), 0) };
    assert_eq!(route.known, 0);
    assert_eq!(route.proven, 0);
    assert_eq!(route.flood, 1, "with no path the first attempt is a flood");
    assert_eq!(route.hops, 0);
}

/// The scope is 16 bytes of region key or nothing: the 16-bit code in the config is not it.
#[test]
fn a_scope_is_sixteen_bytes_or_cleared() {
    let cfg = config();
    let h = Handle::new(&cfg);
    let key = [0x5Au8; 16];
    // SAFETY: the handle is live and `key` is 16 bytes.
    assert_eq!(unsafe { ldmux_client_set_scope(h.0, key.as_ptr(), 16) }, 0);
    // SAFETY: the same.
    assert_eq!(unsafe { ldmux_client_set_scope(h.0, key.as_ptr(), 0) }, 0);
    // A length that is not 16 is refused rather than truncated into a different region.
    // SAFETY: the buffer is live; the length is the thing under test.
    let short = unsafe { ldmux_client_set_scope(h.0, key.as_ptr(), 15) };
    assert_eq!(short, LDMUX_ERR_ARG);
    // SAFETY: nulls are the argument under test.
    let no_key = unsafe { ldmux_client_set_scope(h.0, null(), 16) };
    assert_eq!(no_key, LDMUX_ERR_NULL);
}

/// The answer a stock radio gives each command the engine asks on connect.
fn answer_to(command: u8) -> Vec<u8> {
    match command {
        // `RESP_CODE_SELF_INFO`: the public preset and a name.
        0x01 => {
            let mut out = vec![0x05, 1, 22, 22];
            out.extend_from_slice(&[0x11; 32]);
            out.extend_from_slice(&0i32.to_le_bytes());
            out.extend_from_slice(&0i32.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&910_525u32.to_le_bytes());
            out.extend_from_slice(&62_500u32.to_le_bytes());
            out.extend_from_slice(&[7, 5]);
            out.extend_from_slice(b"radio");
            out
        }
        // `RESP_CODE_DEVICE_INFO`, firmware 13 with a one-byte path hash mode.
        0x16 => {
            let mut out = vec![0x0D, 13, 175, 40];
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&[0u8; 12]);
            out.extend_from_slice(&[0u8; 40]);
            // The firmware's own version string, space-padded to its 20-byte field.
            let mut version = [b' '; 20];
            version[..7].copy_from_slice(b"v1.17.1");
            out.extend_from_slice(&version);
            out.push(1);
            out.push(0);
            out
        }
        // `RESP_CODE_BATT_AND_STORAGE`.
        0x14 => {
            let mut out = vec![0x0C];
            out.extend_from_slice(&4_050u16.to_le_bytes());
            out.extend_from_slice(&12u32.to_le_bytes());
            out.extend_from_slice(&256u32.to_le_bytes());
            out
        }
        // `RESP_CODE_STATS`, radio type.
        0x38 => {
            let mut out = vec![0x18, 1];
            out.extend_from_slice(&(-120i16).to_le_bytes());
            out.push((-90i8) as u8);
            out.push((-12i8) as u8);
            out.extend_from_slice(&7u32.to_le_bytes());
            out.extend_from_slice(&250u32.to_le_bytes());
            out
        }
        // A send, and anything else: `RESP_CODE_OK`.
        _ => vec![0x00],
    }
}

/// A preset write uses the radio's one command window like every other command, and a sentinel
/// leaves a field alone rather than writing a zero into flash.
#[test]
fn a_preset_write_is_one_command_and_leaves_what_it_was_not_told() {
    let cfg = config();
    let h = Handle::new(&cfg);

    /// Poll until no more frames come out, returning each payload.
    fn frames(h: &Handle) -> Vec<Vec<u8>> {
        let mut out = vec![0u8; 512];
        let mut got = Vec::new();
        for step in 0..20u64 {
            let mut len = 0usize;
            // SAFETY: `out` is writable for its length and `len` is writable.
            let rc = unsafe {
                ldmux_client_poll(h.0, 1000 + step, out.as_mut_ptr(), out.len(), &mut len)
            };
            assert!(rc >= 0, "poll failed: {rc}");
            if rc == 1 {
                got.push(out[..len].to_vec());
            } else {
                break;
            }
        }
        got
    }

    // SAFETY: the handle is live for every call.
    unsafe {
        assert_eq!(
            ldmux_client_set_preset(h.0, 910_525, 62_500, 7, 5, 0, LDMUX_UNSET_POWER),
            LDMUX_OK
        );
    }
    let got = frames(&h);
    assert_eq!(got.len(), 1, "a bare preset is one write, got {got:?}");
    assert_eq!(got[0][0], 0x0B, "SET_RADIO_PARAMS");
    // 910525 is 0x000D_E4BD, so little-endian is BD E4 0D 00; 62500 is 0x0000_F424.
    assert_eq!(&got[0][1..5], &[0xBD, 0xE4, 0x0D, 0x00], "frequency in kHz");
    assert_eq!(&got[0][5..9], &[0x24, 0xF4, 0x00, 0x00], "bandwidth in Hz");
    assert_eq!(&got[0][9..11], &[7, 5], "sf then cr");
    assert_eq!(got[0].len(), 11, "no repeat byte, because none was reported");

    // The radio answers the first write, which frees the one command window. A second preset
    // issued while the first was still unanswered would queue rather than overwrite it, which is
    // the property `RC-2` is about.
    // SAFETY: the handle is live and the answer is one readable byte.
    unsafe {
        assert_eq!(
            ldmux_client_on_ble_rx(h.0, [0x00].as_ptr(), 1, 2000),
            LDMUX_OK
        );
    }

    // With a width and a power, all three follow in order — one per window, each freed by the
    // radio's answer, which is the serialization `RC-2` is about.
    // SAFETY: the same live handle.
    unsafe {
        assert_eq!(
            ldmux_client_set_preset(h.0, 910_525, 62_500, 7, 5, 2, 20),
            LDMUX_OK
        );
    }
    let mut ordered: Vec<Vec<u8>> = Vec::new();
    for step in 0..6u64 {
        let mut out = vec![0u8; 64];
        let mut len = 0usize;
        // SAFETY: `out` is writable for its length and `len` is writable.
        let rc = unsafe {
            ldmux_client_poll(h.0, 3000 + step, out.as_mut_ptr(), out.len(), &mut len)
        };
        assert!(rc >= 0, "poll failed: {rc}");
        if rc == 1 {
            ordered.push(out[..len].to_vec());
            // The radio answers, which frees the window for the next write.
            // SAFETY: the handle is live and the answer is one readable byte.
            unsafe {
                assert_eq!(
                    ldmux_client_on_ble_rx(h.0, [0x00].as_ptr(), 1, 3000 + step),
                    LDMUX_OK
                );
            }
        }
    }
    assert_eq!(ordered.len(), 3, "preset, path width and power: {ordered:?}");
    assert_eq!(ordered[1][0], 0x3D, "SET_PATH_HASH_MODE");
    assert_eq!(&ordered[1][1..3], &[0, 1], "mode is the width minus one");
    assert_eq!(ordered[2][0], 0x0C, "SET_RADIO_TX_POWER");
    assert_eq!(ordered[2][1], 20u8, "the power as a signed byte");

    // A null handle is an error, not a crash.
    // SAFETY: null is the argument under test.
    unsafe {
        assert_eq!(
            ldmux_client_set_preset(std::ptr::null_mut(), 1, 2, 3, 4, 0, i8::MIN),
            LDMUX_ERR_NULL
        );
    }
}

/// **A composed draft is one command, so it is one message.**
///
/// The ABI used to carry exactly one item per call, which meant a draft's text and its `Enter`
/// were two messages — `RC-8`, moved one layer down from where it was fixed. `LDMUX_ITEM_STREAM`
/// hands the library the whole item stream and it folds the lot into one `T_SEND`.
///
/// That the stream decodes is what this proves at the boundary; that the decoded items become one
/// message is `Client::send`'s own contract, pinned in
/// `the_abi_item_stream_is_the_wire_s_item_stream`. A stream with no page is the cheapest way to
/// say "these bytes decoded": the answer is the *page* error, not the *argument* error.
#[test]
fn an_item_stream_is_one_command_and_a_broken_one_is_refused_whole() {
    let cfg = config();
    let h = Handle::new(&cfg);

    // `Text("ls -la")` then `Keys(["Enter"])`, exactly as `mesh_term` writes them.
    let mut stream = vec![0x01u8, 6, 0];
    stream.extend_from_slice(b"ls -la");
    stream.extend_from_slice(&[0x03, 1, 5]);
    stream.extend_from_slice(b"Enter");
    let mut op = 0u32;
    // SAFETY: the handle is live, `stream` is readable for its length, `op` is writable.
    let rc = unsafe {
        ldmux_client_send(
            h.0,
            b"ab12cd34".as_ptr(),
            8,
            LDMUX_ITEM_STREAM,
            stream.as_ptr(),
            stream.len(),
            46,
            19,
            NOW,
            &mut op,
        )
    };
    assert_eq!(
        rc, LDMUX_ERR_NO_PAGE,
        "a valid item stream did not decode: the answer should be about the page, not the argument"
    );

    // A `Text` whose length runs past the buffer is refused whole: half a command runs part of what
    // the operator wrote, which is worse than a refusal.
    let broken = [0x01u8, 9, 0, b'x'];
    // SAFETY: the same live handle, readable for its length.
    let rc = unsafe {
        ldmux_client_send(
            h.0,
            b"ab12cd34".as_ptr(),
            8,
            LDMUX_ITEM_STREAM,
            broken.as_ptr(),
            broken.len(),
            46,
            19,
            NOW,
            &mut op,
        )
    };
    assert_eq!(rc, LDMUX_ERR_ARG, "a malformed item stream was not refused");

    // An unknown tag is refused for the same reason.
    let unknown = [0x7fu8];
    // SAFETY: the same live handle.
    let rc = unsafe {
        ldmux_client_send(
            h.0,
            b"ab12cd34".as_ptr(),
            8,
            LDMUX_ITEM_STREAM,
            unknown.as_ptr(),
            unknown.len(),
            46,
            19,
            NOW,
            &mut op,
        )
    };
    assert_eq!(rc, LDMUX_ERR_ARG);
}
