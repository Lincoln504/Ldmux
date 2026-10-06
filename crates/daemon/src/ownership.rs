//! Who is looking at a session (ADR-0010).
//!
//! Port of `packages/daemon/src/ownership.ts` (the source of record), plus
//! `seizing_closes_the_desk_window`, which the TS tree keeps in
//! `daemon.ts` and the ownership test file pins.
//!
//! One device owns a session at a time. The whole reason this is a module and
//! not a field on a record is the failure mode dooboxes' own glossary names
//! *Stale State*: attachment state that outlives the connection **blocks the
//! next attach**. The failure is a lockout, not a cosmetic bug.
//!
//! So ownership is a **map of live claims**, kept honest by three rules:
//! - a device's claims are dropped when its connection ends (`release_device`);
//! - claims for sessions that no longer exist are dropped by `retain`;
//! - a daemon restart does **not** lose the claims: the table is mirrored to
//!   `<state>/claims.json` and reloaded with a re-claim grace
//!   (`LdmuxDaemon::load_claims`), because the clients a restart drops come
//!   straight back and another device must not be able to slip in between. A
//!   restored claim nobody re-attaches to is released when the grace ends, so
//!   the Stale State lockout is still bounded.
//!
//! Pure and synchronous: every rule here is a rule the tests can state
//! directly.

use ldmux_protocol::rpc::DeviceInfo as Device;

/// A device's kind, closed set: `"phone" | "desktop"`.
///
/// The wire is a plain string, so it is modelled as `String` (the closed set
/// is documented here, not enforced): `Device::kind`.
pub type DeviceKind = String;

/// A claim: a device looking at a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub device: Device,
    /// Epoch ms, for "open on Pixel since 14:02".
    pub since: i64,
}

/// The answer to a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimResult {
    /// Acquired (or re-acquired by its holder). `previous` is set when a
    /// takeover displaced someone.
    Acquired {
        claim: Claim,
        previous: Option<Device>,
    },
    /// Someone else holds it. The caller decides whether to ask the user.
    HeldBy { device: Device },
}

/// The claim table.
///
/// `held` is a reverse index purely so a disconnect is O(sessions-of-that-device)
/// rather than O(all claims). Keeping the two maps in step is the only
/// invariant here, and it is why every mutation goes through the private index
/// helpers.
#[derive(Default)]
pub struct Ownership {
    claims: std::collections::BTreeMap<String, Claim>,
    held: std::collections::HashMap<String, Vec<String>>,
}

impl Ownership {
    pub fn new() -> Self {
        Self::default()
    }

    /// The device currently looking at this session, if any.
    pub fn owner(&self, session_id: &str) -> Option<&Claim> {
        self.claims.get(session_id)
    }

