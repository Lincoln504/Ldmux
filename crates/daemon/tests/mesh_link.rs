//! The daemon's mesh link against a fake gateway.
//!
//! This is the seam that matters: the runtime must reach `ready`, take the
//! capacity from `BIND_OK`, absorb `STATUS`, and report a close as a state
//! rather than panicking or hanging.

use std::time::{Duration, Instant};

use ldmux_daemon::mesh::{MeshConfig, MeshRuntime};
use ldmux_mesh::client::MeshClient;
use ldmux_mesh::constants::{CAP_STATUS_PUSH, FrameType, RadioState};
use ldmux_mesh::frame::Decoder;
use ldmux_mesh::messages;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: [u8; 16] = [0x11; 16];
const PEER: [u8; 32] = [0x22; 32];

async fn expect(socket: &mut TcpStream, decoder: &mut Decoder, want: FrameType) {
    let mut buf = [0u8; 512];
    loop {
        let n = socket.read(&mut buf).await.expect("read");
        assert!(n > 0, "closed while waiting for {}", want.name());
        for frame in decoder.push(&buf[..n]).expect("decode") {
            if frame.kind == want {
                return;
            }
        }
    }
}

async fn reply(socket: &mut TcpStream, frame: &ldmux_mesh::Frame) {
    socket.write_all(&frame.encode()).await.expect("write");
}

/// Answers the handshake and one STATUS, then behaves as `after` says.
async fn handshake_and_status(listener: TcpListener, after: AfterStatus) {
    let (mut socket, _) = listener.accept().await.expect("accept");
    let mut decoder = Decoder::new();

    expect(&mut socket, &mut decoder, FrameType::Hello).await;
    reply(
        &mut socket,
        &messages::hello_ok(RadioState::Connected, CAP_STATUS_PUSH),
    )
    .await;

    expect(&mut socket, &mut decoder, FrameType::Pair).await;
    reply(&mut socket, &messages::pair_ok()).await;

    expect(&mut socket, &mut decoder, FrameType::Bind).await;
    reply(
        &mut socket,
        &messages::bind_ok(CAP_STATUS_PUSH, 172).expect("bind_ok"),
    )
    .await;

    reply(
        &mut socket,
        &messages::status(RadioState::Connected, -68, 77, 12, Some(9)),
    )
    .await;

    match after {
        AfterStatus::StayOpen => {
            // Hold the socket until the test's runtime is dropped.
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        AfterStatus::Close => {
            reply(
                &mut socket,
                &messages::close(ldmux_mesh::CloseReason::Shutdown),
            )
            .await;
            let _ = socket.shutdown().await;
        }
    }
}

#[derive(Clone, Copy)]
enum AfterStatus {
    StayOpen,
    Close,
}

async fn wait_for(runtime: &MeshRuntime, mut condition: impl FnMut(&MeshRuntime) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition(runtime) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "condition not met within 5 s; snapshot: {:?}",
        runtime.snapshot()
    );
}

fn config(addr: String) -> MeshConfig {
    MeshConfig {
        addr,
        token: TOKEN,
        peer: PEER,
    }
}

#[tokio::test]
async fn the_link_reaches_ready_and_absorbs_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(handshake_and_status(listener, AfterStatus::StayOpen));

    let runtime = MeshRuntime::start(config(addr));
    wait_for(&runtime, |r| r.snapshot().records_left == Some(9)).await;

    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.state, "ready");
    assert_eq!(snapshot.uplink_record_max, Some(172));
    assert_eq!(snapshot.radio_state, Some(RadioState::Connected));
    assert_eq!(snapshot.rssi, -68);
    assert_eq!(snapshot.battery_percent, 77);
    assert_eq!(snapshot.tx_airtime_tenths, 12);
    assert_eq!(snapshot.peer, "22".repeat(32));
    assert!(snapshot.last_error.is_none());
    server.abort();
}

#[tokio::test]
async fn a_close_is_a_state_and_clears_the_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(handshake_and_status(listener, AfterStatus::Close));

    let runtime = MeshRuntime::start(config(addr));
    wait_for(&runtime, |r| r.snapshot().state == "down").await;

    let snapshot = runtime.snapshot();
    assert!(
        snapshot
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("close"),
        "last_error: {:?}",
        snapshot.last_error
    );
    assert_eq!(snapshot.uplink_record_max, None);
    assert_eq!(snapshot.records_left, None);
    assert!(snapshot.reconnects >= 1);
    server.await.expect("server");
}

#[tokio::test]
async fn a_gateway_that_is_not_there_is_reported_not_fatal() {
    // Bind and immediately drop, so the port is closed and refused.
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        listener.local_addr().expect("addr").to_string()
    };
    let runtime = MeshRuntime::start(config(addr));
    wait_for(&runtime, |r| r.snapshot().state == "down").await;
    let snapshot = runtime.snapshot();
    assert!(snapshot.uplink_record_max.is_none());
    assert!(snapshot.last_error.is_some());
}

#[tokio::test]
async fn a_client_can_talk_to_the_same_fake_gateway() {
    // Guards the fake: if this fails, the runtime test above would be
    // measuring the fake rather than the link.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(handshake_and_status(listener, AfterStatus::Close));

    let mut client = MeshClient::connect(addr).await.expect("connect");
    let handshake = client
        .handshake(&TOKEN, &PEER, CAP_STATUS_PUSH)
        .await
        .expect("handshake");
    assert_eq!(handshake.bind.uplink_record_max, 172);
    let status = client.recv().await.expect("status");
    assert_eq!(
        messages::read_status(&status)
            .expect("status")
            .ops_left_this_hour,
        Some(9)
    );
    let close = client.recv().await.expect("close");
    assert_eq!(
        messages::read_close(&close).expect("close"),
        ldmux_mesh::CloseReason::Shutdown
    );
    server.await.expect("server");
}
