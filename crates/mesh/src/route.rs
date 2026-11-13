//! The peer's path, how good it is, and which carrier the next attempt takes.
//!
//! Three decisions live here, and all three were previously spread across boolean flags on both
//! ends of the link (`directWorks`, `peer_via_flood`, `heardByFlood`, `directsSinceStale`,
//! `pathless_directs`). Those flags latched against each other — each end waited to hear a direct
//! record before sending one, so neither ever heard one — and they conflated two different facts:
//! *an inbound chain exists* and *our traffic crossed a path*. This module keeps them apart.
//!
//! ## A route is a candidate until something proves it
//!
//! Two ways a path arrives, and only one of them is evidence:
//!
//! * **A flood that reached us.** Its travel path is the chain it crossed, and reversing it is a
//!   guess about the outbound direction — a flood is forwarded by every repeater in range, so the
//!   chain that delivered it says nothing about whether our own record would get back. The field
//!   measured the consequence: a cached advert path of `hops=0` adopted while the peer was sixteen
//!   hops away, and every timer in the session priced from it.
//! * **A direct record of ours that came back authenticated, with its path fully consumed.** The
//!   path we sent along worked, in that direction, on this attempt.
//!
//! So [`Candidate`] is what a learned path is, and [`ProofWitness`] is the only way to turn one into
//! [`Proof::Proven`]. There is no constructor for a witness from a flood, a repeater's echo, or an
//! unauthenticated frame, which is what stops the old flag from coming back.
//!
//! ## Ranking is about the demodulator, not about hop count first
//!
//! At range the measured SNR sits within a few dB of the SX126x demodulation floor
//! (`calcRxScore`, `src/helpers/radiolib/RadioLibWrappers.cpp:309-328`): −6.75 dB was the worst
//! reading recorded. A copy heard 0.5 dB above the floor will fail on the next packet whatever its
//! hop count, so margin decides first and hop count breaks ties among the healthy copies.
//!
//! ## The carrier is chosen per attempt
//!
//! `docs/RADIO-PLAN.md` §4.3: the first transmission of a part goes direct when a usable route is
//! known and floods when it is not; every retransmission floods. A retransmission flood is not a
//! waste — it is the only way a record reaches a peer whose inbound route is unknown, and it is one
//! packet per attempt rather than a sticky mode that has to be un-latched.

use core::cmp::Ordering;

/// How long to keep collecting copies of the same flood answer before ranking them.
///
/// A repeater delays a forward by up to `2.5 × airtime` — 1.5 s for a 172-byte record at the public
/// preset, whose airtime is 602.6 ms — so two and a half seconds collects the copies the first two
/// hops produce of one answer. Longer would stall the session behind copies that have not arrived;
/// shorter would rank the first copy to arrive and never see the better one.
pub const BURST_SETTLE_MS: u64 = 2_500;

/// How old a learned path may be and still be used.
///
/// The community's own path-gating window. A path is a claim about the mesh's topology, and a
/// repeater set changes without telling anyone.
pub const ROUTE_FRESH_MS: u64 = 120_000;

/// Below this margin a copy is rejected unless it is the only one that arrived.
pub const MARGIN_REJECT_DB: f32 = 2.0;

/// At or above this margin, hop count decides instead of margin.
pub const MARGIN_CONFIDENT_DB: f32 = 6.0;

/// The first transmission of a part: direct when a route is usable, flood when not.
pub const FIRST_ATTEMPT: u8 = 1;

/// Every retransmission from this attempt onwards floods.
pub const FIRST_FLOOD_ATTEMPT: u8 = 2;

