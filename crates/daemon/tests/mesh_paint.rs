//! A first attach over the mesh profile, end to end: a real keeper, a real shell, the shipped
//! bridge and the phone's own `Mirror`.
//!
//! **The assertion is the one a reader makes**: after one attach round trip the phone holds a pane
//! with the shell's prompt on it, at the phone's grid, without a desk-sized frame and without a
//! second repaint. Measured by hand on the device 2026-11-09 a first paint took 15 s because the
//! attach carried no grid and the host drew the desk's 62x82 pane first; this pins the shape of the
//! answer so that regression cannot come back quietly.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::rig::Rig;
use ldmux_daemon::mesh_bridge::{Bridge, DaemonHost, Link};
use ldmux_protocol::mesh_term::{Decoder, Message, Mirror, VERSION};

#[derive(Default)]
struct Wire {
    inbound: Vec<(u8, Vec<u8>)>,
    sent: Vec<(u8, Vec<u8>)>,
}

#[derive(Clone, Default)]
struct FakeLink(Arc<Mutex<Wire>>);

impl Link for FakeLink {
    fn established(&self) -> bool {
        true
    }
    fn generation(&self) -> u32 {
        1
    }
    fn pending(&self) -> usize {
        0
    }
    fn send(&self, shell: u8, bytes: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().sent.push((shell, bytes.to_vec()));
        Ok(())
    }
    fn take_inbound(&self) -> Vec<(u8, Vec<u8>)> {
        std::mem::take(&mut self.0.lock().unwrap().inbound)
    }
}

impl FakeLink {
    fn phone_says(&self, m: &Message) {
        let bytes = m.encode().expect("encode");
        self.0.lock().unwrap().inbound.push((1, bytes));
    }
}

/// What the phone holds after everything the host sent, and how many records it took.
fn phone_view(link: &FakeLink) -> (Mirror, usize) {
    let mut d = Decoder::new();
    let mut m = Mirror::new();
    let sent = link.0.lock().unwrap().sent.clone();
    for (_, bytes) in &sent {
        for item in d.push(bytes).unwrap() {
            m.apply(&item.unwrap());
        }
    }
    (m, sent.len())
}

#[tokio::test]
async fn a_first_attach_paints_the_pane_at_the_phones_grid() {
    let rig = Rig::start().await;
    let link = FakeLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    // **The attach the phone actually sends: the shell, and the grid it measured.** The grid
    // travelling with the attach is what lets the host fit the pane before its first frame.
    link.phone_says(&Message::Attach {
        version: VERSION,
        selector: rig.session.clone(),
    });
    link.phone_says(&Message::Resize { cols: 46, rows: 17 });
    bridge.tick().await;
    if !bridge.attached(1) {
        link.phone_says(&Message::Takeover {
            selector: rig.session.clone(),
        });
        bridge.tick().await;
    }
    assert!(bridge.attached(1), "the bridge attached the rig's shell");

    // Give the shell time to draw its prompt, then read the phone's model.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut view;
    loop {
        bridge.tick().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        view = phone_view(&link);
        if view.0.lines.iter().any(|l| !l.trim().is_empty()) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "nothing painted: {:?}", view.0.lines);
    }

    let (mirror, records) = view;
    assert_eq!(
        mirror.profile.map(|p| (p.0, p.1)),
        Some((46, 17)),
        "the pane the phone holds is the phone's grid, not the desk's"
    );
    assert!(
        mirror.lines.iter().any(|l| !l.trim().is_empty()),
        "the pane has the shell's text on it: {:?}",
        mirror.lines
    );
    assert!(
        mirror.lines.iter().all(|l| l.chars().count() <= 46),
        "and no line is wider than the grid the phone asked for: {:?}",
        mirror.lines
    );
    // Profile + marks + one framed screen + acks is the honest shape. A desk-sized frame or a
    // second repaint would be far more than this.
    assert!(
        records <= 6,
        "a first paint in {records} records: {:?}",
        mirror.lines
    );
}