    /// Every claim, for persisting.
    pub fn export(&self) -> Vec<(String, Claim)> {
        self.claims
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Put back a claim that was persisted (a restart). Keeps the original `since`.
    pub fn restore(&mut self, session_id: &str, claim: Claim) {
        if self.claims.contains_key(session_id) {
            return;
        }
        self.index(session_id, &claim.device.id);
        self.claims.insert(session_id.to_string(), claim);
    }

    /// Every claimed session id, for broadcast/retain checks.
    pub fn session_ids(&self) -> Vec<String> {
        self.claims.keys().cloned().collect()
    }

    /// Claim a session for a device.
    ///
    /// The same device re-claiming is **idempotent, not a conflict**: a client
    /// reconnects, resizes, or re-attaches constantly, and every one of those
    /// is the holder asking for what it already has. Only a *different* device
    /// is a conflict, and it is never resolved here — `takeover` is the caller
    /// saying the user consented, which is the only way a claim changes hands.
    ///
    /// `now` is the claim's `since` (epoch ms); `None` takes the current time.
    pub fn claim(
        &mut self,
        session_id: &str,
        device: Device,
        takeover: bool,
        now: Option<i64>,
    ) -> ClaimResult {
        let now = now.unwrap_or_else(|| crate::daemon::now_ms() as i64);
        let existing: Option<Claim> = self.claims.get(session_id).cloned();

        if let Some(e) = existing {
            if e.device.id == device.id {
                // Refresh identity (a name can change) without disturbing
                // `since`: the interesting question is when it was taken, not
                // when it was last touched.
                let refreshed = Claim {
                    device: device.clone(),
                    since: e.since,
                };
                self.claims
                    .insert(session_id.to_string(), refreshed.clone());
                return ClaimResult::Acquired {
                    claim: refreshed,
                    previous: None,
                };
            }
            if !takeover {
                return ClaimResult::HeldBy {
                    device: e.device.clone(),
                };
            }
            self.unindex(session_id, &e.device.id);
            let claim = Claim {
                device: device.clone(),
                since: now,
            };
            self.claims.insert(session_id.to_string(), claim.clone());
            self.index(session_id, &device.id);
            return ClaimResult::Acquired {
                claim,
                previous: Some(e.device.clone()),
            };
        }

        let claim = Claim {
            device: device.clone(),
            since: now,
        };
        self.claims.insert(session_id.to_string(), claim.clone());
        self.index(session_id, &device.id);
        ClaimResult::Acquired {
            claim,
            previous: None,
        }
    }

    /// Release one session. Returns whether the caller actually held it.
    ///
    /// A non-holder releasing is a no-op rather than an error: a device that
    /// was already displaced (someone took over) will still send its release,
    /// and that must not free the session from under the new holder.
    pub fn release(&mut self, session_id: &str, device_id: &str) -> bool {
        let existing: Option<Claim> = self.claims.get(session_id).cloned();
        match existing {
            Some(e) if e.device.id == device_id => {
                self.claims.remove(session_id);
                self.unindex(session_id, device_id);
                true
            }
            _ => false,
        }
    }

    /// Drop everything a device holds — what a closing socket calls.
    ///
    /// Returns the freed session ids so the daemon can broadcast exactly the
    /// sessions whose owner changed, rather than all of them.
    pub fn release_device(&mut self, device_id: &str) -> Vec<String> {
        let sessions = self.held.remove(device_id).unwrap_or_default();
        for session_id in &sessions {
            self.claims.remove(session_id);
        }
        sessions
    }

    /// Drop claims for sessions that no longer exist.
    ///
    /// The session-level half of the Stale State problem: a session killed
    /// outside ldmux (`exit`, a hand-run kill of the shell) leaves a claim
    /// pointing at nothing, and a later session reusing that id would inherit
    /// it.
    pub fn retain(&mut self, live_session_ids: &std::collections::HashSet<String>) -> Vec<String> {
        let mut dropped = Vec::new();
        let stale: Vec<(String, String)> = self
            .claims
            .iter()
            .filter(|(session_id, _)| !live_session_ids.contains(*session_id))
            .map(|(id, c)| (id.clone(), c.device.id.clone()))
            .collect();
        for (session_id, device_id) in stale {
            self.claims.remove(&session_id);
            self.unindex(&session_id, &device_id);
            dropped.push(session_id);
        }
        dropped
    }

    fn index(&mut self, session_id: &str, device_id: &str) {
        self.held
            .entry(device_id.to_string())
            .or_default()
            .push(session_id.to_string());
    }

    fn unindex(&mut self, session_id: &str, device_id: &str) {
        let Some(v) = self.held.get_mut(device_id) else {
            return;
        };
        v.retain(|s| s != session_id);
        if v.is_empty() {
            self.held.remove(device_id);
        }
    }
}

/// Whether seizing a session closes its desk window.
///
/// The whole difference between the two kinds of shell, in one sentence: a
/// shell the phone started is followed by its window; a shell the desk started
/// is the desk's and the phone is only borrowing it, so the desk keeps its
/// window and using it again is what takes the shell back.
///
/// The TS source of record keeps this in `daemon.ts`; it lives beside the
/// claim path here because it is a pure rule the ownership tests pin.
pub fn seizing_closes_the_desk_window(created_by: &str) -> bool {
    created_by == "phone"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, kind: &str) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
        }
    }

    fn phone() -> Device {
        device("pixel-8", "Pixel 8", "phone")
    }
    fn desktop() -> Device {
        device("desktop:debian", "debian", "desktop")
    }

    #[test]
    fn a_device_can_claim_an_unclaimed_session() {
        let mut o = Ownership::new();
        let res = o.claim("s1", phone(), false, Some(1000));
        match res {
            ClaimResult::Acquired { .. } => {}
            ClaimResult::HeldBy { .. } => panic!("unexpected heldBy"),
        }
        assert_eq!(o.owner("s1").unwrap().device.id, "pixel-8");
        assert_eq!(o.owner("s1").unwrap().since, 1000);
    }

    #[test]
    fn the_holder_re_claiming_is_idempotent_not_a_conflict() {
        // A client re-attaches, resizes and reconnects constantly. Each of
        // those is the holder asking for what it already has; treating it as a
        // conflict would lock a device out of its own session on its second
        // attach.
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, Some(1000));
        o.claim("s1", phone(), false, Some(5000));
        assert_eq!(o.owner("s1").unwrap().since, 1000); // first claim wins, not last touch
    }

    #[test]
    fn a_re_claim_refreshes_the_device_name_without_moving_since() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, Some(1000));
        let mut d = phone();
        d.name = "Pixel 8 Pro".into();
        o.claim("s1", d, false, Some(2000));
        assert_eq!(o.owner("s1").unwrap().device.name, "Pixel 8 Pro");
        assert_eq!(o.owner("s1").unwrap().since, 1000);
    }

    #[test]
    fn a_second_device_is_refused_and_told_who_holds_it() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        match o.claim("s1", desktop(), false, None) {
            ClaimResult::HeldBy { device } => assert_eq!(device.name, "Pixel 8"),
            _ => panic!("expected heldBy"),
        }
        assert_eq!(o.owner("s1").unwrap().device.id, "pixel-8"); // unchanged
    }

    #[test]
    fn takeover_moves_the_claim_and_reports_who_it_displaced() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, Some(1000));
        match o.claim("s1", desktop(), true, Some(2000)) {
            ClaimResult::Acquired { previous, .. } => {
                assert!(matches!(previous, Some(p) if p.name == "Pixel 8"));
            }
            _ => panic!("expected acquired"),
        }
        assert_eq!(o.owner("s1").unwrap().device.id, "desktop:debian");
        assert_eq!(o.owner("s1").unwrap().since, 2000);
    }

    #[test]
    fn takeover_of_an_unclaimed_session_is_not_an_error() {
        // The phone always sends takeover:true after its dialog, and the desk
        // may have gone away in the meantime. That is success, not a failure.
        let mut o = Ownership::new();
        match o.claim("s1", desktop(), true, None) {
            ClaimResult::Acquired { previous, .. } => assert!(previous.is_none()),
            _ => panic!("expected acquired"),
        }
    }

    #[test]
    fn a_displaced_device_cannot_release_the_session_from_under_the_new_holder() {
        // This is the important one. After a takeover the old device still
        // finishes its in-flight work and sends a release; if that release
        // were honoured the session would end up owned by nobody while someone
        // is actively using it.
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        o.claim("s1", desktop(), true, None);
        assert!(!o.release("s1", "pixel-8"));
        assert_eq!(o.owner("s1").unwrap().device.id, "desktop:debian");
        // …while the real holder's release does free it.
        assert!(o.release("s1", "desktop:debian"));
        assert!(o.owner("s1").is_none());
    }

    #[test]
    fn releasing_an_unclaimed_session_is_a_no_op_not_an_error() {
        let mut o = Ownership::new();
        assert!(!o.release("nope", "pixel-8"));
    }

    #[test]
    fn a_device_may_hold_several_sessions_at_once() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        o.claim("s2", phone(), false, None);
        o.claim("s3", desktop(), false, None);
        assert_eq!(o.owner("s1").unwrap().device.id, "pixel-8");
        assert_eq!(o.owner("s2").unwrap().device.id, "pixel-8");
        assert_eq!(o.owner("s3").unwrap().device.id, "desktop:debian");
    }

    #[test]
    fn a_disconnect_frees_every_session_that_device_held_and_only_those() {
        // The Stale State fix: a claim outliving its connection blocks the
        // next attach, so a socket close has to release everything it was
        // holding.
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        o.claim("s2", phone(), false, None);
        o.claim("s3", desktop(), false, None);
        let mut freed = o.release_device("pixel-8");
        freed.sort();
        assert_eq!(freed, ["s1", "s2"]);
        assert!(o.owner("s1").is_none());
        assert!(o.owner("s2").is_none());
        assert_eq!(o.owner("s3").unwrap().device.id, "desktop:debian"); // untouched

        // Freed sessions are immediately claimable by anyone, including the
        // device that just left — a reconnect must not be refused by its own
        // stale claim.
        let tablet = device("tab-s9", "Tab S9", "phone");
        match o.claim("s1", tablet, false, None) {
            ClaimResult::Acquired { .. } => {}
            ClaimResult::HeldBy { .. } => panic!("reconnect refused by its own stale claim"),
        }
    }

    #[test]
    fn releasing_a_device_that_holds_nothing_is_empty_not_a_throw() {
        let mut o = Ownership::new();
        assert!(o.release_device("never-connected").is_empty());
    }

    #[test]
    fn retain_drops_claims_for_sessions_that_no_longer_exist() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        o.claim("s2", desktop(), false, None);
        let live: std::collections::HashSet<String> = ["s1".to_string()].into_iter().collect();
        let dropped = o.retain(&live);
        assert_eq!(dropped, ["s2"]);
        assert_eq!(o.owner("s1").unwrap().device.id, "pixel-8");
        assert!(o.owner("s2").is_none());
    }

    #[test]
    fn retain_does_not_leave_a_dangling_reverse_index() {
        // If `retain` removed the claim but not the device's entry, the
        // device's next disconnect would report freeing a session it no longer
        // holds — and a later session reusing that id would be silently
        // released by it.
        let mut o = Ownership::new();
        o.claim("s1", desktop(), false, None);
        let live: std::collections::HashSet<String> = std::collections::HashSet::new();
        o.retain(&live);
        assert!(o.release_device("desktop:debian").is_empty());
        // The id is reusable and a fresh claim is not considered a conflict.
        match o.claim("s1", phone(), false, None) {
            ClaimResult::Acquired { .. } => {}
            ClaimResult::HeldBy { .. } => panic!("stale claim inherited"),
        }
    }

    #[test]
    fn session_ids_reports_exactly_the_claimed_sessions() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        o.claim("s2", desktop(), false, None);
        let mut ids = o.session_ids();
        ids.sort();
        assert_eq!(ids, ["s1", "s2"]);
        o.release("s1", "pixel-8");
        assert_eq!(o.session_ids(), ["s2"]);
    }

    #[test]
    fn a_session_id_reused_after_a_kill_is_not_inherited_by_the_old_holder() {
        let mut o = Ownership::new();
        o.claim("s1", phone(), false, None);
        // s1 dies and is retained away, then a new session happens to reuse
        // the id.
        let live: std::collections::HashSet<String> = std::collections::HashSet::new();
        o.retain(&live);
        assert!(o.owner("s1").is_none());
        match o.claim("s1", desktop(), false, None) {
            ClaimResult::Acquired { .. } => {}
            ClaimResult::HeldBy { .. } => panic!("old holder inherited"),
        }
    }

    #[test]
    fn seizing_a_shell_only_disturbs_the_desk_for_a_shell_the_phone_started() {
        assert!(seizing_closes_the_desk_window("phone"));
        for origin in ["local", "adopt", "", "desktop"] {
            assert!(!seizing_closes_the_desk_window(origin));
        }
    }
}
