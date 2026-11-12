//! A deterministic MeshCore mesh, so the radio route can be proved without a radio.
//!
//! Every defect this harness exists for was found in the field, on two radios and a public mesh, and
//! every one of them is invisible to the unit tests that were in place: a packet that is transmitted
//! and never reported, an echo accepted as an arrival, two packets twenty milliseconds apart where
//! only the first survives. Those are properties of the *air and the repeaters*, not of any single
//! function, so the only test that can pin them is one that runs a mesh.
//!
//! What is modelled, and where each rule comes from:
//!
//! | Rule | Firmware | Why it decides whether the link works |
//! |---|---|---|
//! | Receive ceiling at 173 bytes | `MyMesh::logRxRaw` (`examples/companion_radio/MyMesh.cpp:291`) | a longer packet is transmitted, forwarded, heard, and never handed to the application |
//! | Deduplication on `SHA256(type ‖ payload)`, 160 entries, no timeout | `Packet::calculatePacketHash`, `SimpleMeshTables` | a byte-identical retry is swallowed; every retry must be a fresh packet |
//! | A sender does not mark its own packet seen | `CMD_SEND_RAW_PACKET` (`MyMesh.cpp:2047`) does not call `markSeen` | its own first hop's retransmission comes back and is accepted as an arrival |
//! | Half duplex: a transmitting node cannot receive | the radio | two packets close together lose the second at every repeater in between |
//! | Forward delay `nextInt(0, 5·airtime·factor + 1)`, 0.5 flood / 0.3 direct | `getRetransmitDelay` (`examples/simple_repeater/MyMesh.cpp:597-605`) | this is what makes the collision window wide enough to swallow a record |
//! | A flood appends this node's hash; a direct packet consumes the first | `routeRecvPacket`, `removeSelfFromPath` (`src/Mesh.cpp:334, 344`) | a flood's reversed path is the only route a far end can learn |
//! | `flood_max` 64 / `flood_max_unscoped` 64 / `flood_max_advert` 8 | `RoutingPolicy.h`, `MyMesh.cpp:970-972` | a flood that exceeds a site's cap is dropped silently |
//! | A flood whose path already holds this node is a loop | `allowPacketForward` loop check | without it a simulator would forward forever |
//!
//! What is **not** modelled, deliberately: MeshCore's cryptography (a payload is opaque, because the
//! harness tests delivery and not a second cipher), the score-based receive delay (`calcRxDelay`,
//! which is `0` on every stock build because `rx_delay_base` ships at `0.0f`), channel/contact
//! addressing beyond the destination the caller supplies, and the CAD/battery gates.
//!
//! Time is whole milliseconds. Airtime is fractional, so a transmission ending at 223.7 ms is
//! scheduled at 224: the rounding is coarser than any deadline this harness asserts and is stated
//! rather than hidden.

mod packet;
mod rng;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

pub use packet::{
    LOG_RX_HEADER, MAX_FRAME_SIZE, MAX_HASH_SIZE, MAX_PACKET_HASHES, MAX_PATH_SIZE, PAYLOAD_TYPE_ADVERT,
    PAYLOAD_TYPE_RAW_CUSTOM, Packet, RECEIVED_MAX, ROUTE_TYPE_DIRECT, ROUTE_TYPE_FLOOD, Route,
};
pub use rng::Rng;

use crate::airtime::{airtime_ms, Modulation};

/// Identifies one node in a [`VirtualMesh`]. Index into the mesh's node list, so it stays stable
/// across a run and prints as a small number in a failing assertion.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct NodeId(pub usize);

