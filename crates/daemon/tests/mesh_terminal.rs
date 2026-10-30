//! The shell bridge over the whole L3 path (ADR-0025): a fake gateway that speaks the real L2
//! frames, a real `MeshRuntime` and session on the daemon side, a real initiator session as the
//! phone, and the real `Bridge` between the runtime and a fake shell host.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ldmux_daemon::mesh::{MeshConfig, MeshRuntime, SessionSetup};
use ldmux_daemon::mesh_bridge::{Bridge, Host};
use ldmux_mesh::constants::{CAP_STATUS_PUSH, FrameType, RadioState};
use ldmux_mesh::frame::Decoder as L2Decoder;
use ldmux_mesh::messages;
use ldmux_protocol::mesh_term::{Decoder, Message, Mirror, Screen, VERSION};
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
    let mut decoder = L2Decoder::new();
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
    #[allow(dead_code)]
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

#[derive(Clone, Default)]
struct FakeShell(Arc<Mutex<ShellState>>);

type ShellState = (HashMap<String, Screen>, Vec<Vec<u8>>);
impl Host for FakeShell {
    async fn running(&mut self) -> Vec<String> {
        vec!["sess-1".into()]
    }
    async fn screen(&mut self, id: &str) -> Result<Screen, String> {
        self.0
            .lock()
            .unwrap()
            .0
            .get(id)
            .cloned()
            .ok_or_else(|| "none".into())
    }
    async fn input(&mut self, _id: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().1.push(bytes.to_vec());
        Ok(())
    }
    async fn keys(&mut self, _id: &str, _names: &[String]) -> Result<(), String> {
        Err("this fake shell has no key table".into())
    }
}

fn screen(lines: &[&str]) -> Screen {
    Screen {
        cols: 80,
        rows: lines.len() as u8,
        lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        ..Screen::default()
    }
}

/// The phone: poll its session toward the daemon, deliver what the gateway saw back to it, and
/// return the decoded host messages.
struct Phone {
    session: Session,
    now: u64,
    decoder: Decoder,
    mirror: Mirror,
}

impl Phone {
    async fn pump(&mut self, w: &World, bridge: &mut Bridge<FakeShell, MeshRuntime>) {
        for _ in 0..6 {
            for rec in self
                .session
                .poll(
                    self.now,
                    Limits {
                        records_left: 255,
                        l2_slots: 8,
                    },
                )
                .unwrap()
            {
                w.to_daemon.lock().unwrap().push(rec);
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
            bridge.tick().await;
            let back: Vec<Vec<u8>> = w.seen.lock().unwrap().records.drain(..).collect();
            for rec in back {
                if rec.first() == Some(&0x13)
                    && let Ok(Inbound::Data(d)) = self.session.receive(self.now, &rec)
                {
                    for c in d {
                        for item in self.decoder.push(&c.payload).unwrap() {
                            self.mirror.apply(&item.unwrap());
                        }
                    }
                }
            }
            self.now += 4_000;
        }
    }
}

async fn established(tag: &str, left: u8) -> (World, Phone) {
    let w = world(tag, left).await;
    wait_for("budget", &w.runtime, |r| {
        r.snapshot().records_left == Some(left)
    })
    .await;
    let key = Keypair::generate().unwrap();
    let session = phone_handshake(&w, &key, wall_ms()).await;
    wait_for("established", &w.runtime, |r| {
        r.snapshot().session.state == "established"
    })
    .await;
    w.seen.lock().unwrap().records.clear();
    let phone = Phone {
        session,
        now: 0,
        decoder: Decoder::new(),
        mirror: Mirror::new(),
    };
    (w, phone)
}

#[tokio::test]
async fn attach_snapshot_keystroke_and_changed_rows_over_the_real_session() {
    let (w, mut phone) = established("term", 200).await;
    let host = FakeShell::default();
    host.0
        .lock()
        .unwrap()
        .0
        .insert("sess-1".into(), screen(&["$", "", ""]));
    let mut bridge = Bridge::new(host.clone(), w.runtime.clone());

    phone
        .session
        .send(
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    phone.pump(&w, &mut bridge).await;
    assert!(bridge.attached(3));
    assert_eq!(phone.mirror.lines, ["$", "", ""]);
    assert_eq!(
        phone.mirror.profile.map(|p| p.2),
        Some(ldmux_protocol::mesh_term::unsupported::ALL_V1)
    );

    phone
        .session
        .send(3, &Message::Input(b"ls\r".to_vec()).encode().unwrap())
        .unwrap();
    phone.pump(&w, &mut bridge).await;
    assert_eq!(host.0.lock().unwrap().1, vec![b"ls\r".to_vec()]);

    host.0
        .lock()
        .unwrap()
        .0
        .insert("sess-1".into(), screen(&["$ ls", "a.txt", ""]));
    let sent_before = bridge.stats.messages_sent;
    phone.pump(&w, &mut bridge).await;
    assert_eq!(phone.mirror.lines, ["$ ls", "a.txt", ""]);
    assert_eq!(
        bridge.stats.messages_sent - sent_before,
        2,
        "changed rows and a commit, no full screen"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn a_corrupted_record_is_rejected_by_the_session_and_nothing_is_applied() {
    let (w, mut phone) = established("corrupt", 200).await;
    let host = FakeShell::default();
    host.0
        .lock()
        .unwrap()
        .0
        .insert("sess-1".into(), screen(&["$"]));
    let mut bridge = Bridge::new(host.clone(), w.runtime.clone());

    // An Input record with one ciphertext byte flipped.
    phone
        .session
        .send(
            3,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    phone.pump(&w, &mut bridge).await;
    phone
        .session
        .send(3, &Message::Input(b"evil\r".to_vec()).encode().unwrap())
        .unwrap();
    let mut recs = phone
        .session
        .poll(
            phone.now,
            Limits {
                records_left: 255,
                l2_slots: 8,
            },
        )
        .unwrap();
    let mut bad = recs.remove(0);
    let last = bad.len() - 1;
    bad[last] ^= 0x01;
    w.to_daemon.lock().unwrap().push(bad);
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(60)).await;
        bridge.tick().await;
    }
    assert!(
        host.0.lock().unwrap().1.is_empty(),
        "a record that fails its tag never reaches a shell"
    );
    assert!(
        w.runtime.snapshot().session.dropped >= 1,
        "and the drop is counted"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[tokio::test]
async fn the_bridge_never_transmits_past_the_records_the_gateway_says_remain() {
    // Two records of airtime: the handshake reply takes one, the Attach reply is one more.
    let (w, mut phone) = established("bridge-budget", 2).await;
    let host = FakeShell::default();
    host.0
        .lock()
        .unwrap()
        .0
        .insert("sess-1".into(), screen(&["one", "two"]));
    let mut bridge = Bridge::new(host.clone(), w.runtime.clone());
    phone
        .session
        .send(
            1,
            &Message::Attach {
                version: VERSION,
                selector: String::new(),
            }
            .encode()
            .unwrap(),
        )
        .unwrap();
    for _ in 0..8 {
        for rec in phone
            .session
            .poll(
                phone.now,
                Limits {
                    records_left: 255,
                    l2_slots: 8,
                },
            )
            .unwrap()
        {
            w.to_daemon.lock().unwrap().push(rec);
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        bridge.tick().await;
    }
    let on_air = w.seen.lock().unwrap().records.len();
    assert!(
        on_air <= 1,
        "budget 2, one already spent on the handshake reply: {on_air}"
    );
    assert!(w.runtime.snapshot().session.budget_stalls >= 1);
    let _ = std::fs::remove_dir_all(&w.dir);
}
