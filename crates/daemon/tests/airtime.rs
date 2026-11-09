//! How much air a session spends, live mode against batched mode, measured rather than guessed.
//!
//! **Run it explicitly**: `cargo test -p ldmux-daemon --test airtime -- --ignored --nocapture`.
//! It is a measurement, not a gate, and it drives a real pane.
//!
//! What is measured, and why each part matters:
//!
//! - **Records and payload bytes**, taken from the bridge's own link. The radio's budget is counted
//!   in records (`recordsLeftThisHour`), and every record carries a fixed cost the payload does
//!   not: a frame header on the serial side, a packet header and a path byte on the air.
//! - **Air time** from `time_on_air_ms`, at both tuples this project uses: the bench preset
//!   (910.525 MHz, 500 kHz, SF7, CR5) and the US public one (62.5 kHz, SF7, CR5). Air time is the
//!   unit a duty-cycle limit is written in, and the unit the session's own retransmission timers
//!   are scaled by.
//!
//! The workload is a person typing a command and reading the answer: a 40-character line, then a
//! command that prints 30 lines. Live mode sends one `Input` record per keystroke and the host
//! answers each with an echo; batched mode sends the whole line in one `Input` and the shell echoes
//! it once. Records, not bytes, are what that difference is made of.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::rig::Rig;
use ldmux_companion::codec::time_on_air_ms;
use ldmux_daemon::mesh_bridge::{Bridge, DaemonHost, Link};
use ldmux_protocol::mesh_term::{Message, VERSION};

/// One record on the air is the payload plus the packet's header byte and its path byte.
const AIR_OVERHEAD: usize = 2;

/// One acknowledgement on the air: the ARQ header, no application bytes.
const ACK_BYTES: usize = 12;

#[derive(Default)]
struct Wire {
    inbound: Vec<(u8, Vec<u8>)>,
    sent: Vec<(u8, Vec<u8>)>,
}

#[derive(Clone, Default)]
struct CountingLink(Arc<Mutex<Wire>>);

impl Link for CountingLink {
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
    /// The public preset's own pace, so the bridge's synced-echo deferral is the real one:
    /// one full record on the air is 602.6 ms at 62.5 kHz/SF7/CR5.
    fn airtime_ms(&self) -> Option<u32> {
        Some(603)
    }
}

impl CountingLink {
    fn phone_says(&self, m: &Message) {
        let bytes = m.encode().expect("encode");
        self.0.lock().unwrap().inbound.push((1, bytes));
    }

    /// Everything the host has said since the last call, and its cost.
    fn take(&self) -> Spend {
        let sent = std::mem::take(&mut self.0.lock().unwrap().sent);
        let mut s = Spend::default();
        for (_, bytes) in sent {
            s.add(&bytes);
        }
        s
    }
}

/// A session's cost, in the two units that matter.
#[derive(Default, Debug, Clone, Copy)]
struct Spend {
    records: usize,
    payload: usize,
}

impl Spend {
    fn add(&mut self, bytes: &[u8]) {
        self.records += 1;
        self.payload += bytes.len();
    }

    /// The acknowledgements this traffic needs.
    ///
    /// **A record is not free because it is small.** The session acknowledges data in the other
    /// direction, and an ack is a record of its own (`ARQ_HEADER + 0` application bytes) unless it
    /// rides on outgoing data. Counting data records alone under-reported both modes and
    /// under-reported the live one most, because it has the most records.
    fn acks(&self) -> Self {
        Self {
            records: self.records,
            payload: self.records * ACK_BYTES,
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            records: self.records + other.records,
            payload: self.payload + other.payload,
        }
    }

    /// Air time if every record carried the average payload: a lower bound, because a record's air
    /// time is convex in its length and the average is not the shape.
    fn airtime_ms(&self, bw_khz: f64, sf: u8, cr: u8) -> f64 {
        if self.records == 0 {
            return 0.0;
        }
        let each = self.payload / self.records + AIR_OVERHEAD;
        time_on_air_ms(bw_khz, sf, cr, each) * self.records as f64
    }
}