/// The SNR at which an SX126x demodulates each spreading factor, from the firmware's own table.
///
/// `snr_threshold[] = { -7.5, -10, -12.5, -15, -17.5, -20 }`, indexed by `sf - 7`
/// (`src/helpers/radiolib/RadioLibWrappers.cpp:310-317`, with the comment "SF7 needs at least
/// -7.5 dB SNR"). The firmware uses it to score a packet for receive scheduling; here it is the
/// denominator of a margin, which is the same number used for the question it actually answers.
///
/// A spreading factor outside 7..=12 is not something a MeshCore radio can be tuned to
/// (`CMD_SET_RADIO_PARAMS` refuses it), so the fallback is SF7's threshold: if a preset is ever
/// mis-parsed, the ranking must fail toward rejecting a marginal copy rather than accepting one.
#[must_use]
pub fn demod_limit_db(sf: u8) -> f32 {
    match sf {
        7 => -7.5,
        8 => -10.0,
        9 => -12.5,
        10 => -15.0,
        11 => -17.5,
        12 => -20.0,
        _ => -7.5,
    }
}

/// How far above the demodulation floor a copy was heard, in dB.
///
/// Negative means the radio should not have demodulated it at all, which happens: the threshold is
/// a datasheet figure for a 1 % packet error rate, not a cliff.
#[must_use]
pub fn margin_db(snr_db: f32, sf: u8) -> f32 {
    snr_db - demod_limit_db(sf)
}

/// Whether a path is evidence, and of what.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Proof {
    /// Learned from a flood's reversed travel path or a cached advert path. A guess about the
    /// outbound direction.
    Candidate,
    /// Our own authenticated record came back along it with every hop consumed.
    Proven,
}

/// One path to the peer, and what is known about it.
#[derive(Clone, PartialEq, Debug)]
pub struct Candidate {
    path: Vec<u8>,
    path_width: u8,
    hops: u8,
    snr_db: f32,
    heard_at_ms: u64,
    proof: Proof,
}

impl Candidate {
    /// A path learned from a flood's travel path (reversed) or from a cached advert path.
    ///
    /// Always [`Proof::Candidate`]. There is deliberately no argument that changes that: proof is
    /// not a property a learned path can carry, and the defect it prevents is a flood path adopted
    /// as proven.
    #[must_use]
    pub fn learned(path: Vec<u8>, path_width: u8, snr_db: f32, heard_at_ms: u64) -> Self {
        // Clamp before counting, so the hop count and the width describe the same path: four and
        // three are both illegal widths, and a path read one way and stored another is a path that
        // would be sent wrongly.
        let path_width = path_width.clamp(1, 3);
        let width = usize::from(path_width);
        let hops = u8::try_from(path.len() / width).unwrap_or(u8::MAX);
        Self {
            path,
            path_width,
            hops,
            snr_db,
            heard_at_ms,
            proof: Proof::Candidate,
        }
    }

    /// The path bytes, in the order a packet must carry them to reach the peer.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// How many repeaters the path crosses.
    #[must_use]
    pub fn hops(&self) -> u8 {
        self.hops
    }

    /// The hash width the path is encoded with, in bytes (1..=3).
    #[must_use]
    pub fn path_width(&self) -> u8 {
        self.path_width
    }

    /// The SNR this path was last heard at, in dB.
    #[must_use]
    pub fn snr_db(&self) -> f32 {
        self.snr_db
    }

    /// When it was heard, on the caller's clock.
    #[must_use]
    pub fn heard_at_ms(&self) -> u64 {
        self.heard_at_ms
    }

    /// Whether it is evidence ([`Proof::Proven`]) or a learned guess ([`Proof::Candidate`]).
    #[must_use]
    pub fn proof(&self) -> Proof {
        self.proof
    }

    /// Whether it is proven.
    #[must_use]
    pub fn is_proven(&self) -> bool {
        self.proof == Proof::Proven
    }

    /// Whether it is young enough to use at `now_ms`.
    #[must_use]
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.heard_at_ms) <= ROUTE_FRESH_MS
    }

    /// Its SNR margin over the demodulation floor at `sf`, in dB.
    #[must_use]
    pub fn margin_db(&self, sf: u8) -> f32 {
        margin_db(self.snr_db, sf)
    }

    /// Which side of [`MARGIN_REJECT_DB`] and [`MARGIN_CONFIDENT_DB`] it falls on.
    ///
    /// `0` is confident, `1` is marginal, `2` is a copy that should be rejected unless nothing else
    /// arrived. Ordering by this first is what makes a strong long path beat a weak short one.
    fn bucket(&self, sf: u8) -> u8 {
        let margin = self.margin_db(sf);
        if margin >= MARGIN_CONFIDENT_DB {
            0
        } else if margin >= MARGIN_REJECT_DB {
            1
        } else {
            2
        }
    }
}

