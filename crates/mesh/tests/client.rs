//! The client against a fake gateway on a real loopback socket.
//!
//! The fake speaks only the frames this client needs, in order, so a failure
//! here means the client and the contract disagree, not that a mock is
//! incomplete: the byte layout is pinned separately by `vectors.rs`.

use ldmux_mesh::client::{MeshClient, recv_matching};
use ldmux_mesh::constants::{
    CAP_STATUS_PUSH, FrameType, RadioState, SendOutcome, UNKNOWN_BATTERY, UNKNOWN_RSSI,
};
use ldmux_mesh::frame::{Decoder, Frame, MeshError};
use ldmux_mesh::messages;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: [u8; 16] = [0x11; 16];
const PEER: [u8; 32] = [0x22; 32];

/// Reads frames from the socket until one satisfies `want`.
async fn expect(socket: &mut TcpStream, decoder: &mut Decoder, want: FrameType) -> Frame {
    let mut buf = [0u8; 512];
    loop {
        let n = socket.read(&mut buf).await.expect("read");
        assert!(n > 0, "socket closed while waiting for {}", want.name());
        for frame in decoder.push(&buf[..n]).expect("decode") {
            if frame.kind == want {
                return frame;
            }
        }
    }
}

async fn reply(socket: &mut TcpStream, frame: &Frame) {
    socket.write_all(&frame.encode()).await.expect("write");
}

#[tokio::test]
async fn handshake_send_and_downlink_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut decoder = Decoder::new();

        let hello = expect(&mut socket, &mut decoder, FrameType::Hello).await;
        let (version, caps) = messages::read_hello(&hello).expect("hello");
        assert_eq!(version, 1);
        assert_eq!(caps, CAP_STATUS_PUSH);
        reply(
            &mut socket,
            &messages::hello_ok(RadioState::Connected, CAP_STATUS_PUSH),
        )
        .await;

        let pair = expect(&mut socket, &mut decoder, FrameType::Pair).await;
        assert_eq!(messages::read_pair(&pair).expect("pair"), TOKEN);
        reply(&mut socket, &messages::pair_ok()).await;

        let bind = expect(&mut socket, &mut decoder, FrameType::Bind).await;
        assert_eq!(messages::read_bind(&bind).expect("bind"), PEER);
        reply(
            &mut socket,
            &messages::bind_ok(CAP_STATUS_PUSH, 172).expect("bind_ok"),
        )
        .await;

        let record = expect(&mut socket, &mut decoder, FrameType::Record).await;
        let (seq, payload) = messages::read_record(&record).expect("record");
        assert_eq!(seq, 9);
        assert_eq!(payload, [1, 2, 3, 4]);

        // Interleave a STATUS and a RECV before the answer, so the client has
        // to keep reading rather than assuming the next frame is the result.
        reply(
            &mut socket,
            &messages::status(RadioState::Connected, -70, 90, 0, Some(12)),
        )
        .await;
        reply(&mut socket, &messages::recv(&[0xDE, 0xAD]).expect("recv")).await;
        reply(
            &mut socket,
            &messages::send_result(seq, SendOutcome::SentDirect),
        )
        .await;
    });

    let mut client = MeshClient::connect(addr).await.expect("connect");
    let handshake = client
        .handshake(&TOKEN, &PEER, CAP_STATUS_PUSH)
        .await
        .expect("handshake");
    assert_eq!(handshake.hello.radio_state, RadioState::Connected);
    assert_eq!(handshake.bind.uplink_record_max, 172);

    client.send_record(9, &[1, 2, 3, 4]).await.expect("send");

    let mut statuses = Vec::new();
    let mut records = Vec::new();
    let result = recv_matching(&mut client, FrameType::SendResult, |frame| {
        match frame.kind {
            FrameType::Status => statuses.push(messages::read_status(&frame).expect("status")),
            FrameType::Recv => records.push(messages::read_recv(&frame).expect("recv")),
            _ => {}
        }
    })
    .await
    .expect("send result");

    let parsed = messages::read_send_result(&result).expect("result");
    assert_eq!(parsed.seq, 9);
    assert_eq!(parsed.outcome, SendOutcome::SentDirect);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].ops_left_this_hour, Some(12));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, vec![0xDE, 0xAD]);

    client.close().await;
    server.await.expect("server");
}

#[tokio::test]
async fn a_refused_hello_is_an_error_not_a_hang() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut decoder = Decoder::new();
        let _ = expect(&mut socket, &mut decoder, FrameType::Hello).await;
        reply(
            &mut socket,
            &messages::hello_err(ldmux_mesh::HelloError::VersionMismatch),
        )
        .await;
    });

    let mut client = MeshClient::connect(addr).await.expect("connect");
    let err = client
        .hello_and_pair(&TOKEN, 0)
        .await
        .expect_err("version mismatch must fail");
    assert!(matches!(err, MeshError::Refused("hello", 1)), "{err}");
    server.await.expect("server");
}

#[tokio::test]
async fn a_refused_bind_says_why() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut decoder = Decoder::new();
        let _ = expect(&mut socket, &mut decoder, FrameType::Hello).await;
        reply(&mut socket, &messages::hello_ok(RadioState::Connected, 0)).await;
        let _ = expect(&mut socket, &mut decoder, FrameType::Pair).await;
        reply(&mut socket, &messages::pair_ok()).await;
        let _ = expect(&mut socket, &mut decoder, FrameType::Bind).await;
        reply(
            &mut socket,
            &messages::bind_err(ldmux_mesh::BindError::NotAContact),
        )
        .await;
    });

    let mut client = MeshClient::connect(addr).await.expect("connect");
    let err = client
        .handshake(&TOKEN, &PEER, 0)
        .await
        .expect_err("bind must fail");
    assert!(matches!(err, MeshError::Refused("bind", 1)), "{err}");
    server.await.expect("server");
}

#[tokio::test]
async fn unknown_sentinels_and_the_absent_sixth_byte_both_parse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut decoder = Decoder::new();
        let _ = expect(&mut socket, &mut decoder, FrameType::Hello).await;
        reply(&mut socket, &messages::hello_ok(RadioState::Connected, 0)).await;
        let _ = expect(&mut socket, &mut decoder, FrameType::Pair).await;
        reply(&mut socket, &messages::pair_ok()).await;
        let _ = expect(&mut socket, &mut decoder, FrameType::Bind).await;
        reply(&mut socket, &messages::bind_ok(0, 160).expect("bind_ok")).await;
        // A five-byte STATUS, as an older gateway would send.
        reply(
            &mut socket,
            &messages::status(
                RadioState::Disconnected,
                UNKNOWN_RSSI,
                UNKNOWN_BATTERY,
                5,
                None,
            ),
        )
        .await;
    });

    let mut client = MeshClient::connect(addr).await.expect("connect");
    client.handshake(&TOKEN, &PEER, 0).await.expect("handshake");
    let frame = client.recv().await.expect("status");
    let status = messages::read_status(&frame).expect("status");
    assert_eq!(status.rssi, -128);
    assert_eq!(status.battery_percent, 0xFF);
    assert_eq!(status.ops_left_this_hour, None);
    server.await.expect("server");
}
