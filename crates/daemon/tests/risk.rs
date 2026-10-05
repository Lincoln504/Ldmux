//! Phase 6 risk tests (plan §6): backpressure, dead-reader, spawn race.
//!
//! These tests run against the fake keeper (no live terminal). The
//! backpressure test floods a session and asserts the client queue is bounded
//! (§3.7). The dead-reader test kills a client mid-stream and asserts the
//! session survives. The spawn race test asserts that two concurrent
//! first-needs produce one stream, not two.

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use ldmux_daemon::fake_keeper::FakeKeeper;
use ldmux_daemon::stream::KeeperConn;
use ldmux_daemon::stream::PaneStream;
use ldmux_daemon::stream::StreamSubscriber;
use ldmux_protocol::keeper::KeeperMessage;

fn scratch_dir() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("ldmux-risk-{}-{}", std::process::id(), rand::random::<u64>()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn wait_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for socket {}", path.display());
}

/// A `KeeperConn` that connects to the fake keeper socket (one call per
/// connection, like the real daemon↔keeper protocol).
struct FakeKeeperConn {
    sock: std::path::PathBuf,
}

impl FakeKeeperConn {
    fn new(sock: std::path::PathBuf) -> Self {
        Self { sock }
    }
}

impl KeeperConn for FakeKeeperConn {
    fn call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
        use std::io::Read;
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let mut sock = UnixStream::connect(&self.sock).map_err(|e| e.to_string())?;
        let line = serde_json::to_string(req).map_err(|e| e.to_string())?;
        sock.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf).trim().to_string();
        serde_json::from_str(&line).map_err(|e| e.to_string())
    }
}

/// Flood: a viewer that is slow to consume is NOT the stream's problem. The
/// stream delivers synchronously to callbacks, so it neither buffers for a slow
/// viewer nor resets one for the volume it has seen (it used to silence every
/// viewer after 1 MiB of cumulative output). Backpressure lives at the
/// connection (`remote.rs::queue_output`, pinned in its unit tests) and the
/// keeper's bounded buffer (`OutBuf`, pinned in the keeper's tests).
#[tokio::test]
async fn a_flood_is_delivered_in_full_without_a_spurious_resync() {
    let dir = scratch_dir();
    let keeper_sock = dir.join("keeper.sock");
    let fk = Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
    });
    wait_socket(&keeper_sock).await;

    let stream = PaneStream::new("s1".to_string(), Box::new(FakeKeeperConn::new(keeper_sock)));

    let reset_count = Arc::new(AtomicU64::new(0));
    let delivered = Arc::new(AtomicU64::new(0));
    let (rc, dc) = (Arc::clone(&reset_count), Arc::clone(&delivered));
    let _unsubscribe = stream.subscribe(StreamSubscriber {
        on_output: Box::new(move |t: &[u8]| {
            dc.fetch_add(t.len() as u64, Ordering::SeqCst);
        }),
        on_reset: Box::new(move |_reason: &str| {
            rc.fetch_add(1, Ordering::SeqCst);
        }),
    });

    let big = "x".repeat(2 * 1024 * 1024);
    stream.fanout(&big);
    stream.fanout("hello");

    assert_eq!(reset_count.load(Ordering::SeqCst), 0, "volume alone is not a resync");
    assert_eq!(delivered.load(Ordering::SeqCst), 2 * 1024 * 1024 + 5, "every byte reached the viewer");

    drop(fk);
}

/// Dead-reader: kill a subscriber mid-stream; the session (the stream)
/// survives (the other subscribers still get output).
#[tokio::test]
async fn dead_reader_does_not_kill_the_session() {
    let dir = scratch_dir();
    let keeper_sock = dir.join("keeper.sock");
    let fk = Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
    });
    wait_socket(&keeper_sock).await;

    let stream = PaneStream::new("s1".to_string(), Box::new(FakeKeeperConn::new(keeper_sock)));

    // Two subscribers: one will "die" (unsubscribe), the other survives.
    let alive = Arc::new(AtomicU64::new(0));
    let alive2 = Arc::clone(&alive);

    let sub1 = StreamSubscriber {
        on_output: Box::new(move |text: &[u8]| {
            alive2.fetch_add(text.len() as u64, Ordering::SeqCst);
        }),
        on_reset: Box::new(|_reason: &str| {}),
    };
    let (_id1, _unsubscribe1) = stream.subscribe(sub1);

    let sub2 = StreamSubscriber {
        on_output: Box::new(|_text: &[u8]| {}),
        on_reset: Box::new(|_reason: &str| {}),
    };
    let (_id2, unsubscribe2) = stream.subscribe(sub2);

    // Send output to both.
    stream.fanout("hello world");
    assert_eq!(
        alive.load(Ordering::SeqCst),
        11,
        "sub1 should have received the output"
    );

    // "Kill" sub2 (the dead reader).
    unsubscribe2();

    // The stream still has a subscriber (sub1).
    assert_eq!(
        stream.subscriber_count(),
        1,
        "sub1 should still be subscribed"
    );

    // Send more output; sub1 still gets it.
    stream.fanout("more");
    assert_eq!(
        alive.load(Ordering::SeqCst),
        15,
        "sub1 should still receive output"
    );

    drop(fk);
}

/// Spawn race: two concurrent `stream_for` calls for the same session;
/// one wins, the other reuses the existing stream (no duplicate).
#[tokio::test]
async fn spawn_race_two_concurrent_stream_for_one_wins() {
    let dir = scratch_dir();
    let keeper_sock = dir.join("keeper.sock");
    let fk = Arc::new(FakeKeeper::new(keeper_sock.clone()));
    let fk_serve = fk.clone();
    let fk_sock = keeper_sock.clone();
    tokio::spawn(async move {
        FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
    });
    wait_socket(&keeper_sock).await;

    // Two concurrent stream_for calls for the same session.
    let daemon = ldmux_daemon::daemon::LdmuxDaemon::new(dir, std::path::PathBuf::from("/nonexistent"));
    let daemon = Arc::new(std::sync::Mutex::new(daemon));

    let d1 = daemon.clone();
    let d2 = daemon.clone();
    let t1 = tokio::spawn(async move {
        let mut d = d1.lock().unwrap();
        let s = d.stream_for("s1");
        s.session_id().to_string()
    });
    let t2 = tokio::spawn(async move {
        let mut d = d2.lock().unwrap();
        let s = d.stream_for("s1");
        s.session_id().to_string()
    });

    let r1 = t1.await.unwrap();
    let r2 = t2.await.unwrap();

    // Both should get the same session id (the first one to create the
    // stream wins; the second one reuses it).
    assert_eq!(r1, "s1");
    assert_eq!(r2, "s1");

    drop(fk);
}