/// The only thing that can turn a [`Candidate`] into [`Proof::Proven`].
///
/// It exists as its own type so that "prove this route" cannot be spelled by setting a field: a
/// caller has to have had a record come back, and to say so.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProofWitness {
    path: Vec<u8>,
}

impl ProofWitness {
    /// A witness from a direct record the peer sent, authenticated, that arrived with every hop
    /// consumed.
    ///
    /// `path_sent` is the path our own record travelled out; `hops` is the path length the carrier
    /// reports on the arrival. `None` when `hops` is not zero: a direct packet with bytes left in
    /// its path is still in transit to another node and has proved nothing about us (that
    /// distinction is `RC-3`, where a first-hop repeater's own retransmission was accepted as an
    /// inbound record).
    ///
    /// **This type cannot check the authentication.** By the time a caller has a record to hand, it
    /// has already verified the AEAD tag or the PSK MAC; the name says so, and the alternative — a
    /// witness the session layer constructs — would put the session layer's key material in a
    /// routing table.
    #[must_use]
    pub fn authenticated_direct(path_sent: &[u8], hops: u8) -> Option<Self> {
        (hops == 0).then(|| Self {
            path: path_sent.to_vec(),
        })
    }

    /// The path this witness speaks for.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }
}

/// Every path this end currently believes it can reach the peer by.
#[derive(Clone, Debug, Default)]
pub struct RouteTable {
    candidates: Vec<Candidate>,
}

impl RouteTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many paths are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether no path has been learned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Takes a learned path, replacing what is held for the same path bytes.
    ///
    /// A path is identified by its bytes alone, so a fresher measurement of the same road replaces
    /// the older one whether it is better or worse: the SNR is a reading of *now*, and keeping an
    /// old reading because it was kinder is how a dead path stays selected.
    pub fn observe(&mut self, mut candidate: Candidate) {
        if let Some(existing) = self
            .candidates
            .iter_mut()
            .find(|held| held.path == candidate.path)
        {
            // **Being mentioned again does not un-prove a road.** Proof is evidence that this end's
            // own authenticated record came back along the path with every hop consumed, and a later
            // flood carrying the same chain says nothing against it. Replacing the held candidate
            // wholesale reset the proof, so a route proven a moment ago went back to a guess and the
            // next send went back to flooding: measured 2026-11-13, a host that held its adjacent
            // route only as a candidate while the phone was a metre away.
            if existing.proof == Proof::Proven {
                candidate.proof = Proof::Proven;
            }
            *existing = candidate;
        } else {
            self.candidates.push(candidate);
        }
    }

    /// Marks the path a [`ProofWitness`] speaks for as proven.
    ///
    /// Does nothing when no candidate holds that path: a witness about a road this end is not
    /// holding teaches nothing, and inventing a candidate from it would be the same conflation the
    /// witness exists to prevent.
    pub fn adopt_proven(&mut self, witness: &ProofWitness) {
        for held in &mut self.candidates {
            if held.path == witness.path {
                held.proof = Proof::Proven;
            }
        }
    }

    /// Drops every path older than [`ROUTE_FRESH_MS`].
    pub fn prune(&mut self, now_ms: u64) {
        self.candidates.retain(|candidate| candidate.is_fresh(now_ms));
    }

    /// The best usable path at `now_ms` for a radio at spreading factor `sf`.
    ///
    /// `None` when nothing fresh is held. A marginal copy is returned only when it is all there is,
    /// and the lowest-tier copies are returned only when nothing better is fresh — silence is a
    /// worse answer than a bad path, because a flood attempt is what follows either way.
    #[must_use]
    pub fn best(&self, now_ms: u64, sf: u8) -> Option<&Candidate> {
        let fresh: Vec<&Candidate> = self
            .candidates
            .iter()
            .filter(|candidate| candidate.is_fresh(now_ms))
            .collect();
        let eligible: Vec<&Candidate> = fresh
            .iter()
            .copied()
            .filter(|candidate| candidate.bucket(sf) < 2)
            .collect();
        let pool = if eligible.is_empty() { fresh } else { eligible };
        // `Vec` order is insertion order, so an exact tie keeps the path held longest: two copies
        // that rank identically are not a reason to switch roads mid-session.
        pool.into_iter()
            .reduce(|best, next| if ranks_before(next, best, sf) { next } else { best })
    }

    /// Which carrier the `attempt`-th transmission takes.
    ///
    /// `attempt` is 1 for the first transmission of a part and grows from there; attempts at or
    /// above [`FIRST_FLOOD_ATTEMPT`] always flood, because a direct attempt that produced nothing
    /// is evidence about the path and not about the mesh.
    #[must_use]
    pub fn carrier_for(&self, attempt: u8, now_ms: u64, sf: u8) -> Carriage {
        // **A real chain beats a guessed adjacency.** A direct packet with no path reaches only the
        // neighbours that hear it and is forwarded by none, so a wrong guess loses the record with no
        // recovery, while a chain at least reaches its first repeater and can be corrected. "Fewest
        // hops wins" made an unproven empty chain both the most attractive and the most fragile
        // candidate at once: a host with `route 2 repeaters in the path` sent every transaction part
        // `carriage=direct path=[]` at `-72 dBm` from the peer and none of them arrived (2026-11-13).
        //
        // The guess still gets one try when there is no chain at all, because that try is how an
        // adjacent peer is discovered: its record comes back with its path finished and proves it.
        let fresh = || self.candidates.iter().filter(move |c| c.is_fresh(now_ms));
        let chain = fresh()
            .filter(|c| !c.path().is_empty())
            .reduce(|best, next| if ranks_before(next, best, sf) { next } else { best });
        let proven_empty = fresh().find(|c| c.path().is_empty() && c.is_proven());
        let usable = chain.or(proven_empty).or_else(|| self.best(now_ms, sf));
        carrier_for(usable, attempt)
    }

    /// Every path held, for a status line or a test.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }
}

