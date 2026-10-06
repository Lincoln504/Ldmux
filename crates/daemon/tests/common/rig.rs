//! A real daemon + keeper + shell, and raw WebSocket clients for it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use super::wsraw::RawWs;
use ldmux_daemon::daemon::LdmuxDaemon;

pub fn keeper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ldmux-keeper")
}

pub struct Rig {
    pub dir: PathBuf,
    pub arc: Arc<tokio::sync::Mutex<LdmuxDaemon>>,
    pub port: u16,
    pub token: String,
    pub session: String,
    _reaper: super::Reaper,
}

impl Rig {
    pub async fn start() -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "ldmux-wire-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reaper = super::Reaper::new(&dir, &dir.join("keeper.sock"));
        let token = "wire-token".to_string();
        let d = LdmuxDaemon::new(dir.clone(), keeper_bin()).with_remote(
            "127.0.0.1".into(),
            0,
            token.clone(),
        );
        let arc = Arc::new(tokio::sync::Mutex::new(d));
        arc.lock().await.start(&arc).await.unwrap();
        let v = arc
            .lock()
            .await
            .handle("remote_info", serde_json::json!({}))
            .await
            .unwrap();
        let port = v["port"].as_u64().unwrap() as u16;
        let mut rig = Rig {
            dir,
            arc,
            port,
            token,
            session: String::new(),
            _reaper: reaper,
        };
        let mut ws = rig.connect(None).await;
        hello(&mut ws).await;
        ws.send_json(&serde_json::json!({"type":"new","kind":"shell","cwd":"/tmp"}))
            .await;
        let created = ws.until_type("created", &mut Vec::new()).await;
        rig.session = created["sessionId"].as_str().unwrap().to_string();
        rig
    }

    pub async fn connect(&self, offer: Option<&str>) -> RawWs {
        RawWs::connect(self.port, &self.token, offer).await
    }

    /// Attach a fresh connection, 80x24, and return it with the snapshot taken.
    pub async fn attach(&self, offer: Option<&str>) -> (RawWs, serde_json::Value) {
        let mut ws = self.connect(offer).await;
        hello(&mut ws).await;
        ws.reset_counters();
        ws.send_json(
            &serde_json::json!({"type":"attach","sessionId":self.session,"cols":80,"rows":24}),
        )
        .await;
        let mut out = Vec::new();
        ws.until_type("caps", &mut out).await;
        let snap = ws.until_type("snapshot", &mut out).await;
        (ws, snap)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Ok(mut d) = self.arc.try_lock() {
            d.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub async fn hello(ws: &mut RawWs) {
    ws.send_json(&serde_json::json!({"type":"hello","device":{"id":"wire-phone","name":"Wire","kind":"phone"}})).await;
}

/// `hello` that also says which additive behaviours the client understands.
pub async fn hello_features(ws: &mut RawWs, features: &[&str]) {
    ws.send_json(&serde_json::json!({
        "type":"hello",
        "device":{"id":"wire-phone","name":"Wire","kind":"phone"},
        "features": features,
    }))
    .await;
}

impl Rig {
    /// Attach a client that advertised `features`; `small_rcvbuf` makes it back the
    /// daemon's writes up quickly when it stops reading.
    pub async fn attach_with(
        &self,
        features: &[&str],
        small_rcvbuf: bool,
    ) -> (RawWs, serde_json::Value) {
        let mut ws = if small_rcvbuf {
            RawWs::connect_small_rcvbuf(self.port, &self.token, None).await
        } else {
            self.connect(None).await
        };
        hello_features(&mut ws, features).await;
        ws.send_json(
            &serde_json::json!({"type":"attach","sessionId":self.session,"cols":80,"rows":24}),
        )
        .await;
        let mut out = Vec::new();
        ws.until_type("caps", &mut out).await;
        let snap = ws.until_type("snapshot", &mut out).await;
        (ws, snap)
    }

    /// The daemon's local attach socket.
    pub async fn local_sock(&self) -> PathBuf {
        let control = self.dir.join("ldmuxd.sock");
        PathBuf::from(ldmux_protocol::paths::attach_socket_path_from(
            &control.to_string_lossy(),
        ))
    }
}
