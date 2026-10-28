//! The whole L3 path against a fake gateway that speaks the real L2 frames.
//!
//! The fake plays both ends of the radio: it is the L2 gateway the daemon dials, and it relays
//! between the daemon and a "phone" (a `ldmux-session` initiator driven from the test). A
//! record the daemon `RECORD`s is delivered to the phone, and what the phone seals is sent back
//! as `RECV`. Nothing here knows a radio.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ldmux_daemon::mesh::{MeshConfig, MeshRuntime, SessionSetup};
use ldmux_mesh::constants::{CAP_STATUS_PUSH, FrameType, RadioState};
use ldmux_mesh::frame::Decoder;
use ldmux_mesh::messages;
use ldmux_session::arq::Limits;
use ldmux_session::handshake::{Initiator, Keypair};
use ldmux_session::{Inbound, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: [u8; 16] = [0x11; 16];
const PEER: [u8; 32] = [0x22; 32];
const PSK: [u8; 32] = [0x33; 32];

#[derive(Default)]
struct Seen {
    /// Records the daemon handed the gateway, in order.
    records: Vec<Vec<u8>>,
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ldmux-l3-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn write(socket: &mut TcpStream, frame: &ldmux_mesh::Frame) {
    socket.write_all(&frame.encode()).await.expect("write");
}

/// The gateway: answers the L2 handshake, then reports `records_left` and relays.
async fn gateway(
    listener: TcpListener,
    seen: Arc<Mutex<Seen>>,
    to_daemon: Arc<Mutex<Vec<Vec<u8>>>>,
    records_left: Arc<Mutex<u8>>,
) {
    let (mut socket, _) = listener.accept().await.expect("accept");
    let mut decoder = Decoder::new();
    let mut buf = [0u8; 1024];
    let mut last_status: Option<u8> = None;
    let mut handshaken = false;
    loop {
        tokio::select! {
            n = socket.read(&mut buf) => {
                let n = match n { Ok(0) | Err(_) => return, Ok(n) => n };
                for frame in decoder.push(&buf[..n]).expect("decode") {
                    match frame.kind {
                        FrameType::Hello => write(&mut socket, &messages::hello_ok(RadioState::Connected, CAP_STATUS_PUSH)).await,
                        FrameType::Pair => write(&mut socket, &messages::pair_ok()).await,
                        FrameType::Bind => {
                            write(&mut socket, &messages::bind_ok(CAP_STATUS_PUSH, 172).unwrap()).await;
                            handshaken = true;
                        }
                        FrameType::Record => {
                            let (seq, rec) = messages::read_record(&frame).unwrap();
                            seen.lock().unwrap().records.push(rec.to_vec());
                            write(&mut socket, &messages::send_result(seq, ldmux_mesh::SendOutcome::SentDirect)).await;
                        }
                        _ => {}
                    }
                }
            }
            () = tokio::time::sleep(Duration::from_millis(15)) => {
                if !handshaken { continue; }
                let left = *records_left.lock().unwrap();
                if last_status != Some(left) {
                    write(&mut socket, &messages::status(RadioState::Connected, -70, 80, 0, Some(left))).await;
                    last_status = Some(left);
                }
                let pending: Vec<Vec<u8>> = to_daemon.lock().unwrap().drain(..).collect();
                for rec in pending {
                    write(&mut socket, &messages::recv(&rec).unwrap()).await;
                }
            }
        }
    }
}

async fn wait_for(what: &str, runtime: &MeshRuntime, mut f: impl FnMut(&MeshRuntime) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if f(runtime) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "not met within 8 s: {what}; snapshot {:?}",
        runtime.snapshot()
    );
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

struct World {
    runtime: MeshRuntime,
    seen: Arc<Mutex<Seen>>,
    to_daemon: Arc<Mutex<Vec<Vec<u8>>>>,
    records_left: Arc<Mutex<u8>>,
    gateway_public: [u8; 32],
    dir: PathBuf,
}

async fn world(tag: &str, left: u8) -> World {
    let dir = temp_dir(tag);
    let setup = SessionSetup::new(&dir, PSK).unwrap();
    let gateway_public = {
        let hex = setup.public_key_hex();
        let mut k = [0u8; 32];
        for i in 0..32 {
            k[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        k
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let to_daemon = Arc::new(Mutex::new(Vec::new()));
    let records_left = Arc::new(Mutex::new(left));
    tokio::spawn(gateway(
        listener,
        seen.clone(),
        to_daemon.clone(),
        records_left.clone(),
    ));
    let runtime = MeshRuntime::start_with_session(
        MeshConfig {
            addr,
            token: TOKEN,
            peer: PEER,
        },
        Some(setup),
    );
    World {
        runtime,
        seen,
        to_daemon,
        records_left,
        gateway_public,
        dir,
    }
}

/// The phone: handshake, returning an established initiator session.
async fn phone_handshake(w: &World, key: &Keypair, ts: u64) -> Session {
    let (init, m1) = Initiator::start(key, &w.gateway_public, &PSK, ts).unwrap();
    w.to_daemon.lock().unwrap().push(m1);
    wait_for("the daemon answers message 1", &w.runtime, |_| {
        !w.seen.lock().unwrap().records.is_empty()
    })
    .await;
    let m2 = w.seen.lock().unwrap().records[0].clone();
    Session::new(init.finish(&m2).unwrap(), 172, 0).unwrap()
}

#[tokio::test]
async fn the_daemon_completes_a_handshake_and_carries_bytes_both_ways() {
    let w = world("e2e", 30).await;
    wait_for("ready with a budget", &w.runtime, |r| {
        r.snapshot().records_left == Some(30)
    })
    .await;
    assert_eq!(w.runtime.snapshot().session.state, "waiting");

    let key = Keypair::generate().unwrap();
    let mut phone = phone_handshake(&w, &key, wall_ms()).await;
    wait_for("session established", &w.runtime, |r| {
        r.snapshot().session.state == "established"
    })
    .await;
    let snap = w.runtime.snapshot();
    assert_eq!(snap.session.handshakes, 1);
    assert_eq!(
        snap.session.peer_key.as_deref(),
        Some(hex(&key.public).as_str())
    );
    assert_eq!(
        snap.uplink_record_max,
        Some(172),
        "mesh.status fields are untouched"
    );
    assert_eq!(snap.rssi, -70);

    // Phone -> daemon: 400 bytes, over several records, delivered whole and in order.
    let msg: Vec<u8> = (0..400u32).map(|i| (i % 253) as u8).collect();
    phone.send(3, &msg).unwrap();
    let mut got = Vec::new();
    let mut now = 0u64;
    let big = Limits {
        records_left: 255,
        l2_slots: 8,
    };
    let mut sent = 0usize;
    for _ in 0..40 {
        for rec in phone.poll(now, big).unwrap() {
            w.to_daemon.lock().unwrap().push(rec);
            sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        for (shell, bytes) in w.runtime.take_inbound() {
            assert_eq!(shell, 3);
            got.extend(bytes);
        }
        // Whatever the daemon sent back (acks) goes to the phone.
        let back: Vec<Vec<u8>> = w.seen.lock().unwrap().records.drain(..).collect();
        for rec in back {
            if rec.first() == Some(&0x13) {
                let _ = phone.receive(now, &rec);
            }
        }
        now += 4_000;
        if got.len() >= msg.len() {
            break;
        }
    }
    assert_eq!(got, msg);
    assert!(sent >= 3, "400 bytes cannot fit one 144-byte record");

    // Daemon -> phone.
    w.runtime.send(3, b"echo: hello phone").unwrap();
    let mut reply = Vec::new();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let back: Vec<Vec<u8>> = w.seen.lock().unwrap().records.drain(..).collect();
        for rec in back {
            if let Ok(Inbound::Data(d)) = phone.receive(now, &rec) {
                reply.extend(d.into_iter().flat_map(|x| x.payload));
            }
        }
        if !reply.is_empty() {
            break;
        }
    }
    assert_eq!(reply, b"echo: hello phone");
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn the_daemon_never_transmits_past_the_records_the_gateway_says_remain() {
    let w = world("budget", 1).await;
    wait_for("budget of one", &w.runtime, |r| {
        r.snapshot().records_left == Some(1)
    })
    .await;
    let key = Keypair::generate().unwrap();
    // Message 1 arrives; the reply is the one record we are allowed.
    let (init, m1) = Initiator::start(&key, &w.gateway_public, &PSK, wall_ms()).unwrap();
    w.to_daemon.lock().unwrap().push(m1);
    wait_for("reply sent", &w.runtime, |_| {
        w.seen.lock().unwrap().records.len() == 1
    })
    .await;
    let m2 = w.seen.lock().unwrap().records[0].clone();
    let _phone = Session::new(init.finish(&m2).unwrap(), 172, 0).unwrap();
    wait_for("established", &w.runtime, |r| {
        r.snapshot().session.state == "established"
    })
    .await;

    // Budget is now spent. Queue data: nothing may leave.
    w.runtime.send(0, b"must wait").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        w.seen.lock().unwrap().records.len(),
        1,
        "no record beyond the budget"
    );
    assert!(
        w.runtime.snapshot().session.budget_stalls >= 1,
        "the stall is visible"
    );

    // More airtime is reported: the held record goes out.
    *w.records_left.lock().unwrap() = 5;
    wait_for("held record released", &w.runtime, |_| {
        w.seen.lock().unwrap().records.len() == 2
    })
    .await;
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn a_replayed_handshake_and_a_wrong_psk_are_refused_and_visible() {
    let w = world("refuse", 30).await;
    wait_for("budget", &w.runtime, |r| {
        r.snapshot().records_left == Some(30)
    })
    .await;
    let key = Keypair::generate().unwrap();
    let ts = wall_ms();
    let (_i, m1) = Initiator::start(&key, &w.gateway_public, &PSK, ts).unwrap();
    w.to_daemon.lock().unwrap().push(m1.clone());
    wait_for("accepted", &w.runtime, |r| {
        r.snapshot().session.handshakes == 1
    })
    .await;
    // The same message 1 again: replay.
    w.to_daemon.lock().unwrap().push(m1);
    wait_for("replay refused", &w.runtime, |r| {
        r.snapshot().session.handshakes_refused == 1
    })
    .await;
    assert!(
        w.runtime
            .snapshot()
            .session
            .last_error
            .unwrap()
            .contains("replayed")
    );
    // A different PSK.
    let (_i, bad) = Initiator::start(&key, &w.gateway_public, &[0u8; 32], wall_ms() + 5).unwrap();
    w.to_daemon.lock().unwrap().push(bad);
    wait_for("psk refused", &w.runtime, |r| {
        r.snapshot().session.handshakes_refused == 2
    })
    .await;
    assert_eq!(
        w.runtime.snapshot().session.state,
        "established",
        "a refused message 1 does not kill the live session"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn without_a_psk_downlink_records_are_dropped_visibly_as_before() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let to_daemon = Arc::new(Mutex::new(vec![vec![0x13u8; 40]]));
    tokio::spawn(gateway(listener, seen, to_daemon, Arc::new(Mutex::new(9))));
    let runtime = MeshRuntime::start(MeshConfig {
        addr,
        token: TOKEN,
        peer: PEER,
    });
    wait_for("dropped with a reason", &runtime, |r| {
        r.snapshot()
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("no session is configured"))
    })
    .await;
    assert_eq!(runtime.snapshot().session.state, "off");
    assert!(runtime.send(0, b"x").is_err());
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