/// Which carrier a record is handed to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Carriage {
    /// A `RAW_CUSTOM` direct packet along this path.
    Direct {
        /// The path to travel, as a packet must carry it.
        path: Vec<u8>,
    },
    /// A flooded record inside the `LDMR` wrapper.
    Flood,
}

impl Carriage {
    /// Whether it is the flood carrier.
    #[must_use]
    pub fn is_flood(&self) -> bool {
        matches!(self, Self::Flood)
    }

    /// The path a direct carrier travels, or `None` for a flood.
    #[must_use]
    pub fn path(&self) -> Option<&[u8]> {
        match self {
            Self::Direct { path } => Some(path),
            Self::Flood => None,
        }
    }
}

/// The carrier rule itself, as a function of the best path and which attempt this is.
///
/// Separate from [`RouteTable`] so it can be read, and tested, without a table.
#[must_use]
pub fn carrier_for(best: Option<&Candidate>, attempt: u8) -> Carriage {
    match best {
        Some(candidate) if attempt < FIRST_FLOOD_ATTEMPT => Carriage::Direct {
            path: candidate.path.clone(),
        },
        _ => Carriage::Flood,
    }
}

/// Whether `a` should be chosen over `b` at spreading factor `sf`.
///
/// Confident copies are compared by hop count first — a shorter road is fewer transmissions, which
/// is the scarce resource — and marginal copies by margin first, because at 3 dB a longer road that
/// arrives is worth more than a short one that does not. `total_cmp` keeps the order total and
/// reproducible; SNR is never NaN in practice, but a comparator that only *mostly* orders is a sort
/// that occasionally reorders.
fn ranks_before(a: &Candidate, b: &Candidate, sf: u8) -> bool {
    match a.bucket(sf).cmp(&b.bucket(sf)) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => {
            let by_margin = b.margin_db(sf).total_cmp(&a.margin_db(sf));
            let by_hops = a.hops.cmp(&b.hops);
            if a.bucket(sf) == 0 {
                matches!(by_hops.then(by_margin), Ordering::Less)
            } else {
                matches!(by_margin.then(by_hops), Ordering::Less)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-byte-hash path of `hops` byte values that are not all the same.
    fn path(hops: u8) -> Vec<u8> {
        (1..=hops).collect()
    }

    fn candidate(hops: u8, snr_db: f32, heard_at_ms: u64) -> Candidate {
        Candidate::learned(path(hops), 1, snr_db, heard_at_ms)
    }

    /// The table matches the firmware's own thresholds, because a margin computed against a
    /// remembered number is a ranking against a different radio.
    #[test]
    fn the_demodulation_table_is_the_firmwares() {
        for (sf, limit) in [(7, -7.5), (8, -10.0), (9, -12.5), (10, -15.0), (11, -17.5), (12, -20.0)] {
            assert!((demod_limit_db(sf) - limit).abs() < f32::EPSILON, "SF{sf}");
        }
        // A spreading factor the radio cannot be tuned to falls back to the strictest threshold, so
        // a mis-parsed preset rejects a marginal copy rather than accepting it.
        assert!((demod_limit_db(6) - -7.5).abs() < f32::EPSILON);
        assert!((margin_db(-2.0, 7) - 5.5).abs() < 1e-4);
    }

    /// Among healthy copies, fewer hops wins: each hop is another transmission of the same bytes,
    /// and airtime is the scarce resource.
    #[test]
    fn a_confident_copy_prefers_fewer_hops() {
        let mut table = RouteTable::new();
        table.observe(candidate(9, 0.0, 100)); // margin 7.5 dB
        table.observe(candidate(3, -1.0, 100)); // margin 6.5 dB
        let best = table.best(1_000, 7).expect("a path");
        assert_eq!(best.hops(), 3);
    }

    /// Among confident copies on the same road, the stronger one wins — but only as a tiebreak.
    #[test]
    fn a_confident_copy_breaks_a_hop_tie_on_margin() {
        let mut table = RouteTable::new();
        table.observe(Candidate::learned(vec![1, 2], 1, -1.0, 100));
        table.observe(Candidate::learned(vec![3, 4], 1, 1.0, 100));
        let best = table.best(1_000, 7).expect("a path");
        assert_eq!(best.path(), &[3, 4]);
    }

    /// A marginal copy is a copy the demodulator nearly missed, so margin decides before distance.
    ///
    /// Both candidates here are in the 2.0–6.0 dB band, where a packet fails on the next attempt
    /// rather than necessarily on this one.
    #[test]
    fn a_marginal_copy_is_ranked_on_margin_before_hops() {
        let mut table = RouteTable::new();
        // margin 3.0 dB at 2 hops, margin 5.0 dB at 9 hops.
        table.observe(candidate(2, -4.5, 100));
        table.observe(candidate(9, -2.5, 100));
        let best = table.best(1_000, 7).expect("a path");
        assert_eq!(best.hops(), 9);
    }

    /// A strong long road beats a weak short one: the weak one is the copy that will not arrive.
    #[test]
    fn a_confident_copy_beats_a_marginal_one_with_fewer_hops() {
        let mut table = RouteTable::new();
        table.observe(candidate(2, -4.5, 100)); // marginal, 2 hops
        table.observe(candidate(12, 1.0, 100)); // confident, 12 hops
        let best = table.best(1_000, 7).expect("a path");
        assert_eq!(best.hops(), 12);
    }

    /// A copy under 2 dB of margin is only used when it is the only one left, because it is a copy
    /// the radio nearly did not demodulate.
    #[test]
    fn a_copy_under_two_db_is_used_only_when_it_is_all_there_is() {
        let mut table = RouteTable::new();
        table.observe(candidate(1, -7.0, 100)); // margin 0.5 dB
        let best = table.best(1_000, 7).expect("the only copy");
        assert_eq!(best.hops(), 1);

        table.observe(candidate(4, -4.0, 100)); // margin 3.5 dB
        let best = table.best(1_000, 7).expect("a path");
        assert_eq!(best.hops(), 4, "the 0.5 dB copy was preferred over a usable one");
    }

    /// A path is a claim about the mesh and the mesh changes, so an old one is no path at all.
    #[test]
    fn a_stale_path_is_not_a_path() {
        let mut table = RouteTable::new();
        table.observe(candidate(2, 2.0, 0));
        assert!(table.best(ROUTE_FRESH_MS, 7).is_some());
        assert!(table.best(ROUTE_FRESH_MS + 1, 7).is_none());
        assert_eq!(table.len(), 1);
        table.prune(ROUTE_FRESH_MS + 1);
        assert!(table.is_empty());
    }

    /// The defect this type exists for: a path learned from a flood is a guess about the outbound
    /// direction, and no amount of ranking makes it evidence.
    #[test]
    fn a_path_learned_from_a_flood_can_never_be_proven() {
        let mut table = RouteTable::new();
        let learned = path(4);
        // A flood's reversed travel path arrives through `observed`, which cannot set proof.
        table.observe(Candidate::learned(learned.clone(), 1, 9.0, 100));
        assert_eq!(table.best(1_000, 7).expect("a path").proof(), Proof::Candidate);

        // A direct arrival with hops left in its path is in transit to someone else: no witness.
        assert!(ProofWitness::authenticated_direct(&learned, 3).is_none());
        // The same arrival with every hop consumed is the only thing that proves it.
        let witness = ProofWitness::authenticated_direct(&learned, 0).expect("zero hops proves");
        table.adopt_proven(&witness);
        assert_eq!(table.best(1_000, 7).expect("a path").proof(), Proof::Proven);
    }

    /// A witness about a path this end is not holding must not invent one.
    #[test]
    fn a_witness_for_an_unknown_path_teaches_nothing() {
        let mut table = RouteTable::new();
        table.observe(candidate(2, 3.0, 100));
        let witness = ProofWitness::authenticated_direct(&path(5), 0).expect("zero hops proves");
        table.adopt_proven(&witness);
        assert_eq!(table.len(), 1);
        assert!(!table.best(1_000, 7).expect("a path").is_proven());
    }

    /// The first transmission of a part is direct when a path is known, and a flood when it is not;
    /// every retransmission floods.
    #[test]
    fn the_first_attempt_goes_direct_and_every_retry_floods() {
        let with_route = carrier_for(Some(&candidate(2, 3.0, 100)), FIRST_ATTEMPT);
        assert_eq!(
            with_route,
            Carriage::Direct {
                path: path(2)
            }
        );
        assert_eq!(carrier_for(None, FIRST_ATTEMPT), Carriage::Flood);
        for attempt in FIRST_FLOOD_ATTEMPT..=5 {
            assert_eq!(
                carrier_for(Some(&candidate(2, 3.0, 100)), attempt),
                Carriage::Flood,
                "attempt {attempt} kept a direct carrier"
            );
        }
    }

    /// The same road heard twice replaces its own reading rather than accumulating copies, and a
    /// fresher reading wins even when it is worse.
    #[test]
    fn the_same_path_is_held_once_and_its_newest_reading_wins() {
        let mut table = RouteTable::new();
        table.observe(candidate(3, 5.0, 100));
        table.observe(candidate(3, 0.5, 200));
        assert_eq!(table.len(), 1);
        let best = table.best(1_000, 7).expect("a path");
        assert!((best.snr_db() - 0.5).abs() < f32::EPSILON);
        assert_eq!(best.heard_at_ms(), 200);
    }
}

#[cfg(test)]
mod adjacency_ranking {
    //! **The adjacent route is refreshed by the proof, and being mentioned again never un-proves it.**
    //!
    //! Field case, 2026-11-13: two radios a metre apart and the host sending along three repeaters,
    //! because the adjacent candidate expired while a looping flood kept a long chain fresh, and
    //! `observe` reset a proof back to a guess whenever the same chain was mentioned again. The
    //! ranking itself is not the defect — a *marginal* short road does lose to a confident long one
    //! by design (`a_confident_copy_beats_a_marginal_one_with_fewer_hops`) — so what is pinned here
    //! is the refresh and the proof, which is what actually kept the two ends apart.

    use super::*;

    #[test]
    fn a_re_observed_chain_keeps_its_proof() {
        let now = 1_000_000;
        let mut route = RouteTable::new();
        route.observe(Candidate::learned(vec![9, 8, 7], 1, 6.0, now));
        let witness = ProofWitness::authenticated_direct(&[9, 8, 7], 0).expect("a finished path");
        route.adopt_proven(&witness);
        assert!(route.candidates()[0].is_proven(), "the witness did not prove it");

        // The same chain heard again a moment later, as a flood reports it.
        route.observe(Candidate::learned(vec![9, 8, 7], 1, 6.0, now + 1_000));
        assert!(
            route.candidates()[0].is_proven(),
            "a second mention of a proven road turned it back into a guess: {:?}",
            route.candidates()
        );
    }

    #[test]
    fn a_re_observation_refreshes_the_hearing_time() {
        let now = 1_000_000;
        let mut route = RouteTable::new();
        route.observe(Candidate::learned(Vec::new(), 1, 12.0, now));
        assert_eq!(route.best(now, 7).map(|c| c.path().to_vec()), Some(Vec::new()));

        // The proof refreshes it far beyond the freshness window, so it is still the choice later.
        let later = now + ROUTE_FRESH_MS + 1;
        route.observe(Candidate::learned(Vec::new(), 1, 12.0, later));
        assert!(
            route.best(later, 7).is_some_and(|c| c.path().is_empty()),
            "the adjacent route was not refreshed by its own evidence: {:?}",
            route.candidates()
        );
    }

    #[test]
    fn two_fresh_adjacent_routes_are_the_same_route_held_once() {
        let now = 1_000_000;
        let mut route = RouteTable::new();
        route.observe(Candidate::learned(Vec::new(), 1, 12.0, now));
        route.observe(Candidate::learned(Vec::new(), 1, 4.0, now + 500));
        assert_eq!(
            route.candidates().len(),
            1,
            "the adjacent route was held twice: {:?}",
            route.candidates()
        );
        assert!(
            route.candidates()[0].snr_db() == 4.0,
            "the newest reading did not replace the older one"
        );
    }
}

#[cfg(test)]
mod empty_path_needs_proof {
    //! **A learned empty chain is a guess, and the one guess that cannot be recovered.** A direct
    //! packet with no path reaches only the neighbours that hear it and is forwarded by none.

    use super::*;

    #[test]
    fn a_real_chain_beats_a_guessed_adjacency() {
        let mut route = RouteTable::new();
        route.observe(Candidate::learned(Vec::new(), 1, 6.0, 1_000));
        // Alone, the guess is tried once: that try is how an adjacent peer is discovered.
        assert_eq!(
            route.carrier_for(1, 1_000, 7),
            Carriage::Direct { path: Vec::new() },
            "an adjacent guess was never tried, so adjacency could never be proven"
        );
        // With a real chain beside it, the chain wins: the guess cannot be recovered if it is wrong.
        route.observe(Candidate::learned(vec![9], 1, 6.0, 1_000));
        assert_eq!(
            route.carrier_for(1, 1_000, 7),
            Carriage::Direct { path: vec![9] },
            "a guessed adjacency outranked a real chain"
        );
    }

    #[test]
    fn a_proven_adjacency_is_used() {
        let mut route = RouteTable::new();
        route.observe(Candidate::learned(Vec::new(), 1, 6.0, 1_000));
        let witness = ProofWitness::authenticated_direct(&[], 0).expect("an empty chain proves it");
        route.adopt_proven(&witness);
        assert_eq!(
            route.carrier_for(1, 1_000, 7),
            Carriage::Direct { path: Vec::new() },
            "a proven adjacent peer was not sent to directly"
        );
    }
}
