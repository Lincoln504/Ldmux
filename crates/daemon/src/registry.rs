//! Session registry. Crash-safe and restart-safe.
//!
//! Port of `packages/daemon/src/registry.ts` (the source of record).
//!
//! - The terminal layer is authoritative for terminal sessions; the registry
//!   caches metadata it cannot hold (kind, title, created-by, pi session id).
//! - `reconcile` marks vanished terminal-backed sessions dead and reports
//!   orphans.
//!
//! JSON (not SQLite) for the single-user v1 — see docs/SPEC.md §5.3. Types
//! come from the protocol crate, so the daemon, CLI, and (post-port) every
//! consumer share one contract.
//!
//! **Boundary discipline (the TS module's own comment, kept):** `registry.json`
//! is loaded without validation — a boundary this file's own comment already
//! admits. One record missing `cwd` took the whole list down: `sessionTitle`
//! calls `basename(record.cwd)`, which threw, and the throw escaped
//! `describeSessions`, so `list` and every pushed session update failed
//! together; nothing anywhere said why, because each call site ends in
//! `.catch(() => {})`. A record with no id cannot be addressed and a record
//! with no cwd cannot be placed, so both are dropped here, where the untrusted
//! bytes actually arrive. **Skipped, not repaired to `""`**: a first version
//! kept the record and defaulted `cwd` to an empty string, which looked inert
//! because it stopped `basename` throwing. It is not inert: `ldmux shell` /
//! `ldmux run` reuse a session by directory with `resolve(s.cwd) == wantCwd`,
//! and **`resolve("")` is `process.cwd()`** — so the record aliased whatever
//! directory the caller happened to be in. A field that is later passed to
//! `resolve` must never be defaulted to `""`.

use ldmux_protocol::rpc::SessionRecord;
use std::collections::HashMap;

#[derive(serde::Serialize)]
struct RegistryFile {
    version: u32,
    sessions: Vec<serde_json::Value>,
}

/// The registry: the daemon's durable index of sessions.
pub struct Registry {
    path: String,
    order: Vec<String>,
    sessions: HashMap<String, SessionRecord>,
}

impl Registry {
    pub fn new(path: impl AsRef<str>) -> Self {
        Registry {
            path: path.as_ref().to_string(),
            order: Vec::new(),
            sessions: HashMap::new(),
        }
    }

