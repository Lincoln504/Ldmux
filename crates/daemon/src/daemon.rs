//! The `LdmuxDaemon`: the daemon's main state (the glue).
//!
//! It owns the keeper (auto-spawned, [`KeeperSupervisor`]), the session
//! registry (the persisted table), and the ownership (who holds which shell).
//! The retarget of record (plan §3.5): today's tmux control-plane calls
//! (`cp.listSessions()`, `cp.hasSession()`, `cp.listPanes()`) become keeper
//! calls over the socket — a `List` for the live set, a `Read` for liveness,
//! a `State` for the pane facts — so the daemon under test runs against a
//! [fake_keeper](crate::fake_keeper), not a live tmux server.
//!
//! This module is the CORE of the loop (the `keeper_call` helper +
//! `reconcile`); the remote, the streams, the desktop, and the CLI are
//! layered on as they are ported.

use std::collections::HashSet;
use std::path::PathBuf;

use ldmux_protocol::keeper::{self as km, KeeperMessage};

use crate::daemon_lock::DaemonLock;
use crate::keeper_supervisor::KeeperSupervisor;
use crate::ownership::Ownership;
use crate::registry::Registry;

/// The result of a `reconcile` pass (the dead/adopted/pruned sets).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReconcileResult {
    /// Records marked dead (their shell exited).
    pub dead: Vec<String>,
    /// Live sessions the registry did not know (the adoption candidates).
    pub adopted: Vec<String>,
    /// Records pruned (past their dead TTL, over the cap).
    pub pruned: Vec<String>,
}

/// The daemon's main state: the keeper, the registry, the ownership, and the
/// single-instance lock.
pub struct LdmuxDaemon {
    state_dir: PathBuf,
    keeper: KeeperSupervisor,
    registry: Registry,
    ownership: Ownership,
    dead_record_ttl_ms: u64,
    max_records: usize,
    /// The single-instance lock (the `ldmuxd.lock` pid lockfile); `Drop`
    /// releases it.
    lock: DaemonLock,
}

impl LdmuxDaemon {
    /// A daemon rooted at `state_dir`, whose keeper binary is `keeper_bin`.
    ///
    /// The keeper's socket and the registry live under `state_dir`, exactly as
    /// the TS daemon's (the `ldmuxd.sock`, the `ldmuxd.lock`, and the
    /// `registry.json` all live there).
    pub fn new(state_dir: PathBuf, keeper_bin: PathBuf) -> Self {
        Self {
            state_dir: state_dir.clone(),
            keeper: KeeperSupervisor::new(state_dir.clone(), keeper_bin),
            registry: Registry::new(state_dir.join("registry.json").to_string_lossy()),
            ownership: Ownership::new(),
            dead_record_ttl_ms: 24 * 60 * 60 * 1000,
            max_records: 500,
            lock: DaemonLock::new(&state_dir),
        }
    }

    /// The state dir.
    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    /// The keeper's socket path (the daemon's connection to the keeper).
    pub fn keeper_socket_path(&self) -> &std::path::Path {
        self.keeper.socket_path()
    }