/// What a node is and what it is configured to do.
///
/// The defaults are the shipped repeater's own values: one-byte path hashes, 64 / 64 / 8 hop caps,
/// 0.5 and 0.3 transmit-delay factors. A test changes only what it is about.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    /// Whether this node forwards other nodes' packets. A companion radio does not; a repeater does.
    pub repeaters: bool,
    /// Path hash width in bytes, 1..=3. A node whose width differs from its neighbour's cannot read
    /// that neighbour's paths, which is a real and silent incompatibility (`docs/ARCHITECTURE.md` §11).
    pub path_width: u8,
    /// `_prefs.flood_max` — the hop count at which this node stops forwarding any flood.
    pub flood_max: u8,
    /// `_prefs.flood_max_unscoped` — the same for an unscoped flood (`ROUTE_TYPE_FLOOD`).
    pub flood_max_unscoped: u8,
    /// `_prefs.flood_max_advert` — the same for an advert, which is capped far lower.
    pub flood_max_advert: u8,
    /// `_prefs.tx_delay_factor`: the flood forward delay is drawn from `0..5·airtime·this`.
    pub flood_tx_delay_factor: f32,
    /// `_prefs.direct_tx_delay_factor`, the same for a direct packet.
    pub direct_tx_delay_factor: f32,
    /// Pins the forward delay instead of drawing it from the seed.
    ///
    /// `getRetransmitDelay` draws `nextInt(0, 5·airtime·factor + 1)`, so a collision depends on a
    /// draw. A test that has to prove *one* packet is swallowed by a repeater's own transmission
    /// cannot depend on the seed, so `Some(ms)` replaces the draw with a fixed delay and records the
    /// fact in the log. `None` is the firmware's own behaviour and is what any test of aggregate
    /// behaviour uses.
    pub forward_delay_ms: Option<u32>,
    /// The region scopes this node carries, as opaque ids.
    ///
    /// The real thing is two 16-bit transport codes derived by HMAC from a region key
    /// (`TransportKeyStore.cpp`). Verifying that HMAC belongs to the companion codec, not to a
    /// simulator of the air, so a scope here is an identity: a node forwards a scoped flood only when
    /// it carries the scope the flood names, which is the decision `allowPacketForward` makes.
    pub regions: Vec<u16>,
}

impl Default for NodeSpec {
    fn default() -> Self {
        Self {
            repeaters: false,
            path_width: 1,
            flood_max: 64,
            flood_max_unscoped: 64,
            flood_max_advert: 8,
            flood_tx_delay_factor: 0.5,
            direct_tx_delay_factor: 0.3,
            forward_delay_ms: None,
            regions: Vec::new(),
        }
    }
}

impl NodeSpec {
    /// A repeater with the shipped defaults.
    #[must_use]
    pub fn repeater() -> Self {
        Self {
            repeaters: true,
            ..Self::default()
        }
    }

    /// A companion radio (the host's or the phone's) with the shipped defaults.
    ///
    /// It does not repeat, and it is where the 173-byte receive ceiling bites: `logRxRaw` is the
    /// companion interface's own filter, so a node that never reports upward is exactly this one.
    #[must_use]
    pub fn companion() -> Self {
        Self::default()
    }

    /// Sets the path hash width, for the meshes that use two or three bytes.
    #[must_use]
    pub fn with_path_width(mut self, bytes: u8) -> Self {
        self.path_width = bytes;
        self
    }

    /// Sets the region scopes this node carries.
    #[must_use]
    pub fn with_regions(mut self, regions: &[u16]) -> Self {
        self.regions = regions.to_vec();
        self
    }

    /// Pins the forward delay, so a collision test does not hinge on a draw.
    #[must_use]
    pub fn with_forward_delay_ms(mut self, ms: u32) -> Self {
        self.forward_delay_ms = Some(ms);
        self
    }
}

/// What happened to one packet on its way to one node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The packet reached this node and was handed upward.
    Delivered,
    /// This node had seen these exact bytes before and dropped the copy.
    DroppedDedup,
    /// The packet was longer than the companion interface will report.
    DroppedCeiling,
    /// The link lost it. Nothing was received at all.
    DroppedLoss,
    /// This node was transmitting when the packet arrived.
    DeafWhileTransmitting,
    /// A flood exceeded this node's `flood_max`, `flood_max_unscoped` or `flood_max_advert`.
    DroppedHopLimit,
    /// This node does not carry the region a scoped flood names.
    DroppedScope,
    /// This node's own hash was already in the flood's path.
    DroppedLoop,
    /// The path had no room for another hash (`MAX_PATH_SIZE`).
    DroppedPathFull,
    /// A direct packet whose next hop is not this node.
    NotThisHop,
}

impl Outcome {
    /// Whether the packet was handed to the application at this node.
    #[must_use]
    pub fn delivered(self) -> bool {
        matches!(self, Self::Delivered)
    }
}

