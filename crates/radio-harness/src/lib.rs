//! The pieces the radio tests share: a port that puts records on a simulated mesh, a fake companion
//! radio, and the chain that joins two ends.
//!
//! **Why a crate and not a `tests/support` module.** `crates/session/tests/client.rs` proves the
//! phone engine against a host endpoint, and `crates/daemon/src/mesh.rs` proves the daemon loop
//! against a fake host — but nothing ran the two against each other, which is exactly the join
//! `AR-15` showed can rot while both halves stay green. A module under one crate's `tests/` cannot
//! be reached from the other, so the harness lives here and both depend on it.
//!
//! It is a **dev-dependency** of `ldmux-session` and `ldmux-daemon`: a test instrument must not
//! reach a shipping binary. `ldmux-mesh`'s `sim` feature stays where it is — the simulator models
//! the wire, this crate drives it.

#![allow(dead_code)]

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use ldmux_mesh::carrier::{Carrier, Event};
use ldmux_mesh::error::MeshError;
use ldmux_mesh::route::Carriage;
use ldmux_mesh::sim::{Delivery, NodeId, NodeSpec, VirtualMesh};
use ldmux_session::ArrivalInfo;

/// One full record's airtime at the public preset, which is what the pacing is derived from.
pub const AIRTIME_MS: f64 = 602.6;

/// The wall clock the handshake timestamps are drawn from. Only its differences matter.
pub const WALL_BASE_MS: u64 = 1_700_000_000_000;

/// Run a future that never really waits.
///
/// The carriers here resolve on their first poll, so a `Pending` is a bug rather than a wait. There is
/// deliberately no executor dependency: a test that needed one would be testing the executor.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("a carrier in this test never waits, so this is a bug"),
    }
}

/// A carrier that records what it was told to send, so the harness can put it on the air.
///
/// Keeping transmission out of the carrier is what lets one `VirtualMesh` serve both ends: the
/// simulator owns the air, and a port owns only the intent.
pub struct Port {
    pub carriage: Carriage,
    pub out: Vec<Vec<u8>>,
}

impl Port {
    pub fn new() -> Self {
        Self {
            carriage: Carriage::Flood,
            out: Vec::new(),
        }
    }
}

impl Default for Port {
    fn default() -> Self {
        Self::new()
    }
}