const BENCH: (f64, u8, u8) = (500.0, 7, 5);
const US_PUBLIC: (f64, u8, u8) = (62.5, 7, 5);

/// Print one line and return the US-public air time **with** acks.
///
/// Both forms are printed: acks may ride on outgoing data (`ack_delay_ms` lets one wait for it), so
/// the data-only figure is the optimistic end and the acked one the pessimistic end. A conclusion
/// that holds at both ends is a conclusion about the modes, not about the model.
fn report(name: &str, s: Spend) -> f64 {
    let raw = s.airtime_ms(US_PUBLIC.0, US_PUBLIC.1, US_PUBLIC.2);
    let acked = s.plus(s.acks()).airtime_ms(US_PUBLIC.0, US_PUBLIC.1, US_PUBLIC.2);
    println!(
        "{name:30} records {:>4}   payload {:>6} B   bench {:>8.2} ms   us-public {:>9.2} ms ({acked:.2} with acks)",
        s.records,
        s.payload,
        s.airtime_ms(BENCH.0, BENCH.1, BENCH.2),
        raw,
    );
    acked
}

/// Attach the bridge to the rig's shell, taking it over from the rig's own WebSocket viewer.
async fn bridge_on(rig: &Rig) -> (Bridge<DaemonHost, CountingLink>, CountingLink) {
    let link = CountingLink::default();
    let mut bridge = Bridge::new(DaemonHost(rig.arc.clone()), link.clone());
    link.phone_says(&Message::Attach {
        version: VERSION,
        selector: rig.session.clone(),
    });
    bridge.tick().await;
    if !bridge.attached(1) {
        link.phone_says(&Message::Takeover {
            selector: rig.session.clone(),
        });
        bridge.tick().await;
    }
    assert!(bridge.attached(1), "the bridge attached the rig's shell");
    (bridge, link)
}