    /// Load from disk. Missing or corrupt file: start empty. Nothing re-adopts
    /// automatically: a `ldmux-*` session still running in the terminal layer
    /// stays unlisted until `ldmux adopt <name>`.
    pub fn load(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            self.sessions.clear();
            self.order.clear();
            return;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            self.sessions.clear();
            self.order.clear();
            return;
        };
        let Some(sessions) = v.get("sessions").and_then(|s| s.as_array()) else {
            self.sessions.clear();
            self.order.clear();
            return;
        };
        self.sessions.clear();
        self.order.clear();
        for record in sessions {
            // A record that cannot be named is not one that can be shown.
            let Some(_id) = record
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(_cwd) = record
                .get("cwd")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            // The TS kept such records raw (fields read as `undefined`); the
            // Rust contract is a typed `SessionRecord`, so a record that does
            // not shape into it is dropped at the same boundary. Files the
            // daemon itself wrote always shape: this only bites on hand-edited
            // or older files, where dropping is the safe direction.
            let Ok(r) = serde_json::from_value::<SessionRecord>(record.clone()) else {
                continue;
            };
            self.upsert(r);
        }
    }

    /// Save atomically: write `<path>.tmp`, rename over `<path>`.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = std::path::Path::new(&self.path).parent() {
            if parent != std::path::Path::new("") {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = RegistryFile {
            version: 1,
            sessions: Vec::new(),
        };
        for id in &self.order {
            file.sessions.push(
                serde_json::to_value(self.sessions.get(id).unwrap()).unwrap(),
            );
        }
        let tmp = format!("{}.tmp", self.path);
        std::fs::write(&tmp, serde_json::to_string_pretty(&file).unwrap())?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&SessionRecord> {
        self.sessions.get(id)
    }

    /// The record with this terminal name, if any (first in insertion order).
    pub fn by_tmux_name(&self, name: &str) -> Option<&SessionRecord> {
        if name.is_empty() {
            return None;
        }
        self.order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .find(|r| r.tmux_name == name)
    }

    /// All records, most-recently-updated first (stable: ties keep insertion
    /// order, as the TS `Array.sort` does).
    pub fn list(&self) -> Vec<SessionRecord> {
        let mut records: Vec<SessionRecord> =
            self.order.iter().filter_map(|id| self.sessions.get(id).cloned()).collect();
        records.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        records
    }

    /// Insert or replace a record (insertion order is kept on replace).
    pub fn upsert(&mut self, record: SessionRecord) {
        let is_new = !self.sessions.contains_key(&record.id);
        let id = record.id.clone();
        self.sessions.insert(id.clone(), record);
        if is_new {
            self.order.push(id);
        }
    }

    /// Drop exactly one record (a no-op when absent).
    pub fn remove(&mut self, id: &str) {
        self.sessions.remove(id);
        self.order.retain(|x| *x != id);
    }

    /// Bound registry growth: drop dead records older than `max_dead_age_ms`,
    /// then trim the oldest records if still over `max_records`. Returns the
    /// removed ids, in removal order.
    pub fn prune(
        &mut self,
        max_dead_age_ms: Option<u64>,
        max_records: Option<usize>,
        now: i64,
    ) -> Vec<String> {
        let max_dead_age_ms = max_dead_age_ms.unwrap_or(24 * 60 * 60 * 1000);
        let max_records = max_records.unwrap_or(500);
        let mut removed = Vec::new();

        // Pass 1: dead records past their age.
        let stale: Vec<String> = self
            .order
            .iter()
            .filter(|id| {
                let Some(r) = self.sessions.get(id.as_str()) else {
                    return false;
                };
                r.status == "dead"
                    && r.exited_at.map_or(false, |e| now - e > max_dead_age_ms as i64)
            })
            .cloned()
            .collect();
        for id in stale {
            self.sessions.remove(&id);
            self.order.retain(|x| *x != id);
            removed.push(id);
        }

        // Pass 2: hard cap, oldest-updated first.
        if self.sessions.len() > max_records {
            let mut by_updated: Vec<(String, u64)> = self
                .order
                .iter()
                .filter_map(|id| {
                    let r = self.sessions.get(id)?;
                    Some((id.clone(), r.updated_at))
                })
                .collect();
            by_updated.sort_by_key(|(_, u)| *u);
            let n = self.sessions.len() - max_records;
            for (id, _) in by_updated.into_iter().take(n) {
                self.sessions.remove(&id);
                self.order.retain(|x| *x != id);
                removed.push(id);
            }
        }
        removed
    }

    /// Reconcile terminal-backed records against the live session set.
    /// Records with no terminal name are left untouched.
    ///
    /// `live` is the live session names, or `None` when the probe failed
    /// (unknown state: never mark anything dead on unknown). Returns the ids
    /// marked dead and live sessions unknown to the registry (the adoption
    /// candidates), each sorted for determinism.
    pub fn reconcile(
        &mut self,
        live: Option<&std::collections::BTreeSet<String>>,
        now: i64,
    ) -> (Vec<String>, Vec<String>) {
        let mut dead: Vec<String> = Vec::new();
        let mut adopted: Vec<String> = Vec::new();
        let Some(live) = live else {
            return (dead, adopted);
        };
        let mut known = std::collections::BTreeSet::new();
        let ids: Vec<String> = self.order.clone();
        for id in ids {
            let r = match self.sessions.get_mut(&id) {
                Some(r) => r,
                None => continue,
            };
            if r.tmux_name.is_empty() {
                continue;
            } // RPC-only: liveness is the pi process, not the terminal layer.
            let name = r.tmux_name.clone();
            known.insert(name.clone());
            if live.contains(&name) {
                if r.status != "running" {
                    r.status = "running".into();
                    r.exited_at = None;
                    r.exit_code = None;
                    r.updated_at = now as u64;
                }
            } else if r.status != "dead" {
                r.status = "dead".into();
                if r.exited_at.is_none() {
                    r.exited_at = Some(now);
                }
                r.updated_at = now as u64;
                dead.push(id.clone());
            }
        }
        for name in live.iter() {
            if !known.contains(name) {
                adopted.push(name.clone());
            }
        }
        (dead, adopted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TS test's `record()` helper: a complete record with overrides.
    fn record(over: Over) -> SessionRecord {
        let now: i64 = 1_700_000_000_000;
        SessionRecord {
            id: over.id.unwrap_or_else(|| format!("r-{}", uuid::Uuid::new_v4())),
            tmux_name: over.tmux_name.unwrap_or_default(),
            kind: "shell".into(),
            cwd: over.cwd.unwrap_or_else(|| "/tmp".into()),
            title: "t".into(),
            created_at: over.updated_at.map(|v| v as u64).unwrap_or(now as u64),
            updated_at: over.updated_at.map(|v| v as u64).unwrap_or(now as u64),
            created_by: "test".into(),
            status: over.status.unwrap_or_else(|| "running".into()),
            exit_code: over.exit_code,
            exited_at: over.exited_at,
        }
    }

    struct Over {
        id: Option<String>,
        tmux_name: Option<String>,
        cwd: Option<String>,
        status: Option<String>,
        updated_at: Option<i64>,
        exited_at: Option<i64>,
        exit_code: Option<i64>,
    }

    impl Over {
        fn new() -> Self {
            Over {
                id: None,
                tmux_name: None,
                cwd: None,
                status: None,
                updated_at: None,
                exited_at: None,
                exit_code: None,
            }
        }
        fn id(mut self, v: &str) -> Self {
            self.id = Some(v.into());
            self
        }
        fn tmux_name(mut self, v: &str) -> Self {
            self.tmux_name = Some(v.into());
            self
        }
        fn cwd(mut self, v: &str) -> Self {
            self.cwd = Some(v.into());
            self
        }
        fn status(mut self, v: &str) -> Self {
            self.status = Some(v.into());
            self
        }
        fn updated_at(mut self, v: i64) -> Self {
            self.updated_at = Some(v);
            self
        }
        fn exited_at(mut self, v: Option<i64>) -> Self {
            self.exited_at = v;
            self
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ldmux-registry-{}", uuid::Uuid::new_v4()))
    }

    fn path(root: &std::path::Path, name: &str) -> String {
        let _ = std::fs::create_dir_all(root);
        root.join(name).to_string_lossy().to_string()
    }

    #[test]
    fn prune_drops_dead_records_older_than_the_ttl() {
        let root = temp_dir();
        let reg_path = path(&root, "a.json");
        let mut reg = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        let old = record(
            Over::new()
                .id("old")
                .status("dead")
                .exited_at(Some(now - 60_000))
                .updated_at(now - 60_000),
        );
        let fresh = record(
            Over::new().id("fresh").status("dead").exited_at(Some(now)),
        );
        let live = record(Over::new().id("live"));
        reg.upsert(old);
        reg.upsert(fresh);
        reg.upsert(live);
        let removed = reg.prune(Some(30_000), None, now);
        assert_eq!(removed, ["old"]);
        assert!(reg.get("old").is_none());
        assert!(reg.get("fresh").is_some());
        assert!(reg.get("live").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prune_enforces_a_hard_record_cap_by_oldest_updated() {
        let root = temp_dir();
        let reg_path = path(&root, "b.json");
        let mut reg = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        for i in 0..10 {
            reg.upsert(record(
                Over::new().id(&format!("s{i}")).updated_at(now + i),
            ));
        }
        let mut removed = reg.prune(None, Some(4), now);
        removed.sort();
        assert_eq!(removed, ["s0", "s1", "s2", "s3", "s4", "s5"]);
        assert_eq!(reg.list().len(), 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconcile_marks_vanished_terminal_sessions_dead_but_leaves_rpc_only_ones() {
        let root = temp_dir();
        let reg_path = path(&root, "c.json");
        let mut reg = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        reg.upsert(
            record(
                Over::new().id("term").tmux_name("ldmux-x").status("running"),
            ),
        );
        reg.upsert(
            record(
                Over::new().id("rpc").tmux_name("").status("running"),
            ),
        );
        let live: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let (dead, _) = reg.reconcile(Some(&live), now);
        assert_eq!(dead, ["term"]);
        assert_eq!(reg.get("term").unwrap().status, "dead");
        assert_eq!(reg.get("rpc").unwrap().status, "running");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn save_load_round_trips_atomically() {
        let root = temp_dir();
        let reg_path = path(&root, "d.json");
        let reg = Registry::new(&reg_path);
        let mut reg = reg;
        reg.upsert(record(Over::new().id("persist")));
        reg.save().unwrap();
        let mut reloaded = Registry::new(&reg_path);
        reloaded.load();
        assert_eq!(reloaded.get("persist").unwrap().id, "persist");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconcile_none_treats_terminal_state_as_unknown_and_marks_nothing_dead() {
        let root = temp_dir();
        let reg_path = path(&root, "unknown.json");
        let mut reg = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        reg.upsert(
            record(
                Over::new().id("term").tmux_name("ldmux-x").status("running"),
            ),
        );
        let (dead, adopted) = reg.reconcile(None, now);
        assert!(dead.is_empty());
        assert!(adopted.is_empty());
        assert_eq!(reg.get("term").unwrap().status, "running");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remove_drops_exactly_one_record_and_leaves_the_rest() {
        let root = temp_dir();
        let reg_path = path(&root, "remove.json");
        let mut reg = Registry::new(&reg_path);
        reg.upsert(record(Over::new().id("a")));
        reg.upsert(record(Over::new().id("b")));
        reg.remove("a");
        assert!(reg.get("a").is_none());
        assert!(reg.get("b").is_some());
        reg.remove("nope"); // no throw
        assert_eq!(reg.list().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_registry_written_by_an_older_version_with_a_settings_map_still_loads() {
        let root = temp_dir();
        let reg_path = path(&root, "legacy.json");
        let r = record(Over::new().id("legacy"));
        std::fs::write(
            &reg_path,
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "sessions": [r],
                "settings": { "whatever": true },
            }))
            .unwrap(),
        )
        .unwrap();
        let mut reg = Registry::new(&reg_path);
        reg.load();
        assert!(reg.get("legacy").is_some());
        reg.save().unwrap();
        // …and the dead field is not carried forward.
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&reg_path).unwrap()).unwrap();
        assert!(v.get("settings").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_starts_empty_on_a_missing_or_corrupt_file_never_throws() {
        let root = temp_dir();
        let reg_path = path(&root, "does-not-exist.json");
        let mut missing = Registry::new(&reg_path);
        missing.load();
        assert!(missing.list().is_empty());

        let corrupt = path(&root, "corrupt.json");
        std::fs::write(&corrupt, "{ this is not json").unwrap();
        let mut r = Registry::new(&corrupt);
        r.load();
        assert!(r.list().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconcile_revives_a_dead_record_the_terminal_still_has_and_reports_orphans() {
        let root = temp_dir();
        let reg_path = path(&root, "reconcile.json");
        let mut r = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        r.upsert(
            record(
                Over::new()
                    .id("revive")
                    .tmux_name("tmux-revive")
                    .status("dead")
                    .exited_at(Some(5)),
            ),
        );
        // A session the terminal has but the registry does not is an adoption
        // candidate.
        let live: std::collections::BTreeSet<String> =
            ["tmux-revive".to_string(), "tmux-orphan".to_string()].into_iter().collect();
        let (dead, adopted) = r.reconcile(Some(&live), now);
        assert!(dead.is_empty());
        assert_eq!(adopted, ["tmux-orphan"]);
        let revived = r
            .list()
            .into_iter()
            .find(|s| s.id == "revive")
            .expect("revive not in list");
        assert_eq!(revived.status, "running");
        assert!(revived.exited_at.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_malformed_record_does_not_take_the_whole_list_down() {
        // `registry.json` is loaded without validation — a boundary the
        // daemon's own comment already admits. A record that cannot be named
        // is not one that can be shown, so it is dropped where the untrusted
        // bytes arrive.
        let root = temp_dir();
        let reg_path = path(&root, "malformed-records.json");
        let good = record(Over::new().id("good").cwd("/home/me/proj"));
        std::fs::write(
            &reg_path,
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "sessions": [
                    good,
                    { "kind": "shell", "cwd": "/x" },             // no id
                    { "id": "no-cwd", "kind": "shell", "title": "t" }, // id but no cwd
                    { "id": "", "kind": "shell", "cwd": "/y" },     // empty id
                    null,
                ],
            }))
            .unwrap(),
        )
        .unwrap();
        let mut reg = Registry::new(&reg_path);
        reg.load();
        // Only the usable one survives. A record with no directory is not
        // merely ugly: it used to be kept with `cwd: ""`, and `resolve("")` is
        // `process.cwd()` — so it aliased the caller's own directory, and
        // `ldmux shell`'s reuse rule matched it.
        let l = reg.list();
        let ids: Vec<&str> = l.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["good"]);
        // And the survivor is loadable, which is the call that used to throw
        // out of the whole list and blank the launcher.
        let cwd = &reg.get("good").unwrap().cwd;
        assert_eq!(
            std::path::Path::new(cwd)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap(),
            "proj"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