/// One packet's arrival attempt at one node, with the reason it ended there.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Transmission {
    /// When the attempt happened, on the mesh's own clock.
    pub at_ms: u64,
    /// Who transmitted it.
    pub from: NodeId,
    /// Who was meant to hear it.
    pub to: NodeId,
    /// The packet's raw length in bytes, which is the number the ceiling is measured against.
    pub bytes: usize,
    /// Whether it was a flood.
    pub flood: bool,
    /// The hop count it arrived with.
    pub hops: u8,
    /// The packet's deduplication hash, so a test can correlate the copies of one packet.
    pub hash: [u8; MAX_HASH_SIZE],
    /// What this node did with it.
    pub outcome: Outcome,
}

/// A packet that reached an application, with the path it arrived by.
///
/// `path` is the path **as received**, in travel order. For a flood that is the chain every repeater
/// appended, so its reverse is the route home; for a direct packet it is empty, because every
/// repeater consumed its own hash on the way. That asymmetry is the one a routing decision must
/// respect, and it is why this type carries the path rather than only the hop count.
#[derive(Clone, PartialEq, Debug)]
pub struct Delivery {
    /// The node whose application received it.
    pub to: NodeId,
    /// The payload, opaque.
    pub payload: Vec<u8>,
    /// The path as received, in travel order.
    pub path: Vec<u8>,
    /// How many hops the path describes.
    pub hops: u8,
    /// The SNR of the last link, in dB.
    pub snr_db: f32,
    /// Whether it arrived as a flood.
    pub flood: bool,
    /// When it was received.
    pub at_ms: u64,
}

/// A link between two nodes, with the SNR a receiver measures on it.
#[derive(Clone, Copy, Debug)]
struct Link {
    a: NodeId,
    b: NodeId,
    snr_db: f32,
}

/// One node's state: what it is, its identity hash, its dedup ring, and when it is transmitting.
#[derive(Clone, Debug)]
struct Node {
    spec: NodeSpec,
    /// This node's identity hash, truncated to `path_width` when it is written into a path.
    ///
    /// The firmware's `self_id` is a hash of the node's public key. Here it is derived from the
    /// node's index so a run is reproducible; two nodes whose first byte collides is a real
    /// property of a one-byte path and is not hidden.
    hash: [u8; MAX_HASH_SIZE],
    /// The deduplication ring: the last [`MAX_PACKET_HASHES`] hashes this node processed.
    seen: VecDeque<[u8; MAX_HASH_SIZE]>,
    /// Transmit windows this node has claimed, as `(start, end)` in milliseconds. A node is deaf
    /// while one of these contains the instant a packet arrives.
    transmitting: Vec<(u64, u64)>,
}

impl Node {
    fn is_transmitting_at(&self, at_ms: u64) -> bool {
        self.transmitting
            .iter()
            .any(|(start, end)| *start <= at_ms && at_ms < *end)
    }

    /// When this node is next able to transmit, which is the end of its last claimed window — the
    /// radio is one device and cannot start the next packet before the last one finished.
    fn free_at(&self) -> u64 {
        self.transmitting.iter().map(|(_, end)| *end).max().unwrap_or(0)
    }

    fn seen(&self, hash: &[u8; MAX_HASH_SIZE]) -> bool {
        self.seen.iter().any(|h| h == hash)
    }

    fn mark_seen(&mut self, hash: [u8; MAX_HASH_SIZE]) {
        if self.seen.len() == MAX_PACKET_HASHES {
            self.seen.pop_front();
        }
        self.seen.push_back(hash);
    }
}

/// One scheduled arrival.
#[derive(Clone, Debug)]
struct Arrival {
    at_ms: u64,
    from: NodeId,
    to: NodeId,
    packet: Packet,
    snr_db: f32,
    /// Which of the node's hash width to read the path by. The sender's width is the one the path
    /// was written with, and a node reading it at its own width reads a different path.
    path_width: u8,
}

/// Heap ordering for arrivals: earliest first, then the order they were scheduled.
///
/// Equality compares the two scheduling fields only, because an `Arrival` holds an SNR and a packet
/// and neither is what makes two events the same event. The sequence number is unique per mesh, so
/// no two queued items compare equal in practice and the heap never has to choose between them.
#[derive(Debug)]
struct Queued {
    at_ms: u64,
    seq: u64,
    arrival: Arrival,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.at_ms == other.at_ms && self.seq == other.seq
    }
}