async fn settle(bridge: &mut Bridge<DaemonHost, CountingLink>, ms: u64) {
    let rounds = (ms / 25).max(1);
    for _ in 0..rounds {
        bridge.tick().await;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bridge.tick().await;
}

/// Push [script] through the bridge and collect what it cost on both directions, acks included.
///
/// [per_record] ticks the bridge between messages and lets the shell answer, which is what live
/// mode does with every keystroke. Without it the whole script is one record, which is batched mode.
///
/// The wait at the end is "until the link is quiet", not a fixed sleep: a settle that is too short
/// moves a screen from one measurement into the next, and one that is too long costs only time.
async fn measure(
    bridge: &mut Bridge<DaemonHost, CountingLink>,
    link: &CountingLink,
    script: &[Message],
    per_record: bool,
) -> (Spend, Spend) {
    let mut up = Spend::default();
    for m in script {
        up.add(&m.encode().expect("encode"));
        link.phone_says(m);
        if per_record {
            bridge.tick().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    if !per_record {
        bridge.tick().await;
    }
    let mut down = Spend::default();
    let mut quiet = 0;
    // **Quiet means "past the link's own pace".** 300 ms was shorter than one record on the public
    // preset, and shorter than the bridge's synced-echo deferral (one airtime), so a batch command
    // that was merely waiting its turn counted as silence and the frame landed in the next
    // measurement — or in none.
    let quiet_after = 12 + i32::try_from(link.airtime_ms().unwrap_or(0) / 25).unwrap_or(0);
    for _ in 0..120 {
        bridge.tick().await;
        let got = link.take();
        if got.records == 0 {
            quiet += 1;
            if quiet >= quiet_after {
                break;
            }
        } else {
            quiet = 0;
            down = down.plus(got);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    (up.plus(up.acks()), down.plus(down.acks()))
}

/// One keystroke per record, the way live mode sends them.
fn typed(text: &str) -> Vec<Message> {
    text.chars()
        .map(|c| Message::Input(c.to_string().into_bytes()))
        .collect()
}

#[tokio::test]
#[ignore = "measurement, not a gate: run with --ignored --nocapture"]
async fn live_against_batched_mode() {
    let rig = Rig::start().await;
    let (mut bridge, link) = bridge_on(&rig).await;
    settle(&mut bridge, 500).await;
    let _ = link.take();

    // 40 characters: an ordinary command line, typed by hand.
    let line = "git log --oneline --graph --decorate -20\r";

    let (live_up, live_down) = measure(&mut bridge, &link, &typed(line), true).await;

    // Erase the line so the batched run starts from the same prompt.
    link.phone_says(&Message::Input(vec![0x15]));
    bridge.tick().await;
    settle(&mut bridge, 300).await;
    let _ = link.take();

    // **The batch path as the phone sends it**: release the hold, then the line, then hold again.
    let batch = [
        Message::Hold { on: false },
        Message::Input(line.as_bytes().to_vec()),
    ];
    let (batch_up, batch_down) = measure(&mut bridge, &link, &batch, false).await;

    // The command's answer is the same work in either mode, so it is measured apart.
    let answer_script = [
        Message::Hold { on: false },
        Message::Input(b"seq 1 30\r".to_vec()),
    ];
    let (_, answer) = measure(&mut bridge, &link, &answer_script, false).await;

    // A whole agent turn: eight typed commands, each answered.
    let mut turn_script: Vec<Message> = Vec::new();
    for i in 0..8 {
        turn_script.extend(typed(&format!("echo run-{i} && seq 1 30\r")));
    }
    let (turn_up, turn_down) = measure(&mut bridge, &link, &turn_script, true).await;

    // The same turn, batched: one record per command, each read before the next, which is what a
    // person does with a sync button. Sent as one burst it would overrun the input credit and the
    // answers would be measured in the wrong window.
    let mut bturn_up = Spend::default();
    let mut bturn_down = Spend::default();
    for i in 0..8 {
        let one = [
            Message::Hold { on: false },
            Message::Input(format!("echo run-{i} && seq 1 30\r").into_bytes()),
        ];
        let (u, d) = measure(&mut bridge, &link, &one, false).await;
        bturn_up = bturn_up.plus(u);
        bturn_down = bturn_down.plus(d);
    }

    println!("\n=== a 40-character command, typed by hand (acks counted both ways) ===");
    let l_up = report("live:  phone to host", live_up);
    let l_down = report("live:  host to phone (echo)", live_down);
    let b_up = report("batch: phone to host", batch_up);
    let b_down = report("batch: host to phone (echo)", batch_down);
    println!("\n=== the answer to the command (the same in either mode) ===");
    let out = report("output: host to phone", answer);
    println!("\n=== a whole agent turn: eight typed commands, each answered ===");
    let t_up = report("turn:  phone to host (typed)", turn_up);
    let t_down = report("turn:  host to phone (typed)", turn_down);
    let bt_up = report("turn:  phone to host (batched)", bturn_up);
    let bt_down = report("turn:  host to phone (batched)", bturn_down);

    let live = l_up + l_down;
    let batch_total = b_up + b_down;
    println!("\n=== totals, us-public tuple (62.5 kHz SF7 CR5) ===");
    println!(
        "live  typing {:9.2} ms over {} records",
        live,
        live_up.records + live_down.records
    );
    println!(
        "batch typing {:9.2} ms over {} records",
        batch_total,
        batch_up.records + batch_down.records
    );
    if live > 0.0 {
        println!(
            "typing: batched spends {:.1}% less air ({:.2}x cheaper)",
            100.0 * (live - batch_total) / live,
            live / batch_total.max(0.001)
        );
        println!(
            "whole command (typing + answer): batched is {:.1}% of live ({:.2} ms against {:.2} ms)",
            100.0 * (batch_total + out) / (live + out),
            batch_total + out,
            live + out
        );
        println!(
            "one agent turn: typed {:.2} ms, batched {:.2} ms ({:.1}% less air)",
            t_up + t_down,
            bt_up + bt_down,
            100.0 * ((t_up + t_down) - (bt_up + bt_down)) / (t_up + t_down).max(0.001),
        );
        println!(
            "a 1% allowance (36 s) is {:.2} typed turns an hour, or {:.2} batched",
            36_000.0 / (t_up + t_down).max(0.001),
            36_000.0 / (bt_up + bt_down).max(0.001),
        );
    }
}
