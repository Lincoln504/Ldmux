//! Session registry. Crash-safe and restart-safe.
//!
//! Port of `packages/daemon/src/registry.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`).
//!
//! - The terminal layer is authoritative for terminal sessions; the registry
//!   caches metadata it cannot hold (kind, title, created-by, pi session id).
//! - `reconcile` marks vanished terminal-backed sessions dead and reports
//!   orphans.
//!
//! JSON (not SQLite) for the single-user v1, as `docs/ARCHITECTURE.md#1-components` says. Types
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

/// One-time migration of a registry written before the session layer was
/// renamed: the keeper session id was stored under the key `LEGACY_NAME_KEY`
/// and is now `keeperName`. Moves the value across when the record has the old
/// key and no new one; returns whether it changed anything. This is the ONLY
/// place the old key is spelled out; `SessionRecord` itself has no alias, and
/// `load` rewrites the file under the new key straight after.
const LEGACY_NAME_KEY: &str = "tmuxName";

fn migrate_legacy_name_key(record: &mut serde_json::Value) -> bool {
    let Some(obj) = record.as_object_mut() else {
        return false;
    };
    let Some(v) = obj.remove(LEGACY_NAME_KEY) else {
        return false;
    };
    obj.entry("keeperName").or_insert(v);
    true
}

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
            self.quarantine("it is not valid JSON");
            self.sessions.clear();
            self.order.clear();
            return;
        };
        let Some(sessions) = v.get("sessions").and_then(|s| s.as_array()) else {
            self.quarantine("it has no `sessions` list");
            self.sessions.clear();
            self.order.clear();
            return;
        };
        self.sessions.clear();
        self.order.clear();
        let mut migrated = false;
        for record in sessions {
            let mut record = record.clone();
            migrated |= migrate_legacy_name_key(&mut record);
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
            let Ok(mut r) = serde_json::from_value::<SessionRecord>(record) else {
                continue;
            };
            // A record written before `had_window` existed that has a window *now* has had
            // one. Without this, every session already on screen when the field arrived keeps
            // `false` for the rest of its life, and the phone is never told when that window
            // goes away — which is the only case the label exists for.
            if r.live_workspace.is_some() && !r.had_window {
                r.had_window = true;
                migrated = true;
            }
            self.upsert(r);
        }
        // Persist under the current key right away, so the migration runs once
        // and the file on disk never carries the legacy name again.
        if migrated && let Err(e) = self.save() {
            eprintln!("ldmuxd: registry migration save: {e}");
        }
    }

    /// Keep a copy of a registry file that could not be read and say so. Starting
    /// empty is still the recovery, but silently: the next save overwrote the only
    /// copy of every record in it.
    fn quarantine(&self, why: &str) {
        let corrupt = format!("{}.corrupt", self.path);
        match std::fs::copy(&self.path, &corrupt) {
            Ok(_) => eprintln!(
                "ldmuxd: registry {} is unusable ({why}); starting empty, the original is kept at {corrupt}",
                self.path
            ),
            Err(e) => eprintln!(
                "ldmuxd: registry {} is unusable ({why}) and could not be copied to {corrupt}: {e}; starting empty",
                self.path
            ),
        }
    }

    /// Save atomically and durably: write `<path>.tmp`, fsync it, rename over
    /// `<path>`, fsync the directory.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = std::path::Path::new(&self.path).parent()
            && parent != std::path::Path::new("")
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = RegistryFile {
            version: 1,
            sessions: Vec::new(),
        };
        // `order` and `sessions` are kept together, so neither lookup nor the
        // serialisation should ever fail — but this runs on the maintenance
        // tick, and an `unwrap` here would panic *inside* the daemon lock, which
        // poisons the registry mutex and takes every later pass with it. A
        // session skipped here is absent from the file for one tick and the
        // keeper is the authority on what is running, so the next reconcile puts
        // the truth back.
        for id in &self.order {
            if let Some(record) = self.sessions.get(id)
                && let Ok(value) = serde_json::to_value(record)
            {
                file.sessions.push(value);
            }
        }
        let tmp = format!("{}.tmp", self.path);
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return Err(std::io::Error::other(
                "registry: could not serialise the file",
            ));
        };
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            // The state directory is 0700, but `File::create` honours the umask
            // and wrote this file 0664 while `claims.json` and the sockets are
            // 0600. One rule for every plain file here.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        if let Some(parent) = std::path::Path::new(&self.path).parent() {
            let dir = if parent == std::path::Path::new("") {
                std::path::Path::new(".")
            } else {
                parent
            };
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&SessionRecord> {
        self.sessions.get(id)
    }

    /// The record with this terminal name, if any (first in insertion order).
    pub fn by_keeper_name(&self, name: &str) -> Option<&SessionRecord> {
        if name.is_empty() {
            return None;
        }
        self.order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .find(|r| r.keeper_name == name)
    }

    /// All records, most-recently-updated first (stable: ties keep insertion
    /// order, as the TS `Array.sort` does).
    pub fn list(&self) -> Vec<SessionRecord> {
        let mut records: Vec<SessionRecord> = self
            .order
            .iter()
            .filter_map(|id| self.sessions.get(id).cloned())
            .collect();
        records.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
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
                    && r.exited_at
                        .is_some_and(|e| now - e > max_dead_age_ms as i64)
            })
            .cloned()
            .collect();
        for id in stale {
            self.sessions.remove(&id);
            self.order.retain(|x| *x != id);
            removed.push(id);
        }

        // Pass 2: hard cap, oldest-updated first. A running session is never
        // evicted: its record is the only thing that names it, and dropping it
        // would turn a live shell into an "adoptable" stranger. Past the cap with
        // only running records left, the cap yields.
        if self.sessions.len() > max_records {
            let mut by_updated: Vec<(String, u64)> = self
                .order
                .iter()
                .filter_map(|id| {
                    let r = self.sessions.get(id)?;
                    (r.status != "running").then(|| (id.clone(), r.updated_at))
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
            if r.keeper_name.is_empty() {
                continue;
            } // RPC-only: liveness is the pi process, not the terminal layer.
            let name = r.keeper_name.clone();
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
    /// A registry whose index and map disagree still saves.
    ///
    /// It cannot happen through the public API, which is exactly why it is worth
    /// pinning: `save` is on the maintenance tick, and a panic there poisons the
    /// mutex every later pass needs.
    #[test]
    fn a_registry_whose_order_and_map_disagree_still_saves() {
        let dir = std::env::temp_dir().join(format!("ldmux-registry-save-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("registry.json");
        let mut reg = Registry::new(path.to_string_lossy());
        reg.upsert(record(Over {
            id: Some("keep".into()),
            ..Over::default()
        }));
        reg.upsert(record(Over {
            id: Some("lost".into()),
            ..Over::default()
        }));
        // Drop the record but keep the index entry: the disagreement.
        reg.sessions.remove("lost");
        reg.save().expect("save must not panic or fail");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("keep"),
            "the session it has is written: {on_disk}"
        );
        assert!(
            !on_disk.contains("lost"),
            "the one it does not have is skipped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// The TS test's `record()` helper: a complete record with overrides.
    fn record(over: Over) -> SessionRecord {
        let now: i64 = 1_700_000_000_000;
        SessionRecord {
            id: over
                .id
                .unwrap_or_else(|| format!("r-{}", uuid::Uuid::new_v4())),
            keeper_name: over.keeper_name.unwrap_or_default(),
            kind: "shell".into(),
            cwd: over.cwd.unwrap_or_else(|| "/tmp".into()),
            title: "t".into(),
            created_at: over.updated_at.map(|v| v as u64).unwrap_or(now as u64),
            updated_at: over.updated_at.map(|v| v as u64).unwrap_or(now as u64),
            created_by: "test".into(),
            status: over.status.unwrap_or_else(|| "running".into()),
            shell: None,
            exit_code: over.exit_code,
            exited_at: over.exited_at,
            live_workspace: None,
            had_window: false,
            live_pid: None,
            ended_reason: None,
            exit_signal: None,
            ended_kind: None,
        }
    }

    #[derive(Default)]
    struct Over {
        id: Option<String>,
        keeper_name: Option<String>,
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
                keeper_name: None,
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
        fn keeper_name(mut self, v: &str) -> Self {
            self.keeper_name = Some(v.into());
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
        let fresh = record(Over::new().id("fresh").status("dead").exited_at(Some(now)));
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
                Over::new()
                    .id(&format!("s{i}"))
                    .status("dead")
                    .exited_at(Some(now))
                    .updated_at(now + i),
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
        reg.upsert(record(
            Over::new()
                .id("term")
                .keeper_name("ldmux-x")
                .status("running"),
        ));
        reg.upsert(record(
            Over::new().id("rpc").keeper_name("").status("running"),
        ));
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
        reg.upsert(record(
            Over::new()
                .id("term")
                .keeper_name("ldmux-x")
                .status("running"),
        ));
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

    /// A registry written before the rename carries `tmuxName`. Loading it keeps
    /// every session and rewrites the file under `keeperName` (the shape of a
    /// copy of the real `/run/user/1000/ldmux/registry.json`, 2026-10-05).
    #[test]
    fn a_registry_with_the_legacy_name_key_is_migrated_once() {
        let root = temp_dir();
        let p = path(&root, "legacy.json");
        std::fs::write(
            &p,
            r#"{"version":1,"sessions":[
              {"createdAt":1791205161439,"createdBy":"local","cwd":"/home/ldeen","id":"d9f93a03d4","kind":"shell","status":"running","title":"shell","tmuxName":"ldmux-d9f93a03d4","updatedAt":1791205161439},
              {"createdAt":1,"createdBy":"phone","cwd":"/tmp","id":"aaaaaaaaaa","kind":"shell","status":"dead","title":"shell","tmuxName":"ldmux-aaaaaaaaaa","updatedAt":2,"exitedAt":2}
            ]}"#,
        )
        .unwrap();
        let mut r = Registry::new(&p);
        r.load();
        assert_eq!(r.list().len(), 2, "both sessions survive");
        assert_eq!(r.get("d9f93a03d4").unwrap().keeper_name, "ldmux-d9f93a03d4");
        let on_disk = std::fs::read_to_string(&p).unwrap();
        assert!(
            on_disk.contains("\"keeperName\": \"ldmux-d9f93a03d4\""),
            "{on_disk}"
        );
        assert!(
            !on_disk.contains("tmuxName"),
            "the legacy key is gone: {on_disk}"
        );
        // A second load is a plain load.
        let mut again = Registry::new(&p);
        again.load();
        assert_eq!(again.list().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconcile_revives_a_dead_record_the_terminal_still_has_and_reports_orphans() {
        let root = temp_dir();
        let reg_path = path(&root, "reconcile.json");
        let mut r = Registry::new(&reg_path);
        let now: i64 = 1_700_000_000_000;
        r.upsert(record(
            Over::new()
                .id("revive")
                .keeper_name("keeper-revive")
                .status("dead")
                .exited_at(Some(5)),
        ));
        // A session the terminal has but the registry does not is an adoption
        // candidate.
        let live: std::collections::BTreeSet<String> =
            ["keeper-revive".to_string(), "keeper-orphan".to_string()]
                .into_iter()
                .collect();
        let (dead, adopted) = r.reconcile(Some(&live), now);
        assert!(dead.is_empty());
        assert_eq!(adopted, ["keeper-orphan"]);
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

    #[test]
    fn a_corrupt_registry_is_kept_aside_not_silently_replaced() {
        let root = temp_dir();
        let reg_path = path(&root, "reg.json");
        std::fs::write(&reg_path, "{ this is not json").unwrap();
        let mut reg = Registry::new(&reg_path);
        reg.load();
        assert!(reg.list().is_empty());
        assert_eq!(
            std::fs::read_to_string(format!("{reg_path}.corrupt")).unwrap(),
            "{ this is not json"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cap_never_evicts_a_running_session() {
        let root = temp_dir();
        let mut reg = Registry::new(path(&root, "cap.json"));
        reg.upsert(record(
            Over::new()
                .id("old-running")
                .status("running")
                .updated_at(1),
        ));
        reg.upsert(record(
            Over::new().id("dead-1").status("dead").updated_at(2),
        ));
        reg.upsert(record(
            Over::new().id("run-2").status("running").updated_at(3),
        ));
        let removed = reg.prune(Some(u64::MAX / 4), Some(2), 1_700_000_000_000);
        assert_eq!(removed, vec!["dead-1".to_string()]);
        assert!(reg.get("old-running").is_some());
        // Only running records left over the cap: the cap yields.
        let removed = reg.prune(Some(u64::MAX / 4), Some(1), 1_700_000_000_000);
        assert!(removed.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