impl Eq for Queued {}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.at_ms
            .cmp(&other.at_ms)
            .then(self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A simulated MeshCore mesh: nodes, the links between them, and a clock.
///
/// Build it, `send_*` something, and `advance` the clock; then read [`VirtualMesh::log`] for what
/// happened to every copy at every node and [`VirtualMesh::deliveries`] for what reached an
/// application.
#[derive(Debug)]
pub struct VirtualMesh {
    now_ms: u64,
    nodes: Vec<Node>,
    links: Vec<Link>,
    queue: BinaryHeap<Reverse<Queued>>,
    seq: u64,
    rng: Rng,
    loss: f64,
    modulation: Modulation,
    log: Vec<Transmission>,
    deliveries: Vec<Delivery>,
}

impl VirtualMesh {
    /// An empty mesh with a seed and the public preset.
    ///
    /// The preset is the public USA/Canada tuple because that is where the field failures happened;
    /// a test that needs another calls [`VirtualMesh::set_modulation`].
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            now_ms: 0,
            nodes: Vec::new(),
            links: Vec::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            rng: Rng::new(seed),
            loss: 0.0,
            modulation: Modulation::PUBLIC_PRESET,
            log: Vec::new(),
            deliveries: Vec::new(),
        }
    }

    /// Adds a node and returns its id.
    pub fn add_node(&mut self, spec: NodeSpec) -> NodeId {
        let id = NodeId(self.nodes.len());
        // A hash that is stable for the id and **proved** distinct between ids, so a path written
        // by one node is not read as another's at any of the three widths. The firmware's is a hash
        // of the public key; the id stands in for it.
        //
        // Distinctness is enforced rather than hoped for. Truncating SHA-256 gives no such promise:
        // nodes 0 and 1 collided in their first byte, and because the firmware's minimal loop
        // detection compares a flood's path against the receiver's own hash, the phone then dropped
        // every flood that had crossed the first repeater as `DroppedLoop` — a whole class of test
        // that silently proved nothing. The loop below rejects any hash whose one- or two-byte
        // prefix is already taken, and repeating the digest with a counter is what makes it cheap.
        let mut hash = [0u8; MAX_HASH_SIZE];
        let mut attempt = 0u64;
        loop {
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest as _;
            hasher.update(b"ldmux-virtual-mesh-node");
            hasher.update(u64::try_from(id.0).unwrap_or(0).to_le_bytes());
            hasher.update(attempt.to_le_bytes());
            let digest = hasher.finalize();
            hash.copy_from_slice(&digest[..MAX_HASH_SIZE]);
            let clashes = self.nodes.iter().any(|node| {
                node.hash[..1] == hash[..1] || node.hash[..2] == hash[..2]
            });
            if !clashes {
                break;
            }
            attempt = attempt.saturating_add(1);
            assert!(attempt < 1_000, "no distinct node hash for id {}", id.0);
        }
        self.nodes.push(Node {
            spec,
            hash,
            seen: VecDeque::new(),
            transmitting: Vec::new(),
        });
        id
    }

    /// Records that `a` and `b` hear each other at `snr_db`.
    ///
    /// The link is symmetric, which is a simplification worth naming: real links are asymmetric, and
    /// an asymmetric one is the reason a route proven in one direction can fail in the other. A test
    /// that needs that builds it as two links with different SNRs — which this API cannot express,
    /// because `link` stores one figure. Read it as "the SNR a receiver measures", and give one node
    /// a longer chain when direction matters.
    pub fn link(&mut self, a: NodeId, b: NodeId, snr_db: f32) {
        self.links.push(Link { a, b, snr_db });
    }

    /// Sets the loss applied to every transmission, `0.0..=1.0`.
    pub fn set_loss(&mut self, p: f64) {
        self.loss = p.clamp(0.0, 1.0);
    }

    /// Sets the radio tuple every node is tuned to.
    pub fn set_modulation(&mut self, m: Modulation) {
        self.modulation = m;
    }

    /// The mesh's clock.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Every arrival attempt so far, in the order it happened.
    #[must_use]
    pub fn log(&self) -> &[Transmission] {
        &self.log
    }

    /// Every packet handed to an application so far, in the order it happened.
    #[must_use]
    pub fn deliveries(&self) -> &[Delivery] {
        &self.deliveries
    }

    /// How long one packet of `bytes` occupies the channel under this mesh's tuple.
    #[must_use]
    pub fn airtime_of(&self, bytes: usize) -> f64 {
        airtime_ms(self.modulation, bytes)
    }

    /// Sends a direct packet from `from` along `path`.
    ///
    /// The path is the chain of hashes to travel, in order, at `from`'s own width. The packet is
    /// acknowledged by nothing: whether it arrived is what [`VirtualMesh::deliveries`] answers, which
    /// is the whole point of carrying a session record this way.
    pub fn send_direct(&mut self, from: NodeId, path: &[u8], payload: &[u8]) -> Vec<Delivery> {
        let packet = Packet::direct(PAYLOAD_TYPE_RAW_CUSTOM, path.to_vec(), payload.to_vec());
        self.transmit(from, packet);
        self.run_until(self.now_ms)
    }

    /// Sends a flood from `from`, carrying `payload`.
    pub fn send_flood(&mut self, from: NodeId, payload: &[u8]) -> Vec<Delivery> {
        let packet = Packet::flood(PAYLOAD_TYPE_RAW_CUSTOM, payload.to_vec());
        self.transmit(from, packet);
        self.run_until(self.now_ms)
    }

    /// Sends a flood with a chosen payload type, for the tests that need an advert's hop cap.
    pub fn send_flood_as(
        &mut self,
        from: NodeId,
        payload_type: u8,
        payload: &[u8],
    ) -> Vec<Delivery> {
        let packet = Packet::flood(payload_type, payload.to_vec());
        self.transmit(from, packet);
        self.run_until(self.now_ms)
    }

    /// Sends a flood scoped to `scope`, which only repeaters carrying that region will forward.
    pub fn send_flood_scoped(
        &mut self,
        from: NodeId,
        payload_type: u8,
        scope: u16,
        payload: &[u8],
    ) -> Vec<Delivery> {
        let packet = Packet::flood_scoped(payload_type, scope, payload.to_vec());
        self.transmit(from, packet);
        self.run_until(self.now_ms)
    }

    /// Advances the clock by `ms`, delivering everything that comes due, and returns the deliveries
    /// that happened in the window.
    pub fn advance(&mut self, ms: u64) -> Vec<Delivery> {
        let target = self.now_ms.saturating_add(ms);
        let from = self.deliveries.len();
        self.run_until(target);
        self.now_ms = target;
        self.deliveries[from..].to_vec()
    }

    /// Runs the queue until nothing is left due at or before `target`, without moving the clock past
    /// it.
    fn run_until(&mut self, target: u64) -> Vec<Delivery> {
        let from = self.deliveries.len();
        while let Some(Reverse(next)) = self.queue.peek() {
            if next.at_ms > target {
                break;
            }
            let Reverse(next) = self.queue.pop().unwrap_or_else(|| unreachable!("just peeked"));
            self.now_ms = self.now_ms.max(next.at_ms);
            self.arrive(next.arrival);
        }
        self.deliveries[from..].to_vec()
    }

    /// Claims a transmit window for `from` starting no earlier than now, and schedules the packet's
    /// arrivals from the end of it.
    ///
    /// The radio serialises its own transmissions, so a packet handed over while the previous one is
    /// still on the air waits. That wait is not a detail: it is what turns "three copies fired 20 ms
    /// apart" into three copies fired back to back, which is the shape `RC-1` describes.
    fn transmit(&mut self, from: NodeId, packet: Packet) {
        let Some(node) = self.nodes.get(from.0) else {
            return;
        };
        let path_width = node.spec.path_width;
        let airtime = self.airtime_of(packet.raw_len());
        let airtime_ms = airtime.round().max(1.0) as u64;
        let start = self.now_ms.max(node.free_at());
        let end = start.saturating_add(airtime_ms);
        self.nodes[from.0].transmitting.push((start, end));

        let neighbours = self.neighbours(from);
        for (to, snr_db) in neighbours {
            self.seq = self.seq.wrapping_add(1);
            self.queue.push(Reverse(Queued {
                at_ms: end,
                seq: self.seq,
                arrival: Arrival {
                    at_ms: end,
                    from,
                    to,
                    packet: packet.clone(),
                    snr_db,
                    path_width,
                },
            }));
        }
    }

    /// Every node that hears `from`, with the SNR it measures.
    fn neighbours(&self, from: NodeId) -> Vec<(NodeId, f32)> {
        let mut out = Vec::new();
        for link in &self.links {
            if link.a == from && link.b != from {
                out.push((link.b, link.snr_db));
            } else if link.b == from && link.a != from {
                out.push((link.a, link.snr_db));
            }
        }
        out
    }

    /// One packet arriving at one node: the whole decision, in the firmware's own order.
    fn arrive(&mut self, arrival: Arrival) {
        let Arrival {
            at_ms,
            from,
            to,
            packet,
            snr_db,
            path_width,
        } = arrival;
        if to.0 >= self.nodes.len() {
            return;
        }
        let hash = packet.hash();
        let bytes = packet.raw_len();
        let hops = packet.hops(path_width);
        let flood = packet.route.is_flood();
        let mut outcome = self.admit(to, &packet, path_width, hops, hash, at_ms);

        // The receiver's copy, before any forwarding. Only a companion node has an application to
        // hand a packet to; a repeater accepting a packet is not a delivery, it is a decision to
        // relay, and calling that a delivery would let a test count a repeater as an endpoint.
        if outcome == Outcome::Delivered && !self.nodes[to.0].spec.repeaters {
            let path = packet.path.clone();
            self.deliveries.push(Delivery {
                to,
                payload: packet.payload.clone(),
                path,
                hops,
                snr_db,
                flood,
                at_ms,
            });
        }

        // A repeater forwards an admitted packet after its own delay, if it has room for one more
        // hash. Admission and forwarding are one decision in the firmware (`routeRecvPacket`), so
        // they are one here too.
        if outcome == Outcome::Delivered && self.nodes[to.0].spec.repeaters {
            if flood {
                let width = usize::from(self.nodes[to.0].spec.path_width);
                let next_count = usize::from(hops) + 1;
                if next_count * width > MAX_PATH_SIZE {
                    outcome = Outcome::DroppedPathFull;
                } else {
                    let mut forwarded = packet.clone();
                    forwarded.path.extend_from_slice(
                        &self.nodes[to.0].hash[..width],
                    );
                    self.forward(to, forwarded, true);
                }
            } else if !packet.path.is_empty() {
                // `removeSelfFromPath`: the first hash was ours and is consumed, so the next hop sees
                // a shorter path and the destination sees an empty one.
                let width = usize::from(path_width);
                let mut forwarded = packet.clone();
                forwarded.path.drain(..width.min(forwarded.path.len()));
                self.forward(to, forwarded, false);
            }
        }

        self.log.push(Transmission {
            at_ms,
            from,
            to,
            bytes,
            flood,
            hops,
            hash,
            outcome,
        });
    }

    /// The admission decision for one packet at one node, in the firmware's order:
    /// loss, then the receive ceiling, then deduplication, then hop limits, then loop detection.
    ///
    /// The order matters and is not arbitrary. Loss is the air. The ceiling is `logRxRaw`, which runs
    /// before the packet is even parsed (`Dispatcher::checkRecv`). Deduplication is checked in each
    /// payload-type branch, before `routeRecvPacket` decides whether to forward. And the hop limit is
    /// inside `allowPacketForward`, which `routeRecvPacket` calls — so a packet that exceeds a site's
    /// cap is marked seen and then dropped, never the other way round.
    fn admit(
        &mut self,
        to: NodeId,
        packet: &Packet,
        path_width: u8,
        hops: u8,
        hash: [u8; MAX_HASH_SIZE],
        at_ms: u64,
    ) -> Outcome {
        if self.nodes[to.0].is_transmitting_at(at_ms) {
            return Outcome::DeafWhileTransmitting;
        }
        if self.rng.chance(self.loss) {
            return Outcome::DroppedLoss;
        }
        // The ceiling is the companion interface's, so it applies to the nodes that have one. A
        // repeater does not need to report a packet upward to forward it, which is exactly why an
        // oversized record crosses a mesh and dies at the far end.
        if !self.nodes[to.0].spec.repeaters && packet.raw_len() > RECEIVED_MAX {
            return Outcome::DroppedCeiling;
        }
        if self.nodes[to.0].seen(&hash) {
            return Outcome::DroppedDedup;
        }
        self.nodes[to.0].mark_seen(hash);

        if packet.route.is_flood() {
            let spec = self.nodes[to.0].spec.clone();
            let over = hops >= spec.flood_max
                || (packet.route == Route::Flood && hops >= spec.flood_max_unscoped)
                || (packet.payload_type == PAYLOAD_TYPE_ADVERT && hops >= spec.flood_max_advert);
            if over {
                return Outcome::DroppedHopLimit;
            }
            if packet.route == Route::TransportFlood {
                // A scoped flood is admitted only by a node that carries its region, because a
                // repeater cannot verify the transport codes of a region it does not hold and
                // `allowPacketForward` refuses what it cannot resolve. To the sender every such
                // refusal is the same silence.
                if !packet.scope.is_some_and(|scope| spec.regions.contains(&scope)) {
                    return Outcome::DroppedScope;
                }
            }
            if self.path_holds_own_hash(to, packet, path_width) {
                return Outcome::DroppedLoop;
            }
            return Outcome::Delivered;
        }

        // A direct packet travels only to the next hash in its path.
        if !packet.path.is_empty() {
            let width = usize::from(path_width);
            let own = &self.nodes[to.0].hash;
            let next_hop =
                packet.path.len() >= width && packet.path[..width] == own[..width];
            if !next_hop {
                return Outcome::NotThisHop;
            }
        }
        Outcome::Delivered
    }

    /// Whether this node's own hash already appears in a flood's path, which is the firmware's
    /// minimal loop detection.
    ///
    /// The firmware's `LOOP_DETECT_MINIMAL` checks a bounded window from the end (`max_loop_minimal`)
    /// and this checks the whole path, which is strictly stricter: a loop this harness misses is a
    /// loop the firmware would also miss, so the harness cannot manufacture a forwarding storm.
    fn path_holds_own_hash(&self, node: NodeId, packet: &Packet, path_width: u8) -> bool {
        let width = usize::from(path_width);
        if width == 0 {
            return false;
        }
        let own = &self.nodes[node.0].hash[..width];
        packet
            .path
            .chunks_exact(width)
            .any(|hop| hop == own)
    }

    /// Schedules a repeater's retransmission, with the firmware's own forward delay.
    fn forward(&mut self, node: NodeId, packet: Packet, flood: bool) {
        let spec = self.nodes[node.0].spec.clone();
        let airtime = self.airtime_of(packet.raw_len());
        let delay = match spec.forward_delay_ms {
            Some(pinned) => f64::from(pinned),
            None => {
                let factor = if flood {
                    f64::from(spec.flood_tx_delay_factor)
                } else {
                    f64::from(spec.direct_tx_delay_factor)
                };
                // `getRetransmitDelay`: `nextInt(0, 5 * airtime * factor + 1)`, an integer number of
                // milliseconds drawn uniformly from a window that grows with the packet.
                let span = (5.0 * airtime * factor).max(0.0);
                let upper = span.round().max(1.0) as u32;
                f64::from(self.rng.below(upper.saturating_add(1)))
            }
        };
        let start = self.now_ms.max(self.nodes[node.0].free_at())
            .saturating_add(delay.max(0.0).round() as u64);
        let end = start.saturating_add(airtime.round().max(1.0) as u64);
        self.nodes[node.0].transmitting.push((start, end));

        let path_width = spec.path_width;
        for (to, snr_db) in self.neighbours(node) {
            self.seq = self.seq.wrapping_add(1);
            self.queue.push(Reverse(Queued {
                at_ms: end,
                seq: self.seq,
                arrival: Arrival {
                    at_ms: end,
                    from: node,
                    to,
                    packet: packet.clone(),
                    snr_db,
                    path_width,
                },
            }));
        }
    }

    /// The path a flood builds travelling through `hops`, in that order, at each node's own width.
    ///
    /// This is what a flood's arrival path looks like from the receiving end, and reversing it is
    /// the route home — so a test that answers a flood with a direct packet builds the answer's path
    /// from this.
    #[must_use]
    pub fn path_through(&self, hops: &[NodeId]) -> Vec<u8> {
        let mut out = Vec::new();
        for hop in hops {
            let spec = &self.nodes[hop.0].spec;
            let width = usize::from(spec.path_width);
            out.extend_from_slice(&self.nodes[hop.0].hash[..width]);
        }
        out
    }
}

#[cfg(test)]
mod tests;
