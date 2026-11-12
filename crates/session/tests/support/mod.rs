//! Pieces the radio tests share: a port that puts records on a simulated mesh, and the loop that
//! joins two ends.
//!
//! This exists so a test can say what it is about instead of rebuilding a radio. `endpoint.rs` has
//! its own copy of the older half of this (it drives two raw endpoints), and folding that in is a
//! follow-up rather than a risk taken in a green suite; what is here is the part a *client* test
//! needs, named for what it is.
#![allow(dead_code)]

use std::future::Future;
use std::pin::pin;
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

/// How a delivery reached its node.
pub fn info_of(delivery: &Delivery) -> ArrivalInfo {
    if delivery.flood {
        ArrivalInfo::flood(delivery.path.clone(), 1, delivery.snr_db)
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