impl Carrier for Port {
    async fn send_record(&mut self, _seq: u8, record: &[u8]) -> Result<(), MeshError> {
        self.out.push(record.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        // The harness reads the air itself; nothing is ever pushed at a port.
        Err(MeshError::Closed)
    }
}

impl ldmux_session::CarriageSink for Port {
    async fn set_carriage(&mut self, carriage: &Carriage) -> Result<(), MeshError> {
        self.carriage = carriage.clone();
        Ok(())
    }
}

/// A port whose output the harness can drain **while the loop owns it**.
///
/// `Port` keeps its records in a plain `Vec`, which is right for a test that owns the carrier. The
/// daemon's loop owns its carrier, so the full-stack gate needs the records to be reachable from
/// outside: the same intent, shared.
pub struct SharedPort {
    pub carriage: Carriage,
    pub out: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl SharedPort {
    pub fn new() -> Self {
        Self {
            carriage: Carriage::Flood,
            out: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Take everything the loop handed the carrier since the last call.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        let mut guard = self.out.lock().expect("the port's output is not poisoned");
        std::mem::take(&mut *guard)
    }
}

impl Default for SharedPort {
    fn default() -> Self {
        Self::new()
    }
}

impl Carrier for SharedPort {
    async fn send_record(&mut self, _seq: u8, record: &[u8]) -> Result<(), MeshError> {
        self.out
            .lock()
            .expect("the port's output is not poisoned")
            .push(record.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        // The harness reads the air itself; nothing is ever pushed at a port.
        Err(MeshError::Closed)
    }
}

impl ldmux_session::CarriageSink for SharedPort {
    async fn set_carriage(&mut self, carriage: &Carriage) -> Result<(), MeshError> {
        self.carriage = carriage.clone();
        Ok(())
    }
}

/// How a delivery reached its node, at a one-byte path width.
pub fn info_of(delivery: &Delivery) -> ArrivalInfo {
    info_of_width(delivery, 1)
}

/// How a delivery reached its node, at `path_width` bytes per hash.
///
/// The width is not decoration: a flood's `path` is `hops × width` bytes, and the endpoint divides
/// by it to count hops. A harness that assumed one byte would read a two-byte path as twice the
/// hops it crossed.
pub fn info_of_width(delivery: &Delivery, path_width: u8) -> ArrivalInfo {
    if delivery.flood {
        ArrivalInfo::flood(delivery.path.clone(), path_width, delivery.snr_db)
    } else if delivery.hops == 0 {
        ArrivalInfo::direct(delivery.snr_db)
    } else {
        ArrivalInfo::in_transit(delivery.hops, delivery.snr_db)
    }
}

/// A mesh with a phone at one end, `repeaters` in the middle, and a host at the other.
///
/// Returns the mesh, the phone's node, the host's node, the host's static public key (what the phone
/// addresses) and the phone's static keypair (what the host pins).
pub fn chain(
    seed: u64,
    repeaters: usize,
    loss: f64,
    path_width: u8,
) -> (VirtualMesh, NodeId, NodeId) {
    let mut mesh = VirtualMesh::new(seed);
    let phone = mesh.add_node(NodeSpec::companion().with_path_width(path_width));
    let mut last = phone;
    for _ in 0..repeaters {
        let id = mesh.add_node(NodeSpec::repeater().with_path_width(path_width));
        mesh.link(last, id, 8.0);
        last = id;
    }
    let host = mesh.add_node(NodeSpec::companion().with_path_width(path_width));
    mesh.link(last, host, 8.0);
    mesh.set_loss(loss);
    (mesh, phone, host)
}

/// A companion radio that answers the way a stock one does, for tests that need the real window.
///
/// The engine now owns the frame: a record is a `CMD_SEND_RAW_PACKET` payload and every question is
/// a command, so a test that only read records out of a carrier would never see the window move. This
/// stands in for the hardware: it answers each command with the shape the firmware answers it in, and
/// hands back the packet a send carried so the harness can put it on the mesh.
pub struct FakeRadio {
    /// Packets a `CMD_SEND_RAW_PACKET` asked the radio to transmit, in order, with the route the
    /// packet's own header names.
    ///
    /// **The carriage is in the packet, not beside it.** The endpoint chose it before the record was
    /// sealed, and the packet builder wrote it into the route bits; a harness that kept a separate
    /// "current carriage" would be remembering something the wire already says.
    pub transmissions: Vec<(Carriage, Vec<u8>)>,
    /// The tuple it reports from `SELF_INFO`.
    pub freq_khz: u32,
    pub bw_hz: u32,
    pub sf: u8,
    pub cr: u8,
    /// How many commands it has answered, so a test can say the window moved.
    pub answered: usize,
}

impl FakeRadio {
    /// A radio on the public preset with a plausible battery and a quiet air.
    pub fn new() -> Self {
        Self {
            transmissions: Vec::new(),
            freq_khz: 910_525,
            bw_hz: 62_500,
            sf: 7,
            cr: 5,
            answered: 0,
        }
    }

    /// Answer one frame the engine produced: the answers to feed back, and any packet to transmit.
    ///
    /// `SEND_RAW_PACKET` is the only command whose answer is not a report: it carries a packet the
    /// radio transmits, so the packet comes back here and the harness puts it on the air.
    pub fn answer(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        self.answered += 1;
        match frame.first().copied() {
            // `CMD_SEND_RAW_PACKET`: `[0x41][priority][packet]`, answered `OK`.
            Some(0x41) => {
                if let Some((carriage, record)) = frame.get(2..).and_then(split_packet) {
                    self.transmissions.push((carriage, record));
                }
                vec![vec![0x00]]
            }
            Some(0x01) => vec![self.self_info()],
            Some(0x16) => vec![self.device_info()],
            Some(0x14) => vec![self.battery()],
            Some(0x38) => vec![self.radio_stats()],
            // The setters, and anything this fake does not model: a plain `OK`.
            Some(_) => vec![vec![0x00]],
            None => Vec::new(),
        }
    }

    fn self_info(&self) -> Vec<u8> {
        let mut out = vec![0x05, 1, 22, 22];
        out.extend_from_slice(&[0x11; 32]);
        out.extend_from_slice(&0i32.to_le_bytes()); // lat
        out.extend_from_slice(&0i32.to_le_bytes()); // lon
        out.extend_from_slice(&[0, 0, 0, 0]); // multi-acks, loc policy, telemetry, manual
        out.extend_from_slice(&self.freq_khz.to_le_bytes());
        out.extend_from_slice(&self.bw_hz.to_le_bytes());
        out.push(self.sf);
        out.push(self.cr);
        out.extend_from_slice(b"test-radio");
        out
    }

    fn device_info(&self) -> Vec<u8> {
        let mut out = vec![0x0D, 13, 175, 40];
        out.extend_from_slice(&0u32.to_le_bytes());
        let push_text = |out: &mut Vec<u8>, text: &[u8], width: usize| {
            let mut field = vec![0u8; width];
            field[..text.len().min(width)].copy_from_slice(&text[..text.len().min(width)]);
            out.extend_from_slice(&field);
        };
        push_text(&mut out, b"build", 12);
        push_text(&mut out, b"manufacturer", 40);
        push_text(&mut out, b"1.17.1", 20);
        out.push(1); // repeat enabled
        out.push(0); // path hash mode 0 -> one byte
        out
    }

    fn battery(&self) -> Vec<u8> {
        let mut out = vec![0x0C];
        out.extend_from_slice(&4_050u16.to_le_bytes());
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&256u32.to_le_bytes());
        out
    }

    fn radio_stats(&self) -> Vec<u8> {
        let mut out = vec![0x18, 1];
        out.extend_from_slice(&(-120i16).to_le_bytes());
        out.push((-90i8) as u8);
        out.push((-12i8) as u8); // -3 dB in quarters
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&250u32.to_le_bytes());
        out
    }
}

impl Default for FakeRadio {
    fn default() -> Self {
        Self::new()
    }
}

/// A packet, split into the route its header names and the record inside it.
///
/// **The carriage is in the packet, not beside it.** The endpoint chose it before the record was
/// sealed and the builder wrote it into the route bits, so a harness that kept a separate "current
/// carriage" would be remembering something the wire already says. The record's offset differs by
/// carrier: a flood carries its travel path, its two hashes, the `LDMR` magic and a four-byte nonce
/// in front of it, and a direct packet carries only its path.
fn split_packet(packet: &[u8]) -> Option<(Carriage, Vec<u8>)> {
    let &header = packet.first()?;
    let encoded = packet.get(1).copied()?;
    let width = usize::from((encoded >> 6) + 1);
    let hops = usize::from(encoded & 0x3F);
    let path_len = hops * width;
    let path = packet.get(2..2 + path_len)?.to_vec();
    match header & 0x03 {
        // `ROUTE_TYPE_DIRECT`: the record follows the path.
        2 => {
            let record = packet.get(2 + path_len..)?.to_vec();
            Some((Carriage::Direct { path }, record))
        }
        // A flood: the path, the destination and source hashes, the magic and the nonce.
        _ => {
            let start = 2 + path_len + ldmux_mesh::budget::FLOOD_WRAPPER;
            let record = packet.get(start..)?.to_vec();
            Some((Carriage::Flood, record))
        }
    }
}