    /// A handle to the registry (test/inspection: seed records, read them).
    pub fn registry(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// The ownership (test/inspection: read the claims).
    pub fn ownership(&self) -> &Ownership {
        &self.ownership
    }

    /// Talk to the keeper: one request line → one response line.
    ///
    /// The same client protocol as the WS connection, minus auth (plan §3.5) —
    /// the [fake_keeper](crate::fake_keeper) answers it in tests, the real
    /// keeper in production. A keeper `Error` answer is a failure.
    async fn keeper_call(&self, req: &KeeperMessage) -> Result<KeeperMessage, String> {
        ldmux_protocol::keeper_call::keeper_call(
            self.keeper.socket_path().to_str().unwrap(),
            req,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .map_err(|e| e.0)
    }

    /// Reconcile the registry against the keeper's live sessions.
    ///
    /// The retarget of the TS `reconcile`: the live set comes from a keeper
    /// `List` (not a tmux `list-sessions`), each keeper `session_id` mapping to
    /// its terminal name `ldmux-<id>`. The dead/adopted sets drive the
    /// ownership retain (drop claims for sessions that no longer exist) and the
    /// registry save. The `dead`/`adopted`/`pruned` sets are returned (the caller
    /// decides what is news; a change in the unknown set is news, not its mere
    /// presence).
    pub async fn reconcile(&mut self) -> Result<ReconcileResult, String> {
        let resp = self.keeper_call(&KeeperMessage::List(km::List {})).await?;
        let states = match resp {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => return Err("keeper did not answer a ListResult".into()),
        };
        // The live set, as terminal names (`ldmux-<id>`), the registry's key.
        let live: std::collections::BTreeSet<String> = states
            .iter()
            .map(|s| format!("ldmux-{}", s.session_id))
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Reconcile the registry (the dead/adopted sets).
        let (dead, adopted) = self.registry.reconcile(Some(&live), now);
        // Prune the registry (the dead TTL, the cap).
        let pruned = self.registry.prune(
            Some(self.dead_record_ttl_ms),
            Some(self.max_records),
            now,
        );
        if !dead.is_empty() || !pruned.is_empty() {
            let _ = self.registry.save();
        }
        // Claims for sessions that no longer exist are dropped (a session killed
        // outside ldmux must not leave an owner pointing at nothing).
        let known: HashSet<String> = self
            .registry
            .list()
            .into_iter()
            .map(|r| r.id)
            .collect();
        self.ownership.retain(&known);
        Ok(ReconcileResult { dead, adopted, pruned })
    }

    /// Start the daemon's main loop (the core of the TS `start`).
    ///
    /// The sequence, retargeted onto the keeper: acquire the single-instance
    /// lock (a concurrent daemon refuses — callers treat that as success,
    /// someone else won), ensure the keeper (auto-spawn if dead), load the
    /// registry, adopt the daemon's own sessions (the keeper's `ldmux-<id>`
    /// panes the registry does not list), and reconcile. The rest of the TS
    /// `start` (the control socket server, the remote, the live timer) is
    /// layered on as it is ported.
    pub async fn start(&mut self) -> Result<(), String> {
        // Acquire the lock (the single-instance pid lockfile).
        self.lock.acquire().map_err(|e| e.to_string())?;
        // Ensure the keeper (auto-spawn if dead).
        self.keeper.ensure().await?;
        // Load the registry.
        self.registry.load();
        // Adopt the daemon's own sessions.
        self.adopt_own_sessions().await?;
        // Reconcile.
        let _ = self.reconcile().await?;
        Ok(())
    }

    /// Adopt the daemon's own sessions (the keeper's `ldmux-<id>` panes the
    /// registry does not list). Mirrors the TS `adoptOwnSessions`: a session the
    /// registry does not know is adopted (a record is created for it), and the
    /// adoption is logged.
    ///
    /// Returns the count adopted.
    pub async fn adopt_own_sessions(&mut self) -> Result<usize, String> {
        let resp = self.keeper_call(&KeeperMessage::List(km::List {})).await?;
        let states = match resp {
            KeeperMessage::ListResult(lr) => lr.sessions,
            _ => return Err("expected a listresult".into()),
        };
        let mut adopted = 0;
        for s in &states {
            let tmux_name = format!("ldmux-{}", s.session_id);
            // A session the registry does not know (by name or by id) is an
            // adoption candidate. A failed adopt is not counted (a dead or
            // gone session is not adopted).
            if self.registry.by_tmux_name(&tmux_name).is_none()
                && self.registry.get(&s.session_id).is_none()
                && self.adopt(&s.session_id).await.is_ok()
            {
                adopted += 1;
            }
        }
        Ok(adopted)
    }

    /// Adopt a single keeper session into the registry (the TS `adopt`).
    ///
    /// Verifies the keeper session is live (a `Read`), and creates a record for
    /// it if one does not already exist. The terminal name is `ldmux-<id>`;
    /// the record's `id` is the keeper `session_id` (one of this daemon's own
    /// sessions keeps its id: it is in the name, in the shell's
    /// `LDMUX_SESSION_ID`, and in any window class made for it).
    pub async fn adopt(&mut self, session_id: &str) -> Result<ldmux_protocol::rpc::SessionRecord, String> {
        // Verify the keeper session is live (a `Read`).
        let resp = self
            .keeper_call(&KeeperMessage::Read(km::Read {
                session_id: session_id.into(),
            }))
            .await?;
        let state = match resp {
            KeeperMessage::State(s) => s,
            _ => return Err("expected a state".into()),
        };
        if state.dead {
            return Err("session is dead".into());
        }
        // A record already exists for this session: return it (the adoption is
        // idempotent; two concurrent adopts of one session race to one record).
        let tmux_name = format!("ldmux-{}", session_id);
        if let Some(existing) = self.registry.by_tmux_name(&tmux_name) {
            return Ok(existing.clone());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let record = ldmux_protocol::rpc::SessionRecord {
            id: session_id.to_string(),
            tmux_name,
            kind: "shell".into(),
            cwd: state.cwd,
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
        };
        self.registry.upsert(record.clone());
        let _ = self.registry.save();
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_keeper::FakeKeeper;
    use ldmux_protocol::keeper::{Kill, Spawn};
    use ldmux_protocol::rpc::SessionRecord;

    fn scratch() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ldmux-daemon-core-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Wait for the fake keeper's socket to exist (the bind→connect race the
    /// fake keeper's own end-to-end test bridges).
    async fn wait_socket(p: &std::path::Path) {
        tokio::time::timeout(std::time::Duration::from_millis(2000), async {
            loop {
                if p.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    /// The daemon's `reconcile`, run against a fake keeper, mirrors the TS
    /// `reconcile`: a live keeper session keeps its record running, and killing
    /// it marks the record dead. The daemon under test cannot tell the fake from
    /// the real keeper (plan §3.5).
    #[tokio::test]
    async fn reconcile_drives_the_registry_against_the_keeper() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));
        assert_eq!(d.keeper_socket_path(), keeper_sock.as_path());

        // Spawn a session on the fake keeper (the daemon's keeper_call path).
        let spawn = KeeperMessage::Spawn(Spawn {
            session_id: "s1".into(),
            cols: 80,
            rows: 24,
            cwd: None,
        });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &spawn,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // A registry record for the session (the terminal name `ldmux-s1`).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        d.registry().upsert(SessionRecord {
            id: "s1".into(),
            tmux_name: "ldmux-s1".into(),
            kind: "shell".into(),
            cwd: "/".into(),
            title: "shell".into(),
            created_at: now,
            updated_at: now,
            created_by: "adopt".into(),
            status: "running".into(),
            exit_code: None,
            exited_at: None,
        });

        // Pass 1: the session is live, so nothing is marked dead.
        let r = d.reconcile().await.unwrap();
        assert!(r.dead.is_empty(), "a live session is not dead: {:?}", r);
        assert!(r.adopted.is_empty());

        // Kill the session on the fake keeper.
        let kill = KeeperMessage::Kill(Kill { session_id: "s1".into() });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &kill,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // Pass 2: the session is gone, so the record is marked dead.
        let r = d.reconcile().await.unwrap();
        assert_eq!(r.dead, vec!["s1".to_string()]);

        // The record is dead in the registry.
        assert_eq!(d.registry().get("s1").unwrap().status, "dead");

        drop(fk);
    }

    /// `start` (the main loop) against a fake keeper: a keeper session the
    /// registry does not know is adopted (a record is created for it). The
    /// daemon's `start` acquires the lock, ensures the keeper (the fake is
    /// already alive, so no spawn), loads the registry, adopts the session, and
    /// reconciles — all over the keeper socket, never a tmux control plane.
    #[tokio::test]
    async fn start_adopts_the_daemons_own_sessions_against_the_fake_keeper() {
        let dir = scratch();
        let keeper_sock = dir.join("keeper.sock");
        let fk = std::sync::Arc::new(FakeKeeper::new(keeper_sock.clone()));
        let fk_serve = fk.clone();
        let fk_sock = keeper_sock.clone();
        tokio::spawn(async move {
            FakeKeeper::serve(&fk_serve, fk_sock.as_path()).await.unwrap();
        });
        wait_socket(&keeper_sock).await;

        // A daemon rooted at the same dir (its keeper socket == the fake's).
        let mut d = LdmuxDaemon::new(dir.clone(), PathBuf::from("/nonexistent"));

        // A session the registry does not know (the daemon's own session).
        let spawn = KeeperMessage::Spawn(Spawn {
            session_id: "s1".into(),
            cols: 80,
            rows: 24,
            cwd: None,
        });
        ldmux_protocol::keeper_call::keeper_call(
            keeper_sock.to_str().unwrap(),
            &spawn,
            ldmux_protocol::keeper_call::DEFAULT_TIMEOUT_MS,
        )
        .await
        .unwrap();

        // `start` (the main loop).
        d.start().await.unwrap();

        // The session was adopted: the registry has a record for it.
        let r = d.registry().get("s1").unwrap();
        assert_eq!(r.tmux_name, "ldmux-s1");
        assert_eq!(r.status, "running");
        assert_eq!(r.created_by, "adopt");

        drop(fk);
    }
}
