//! The host's own radio, on a serial port, as a carrier.
//!
//! This is the other side of `crates/mesh`'s [`Carrier`] seam. The gateway carrier
//! dials a separate process that owns the radio; this one owns the radio itself
//! through `crates/companion`, so the host needs no gateway, no socket, no token
//! and no second language to carry a byte.
//!
//! The companion protocol is synchronous and the daemon is tokio, so the radio
//! lives on one dedicated thread and talks to the loop over channels:
//!
//! ```text
//! session loop ── Command::Record(seq, bytes) ──▶ radio thread ──▶ serial
//! session loop ◀── Event::Recv / SendResult / Status ── radio thread ◀── serial
//! ```
//!
//! Two things are deliberately borrowed from the gateway's behaviour so the loop
//! above cannot tell the carriers apart:
//!
//! * a `Status` follows every send, carrying the budget that is left, because the
//!   loop treats an unknown budget as zero and would otherwise never send again;
//! * a refused send is a `SendResult`, not a carrier error, because the retry
//!   ladder has to give the slot back rather than tear the link down.
//!
//! The budget is computed from the preset the radio reports rather than assumed:
//! `36 s/hour / airtime(172 bytes)`, which is the 1 % self-imposed policy in
//! `docs/RELEASE-AUDIT.md` appendix B, not a limit the firmware enforces. The
//! firmware has its own governor; this number only keeps LDMUX's meter honest.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ldmux_companion::codec::{
    parse_advert_path, parse_packet, parse_route_reply, parse_route_request,
};
use ldmux_companion::{
    ByteLink, Companion, FloodScope, KNOWN_SCOPE_TAGS, RouteRequest, SerialLink, SessionError,
    identify_scope,
};
use ldmux_mesh::carrier::{Bound, Budget, Carrier, Event, Signal};
use ldmux_mesh::constants::{
    RECORD_MAX, RECORD_RECEIVED_MAX as RECEIVED_RECORD_MAX, RadioState, SendOutcome,
    UNKNOWN_BATTERY, UNKNOWN_RSSI,
};
use ldmux_mesh::messages::ScopeInfo;
use ldmux_mesh::error::MeshError;
use ldmux_mesh::messages::{SendResult, Status};
use tokio::sync::mpsc as async_mpsc;

/// How long the radio thread waits for a command before checking the air.
const COMMAND_WAIT: Duration = Duration::from_millis(10);

/// How often the radio's countdown is republished while the link is idle.
///
/// The session waits on this number: at zero it sends nothing, so nothing is transmitted, so a
/// status tied to transmission would never refresh. Half a second is invisible to a person and
/// costs no airtime.
const STATUS_REFRESH: Duration = Duration::from_millis(500);
/// How often the radio's own settings are re-read while the link is quiet.
///
/// **Another app can retune this radio and nothing announces it.** The research is explicit: the
/// companion protocol has no client identity and no notification of a parameter change, so a radio
/// an operator retuned from the phone (or from a CLI session) keeps our snapshot lying about the
/// frequency we are on. `APP_START` re-reads `SELF_INFO`, which carries the tuple, the power and the
/// configured width; it is a serial command with **no airtime**, so a minute costs nothing.
const PARAM_WATCH: Duration = Duration::from_secs(60);

/// How often the radio is asked what it measured on the air.
const STATS_WATCH: Duration = Duration::from_secs(20);

/// The firmware's own ceiling for a path, in bytes (`MAX_PATH_SIZE`, `MeshCore.h`).
const MAX_PATH_SIZE: usize = 64;

/// How often this radio floods an advert, or `None` when adverts are off.
///
/// **An advert is no longer how a peer learns a route.** That job belongs to the on-demand route
/// request (`ROUTE_FLOOD_HEADER` in `ldmux_companion::codec`), which is answered in one round trip
/// and crosses the mesh's real distance; an advert is a *broadcast* that every repeater in range
/// repeats, and a repeater drops it after `flood_max_advert` hops — **8 by default**
/// (`examples/simple_repeater/MyMesh.cpp:907`) — so an advert-only design cannot reach a peer nine
/// hops away at all.
///
/// **So this defaults to off, which is the firmware's own answer for a companion.** MeshCore's
/// repeater default is a 47-hour flood advert, its companion default is *never*, and the regional
/// guides push repeaters further out still (Switzerland 49 hours) — because an advert is airtime
/// every node in range pays for, and a repeater drops it after eight hops anyway. ldmux advertised
/// every five minutes, 564 times the community's cadence, and no mechanism now depends on it:
/// discovery is a request, and the receive-log advert event keeps the old path working for an
/// operator who does advertise.
///
/// `LDMUX_MESH_ADVERT_SECS=<seconds>` turns it on (3600 keeps the node visible in other operators'
/// contact lists for about 0.07 % of a ten-percent allowance at 62.5 kHz).
fn advert_interval() -> Option<Duration> {
    let secs = std::env::var("LDMUX_MESH_ADVERT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// How long a dropped carrier waits for the radio thread before leaving it to finish.
///
/// The wait exists so the port is closed and its advisory lock released before a reconnect
/// opens the same device. It is **bounded** because the thread can be inside a write to a
/// device that stopped draining: the write itself is bounded (`companion::link::WRITE_TIMEOUT`),
/// and this wait is shorter still, so a wedged radio costs one short pause rather than a hung
/// supervisor. A thread that outlives the wait closes the port and releases the lock when it
/// does finish; the descriptor is not inherited (`O_CLOEXEC`), so nothing else keeps it.
const CLOSE_WAIT: Duration = Duration::from_millis(250);

/// One second of the hour the budget is measured over.
const HOUR: Duration = Duration::from_secs(3_600);

/// How long the peer's route may go unrefreshed before it is asked for again.
///
/// A route is only true while the repeaters that carried it are still there, and a phone that moved
/// has a different one. The read is a serial command, so it costs nothing on the air; two minutes is
/// short enough that a walk is felt as a pause rather than as a dead link.
const PEER_ROUTE_REFRESH: Duration = Duration::from_secs(120);

/// How soon to ask again while no route is known.
///
/// A phone that has just arrived announces itself, and that advert takes a moment to cross the
/// repeaters, so the first lookup legitimately answers "not heard". Fifteen seconds is short enough
/// that arriving somewhere new is felt as a pause, and the read is a serial command with no airtime
/// at all.
const PEER_ROUTE_RETRY: Duration = Duration::from_secs(15);
/// The shortest gap between two route re-reads caused by traffic.
///
/// A record from the peer is the moment the route matters and the moment it may have changed: the
/// phone opened a shell after a walk, or moved while it was idle. The re-read is **one serial
/// command and no airtime**, so it is cheap; this floor is what keeps a burst of records from
/// spending the radio's single answer window on route questions.
const ROUTE_REASK_FLOOR: Duration = Duration::from_secs(2);

/// How old a route may be before this end asks about it again, even though it has one.
///
/// The phone uses the same figure. A route is only true while the repeaters that carried it are
/// still there, and a peer that walks away leaves a well-formed path that nothing refuses — the
/// radio transmits and nobody answers.
/// How long a route beats a fresher candidate with more hops.
///
/// Sixty seconds is the window the community's own path-gating proposal settled on (accept a
/// candidate when the route is unknown, when it has fewer hops, or when the one in hand is older
/// than a minute). Two minutes was long enough for a dead one-hop route to outlive the phone that
/// had moved three repeaters away, and a shorter window costs nothing while a route is working:
/// an equal-or-shorter candidate always wins anyway.
const PEER_ROUTE_STALE: Duration = Duration::from_secs(60);

/// The same window for a route of `hops` repeaters.
///
/// **A minute is a neighbour's figure.** The window exists so a route that has stopped carrying is
/// replaced by a fresher candidate, and "stopped carrying" has to be measured against how long one
/// round trip over that route takes: 2 airtimes a hop (the firmware's own forward delay), so a
/// 28-hop route needs 34 s before anything is late, and a flat minute would call it dead while its
/// first exchange was still crossing. The base is the community's own path-gating window
/// (`max(60 s, 2 × hops × airtime)`, with 603 ms an airtime at the public tuple).
fn route_stale(hops: usize) -> Duration {
    let round_trip = Duration::from_millis(2 * 603 * hops.max(1) as u64);
    PEER_ROUTE_STALE.max(round_trip)
}

/// The least time between two **route replies** to the same peer.
///
/// A request is ~40 bytes and a reply ~40 bytes plus the path, so answering every request is cheap;
/// the floor is what stops a peer (or a spoofed source) from turning this radio into a responder
/// that repeats itself. A peer asks again on its own ladder, so one floor per five seconds is far
/// below anything a working connection needs.
const ROUTE_REPLY_FLOOR: Duration = Duration::from_secs(5);


/// The four bytes that mark a record as a **route hint** rather than a session message.
const ROUTE_HINT_MAGIC: &[u8; 4] = b"LDRH";

/// **Route events are logged always, and packets only on request.**
///
/// The field failure this change came from could not be diagnosed: the daemon knew whether the
/// peer's advert had been heard and at how many hops, and said nothing. A route exchange is a
/// handful of lines per connection, so they go to the journal unconditionally; `LDMUX_MESH_TRACE=1`
/// additionally logs every received packet, which is 1000x more lines and belongs to a bench.
fn route_log(message: &str) {
    eprintln!("ldmuxd: route {message}");
}

/// The same, at most once a minute: a failing link asks every 10 s and would otherwise write 360
/// identical lines an hour, which buries the one line that changed.
fn route_log_rare(last: &mut Option<Instant>, message: &str) {
    if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
        *last = Some(Instant::now());
        route_log(message);
    }
}

fn trace_on() -> bool {
    std::env::var_os("LDMUX_MESH_TRACE").is_some()
}

fn trace_rx(frame: &[u8]) {
    const NAMES: [&str; 16] = [
        "REQ", "RESP", "TXT", "ACK", "ADVERT", "GRP_TXT", "GRP_DATA", "ANON_REQ", "PATH",
        "TRACE", "MULTI", "CONTROL", "?12", "?13", "?14", "RAW_CUSTOM",
    ];
    if frame.first() != Some(&0x88) || frame.len() < 5 {
        return;
    }
    let packet = &frame[3..];
    let header = packet[0];
    let route = header & 0x03;
    let ty = usize::from((header >> 2) & 0x0F);
    let mut off = 1;
    if route == 0 || route == 3 {
        off += 4;
    }
    let Some(&plen) = packet.get(off) else { return };
    off += 1;
    let width = usize::from((plen >> 6) + 1);
    let hops = usize::from(plen & 0x3F);
    let body = off + hops * width;
    let mut extra = String::new();
    if ty == 4
        && let Some(key) = packet.get(body..body + 32)
    {
        let hex: String = key.iter().take(8).map(|b| format!("{b:02x}")).collect();
        extra = format!(" key={hex}");
    }
    eprintln!(
        "ldmuxd: trace rx route={route} type={} hops={hops} w={width} len={} snr={} rssi={}{extra}",
        NAMES.get(ty).copied().unwrap_or("?"),
        packet.len(),
        i8::try_from(frame[1]).unwrap_or(0) as f32 / 4.0,
        frame[2] as i8,
    );
}

/// `[LDRH][32-byte radio public key]`: the peer telling this radio which node it is.
///
/// **Why a record and not a field somewhere.** The reply route has to exist before the session
/// does — the handshake's own records need an answer — so the peer's radio identity cannot ride in
/// the session, and it cannot be inferred from an arriving record either, because a direct packet's
/// path is consumed by the repeaters that carry it. Thirty-six unencrypted bytes before the
/// handshake are what make the lookup possible. They are a routing hint and nothing more: a forged
/// one can only redirect where records are *sent*, and everything inside them is sealed by the
/// session.
fn route_hint(record: &[u8]) -> Option<[u8; 32]> {
    // Four bytes of nonce may follow the key (a retried hint is otherwise a duplicate packet the
    // radio drops), so the length is a range rather than one number.
    if !(4 + 32..=4 + 32 + 8).contains(&record.len()) || record.get(..4)? != ROUTE_HINT_MAGIC {
        return None;
    }
    record.get(4..36).and_then(|k| k.try_into().ok())
}


/// Answer one route request: the **path this end uses to reach the requester**, so the requester can
/// reverse it and hold a route of its own.
///
/// One rule serves both directions: a reply carries the sender's sending-order path and the reader
/// reverses it. Getting that backwards would produce a route that is the mirror of the truth, which
/// on a single-repeater path is indistinguishable from a dead link.
fn send_route_reply<L: ByteLink>(
    companion: &mut Companion<L>,
    req: &RouteRequest,
    own_key: &[u8; 32],
    scope: Option<&FloodScope>,
) -> bool {
    let Some(send) = PeerPath::reversed(
        usize::from(req.path.hash_size),
        usize::from(req.path.hops),
        &req.path.bytes,
    ) else {
        return false;
    };
    // **The reply echoes the request's nonce**, so answering the same question twice produces two
    // different packets: an identical one would be dropped as a duplicate by the phone's radio
    // (`Packet::calculatePacketHash`), and the phone would learn nothing. A flooded reply that the
    // phone heard a second time would otherwise be discarded as its own duplicate, and a *cannot
    // arrive* is worse than a *arrives twice* on a mesh where the first copy is what a lost hop ate.
    let Ok(packet) = ldmux_companion::build_route_reply_flood_in_scope(
        own_key,
        &req.peer_key,
        send.hash_size,
        send.hops,
        &send.bytes,
        &req.nonce,
        scope,
    ) else {
        return false;
    };
    // **Flooded, because a direct reply depends on one chain and the request did not.** The request
    // crossed repeaters as an un-scoped `REQ` flood, which travels `flood_max_unscoped` (64) hops,
    // and MeshCore's own rule for answering one is a flooded path return
    // (`ReplyRoute::REPLY_ROUTE_PATH_RETURN`). A direct reply is one packet along one chain: if a
    // single repeater on it drops the packet — measured 2026-11-09 on a 1-hop link at −11 dB SNR —
    // the phone learns nothing, waits, and asks again. This costs one transmission per repeater in
    // range, only while the phone has no route at all, and after the first answer every record is
    // direct again.
    //
    // **Two seconds, not ten.** This runs inside the push handler, before the record that arrived is
    // delivered upward, so a radio that stopped answering must not hold the loop for ten seconds.
    companion
        .send_packet_within(&packet, Duration::from_secs(2))
        .is_ok()
}

/// The route a peer's `LDRR` reply names: the path it uses towards us, reversed into ours.
fn route_reply(record: &[u8]) -> Option<PeerPath> {
    let path = parse_route_reply(record)?;
    PeerPath::reversed(
        usize::from(path.hash_size),
        usize::from(path.hops),
        &path.bytes,
    )
}

/// The scope a reply should go back under: **the request's own**, once matched to a key.
///
/// A dense mesh denies unscoped floods (`region denyf *`) and may cap them at a few hops, so an
/// unscoped answer to a scoped request never crosses the region. MeshCore's own rule is
/// `ReplyScope::REPLY_SCOPE_REQUEST`; this end can only honour it for a scope it can name, so the
/// order is: this radio's own scope when the request's code matches it, then a public region whose
/// derived key reproduces the code, then nothing (unscoped, which is what an unscoped request gets).
fn reply_scope(frame: &[u8], own: Option<&FloodScope>) -> Option<FloodScope> {
    let view = frame_packet(frame)?;
    let code = view.transport_codes?[0];
    if let Some(scope) = own
        && ldmux_companion::transport_code(&scope.key, view.payload_type, view.payload) == code
    {
        return Some(scope.clone());
    }
    identify_scope(&view, KNOWN_SCOPE_TAGS)
}

/// A flooded route request in a receive-log frame, when it is addressed to `own_key`.
///
/// The log frame is `[0x88][snr×4][rssi][packet]` (`MyMesh.cpp:286-296`), which is the same view the
/// records come from; a request is recognised by its route (flood), its payload type (`REQ`), and the
/// destination hash naming this radio.
fn frame_route_request(frame: &[u8], own_key: &[u8; 32]) -> Option<RouteRequest> {
    if frame.first() != Some(&ldmux_companion::codes::LOG_RX_DATA) {
        return None;
    }
    parse_route_request(frame.get(3..)?, own_key)
}

/// Whether a candidate route should replace the one in hand.
///
/// **Shortest wins; stale is no route at all.** A flood is re-forwarded by every repeater in range,
/// so this radio sees the same request arrive again by longer paths — measured on the bench
/// 2026-11-08: a request from a phone in the same room logged at 0, then 1, then 4 hops. Each copy
/// is a route that works, and taking the last one made every reply take a four-repeater detour.
/// A route older than [`PEER_ROUTE_STALE`] is replaced by anything, so a phone that moved is not
/// pinned to the old chain by a shorter-but-dead one.
fn prefer_route(current: Option<&PeerPath>, heard: Option<Instant>, candidate: &PeerPath) -> bool {
    let Some(current) = current else {
        return true;
    };
    if heard.is_none_or(|t| t.elapsed() >= route_stale(usize::from(current.hops))) {
        return true;
    }
    candidate.hops <= current.hops
}

/// The largest record this link can carry **now**, given the path it is using.
///
/// The sender's ceiling is `RECORD_MAX - path` — a raw send is
/// `[0x41][priority][header][path byte][path][record]` inside the firmware's 176-byte frame — and
/// the receiver's is [`RECORD_RECEIVED_MAX`], because a radio does not push a frame it cannot fit in
/// that same 176 bytes. `min` of the two is what a full record may be. A **fixed** 172 was the old
/// answer, and it was refused by ldmux's own packet builder at the first path byte.
fn record_limit(path: Option<&PeerPath>) -> u8 {
    let used = path.map_or(0, |p| p.bytes.len());
    let max = RECORD_MAX
        .saturating_sub(used)
        .min(RECEIVED_RECORD_MAX);
    u8::try_from(max).unwrap_or(1).max(1)
}

/// Ask the radio for the route to `peer_radio`, and keep it when the answer has one.
///
/// **This is a serial command, not a transmission**, so it can be asked as often as is useful; what
/// it must not do is ask while another command's answer window is open, which is why every caller is
/// on the idle path. A `Rejected` answer means the radio has not heard that node's advert yet — the
/// phone may be announcing right now and the flood may still be crossing the repeaters — so the
/// caller retries sooner than the calm refresh.
fn refresh_peer_route<L: ByteLink>(
    companion: &mut Companion<L>,
    peer_radio: Option<&[u8; 32]>,
    peer_path: &mut Option<PeerPath>,
    peer_path_heard: &mut Option<Instant>,
) -> bool {
    let Some(key) = peer_radio else {
        return false;
    };
    let before = peer_path.clone();
    match companion.get_advert_path(key) {
        Ok(path) => {
            let age = unix_now().saturating_sub(u64::from(path.received_at));
            // The radio stamps the entry when the advert was **heard**, and the clock was set at
            // open so that stamp is a wall clock. An implausible one — a radio that never took the
            // time — counts as old rather than fresh.
            let plausible = u64::from(path.received_at) + 86_400 > unix_now();
            let heard = if plausible {
                Instant::now().checked_sub(Duration::from_secs(age))
            } else {
                None
            };
            if let Some(candidate) = PeerPath::reversed(
                usize::from(path.hash_size),
                usize::from(path.hops),
                &path.bytes,
            ) && prefer_route(peer_path.as_ref(), *peer_path_heard, &candidate)
            {
                // **A zero-hop answer is a route, and the shortest one.** The phone coming back
                // into earshot advertises with no path, and keeping the last repeater chain would
                // send every reply into a link that no longer reaches it.
                *peer_path = Some(candidate);
                *peer_path_heard = heard;
            }
            if *peer_path != before {
                route_log(&format!("cache answer hops={} age={age}s", path.hops));
            }
            true
        }
        // **A failed read never invalidates a working route.** The cache is 16 slots in RAM and
        // evicts by age, so `ERR_CODE_NOT_FOUND` for a node heard a minute ago is normal on a busy
        // channel; the route in hand is kept and the request path is what re-establishes one.
        Err(_) => false,
    }
}



/// The flood scope this node broadcasts under, and where it came from.
///
/// **Nothing here is a setting.** The order is: what the radio itself says (`region default` in the
/// operator's MeshCore app, or a regional firmware build's `DEFAULT_FLOOD_SCOPE_NAME`), then
/// `LDMUX_MESH_SCOPE` for an operator who wants to pin it, then a region ldmux *recognised* from a
/// flood it overheard, then none — which is unscoped, and is where every MeshCore client starts.
///
/// The order matters because a wrong scope is worse than none: a repeater forwards a scoped flood
/// only if it carries that region, and drops it *silently* otherwise (`allowPacketForward`,
/// `RegionMap::findMatch`), while an unscoped flood is forwarded by any repeater whose `*` entry
/// allows floods — the default.
#[derive(Clone, Debug)]
struct ScopeChoice {
    scope: Option<FloodScope>,
    /// One word for the diagnostics line: `radio`, `env`, `learned`, `off` or `none`.
    source: &'static str,
}

impl ScopeChoice {
    /// What the radio reports, an explicit pin winning over it, and an explicit `off`.
    fn resolve<L: ByteLink>(companion: &mut Companion<L>, log: bool) -> Self {
        let pinned = std::env::var("LDMUX_MESH_SCOPE")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if let Some(pin) = pinned {
            if pin.eq_ignore_ascii_case("off") {
                if log {
                    route_log("scope off (LDMUX_MESH_SCOPE=off): every flood is unscoped");
                }
                return Self {
                    scope: None,
                    source: "off",
                };
            }
            // **A pin is checked before it is used.** Region names are lower-case letters, digits
            // and hyphens (the firmware's `is_name_char` rejects space, `*`, `,` and `:` as well),
            // the hash covers the exact bytes, and a name nobody has is not a degraded scope — it is
            // a scope every repeater drops. Refusing a bad pin and staying unscoped is the safe
            // direction, and it is said out loud rather than silently ignored.
            //
            // **A `$`-prefixed name is refused here by the character rule**, which is the right
            // answer for a different reason too: the firmware treats a leading `$` as a *stored key*
            // and never hashes it (`RegionMap.cpp:173-188`), so a derived key would name a region no
            // repeater carries and only floods would fail — silently.
            let bare = pin.trim().trim_start_matches('#');
            let valid = !bare.is_empty()
                && bare.len() <= 30
                && bare
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
            if !valid {
                route_log(&format!(
                    "scope '{pin}' is not a region name (lower-case letters, digits and '-', up to 30); staying unscoped"
                ));
                return Self {
                    scope: None,
                    source: "none",
                };
            }
            let scope = FloodScope::from_name(bare);
            if log {
                route_log(&format!("scope {} pinned by LDMUX_MESH_SCOPE", scope.name));
            }
            return Self {
                scope: Some(scope),
                source: "env",
            };
        }
        match companion.get_default_flood_scope() {
            Ok(Some(scope)) => {
                if log {
                    route_log(&format!("scope {} from the radio", scope.name));
                }
                Self {
                    scope: Some(scope),
                    source: "radio",
                }
            }
            // A radio with none is the firmware's companion default, and unscoped floods are what
            // most meshes forward. It is not an error, and it is not a place to guess.
            Ok(None) | Err(_) => Self {
                scope: None,
                source: "none",
            },
        }
    }

    /// The scope for a builder, or `None` for an unscoped flood.
    fn as_ref(&self) -> Option<&FloodScope> {
        self.scope.as_ref()
    }

    /// What the snapshot shows: the name and the key, or nothing.
    fn info(&self) -> Option<ScopeInfo> {
        self.scope.as_ref().map(|s| ScopeInfo {
            name: s.name.clone(),
            key: s.key,
        })
    }
}

/// The airtime allowance this daemon holds itself to, in milliseconds per hour: the law, or nothing.
///
/// **The allowance is the law, or nothing at all.**
///
/// Inside a European sub-band with a duty cycle, the meter is on and the figure is that band's own
/// limit — for 869.4-869.65, a tenth of its 10 % because that band is shared with alarms and ldmux
/// would rather be a quiet neighbour than a compliant one. Outside every such window there is no
/// limit to enforce, so there is **no allowance at all**: the record counter is kept as a
/// measurement and nothing is refused. A "ten percent courtesy cap" was invented here and it was
/// wrong in both directions — it stopped a US session at a fifth of what the firmware permits while
/// the operator believed it was a rule, and it read as a legal statement about a band that has no
/// such rule.
fn airtime_budget_ms(freq_mhz: f64) -> Option<f64> {
    match freq_mhz {
        // 868.0-868.6 MHz: 1 % by law (ERC/REC 70-03 Annex 1, h1.3.1).
        f if (868.0..=868.6).contains(&f) => Some(ONE_PERCENT_MS),
        // 869.4-869.65: the law allows 10 %, and ldmux holds itself to a tenth of it.
        f if (869.4..=869.65).contains(&f) => Some(AIRTIME_BUDGET_MS),
        // 868.7-869.2: a tenth of one percent — the alarm band, 3.6 s an hour.
        f if (868.7..=869.2).contains(&f) => Some(TENTH_PERCENT_MS),
        _ => None,
    }
}

/// The allowance inside 869.4-869.65 MHz: one percent of an hour, a tenth of the ten the band
/// allows. Also the figure the app meters with while the phone owns the radio, and a test on the
/// phone's side reads this literal to keep the two languages in step.
const AIRTIME_BUDGET_MS: f64 = 36_000.0;

/// The allowance inside 868.0-868.6 MHz: one percent, which is the law there rather than a policy.
const ONE_PERCENT_MS: f64 = 36_000.0;

/// The allowance inside 868.7-869.2 MHz: a tenth of one percent, which is the law there.
const TENTH_PERCENT_MS: f64 = 3_600.0;

/// The one command the loop has for the radio.
enum Command {
    Record { seq: u8, record: Vec<u8> },
    /// Tune the radio, and answer on this channel when the firmware has.
    SetPreset {
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// Set the path hash width in bytes (1..=3), answered the same way.
    SetPathWidth {
        bytes: u8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// Set transmit power in dBm, answered the same way.
    SetPower {
        dbm: i8,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
}

/// The radio, seen as a carrier.
pub struct SerialCarrier {
    /// `None` once dropped, so the radio thread sees the channel close.
    commands: Option<Sender<Command>>,
    events: async_mpsc::Receiver<Result<Event, MeshError>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Set by `drop`; the radio thread checks it between commands.
    stop: Arc<AtomicBool>,
    /// Fires once the radio thread has returned, which is when the port is closed.
    stopped: Option<Receiver<()>>,
}

impl SerialCarrier {
    /// Open the device, identify the radio, and start serving it.
    pub async fn open(
        device: &str,
        window_record: Option<std::path::PathBuf>,
        peer_record: Option<std::path::PathBuf>,
    ) -> Result<(Self, Bound, RadioPreset), MeshError> {
        let path = device.to_owned();
        let prepared = tokio::task::spawn_blocking(move || {
            prepare(&path, window_record, peer_record)
        })
            .await
            .map_err(|e| MeshError::Io(std::io::Error::other(e.to_string())))?;
        let (companion, bound, settings) = prepared?;
        // Read before `settings` moves into the thread: the caller wants the tuple it is on.
        let preset = settings.preset;

        let (command_tx, command_rx) = channel();
        let (event_tx, event_rx) = async_mpsc::channel(64);
        let (stopped_tx, stopped_rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("ldmux-mesh-serial".to_owned())
            .spawn(move || {
                serve(companion, settings, command_rx, event_tx, thread_stop);
                let _ = stopped_tx.send(());
            })
            .map_err(MeshError::Io)?;
        Ok((
            Self {
                commands: Some(command_tx),
                events: event_rx,
                worker: Some(worker),
                stop,
                stopped: Some(stopped_rx),
            },
            bound,
            preset,
        ))
    }
}

impl Carrier for SerialCarrier {
    async fn set_preset(
        &mut self,
        freq_khz: u32,
        bw_hz: u32,
        sf: u8,
        cr: u8,
    ) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .as_ref()
            .ok_or(MeshError::Closed)?
            .send(Command::SetPreset {
                freq_khz,
                bw_hz,
                sf,
                cr,
                reply,
            })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!("the radio refused the tuple: {e}"))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the retune")),
        }
    }

    async fn set_path_width(&mut self, bytes: u8) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .as_ref()
            .ok_or(MeshError::Closed)?
            .send(Command::SetPathWidth { bytes, reply })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!(
                "the radio refused the path width: {e}"
            ))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the path width")),
        }
    }

    async fn set_tx_power(&mut self, dbm: i8) -> Result<(), MeshError> {
        let (reply, answer) = channel();
        self.commands
            .as_ref()
            .ok_or(MeshError::Closed)?
            .send(Command::SetPower { dbm, reply })
            .map_err(|_| MeshError::Closed)?;
        match answer.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(MeshError::Unsupported(format!(
                "the radio refused the power: {e}"
            ))),
            Err(_) => Err(MeshError::Timeout("the radio did not answer the power change")),
        }
    }

    async fn send_record(&mut self, seq: u8, record: &[u8]) -> Result<(), MeshError> {
        let commands = self.commands.as_ref().ok_or(MeshError::Closed)?;
        commands
            .send(Command::Record {
                seq,
                record: record.to_vec(),
            })
            .map_err(|_| MeshError::Closed)
    }

    async fn recv(&mut self) -> Result<Event, MeshError> {
        match self.events.recv().await {
            Some(result) => result,
            None => Err(MeshError::Closed),
        }
    }
}

impl Drop for SerialCarrier {
    fn drop(&mut self) {
        // Dropping the sender disconnects the channel and the flag covers a thread already
        // inside a command. **Never `join` without a bound**: this runs on the session loop's
        // teardown path, and a thread parked in a write to a radio that stopped draining used
        // to hang it — the snapshot then read `ready / established` for ever, the shells froze
        // and only a daemon restart recovered. The handle is dropped (detached) when the wait
        // expires; the port closes and the lock releases when the thread does return.
        drop(self.commands.take());
        self.stop.store(true, Ordering::Relaxed);
        if let Some(stopped) = self.stopped.take() {
            // The wait is bounded, and on the daemon's multi-thread runtime it runs off the
            // worker (`block_in_place` hands the thread back while it waits). A current-thread
            // runtime — the tests — takes it inline; 250 ms is short enough for that, and it
            // keeps the teardown testable without a second runtime.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                    let _ = tokio::task::block_in_place(|| stopped.recv_timeout(CLOSE_WAIT));
                }
                _ => {
                    let _ = stopped.recv_timeout(CLOSE_WAIT);
                }
            }
        }
        // Dropping the handle detaches rather than joins: a late finish is harmless, and
        // blocking here is the very thing this teardown must not do.
        let _ = self.worker.take();
    }
}

/// What the radio told us at open time, and what the loop needs from it.
///
/// Not `Copy`: it owns the record paths now, and copying a path to a state file to then write
/// through the copy is the kind of quiet aliasing this file does not need.
#[derive(Clone)]
struct Settings {
    /// Bytes per path hash, from the firmware's reported path-hash mode.
    hash_size: u8,
    /// Time on air for a full record, from the preset the radio reports.
    airtime_ms: f64,
    /// **What this radio is actually tuned to.** Read from `SELF_INFO` at open and published in
    /// the snapshot, because the two radios must be on the same tuple and nothing else in the
    /// system can say what the host's is: the phone knows its own, the host knew nothing, and a
    /// mismatch looked exactly like being out of range. The user needs to be able to compare the
    /// two numbers.
    preset: RadioPreset,
    /// Where the **peer's radio key** is remembered, so a restarted daemon can still ask the peer
    /// for a route.
    ///
    /// **This used to be learned only from a route hint and lost on every restart.** The hint is a
    /// direct record, so it arrives only when the peer already has a route to us; a daemon that
    /// restarted on a phone that had moved could not address anything to it and fell back to
    /// waiting for an advert, which is exactly the five-minute wait this change removes.
    peer_record: Option<std::path::PathBuf>,
    /// The peer's radio key as the record held it at open, or `None` when this radio has never
    /// heard from that peer.
    peer_key: Option<[u8; 32]>,
    /// The radio's own client-repeat flag, as the firmware reported it.
    ///
    /// Carried into every preset write because `CMD_SET_RADIO_PARAMS`'s sixth byte is *persisted* as
    /// `setRepeatEn(repeat != 0)`: a frame that stops at the coding rate clears an operator's repeat
    /// setting, and nothing on the radio says so afterwards.
    repeat_enabled: Option<bool>,
    /// The flood scope this end broadcasts under, and where it came from.
    scope: ScopeChoice,
    /// Where the hour's spend is recorded, so the allowance survives a daemon restart.
    ///
    /// **The window was per process.** `sent` started at zero whenever the worker started, so a
    /// restart handed out a fresh 36 s on a band whose allowance is 1 % of any hour rather than 1 %
    /// of a process's life. Measured 2026-11-07: an afternoon of restarts left `airtime 61.4 s this
    /// hour` from a budget of 36 s, because each restart forgot what the previous one had spent.
    window_record: Option<std::path::PathBuf>,
}

/// The four numbers that decide whether two radios can hear each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioPreset {
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub sf: u8,
    pub cr: u8,
    /// Transmit power in dBm, as the radio reports it. Read from `SELF_INFO` byte 2, which is the
    /// *configured* power rather than the board's ceiling (byte 3).
    pub tx_power_dbm: i8,
    /// Bytes per path hash this radio puts on its own packets, after the default was applied.
    ///
    /// Published with the tuple because it is the other half of what two radios have to agree
    /// about to find each other: the width the host's adverts carry. A phone on a different width
    /// still hears them (a repeater forwards whatever arrives), so this is a fact to read, not a
    /// constraint to match — but it is the first thing to check when a peer's advert path never
    /// resolves.
    pub hash_size: u8,
    /// This radio's own public key, from the same `SELF_INFO`.
    ///
    /// The daemon owns the radio, so it can read the key a phone must bind. Requiring the operator
    /// to type it into `LDMUX_MESH_SELF` was a step that could only be got wrong: a stale or
    /// mistyped key makes a pairing code that no phone can use.
    pub public_key: [u8; 32],
}





fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The hour's spend as `unix_start sent`, or `None` when absent, unreadable or malformed.
fn load_window(path: &std::path::Path) -> Option<(u64, u32)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace();
    let start = it.next()?.parse::<u64>().ok()?;
    let sent = it.next()?.parse::<u32>().ok()?;
    Some((start, sent))
}

/// Record the hour's spend. Best effort: a state directory that cannot be written must not stop the
/// radio from working, it only means the next restart forgets again.
///
/// **Written per record, deliberately.** A 20-byte file costs about 5 us and the page cache absorbs
/// it, and the alternative — a counter flushed on the status beat — loses up to half a second of
/// spend if the process dies, which is the one case this file exists for.
fn save_window(path: &std::path::Path, start_unix: u64, sent: u32) {
    let _ = std::fs::write(path, format!("{start_unix} {sent}"));
}

/// The peer's radio key as the state directory holds it, or `None` when absent or malformed.
///
/// Held as 64 hex characters, the same shape as the width record, because a person may have to read
/// it out of a state directory on a host whose phone is not with them.
fn load_peer(path: &std::path::Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

/// Remember the peer's radio key. Best effort, like the other state records: a state directory that
/// cannot be written must not stop the radio from working, it only means the next restart has to
/// learn the key again.
fn save_peer(path: &std::path::Path, key: &[u8; 32]) {
    let _ = std::fs::write(path, hex_key(key));
}


fn hex_key(key: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in key {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Open the port and identify the radio. Runs off the async runtime: it blocks.
fn prepare(
    device: &str,
    window_record: Option<std::path::PathBuf>,
    peer_record: Option<std::path::PathBuf>,
) -> Result<(Companion<SerialLink>, Bound, Settings), MeshError> {
    let link = SerialLink::open(device).map_err(MeshError::Io)?;
    link.lock_exclusive().map_err(|e| {
        MeshError::Io(std::io::Error::other(format!(
            "{device}: another process holds this radio ({e})"
        )))
    })?;
    let mut companion = Companion::new(link);
    let self_info = companion.app_start("ldmux").map_err(session_error)?;
    // **The radio's clock is what stamps an advert.** A companion has no network and starts its
    // clock at zero at boot, so `CMD_GET_ADVERT_PATH` would report route ages against the epoch and
    // every "is this path still true" question would be unanswerable. One command, best effort.
    let _ = companion.set_time(u32::try_from(unix_now()).unwrap_or(u32::MAX));
    // The version this sends is what the firmware latches to decide whether it
    // emits its V3 frames (MyMesh.cpp:1028-1031); `crates/companion` sends 3.
    let device_info = companion.device_query().map_err(session_error)?;
    // **Two bytes per hop, the community default, unless this radio already says otherwise.**
    // The firmware ships at mode 0 (one byte), which the 2026 MeshCore guidance is explicit about
    // not recommending for new deployments: 254 usable one-byte IDs collide at ≈20 co-channel
    // repeaters (a coin flip), 65 536 two-byte IDs at ≈300, and a collision is not cosmetic —
    // two repeaters with one prefix both forward a direct message meant for one of them. Two bytes
    // also still allows 32 hops, against observed regional paths in the mid-teens.
    //
    // **The radio's own preference wins when it has one**, because an operator who set three bytes
    // for map disambiguation meant it. Mode 0 is indistinguishable from "never configured", so that
    // is the one value ldmux upgrades, once, at bind. The write is safe against a stock radio: it
    // answers OK or ILLEGAL_ARG and the mode is a preference about this node's own adverts, not
    // about what it forwards.
    // **The radio's width is the radio's, and ldmux does not write it.**
    //
    // It used to raise a radio reporting mode 0 to two bytes, arguing that 254 one-byte hashes
    // collide at about twenty co-channel repeaters. That argument is real, and it is also not ours
    // to act on: we are optimising for the **public MeshCore mesh**, where one byte is the
    // convention every client and repeater shares. A node that quietly widens itself costs one
    // extra byte per hop in every record it sends, for a property only its neighbours see, and it
    // stops matching the hashes the rest of the mesh stores for it.
    //
    // Read what the firmware reports, publish it, and follow it. An operator who wants two or three
    // bytes sets it in their own app.
    let reported_mode = device_info.path_hash_mode;
    let hash_size = reported_mode.map_or(1, |m| m.saturating_add(1).clamp(1, 3));
    eprintln!("ldmuxd: host radio path hashes: {hash_size} byte(s)");
    let airtime_ms = self_info.time_on_air_ms(RECORD_MAX);
    let repeat_enabled = device_info.repeat_enabled;
    let bound = Bound {
        // No route yet, so no path bytes are spent: the receiver's cap is the whole answer until
        // `Event::Limit` revises it as soon as a route is learned.
        uplink_record_max: record_limit(None),
        radio_state: RadioState::Connected,
        // No gateway is in the path: this process owns the radio, and the firmware is the meter.
        budget: Budget::RadioGoverned,
        airtime_ms: Some(airtime_ms),
    };
    let preset = RadioPreset {
        freq_mhz: self_info.radio_freq_mhz,
        bw_khz: self_info.radio_bw_khz,
        sf: self_info.radio_sf,
        cr: self_info.radio_cr,
        tx_power_dbm: i8::try_from(self_info.tx_power).unwrap_or(0),
        public_key: self_info.public_key,
        hash_size,
    };
    let peer_key = peer_record
        .as_deref()
        .and_then(load_peer)
        .filter(|key| *key != self_info.public_key);
    // **What the radio itself says about flood scoping**, read before anything is transmitted: the
    // operator's own MeshCore app sets it there (`region default`), and a regional firmware build
    // can ship one. Reading it is the difference between following a decision someone made and
    // inventing one.
    let scope = ScopeChoice::resolve(&mut companion, true);
    Ok((
        companion,
        bound,
        Settings {
            hash_size,
            repeat_enabled,
            airtime_ms,
            preset,
            window_record,
            peer_record,
            peer_key,
            scope,
        },
    ))
}

/// The radio thread: serve commands and the air until the loop lets go.
fn serve<L: ByteLink>(
    companion: Companion<L>,
    settings: Settings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
    stop: Arc<AtomicBool>,
) {
    serve_loop(companion, settings, commands, events, &stop);
}

/// The radio thread's body, split out so every exit path reaches the `stopped` signal.
fn serve_loop<L: ByteLink>(
    mut companion: Companion<L>,
    mut settings: Settings,
    commands: Receiver<Command>,
    events: async_mpsc::Sender<Result<Event, MeshError>>,
    stop: &AtomicBool,
) {
    // Recomputed on a retune: a budget is records per hour at *this* tuple's airtime, so a
    // radio moved to a slower spreading factor can spend fewer records than this number said.
    // **The hour's allowance in records, uncapped.** The session only needs "is there any left" —
    // it may emit at most the L2 credit window per poll — so a count that is truncated to the
    // one-byte wire field is fine to *publish* but must never be the number this end counts
    // against: at 62.5 kHz a ten-percent hour is 597 records, and a 255-byte ceiling stopped the
    // link at 43 % of its own allowance.
    let mut budget = budget_per_hour(settings.airtime_ms, settings.preset.freq_mhz);
    // **The allowance belongs to the hour, not to this process.** A restart used to hand out a
    // fresh 36 s; the spend is now read back from the state directory and the window resumes where
    // the previous process left it, as long as it is still the same hour.
    let mut window = Instant::now();
    let mut window_start_unix = unix_now();
    let mut sent: u32 = 0;
    if let Some(path) = settings.window_record.as_deref()
        && let Some((start_unix, spent)) = load_window(path)
    {
        let age = unix_now().saturating_sub(start_unix);
        if age < HOUR.as_secs()
            && let Some(base) = Instant::now().checked_sub(Duration::from_secs(age))
        {
            window = base;
            window_start_unix = start_unix;
            sent = spent;
        }
    }
    // The route to the peer's radio. **A direct packet's path is consumed in transit**, so it
    // cannot be learned from an arriving record: each repeater removes its own hash
    // (`Mesh::removeSelfFromPath`), which leaves a packet that crossed repeaters with an empty path.
    // The route therefore comes from the peer's *advert*, whose path accumulates because flooded
    // packets append one hash per repeater (`Mesh::routeRecvPacket`), and is read back out of the
    // radio's advert-path cache.
    let mut peer_path: Option<PeerPath> = None;
    // When the peer was last **heard** on that route, so an old answer cannot silence the request
    // path. A cache entry may be hours old — `received_at` says so once the radio's clock is set —
    // and a route that merely exists is not a route that works.
    let mut peer_path_heard: Option<Instant> = None;
    // Which radio the peer is: from the state record, a route hint or a route request. Without it
    // there is no prefix to look a route up by, and nothing to address a route request to.
    let mut peer_radio: Option<[u8; 32]> = settings.peer_key;
    // When the last route request went out, and when the last reply was sent, so neither mechanism
    // can be turned into an amplifier by a peer that repeats itself.
    // The scope this end broadcasts under. Mutable: a radio with none of its own can learn one from
    // a flood it overhears, and every later request then uses it.
    let mut scope = settings.scope.clone();
    // Whether the radio has already been asked to re-read what it actually has, so a mismatch
    // noticed later does not rewrite the operator's choice twice.
    let mut scope_followed_radio = scope.source != "none";
    // A region name matched once, and a fingerprint of the payload that matched it. **Two matches
    // over different payloads before anything is adopted**: a 16-bit code has one chance in 65 536
    // of matching by accident per candidate, so one match across ~70 names is a coin toss once every
    // few thousand wideband packets, while two independent payloads settling on the same name is
    // evidence. Nothing is written to the radio on a single match.
    let mut scope_candidate: Option<(String, u64)> = None;
    // A scope adopted from the air **before anything has answered under it**. The probe is one
    // request; until a reply comes back the region is not written to the radio, because a wrong
    // scope in flash is the one state in which discovery cannot work at all.
    let mut scope_unproven = false;
    // The least gap between two identification attempts: naming a region costs one HMAC per
    // candidate, and a busy scoped mesh would pay for it on every packet.
    let mut scope_checked: Option<Instant> = None;
    // The radio's own client-repeat flag, as the firmware reported it, so a preset write can keep
    // it: the command's sixth byte is persisted as `repeat != 0`, and omitting it clears the setting.

    // When a request from an unpinned radio was last reported, so a mesh of strangers cannot fill
    // the journal with the same line.
    let mut last_stranger_log: Option<Instant> = None;

    // When the "request sent" line last went to the journal, so a failing link cannot flood it.

    let mut last_reply: Option<Instant> = None;
    // When the peer's route was last asked for, so a quiet link does not ask on every tick.
    let mut last_route_ask: Option<Instant> = None;
    // The last record heard from the peer, and the last one this end handed to the radio. They are
    // the only evidence the carrier has that a session is trying to cross: without a route it can
    // neither send nor be understood, so a pathless host asks the peer while one of them is recent
    // and stays quiet when nothing has moved for a couple of minutes.
    let mut last_peer_record: Option<Instant> = None;
    // The RSSI and SNR the radio measured for the last record **from the peer**, from the receive
    // log frame that carried it (`[0x88][snr×4][rssi][packet]`). The radio's own `GET_STATS` answers
    // "the last packet I heard", which on a busy mesh is whatever a neighbour said most recently.
    let mut peer_signal: Option<(i8, i8)> = None;
    let mut last_outbound: Option<Instant> = None;
    // When the last advert went out; `None` means none yet, so the first idle moment sends one.
    let mut last_advert: Option<Instant> = None;
    let advert_every = advert_interval();
    // When the last status went out. **A status is published on a timer as well as after a send**,
    // because the session gates its sends on the countdown this carries: with the countdown at zero
    // the session polls nothing, the radio transmits nothing, and a status that is only published
    // after a transmission could never say the hour had rolled over. Measured 2026-11-06: a phone
    // waiting on "the host has not described a terminal yet" for as long as the countdown stayed
    // stale.
    let mut last_status = Instant::now();
    // When the radio's own settings were last re-read, so a quiet link notices a retune by another
    // app within a minute rather than never.
    let mut last_params = Instant::now();
    // A reading is wanted at once (hence `None`, which fires on the first pass) and then every twenty
    // seconds: often enough that a walk out of range shows its effect, rare enough to be one serial
    // frame in twenty.
    let mut last_stats: Option<Instant> = None;

    // One status before anything else: the loop starts with no budget, and an
    // unknown budget is zero, so without this it would never send the first record.
    if events
        .blocking_send(Ok(Event::Status(status(&settings, budget, 0))))
        .is_err()
    {
        return;
    }
    if events.blocking_send(Ok(Event::Scope(scope.info()))).is_err() {
        return;
    }
    // The record size for the path in hand (none yet: a bare record is the largest).
    let mut published_limit = record_limit(peer_path.as_ref());
    if events
        .blocking_send(Ok(Event::Limit {
            record_max: published_limit,
            hops: peer_path.as_ref().map_or(0, |p| p.hops),
        }))
        .is_err()
    {
        return;
    }

    loop {
        // A dropped carrier may be waiting for this thread; do not sit inside a command
        // interval after the loop has let go.
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if window.elapsed() >= HOUR {
            window = Instant::now();
            window_start_unix = unix_now();
            sent = 0;
            if let Some(path) = settings.window_record.as_deref() {
                save_window(path, window_start_unix, sent);
            }
        }

        match commands.recv_timeout(COMMAND_WAIT) {
            Ok(Command::Record { seq, record }) => {
                // Something is trying to cross: the asker above uses this, and the last record
                // heard from the peer, to decide whether a pathless host should ask at all. Without
                // it a host with no phone in range floods on a timer for ever; with it, nothing
                // moved for two minutes means nothing wants to.
                last_outbound = Some(Instant::now());
                // **The reply follows the path the request arrived by.** Without it a phone behind
                // a repeater is unreachable: a direct packet with no path travels only as far as
                // this radio's own antenna.
                let (hash_size, path) = match &peer_path {
                    Some(p) => (p.hash_size, p.bytes.as_slice()),
                    None => (settings.hash_size, &[][..]),
                };
                // **Two seconds, not ten.** The firmware acknowledges `SEND_RAW_PACKET` *before* it
                // transmits (`MyMesh.cpp:2003-2016`), so a 10-second wait bought nothing on a busy
                // channel and meant one stalled radio held the command queue — and the route
                // requests behind it — for ten seconds. Two is still 100x a 172-byte record at
                // 115200 baud; the route reply already used the short form for the same reason.
                let outcome = match companion.send_raw_within(
                    hash_size,
                    path,
                    &record,
                    std::time::Duration::from_secs(2),
                ) {
                    Ok(()) => {
                        sent = sent.saturating_add(1);
                        if let Some(path) = settings.window_record.as_deref() {
                            save_window(path, window_start_unix, sent);
                        }
                        SendOutcome::SentDirect
                    }
                    Err(e) => {
                        // The one refusal that is not about this record: a radio whose firmware
                        // predates raw-packet composition cannot send anything at all, and "the
                        // radio refused (busy)" is the wrong sentence for it.
                        if let SessionError::Rejected { code: 1, .. } = &e {
                            eprintln!(
                                "ldmuxd: this radio does not accept a raw packet send (needs MeshCore companion v1.16.0 or newer)"
                            );
                        }
                        if let SessionError::Codec(why) = &e {
                            // Said once per record, at the place the number is known: a record that
                            // does not fit the path is a sizing bug, not a radio one, and the
                            // `Limit` event should have prevented it.
                            eprintln!(
                                "ldmuxd: a {}-byte record does not fit a {}-byte path: {why}",
                                record.len(),
                                path.len()
                            );
                        }
                        refused(&e)
                    }
                };
                if events
                    .blocking_send(Ok(Event::SendResult(SendResult { seq, outcome })))
                    .is_err()
                {
                    return;
                }
                if events
                    .blocking_send(Ok(Event::Status(status(&settings, budget, sent))))
                    .is_err()
                {
                    return;
                }
            }
            Ok(Command::SetPreset {
                freq_khz,
                bw_hz,
                sf,
                cr,
                reply,
            }) => {
                // The firmware validates the tuple and answers OK or an error, so a refusal is
                // reported rather than leaving the radio silently where it was.
                //
                // **The write carries the radio's own client-repeat flag.** The command's sixth
                // optional byte is persisted as `setRepeatEn(repeat != 0)`, so a frame that stops at
                // the coding rate silently cleared an operator's repeat setting on every preset
                // write. `repeat_enabled` is what the firmware reported; `None` leaves its stored
                // value alone rather than guessing.
                let repeat = settings.repeat_enabled;
                let result = companion
                    .set_radio_params_keeping(freq_khz, bw_hz, sf, cr, repeat)
                    .map_err(|e| format!("{e:?}"));
                if result.is_ok() {
                    // A retune changes the tuple and nothing else: the radio keeps its key.
                    settings.preset = RadioPreset {
                        freq_mhz: f64::from(freq_khz) / 1000.0,
                        bw_khz: f64::from(bw_hz) / 1000.0,
                        sf,
                        cr,
                        public_key: settings.preset.public_key,
                        // A retune does not touch the path width; it stays what the radio reports.
                        hash_size: settings.preset.hash_size,
                        // ...and neither does it touch the power.
                        tx_power_dbm: settings.preset.tx_power_dbm,
                    };
                    // The airtime estimate follows the new tuple, or the budget keeps measuring
                    // the radio that is no longer in use.
                    settings.airtime_ms = ldmux_companion::codec::time_on_air_ms(
                        settings.preset.bw_khz,
                        settings.preset.sf,
                        settings.preset.cr,
                        RECORD_MAX,
                    );
                    // The report was computed from the old airtime, so it kept saying the old
                    // tuple's allowance while the meter below it measured the new one.
                    budget = budget_per_hour(settings.airtime_ms, settings.preset.freq_mhz);
                }
                let _ = reply.send(result);
                let _ = events.blocking_send(Ok(Event::Status(status(
                    &settings, budget, sent,
                ))));
            }
            Ok(Command::SetPathWidth { bytes, reply }) => {
                // The firmware's mode is zero-based (`0..2` for one to three bytes). The wire and
                // the CLI speak bytes, because that is the unit the status line prints and the unit
                // a person compares against a neighbour's radio.
                let bytes = bytes.clamp(1, 3);
                let result = match companion.set_path_hash_mode(bytes - 1) {
                    Ok(()) => {
                        settings.preset.hash_size = bytes;
                        Ok(())
                    }
                    Err(e) => Err(format!("{e:?}")),
                };
                let _ = reply.send(result);
                // The status carries the width, so the daemon's snapshot follows the radio instead
                // of the value it was started with.
                let _ = events.blocking_send(Ok(Event::Status(status(
                    &settings, budget, sent,
                ))));
            }
            Ok(Command::SetPower { dbm, reply }) => {
                let result = match companion.set_tx_power(dbm) {
                    Ok(()) => {
                        settings.preset.tx_power_dbm = dbm;
                        Ok(())
                    }
                    Err(e) => Err(format!("{e:?}")),
                };
                let _ = reply.send(result);
                // The status carries the power, so the daemon's snapshot follows the radio.
                let _ = events.blocking_send(Ok(Event::Status(status(
                    &settings, budget, sent,
                ))));
            }
            Err(RecvTimeoutError::Timeout) => {
                // **The route to the peer, from its advert.** Asked for when the peer announces
                // itself and then every `PEER_ROUTE_REFRESH` while the link is quiet, because a
                // phone that moved has a new route and the old one silently carries nothing. This
                // is one serial command: it costs no airtime.
                let due = last_route_ask.is_none_or(|t| {
                    // While there is no route, ask again quickly: the phone may be mid-announce and
                    // the flood still crossing the repeaters. Once one is in hand, ask calmly.
                    let interval = if peer_path.is_some() {
                        PEER_ROUTE_REFRESH
                    } else {
                        PEER_ROUTE_RETRY
                    };
                    t.elapsed() >= interval
                });
                // **Not gated on hearing the peer.** The failure this covers is "we hear them, they
                // cannot hear us": a phone that moved still reaches us, so a gate on inbound
                // silence would never let the route be re-read.
                if peer_radio.is_some() && due {
                    last_route_ask = Some(Instant::now());
                    refresh_peer_route(&mut companion, peer_radio.as_ref(), &mut peer_path, &mut peer_path_heard);
                    // **The cache is a shortcut, not the mechanism.** When it has nothing, ask the
                    // peer for a route: the reply is one direct record and it crosses whatever
                    // distance the mesh actually carries, which an advert does not (a repeater
                    // drops an advert after 8 hops by default).
                    // **The host asks too, because a restarted daemon otherwise cannot answer.**
                    //
                    // A direct packet's path is consumed by the repeaters that carry it, so the
                    // session records arriving from the phone teach this end nothing about where it
                    // is. After a restart the host holds no route, and a phone holding a fresh one
                    // never asks: the phone sends `msg1`, this end cannot address `msg2`, and
                    // nothing moves until the phone's handshake ladder gives up and drops the route
                    // — three unanswered handshakes, a minute of air, for a path the phone already
                    // knew. The phone now answers a request with a flooded reply (it has the same
                    // reader this end has), so asking is one round trip instead.
                    //
                    // **Only while something is trying to cross**, because a host with no phone in
                    // range and nothing to send would otherwise flood on a timer for ever.
                    if peer_path.is_none()
                        && (last_peer_record.is_some_and(|t| t.elapsed() < Duration::from_secs(120))
                            || last_outbound.is_some_and(|t| t.elapsed() < Duration::from_secs(120)))
                        && left(budget, sent) != Some(0)
                        && let Some(key) = peer_radio
                        && let Ok(packet) = ldmux_companion::build_route_request_in_scope(
                            settings.preset.hash_size,
                            &key,
                            &settings.preset.public_key,
                            scope.as_ref(),
                        )
                    {
                        let ok = companion
                            .send_packet_within(&packet, Duration::from_secs(2))
                            .is_ok();
                        route_log(&format!("asked the peer for a route ok={ok}"));
                        if ok {
                            sent = sent.saturating_add(1);
                            if let Some(path) = settings.window_record.as_deref() {
                                save_window(path, window_start_unix, sent);
                            }
                        }
                    }
                }
                // The countdown, refreshed while the link is idle. Not a transmission: it is a
                // message to this process only, and it is what lets the session see the hour roll
                // over when it has been held back for the whole hour.
                // **The radio's settings, re-read from the radio.** A retune by another app is not
                // announced by anything, so the snapshot would otherwise keep claiming the old
                // frequency until the daemon restarts. One `APP_START` per minute is a serial
                // command: no airtime, and no effect on the session.
                if last_params.elapsed() >= PARAM_WATCH {
                    last_params = Instant::now();
                    if let Ok(info) = companion.app_start("ldmux") {
                        let seen = SeenParams {
                            freq_khz: (info.radio_freq_mhz * 1000.0).round() as u32,
                            bw_hz: (info.radio_bw_khz * 1000.0).round() as u32,
                            sf: info.radio_sf,
                            cr: info.radio_cr,
                            tx_power_dbm: i8::try_from(info.tx_power).unwrap_or(0),
                        };
                        if seen != SeenParams::of(&settings.preset) {
                            // Say it once, where an operator will look, and publish the truth: the
                            // phone compares its own tuple against this one, so a snapshot left
                            // stale is a mismatch nobody can see.
                            eprintln!(
                                "ldmuxd: the radio's settings changed under us: {:.3} MHz  {:.1} kHz  SF{}  CR{}  {} dBm",
                                f64::from(seen.freq_khz) / 1000.0,
                                f64::from(seen.bw_hz) / 1000.0,
                                seen.sf,
                                seen.cr,
                                seen.tx_power_dbm
                            );
                            settings.preset.freq_mhz = f64::from(seen.freq_khz) / 1000.0;
                            settings.preset.bw_khz = f64::from(seen.bw_hz) / 1000.0;
                            settings.preset.sf = seen.sf;
                            settings.preset.cr = seen.cr;
                            settings.preset.tx_power_dbm = seen.tx_power_dbm;
                            // The width is a register of its own on the same radio, and another app
                            // setting it is the case that cost a whole afternoon: read it too.
                            if let Ok(info) = companion.device_query() {
                                if let Some(mode) = info.path_hash_mode {
                                    settings.preset.hash_size = mode.saturating_add(1).clamp(1, 3);
                                }
                                // Another app can flip repeat too, and the next preset write must
                                // carry whatever the radio says now.
                                settings.repeat_enabled = info.repeat_enabled;
                            }
                            // The airtime estimate follows the tuple, and so must the allowance: a
                            // radio moved from 500 kHz to 62.5 kHz inside the European sub-band
                            // keeps 255 records an hour unless this is recomputed.
                            settings.airtime_ms = info.time_on_air_ms(RECORD_MAX);
                            budget =
                                budget_per_hour(settings.airtime_ms, settings.preset.freq_mhz);
                            if events
                                .blocking_send(Ok(Event::Status(status(&settings, budget, sent))))
                                .is_err()
                            {
                                return;
                            }
                        }
                        // **The scope is its own register, and another app owns it too.** It is
                        // watched on every pass rather than inside the tuple branch: an operator
                        // who only clears `region default` leaves the tuple untouched, and this
                        // daemon would keep flooding under a region its own radio no longer has.
                        // Whatever the radio says now wins, including "nothing".
                        let seen_scope = ScopeChoice::resolve(&mut companion, false);
                        if seen_scope.info() != scope.info() {
                            match seen_scope.info() {
                                Some(s) => route_log(&format!(
                                    "scope {} set on the radio by another app",
                                    s.name
                                )),
                                None => route_log("scope cleared on the radio by another app"),
                            }
                            scope = seen_scope;
                            scope_followed_radio = scope.source != "none";
                            if events.blocking_send(Ok(Event::Scope(scope.info()))).is_err() {
                                return;
                            }
                        }
                    }
                }

                // **What the radio measured, from the radio.** Its own cadence rather than the
                // settings watch's minute: the reading is the answer to "is this link reaching", and
                // a minute of blank status after a restart is a minute the operator spends wondering
                // whether the radio is even attached. One serial command, no airtime.
                // **A waiting frame goes first.** The port is shared, so a stats read must not sit
                // in front of a received packet (see `Companion::has_pending`).
                if !companion.has_pending() && last_stats.is_none_or(|t| t.elapsed() >= STATS_WATCH) {
                    last_stats = Some(Instant::now());
                    if let Ok(stats) = companion.radio_stats() {
                        // **The peer's record wins over "whatever was heard last".** Both are true
                        // facts about the air; only one of them is the link the session is using.
                        let (rssi, snr, from_peer) = match peer_signal {
                            Some((snr, rssi)) => (Some(rssi), snr, true),
                            None => (
                                stats.has_signal().then_some(stats.last_rssi),
                                stats.last_snr_quarter_db,
                                false,
                            ),
                        };
                        if events
                            .blocking_send(Ok(Event::Signal(Signal {
                                noise_floor_dbm: stats.noise_floor_dbm,
                                rssi_dbm: rssi,
                                snr_quarter_db: snr,
                                tx_air_secs: stats.tx_air_secs,
                                rx_air_secs: stats.rx_air_secs,
                                from_peer,
                            })))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                if last_status.elapsed() >= STATUS_REFRESH {
                    last_status = Instant::now();
                    if events
                        .blocking_send(Ok(Event::Status(status(&settings, budget, sent))))
                        .is_err()
                    {
                        return;
                    }
                }
                // Only when nothing is queued: an advert waits its turn behind records, because a
                // record is an answer to something the user is waiting for.
                if let Some(every) = advert_every
                    && last_advert.is_none_or(|t| t.elapsed() >= every)
                {
                    // **An advert is airtime too.** It was unmetered, so the 1 % budget this
                    // daemon enforces counted only the records it was asked to send and quietly
                    // spent the rest on broadcasts — twelve an hour at the default interval, on a
                    // band where the whole allowance is 36 s. One record's worth is charged: an
                    // advert is smaller than a record at every tuple, so this over-counts rather
                    // than under-counts, and the operator's usable records move by a dozen an hour.
                    // **Nothing goes out of a budget that is spent.** The advert is charged below,
                    // so it must also be refused when the charge would take the hour past its
                    // allowance: an advert is the host's courtesy, not an answer to anything the
                    // operator asked for. Measured 2026-11-07: an afternoon of reconnects at
                    // 62.5 kHz left `records 0 left this hour` with 14 records pending, and the
                    // broadcast would otherwise have kept spending a band the daemon had already
                    // declared exhausted.
                    let advert_ok = left(budget, sent) != Some(0) && companion.send_self_advert(true).is_ok();
                    if trace_on() {
                        eprintln!("ldmuxd: trace advert scheduled ok={advert_ok}");
                    }
                    if advert_ok {
                        sent = sent.saturating_add(1);
                        if let Some(path) = settings.window_record.as_deref() {
                            save_window(path, window_start_unix, sent);
                        }
                    }
                    // **A failed advert is not an advert sent.** Advancing the clock on a refusal
                    // (the radio still settling after a boot is the common one) hid the node for a
                    // whole interval; a retry a minute later costs one broadcast and fixes it.
                    last_advert = Some(if advert_ok {
                        Instant::now()
                    } else {
                        Instant::now() - every.saturating_sub(Duration::from_secs(60))
                    });
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = events.blocking_send(Err(MeshError::Closed));
                return;
            }
        }

        let limit_now = record_limit(peer_path.as_ref());
        if limit_now != published_limit {
            // The route changed and took record bytes with it. The session is told, so a screen
            // update is split to fit instead of being refused by the packet builder.
            published_limit = limit_now;
            if events
                .blocking_send(Ok(Event::Limit {
                    record_max: limit_now,
                    hops: peer_path.as_ref().map_or(0, |p| p.hops),
                }))
                .is_err()
            {
                return;
            }
        }
        match companion.next_push(Duration::from_millis(1)) {
            Ok(Some(frame)) => {
                if trace_on() {
                    trace_rx(&frame);
                }
                // **Learn the mesh's scope from a packet it scoped.** A scoped flood proves the
                // region is carried by the repeaters that forwarded it, so recognising one is the
                // only evidence available: the region's name is never on the air, only a 16-bit
                // code over the payload, and a name is what has to be hashed to answer it.
                //
                // This runs only while this end has no scope. Once the radio reports one, or an
                // operator pinned one, that decision stands.
                // **The floor is for the work, not for the evidence.** While a name is waiting to
                // be confirmed, every scoped flood is tested — that is the case where a second
                // sample settles it — and once nothing is pending the work is rate-limited, because
                // naming a region costs one HMAC per candidate and a busy mesh would pay it on
                // every packet.
                if !scope_followed_radio
                    && (scope_candidate.is_some()
                        || scope_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)))
                    && let Some(view) = frame_packet(&frame)
                    && view.is_flood()
                    // **A zero-hop flood proves nothing about the repeaters.** It was never
                    // forwarded, so a matching tag says only that an adjacent radio used it; the
                    // repeaters this end needs may not carry that region at all.
                    && view.path.hops >= 1
                    && view.transport_codes.is_some()
                {
                    scope_checked = Some(Instant::now());
                    if let Some(learned) = identify_scope(&view, KNOWN_SCOPE_TAGS) {
                        use std::hash::{Hash as _, Hasher as _};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        view.payload_type.hash(&mut hasher);
                        view.payload.hash(&mut hasher);
                        let fingerprint = hasher.finish();
                        let confirmed = matches!(
                            &scope_candidate,
                            Some((name, seen)) if name == &learned.name && *seen != fingerprint
                        );
                        scope_candidate = Some((learned.name.clone(), fingerprint));
                        if confirmed {
                            route_log(&format!(
                                "scope {} proven by two scoped floods on the mesh",
                                learned.name
                            ));
                            // **Adopted for this end's own floods, not yet written to the radio.**
                            // The next request goes out under it; when something answers under it
                            // the region is proved and the radio can be told too.
                            scope = ScopeChoice {
                                scope: Some(learned),
                                source: "learned",
                            };
                            scope_followed_radio = true;
                            scope_unproven = true;
                            if events.blocking_send(Ok(Event::Scope(scope.info()))).is_err() {
                                return;
                            }
                        }
                    }
                }
                // **Proved: something came back over the scope this end is using.** Only now is the
                // radio given it, because an advert is firmware-built and uses the radio's own
                // default scope — a daemon that only scoped its raw packets would keep advertising
                // unscoped on the one mesh that needs the scope.
                let answered = frame_route_request(&frame, &settings.preset.public_key).is_some()
                    || incoming_record(&frame)
                        .and_then(|i| route_reply(&i.record))
                        .is_some();
                if scope_unproven && answered {
                    scope_unproven = false;
                    if let Some(learned) = scope.as_ref() {
                        match companion.set_default_flood_scope(Some(learned)) {
                            Ok(()) => route_log(&format!(
                                "scope {} proved by an answered request, written to the radio",
                                learned.name
                            )),
                            Err(e) => route_log(&format!(
                                "scope {} proved, but the radio refused the write ({e})",
                                learned.name
                            )),
                        }
                    }
                }
                if let Some(req) = frame_route_request(&frame, &settings.preset.public_key) {
                    // **A peer asking where we are.** The request is a flood, so its path is what it
                    // crossed to reach us; reversed, that is our route to the peer, which means
                    // neither end has to read the radio's advert cache to hold one.
                    // **A request for a route is proof the route in hand is dead.** The phone asks
                    // only when it cannot send, and it cannot send because nothing it sends arrives,
                    // so the path this end holds is not carrying anything either. Without this the
                    // shortest-path rule kept a dead one-hop route for the full two minutes of
                    // `PEER_ROUTE_STALE` while the peer asked every few seconds, and every answer
                    // went back down the chain that had just failed. Marking the route unheard makes
                    // the candidate from this request win, which is the route the phone can actually
                    // receive on.
                    peer_path_heard = None;
                    let candidate = PeerPath::reversed(
                        usize::from(req.path.hash_size),
                        usize::from(req.path.hops),
                        &req.path.bytes,
                    );
                    if let Some(candidate) = candidate
                        && prefer_route(peer_path.as_ref(), peer_path_heard, &candidate)
                    {
                        peer_path = Some(candidate);
                        peer_path_heard = Some(Instant::now());
                    }
                    // **A flood is not an identity.** The request carries only the requester's
                    // radio key and the destination is matched by one hash byte, so any node on the
                    // mesh can send one addressed to us; taking its key as *the* peer displaced the
                    // pinned one (and survived restarts), which is a denial of service against the
                    // real phone and a capture of where this host sends. The key is learned only
                    // while nothing is pinned, and a different one is answered but not adopted.
                    let pinned = settings.peer_key;
                    let adopt = pinned.is_none() || pinned == Some(req.peer_key);
                    if pinned.is_some() && pinned != Some(req.peer_key) {
                        route_log_rare(
                            &mut last_stranger_log,
                            &format!(
                                "request from {} ignored for routing: the peer is pinned",
                                &hex_key(&req.peer_key)[..8]
                            ),
                        );
                    }
                    peer_radio = pinned.or(Some(req.peer_key));
                    route_log(&format!(
                        "request from {} hops={}",
                        &hex_key(&req.peer_key)[..8],
                        req.path.hops
                    ));
                    if adopt && settings.peer_key != Some(req.peer_key) {
                        settings.peer_key = Some(req.peer_key);
                        if let Some(record) = settings.peer_record.as_deref() {
                            save_peer(record, &req.peer_key);
                        }
                    }
                    if last_reply.is_none_or(|t| t.elapsed() >= ROUTE_REPLY_FLOOR)
                        && left(budget, sent) != Some(0)
                    {
                        last_reply = Some(Instant::now());
                        let scope = reply_scope(&frame, scope.as_ref());
                        let ok = send_route_reply(
                            &mut companion,
                            &req,
                            &settings.preset.public_key,
                            scope.as_ref(),
                        );
                        route_log(&format!("reply sent ok={ok}"));
                        if ok {
                            sent = sent.saturating_add(1);
                            if let Some(path) = settings.window_record.as_deref() {
                                save_window(path, window_start_unix, sent);
                            }
                        }
                    }
                } else if let Some(view) = frame_packet(&frame)
                    && view.is_flood()
                    && view.payload_type == 0x00
                    // **The peer's answer to our own request, as a flood.** A direct reply depends on
                    // one chain of repeaters and this end's request may have crossed several, so the
                    // phone answers the way this end answers: flooded. Without this branch the answer
                    // was on the air and unread, which is the standoff it exists to end.
                    && let Some(key) = peer_radio
                    && let Some(packet) = frame.get(3..)
                    && let Some(candidate) = ldmux_companion::parse_route_reply_flood(packet, &key)
                    && let Some(reply) = PeerPath::reversed(
                        usize::from(candidate.hash_size),
                        usize::from(candidate.hops),
                        &candidate.bytes,
                    )
                {
                    // A request is proof the old route is dead, and this is its answer: the path the
                    // peer can be reached on now.
                    route_log(&format!("reply from a flood hops={}", reply.hops));
                    peer_path = Some(reply);
                    peer_path_heard = Some(Instant::now());
                } else if let Some(view) = frame_packet(&frame)
                    && view.is_flood()
                    && view.payload_type == 0x04
                    && peer_radio.is_some_and(|k| view.payload.starts_with(&k))
                {
                    // **An advert carries its own path, and the receive log shows it.** The radio's
                    // advert cache is 16 slots of RAM and evicts by age, so a route that has been
                    // heard can be gone by the time the daemon asks; the log fires for every packet
                    // before any protocol handling (`Dispatcher.cpp:196-199`), so a flooded advert
                    // from the peer is a route with no query and no eviction. Nothing is spent on
                    // the air: this is a packet that was already received.
                    if let Some(candidate) = PeerPath::reversed(
                        usize::from(view.path.hash_size),
                        usize::from(view.path.hops),
                        &view.path.bytes,
                    ) && prefer_route(peer_path.as_ref(), peer_path_heard, &candidate)
                    {
                        route_log(&format!("route from an advert hops={}", view.path.hops));
                        peer_path = Some(candidate);
                        peer_path_heard = Some(Instant::now());
                    }
                } else if let Some(path) = parse_advert_path(&frame) {
                    // A late answer to a request that timed out, under the same rule as every other
                    // candidate: the shortest path wins, and a zero-hop answer is the shortest.
                    if let Some(candidate) = PeerPath::reversed(
                        usize::from(path.hash_size),
                        usize::from(path.hops),
                        &path.bytes,
                    ) && prefer_route(peer_path.as_ref(), peer_path_heard, &candidate)
                    {
                        peer_path = Some(candidate);
                        peer_path_heard = Some(Instant::now());
                    }
                } else if let Some(incoming) = incoming_record(&frame) {
                    // The peer announcing which radio it is, so the route above can be looked up.
                    if let Some(key) = route_hint(&incoming.record) {
                        peer_radio = Some(key);
                        route_log(&format!(
                            "hint from {} route={}",
                            &hex_key(&key)[..8],
                            peer_path.is_some()
                        ));
                        // **A hint is already a working route.** It arrives as a direct record,
                        // which means the peer reaches this radio; an empty path is therefore the
                        // *correct* way to answer a direct neighbour, and the advert cache is re-read
                        // below for a path when the peer is not one. Nothing is asked here: the asker
                        // runs on its own timer, only while there is no route and traffic has moved.
                        //
                        // Remember who the peer is, so a restarted daemon can address it.
                        if settings.peer_key != Some(key) {
                            settings.peer_key = Some(key);
                            if let Some(record) = settings.peer_record.as_deref() {
                                save_peer(record, &key);
                            }
                        }
                        if refresh_peer_route(&mut companion, peer_radio.as_ref(), &mut peer_path, &mut peer_path_heard) {
                            last_route_ask = Some(Instant::now());
                        }
                    } else if let Some(reply) = route_reply(&incoming.record) {
                        // **The answer to our own route request.** It carries the path the peer
                        // uses towards us, so reversing it gives ours; both ends hold a route after
                        // one round trip, and the session's own records can follow.
                        let hops = reply.hops;
                        route_log(&format!("reply received hops={hops}"));
                        if prefer_route(peer_path.as_ref(), peer_path_heard, &reply) {
                            peer_path = Some(reply);
                            peer_path_heard = Some(Instant::now());
                        }
                    } else {
                        // **Learn the route again whenever the peer talks.** A link that is up can
                        // still hold a stale path: the phone walks, the session survives on
                        // retransmissions, and the first record of a *newly opened shell* then goes
                        // down a route that no longer exists. Re-reading the advert cache here costs
                        // one serial command, so the answer to that record uses the path that is
                        // true now.
                        if peer_radio.is_some()
                            && last_route_ask.is_none_or(|t| t.elapsed() >= ROUTE_REASK_FLOOR)
                        {
                            last_route_ask = Some(Instant::now());
                            refresh_peer_route(&mut companion, peer_radio.as_ref(), &mut peer_path, &mut peer_path_heard);
                        }
                        // **A record that arrived on the path is the proof the path works.** Nothing
                        // stamped this before, so `route_stale` aged out on a link that was busy in
                        // both directions, and the host re-flooded a route request every
                        // `PEER_ROUTE_STALE` for no reason. Arrival is stronger evidence than any
                        // timer: the peer's own record came down this path.
                        if peer_path.is_some() {
                            peer_path_heard = Some(Instant::now());
                        }
                        last_peer_record = Some(Instant::now());
                        // The frame's own reading: SNR in quarters of a dB, then RSSI in dBm.
                        if let (Some(&snr), Some(&rssi)) = (frame.get(1), frame.get(2)) {
                            peer_signal = Some((snr as i8, rssi as i8));
                        }
                        if events
                            .blocking_send(Ok(Event::Recv(incoming.record)))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = events.blocking_send(Err(MeshError::Io(std::io::Error::other(
                    e.to_string(),
                ))));
                return;
            }
        }
    }
}

/// The part of a radio's state that another app can change under us, comparable in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeenParams {
    freq_khz: u32,
    bw_hz: u32,
    sf: u8,
    cr: u8,
    tx_power_dbm: i8,
}

impl SeenParams {
    fn of(preset: &RadioPreset) -> Self {
        Self {
            freq_khz: (preset.freq_mhz * 1000.0).round() as u32,
            bw_hz: (preset.bw_khz * 1000.0).round() as u32,
            sf: preset.sf,
            cr: preset.cr,
            tx_power_dbm: preset.tx_power_dbm,
        }
    }
}

#[cfg(test)]
mod seen_params_tests {
    use super::{RadioPreset, SeenParams};

    fn preset() -> RadioPreset {
        RadioPreset {
            freq_mhz: 910.525,
            bw_khz: 62.5,
            sf: 7,
            cr: 5,
            tx_power_dbm: 22,
            hash_size: 1,
            public_key: [0; 32],
        }
    }

    #[test]
    fn a_radio_reporting_what_we_last_set_is_not_a_change() {
        let p = preset();
        // Through the wire's integer units and back: the snapshot must not "notice" its own values.
        assert_eq!(SeenParams::of(&p), SeenParams::of(&p));
        assert_eq!(SeenParams::of(&p).freq_khz, 910_525);
        assert_eq!(SeenParams::of(&p).bw_hz, 62_500);
    }

    #[test]
    fn a_retune_or_a_power_change_is_a_change() {
        let p = preset();
        let mut q = p;
        q.freq_mhz = 915.8;
        assert_ne!(SeenParams::of(&p), SeenParams::of(&q));
        let mut r = p;
        r.tx_power_dbm = 14;
        assert_ne!(SeenParams::of(&p), SeenParams::of(&r));
    }
}

/// The path a peer's record arrived by, reversed: the hops to reach *it*.
///
/// MeshCore appends one hash per repeater as a packet travels, so the path on an arriving packet is
/// this node's route *to the sender read backwards*. Reversing the hop order (each hop is
/// `hash_size` bytes) gives a usable path for the reply, which is what makes a phone behind a
/// repeater reachable at all: a direct packet with no path only travels as far as the sender's own
/// radio, so every reply to a repeater-reached phone would be heard by nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerPath {
    hash_size: u8,
    hops: u8,
    /// `hops * hash_size` bytes, in send order.
    bytes: Vec<u8>,
}

impl PeerPath {
    /// The reverse of a path that arrived, or `None` when the length is malformed or the path is
    /// longer than the firmware's 64-byte limit.
    ///
    /// **Zero hops is a path, not the absence of one.** "The peer is adjacent" and "no route is
    /// known yet" are different answers, and collapsing them into `None` made the two mechanisms
    /// that read it run as if nothing were known: the periodic read stayed on its 15-second retry
    /// cadence for ever on a bench (instead of settling to its 120-second refresh), and every route
    /// hint from an adjacent peer looked like a peer with no route and was allowed to trigger an
    /// advert. A zero-hop `PeerPath` is what a send to that peer uses — an empty path is exactly
    /// what "direct, no repeaters" means on the wire.
    ///
    /// **Hash size 4 is refused.** The encoded length byte has two bits for the hash width, giving
    /// 1..=4, and the firmware reserves 4 for the future: `Packet::isValidPathLen` rejects it
    /// outright (`src/Packet.cpp:13-18`), so a frame that claims it is malformed, not a wide path.
    fn reversed(hash_size: usize, hops: usize, path: &[u8]) -> Option<Self> {
        if !(1..=3).contains(&hash_size) || path.len() != hops * hash_size {
            return None;
        }
        if path.len() > MAX_PATH_SIZE {
            return None;
        }
        let mut bytes = Vec::with_capacity(path.len());
        for hop in path.chunks_exact(hash_size).rev() {
            bytes.extend_from_slice(hop);
        }
        Some(Self {
            hash_size: u8::try_from(hash_size).ok()?,
            hops: u8::try_from(hops).ok()?,
            bytes,
        })
    }
}

/// One received packet, reduced to the record inside it.
///
/// The guard is the header: only a packet LDMUX itself built (DIRECT with payload RAW_CUSTOM) can
/// be a record. Everything else on the air — adverts, route requests, trace routes, another
/// network's channel traffic — is not ours and is ignored rather than handed to the session, which
/// would have to reject it and would show the rejection as a defect.
///
/// **A flooded `RAW_CUSTOM` cannot be one of ours**: the firmware drops flood-routed raw packets
/// before the application sees them (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM` accepts only
/// `isRouteDirect()`), so a record always arrives direct.
///
/// **The path on a direct packet is consumed, not accumulated.** Each repeater removes its own hash
/// before retransmitting (`Mesh::removeSelfFromPath`), so a record that crossed any number of them
/// arrives with an empty path and teaches no route. That is why a route comes from a *flood*: the
/// peer's advert, or the route request and reply that replaced it for anything the advert cannot
/// reach.
#[derive(Debug, PartialEq, Eq)]
struct Incoming {
    record: Vec<u8>,
}

fn incoming_record(frame: &[u8]) -> Option<Incoming> {
    let view = frame_packet(frame)?;
    // Route DIRECT only: a flooded `RAW_CUSTOM` is dropped by every receiver before the application
    // sees it, so a flood-routed one cannot be a record.
    if view.route != 2 || view.payload_type != 0x0F {
        return None;
    }
    Some(Incoming {
        record: view.payload.to_vec(),
    })
}

/// The packet inside a receive-log frame, or `None` when the frame is not one.
///
/// `MyMesh.cpp:286-296`: `[0x88][snr×4][rssi][packet]`. **The receive log is the only view that
/// carries the path**: the decoded pushes describe a contact or a message and drop the accumulated
/// route, which is exactly what a reply needs.
fn frame_packet(frame: &[u8]) -> Option<ldmux_companion::PacketView<'_>> {
    if frame.first() != Some(&ldmux_companion::codes::LOG_RX_DATA) || frame.len() < 5 {
        return None;
    }
    parse_packet(frame.get(3..)?)
}

/// A carrier-level failure, as the loop's error type.
fn session_error(e: SessionError) -> MeshError {
    MeshError::Io(std::io::Error::other(e.to_string()))
}

/// A refused send, in the vocabulary the retry ladder understands.
fn refused(e: &SessionError) -> SendOutcome {
    match e {
        SessionError::Rejected { code, .. } => match code {
            2 => SendOutcome::ErrNotFound,
            6 => SendOutcome::ErrTooLong,
            _ => SendOutcome::ErrBusy,
        },
        // **Our own packet builder refusing the packet is not the radio's word.** It means the
        // record is larger than the path leaves room for — a sizing fault on this end, which the
        // retry ladder must not read as "the radio is busy" and retry unchanged for 30 s.
        SessionError::Codec(_) => SendOutcome::ErrTooLong,
        // A radio in another state is a radio to try again, not a fault to report.
        SessionError::Disabled => SendOutcome::ErrBusy,
        _ => SendOutcome::ErrBusy,
    }
}

/// Records of the full size that fit the allowance in an hour, **not** capped by the wire field.
///
/// The published count is one byte, so [`left`] clamps it to 255 — but the figure this end counts
/// against is the policy's own: at 62.5 kHz SF7 a ten-percent hour is 597 records, and clamping the
/// counter at 255 stopped the link at 43 % of its allowance while `mesh status` said "0 left" beside
/// an airtime figure that had minutes to run.
fn budget_per_hour(airtime_ms: f64, freq_mhz: f64) -> Option<u32> {
    let allowance = airtime_budget_ms(freq_mhz)?;
    if airtime_ms <= 0.0 {
        return None;
    }
    let n = (allowance / airtime_ms).floor();
    Some(u32::try_from(n as u64).unwrap_or(u32::MAX).max(1))
}

/// What is left of the hour, in the one byte the contract carries: capped, never zeroed by the cap.
///
/// `None` means there is no allowance to spend — an unregulated band — and it is published as
/// `None` too, because "no limit" and "no records left" are opposite facts.
fn left(budget: Option<u32>, sent: u32) -> Option<u8> {
    // `None` is "this band has no duty cycle", which is not the same fact as "zero left": the
    // session must be free to send, and the phone must not print a countdown for a limit that does
    // not exist.
    budget.map(|b| u8::try_from(b.saturating_sub(sent)).unwrap_or(u8::MAX))
}

/// The status a serial radio actually knows: no RSSI or battery reading, and a
/// budget computed from its own preset rather than reported by a gateway.
fn status(settings: &Settings, budget: Option<u32>, sent: u32) -> Status {
    let left = left(budget, sent);
    let tenths = (f64::from(sent) * settings.airtime_ms / 100.0).round();
    Status {
        radio_state: RadioState::Connected,
        rssi: UNKNOWN_RSSI,
        battery_percent: UNKNOWN_BATTERY,
        tx_airtime_tenths_this_hour: u16::try_from(tenths as u64).unwrap_or(u16::MAX),
        ops_left_this_hour: left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldmux_companion::{MemoryLink, build_raw_direct};

    fn log_frame(packet: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x88, 0xE0, 0xA1];
        frame.extend_from_slice(packet);
        frame
    }

    /// A scoped request with one repeater's hash spliced into its path.
    ///
    /// **One hop is the point**: a zero-hop flood was never forwarded, so a matching region in it
    /// proves nothing about the repeaters this end needs — that rule is what keeps a neighbour's
    /// tag from pinning the radio to a region the mesh does not carry.
    fn scoped_request_with_a_hop(
        peer: &[u8; 32],
        own: &[u8; 32],
        scope: &ldmux_companion::FloodScope,
        hop: u8,
    ) -> Vec<u8> {
        let mut packet =
            ldmux_companion::build_route_request_in_scope(1, peer, own, Some(scope)).unwrap();
        packet[5] = ldmux_companion::codec::path_byte(1, 1).unwrap();
        packet.insert(6, hop);
        packet
    }

    #[test]
    fn the_hours_spend_is_remembered_across_a_restart() {
        // **The allowance is 1 % of an hour, not 1 % of a process's life.** A restart used to hand
        // out a fresh 36 s; the record now carries the hour's start and what it has spent.
        let dir = std::env::temp_dir().join(format!("ldmux-airtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mesh-airtime");
        assert!(load_window(&path).is_none(), "nothing spent yet");
        let start = unix_now();
        save_window(&path, start, 41);
        assert_eq!(load_window(&path), Some((start, 41)), "what the hour spent");
        // A different hour is a different allowance: the caller compares the start against now.
        save_window(&path, start - HOUR.as_secs() - 1, 66);
        let (old, spent) = load_window(&path).unwrap();
        assert!(unix_now() - old > HOUR.as_secs(), "an hour ago is a spent window");
        assert_eq!(spent, 66, "and its count is still readable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_a_carrier_whose_thread_is_slow_does_not_block_the_loop() {
        // A real radio that keeps the port open and stops draining is not reproducible here,
        // so the shape is: a thread that has not signalled it stopped. `drop` must return
        // after the bounded wait, not wait for the thread.
        let (command_tx, command_rx) = channel();
        let (_event_tx, event_rx) = async_mpsc::channel(1);
        let (_stopped_tx, stopped_rx) = channel::<()>();
        let worker = std::thread::spawn(move || {
            // Hold the receiver so the sender stays connected, then outlive the wait.
            std::thread::sleep(Duration::from_millis(400));
            drop(command_rx);
        });
        let carrier = SerialCarrier {
            commands: Some(command_tx),
            events: event_rx,
            worker: Some(worker),
            stop: Arc::new(AtomicBool::new(false)),
            stopped: Some(stopped_rx),
        };
        let started = Instant::now();
        drop(carrier);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "drop waited {elapsed:?} on a thread that had not stopped"
        );
        assert!(elapsed >= CLOSE_WAIT, "it did give the thread its bounded wait");
    }

    #[test]
    fn a_direct_raw_packet_yields_its_record() {
        // What `build_raw_direct(1, &[], record)` produces: header, zero-hop path byte.
        let packet = build_raw_direct(1, &[], &[9, 8, 7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![9, 8, 7]);
    }

    #[test]
    fn a_two_byte_hash_path_is_skipped_before_the_record() {
        let packet = build_raw_direct(2, &[0xAA, 0xBB], &[1, 2]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![1, 2]);
    }

    /// **A path on a record is skipped, never learned from.** A direct packet loses one hash per
    /// repeater (`Mesh::removeSelfFromPath`), so what arrives has no route in it at all; the reply
    /// route comes from the peer's advert instead.
    #[test]
    fn a_path_on_a_record_is_skipped_before_the_record_and_not_learned() {
        // One-byte hashes, two hops, exactly what a sender writes before repeaters consume them.
        let packet = build_raw_direct(1, &[0x11, 0x22], &[7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![7], "the record starts after the path");

        // Two-byte hashes, two hops: the offset must count whole hops, not bytes.
        let packet = build_raw_direct(2, &[0xA1, 0xA2, 0xB1, 0xB2], &[7]).unwrap();
        let incoming = incoming_record(&log_frame(&packet)).unwrap();
        assert_eq!(incoming.record, vec![7]);
    }

    /// **The route that is learned is the reverse of what an advert carries.** Flooded packets
    /// append a hash per repeater, so the path read out of the advert cache is in travel order and
    /// the reply needs the hop groups the other way round.
    /// A `RESP_CODE_ADVERT_PATH` frame as the firmware writes it: the code, the advert's timestamp,
    /// the encoded path byte, then the hops in travel order.
    fn advert_path_frame(encoded: u8, hops: &[u8]) -> Vec<u8> {
        let mut v = vec![ldmux_companion::codes::ADVERT_PATH];
        v.extend(0u32.to_le_bytes());
        v.push(encoded);
        v.extend_from_slice(hops);
        v
    }

    /// **The route is learned from the radio, and a zero-hop answer clears it.**
    ///
    /// Driven through [`refresh_peer_route`] and a real [`Companion`] over [`MemoryLink`], not
    /// through `PeerPath::reversed` alone: the bug this covers was in the caller — a `None` from the
    /// reverse was ignored, so a phone that came home kept a repeater chain that no longer reached
    /// it and went deaf until the daemon restarted. A test of the helper alone could not see that.
    #[test]
    fn the_route_is_learned_and_a_zero_hop_answer_clears_it() {
        let peer = [7u8; 32];
        let mut link = MemoryLink::new();
        link.push_payload(&advert_path_frame(0x02, &[0x11, 0x22]));
        let mut companion = Companion::new(link);
        let mut path: Option<PeerPath> = None;
        let mut heard: Option<Instant> = None;
        assert!(
            refresh_peer_route(&mut companion, Some(&peer), &mut path, &mut heard),
            "an answer arrived"
        );
        let learned = path.clone().expect("two hops is a route");
        assert_eq!(learned.bytes, vec![0x22, 0x11], "reversed into sending order");
        assert_eq!((learned.hash_size, learned.hops), (1, 2));

        // The same phone, back in earshot: its advert arrives with no hops.
        companion.link_mut().push_payload(&advert_path_frame(0x00, &[]));
        assert!(
            refresh_peer_route(&mut companion, Some(&peer), &mut path, &mut heard),
            "an answer arrived"
        );
        let adjacent = path.clone().expect("no hops is an answer: the peer is adjacent");
        assert_eq!(adjacent.hops, 0, "the stale repeater chain is gone");
        assert!(adjacent.bytes.is_empty());
        // **Adjacent, not "unknown".** A `None` here would keep the periodic read on its retry
        // cadence for ever and let a route hint from a peer in the same room trigger an advert.
        assert!(
            path.is_some(),
            "zero hops is a route: it is what a direct send with an empty path means"
        );

        // ...and each request carries the whole key, which is the firmware's guard
        // (`len >= PUB_KEY_SIZE + 2`): two commands, each `[0x3C][u16 len][0x2A][0][key]`.
        let written = &companion.link_mut().written;
        assert_eq!(written.len(), 2 * (3 + 2 + 32), "two 34-byte command frames");
        for frame in written.chunks(3 + 2 + 32) {
            assert_eq!(frame[3], ldmux_companion::commands::GET_ADVERT_PATH);
            assert_eq!(frame[4], 0, "reserved");
            assert_eq!(&frame[5..], &peer, "the key, not a six-byte prefix");
        }
    }

    #[test]
    fn the_advert_path_is_reversed_by_hop_for_the_reply() {
        // One-byte hashes, two hops.
        let path = PeerPath::reversed(1, 2, &[0x11, 0x22]).expect("two hops is a route");
        assert_eq!(path.bytes, vec![0x22, 0x11]);
        assert_eq!((path.hash_size, path.hops), (1, 2));

        // Two-byte hashes: the pairs must not be split.
        let path = PeerPath::reversed(2, 2, &[0xA1, 0xA2, 0xB1, 0xB2]).expect("two hops is a route");
        assert_eq!(path.bytes, vec![0xB1, 0xB2, 0xA1, 0xA2]);
        assert_eq!((path.hash_size, path.hops), (2, 2));
    }

    /// **A region is adopted only on evidence, and then written to the radio.** One match of a
    /// 16-bit code across ~70 candidate names is a coin toss; two matches over different payloads
    /// settling on the same name is proof, and only then does anything change.
    ///
    /// Driven through `serve_loop` rather than the helpers, because the bug this guards is the
    /// wiring: a `continue` in the wrong place would drop the very request that arrived in the same
    /// frame, and a helper test could not see that.
    #[test]
    fn a_scope_is_adopted_only_after_two_scoped_floods_and_then_written_to_the_radio() {
        let own = [0x91u8; 32];
        let scope = ldmux_companion::FloodScope::from_name("us-co");
        // Two requests to *other* radios (so nothing replies), scoped to the same region and
        // carrying different payloads because the requester key differs.
        let first = scoped_request_with_a_hop(&[0x11; 32], &own, &scope, 0x31);
        let second = scoped_request_with_a_hop(&[0x22; 32], &[0x92u8; 32], &scope, 0x32);
        let mut link = MemoryLink::new();
        link.push_payload(&log_frame(&first));
        link.push_payload(&log_frame(&second));
        // The firmware's answer to the scope write, and to nothing else.
        link.push_payload(&[0x00]);
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(64);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 602.6,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                    preset: RadioPreset {
                        freq_mhz: 910.525,
                        bw_khz: 62.5,
                        sf: 7,
                        cr: 5,
                        tx_power_dbm: 22,
                        public_key: own,
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });

        // Read events until the scope arrives, bounded: statuses refresh on their own.
        let mut adopted = None;
        for _ in 0..40 {
            match event_rx.blocking_recv() {
                Some(Ok(Event::Scope(scope))) if scope.is_some() => {
                    adopted = scope.map(|s| s.name);
                    break;
                }
                Some(Ok(_)) => {}
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(
            adopted,
            Some("us-co".to_owned()),
            "two scoped floods over different payloads name the region"
        );
        drop(command_tx);
        worker.join().unwrap();
    }

    /// One scoped flood is not proof: nothing is adopted, and nothing is written to the radio.
    #[test]
    fn a_single_scoped_flood_changes_nothing() {
        let own = [0x91u8; 32];
        let scope = ldmux_companion::FloodScope::from_name("us-co");
        let request = scoped_request_with_a_hop(&[0x11; 32], &own, &scope, 0x31);
        let mut link = MemoryLink::new();
        link.push_payload(&log_frame(&request));
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(64);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 602.6,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                    preset: RadioPreset {
                        freq_mhz: 910.525,
                        bw_khz: 62.5,
                        sf: 7,
                        cr: 5,
                        tx_power_dbm: 22,
                        public_key: own,
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });
        // The opening scope is the only one this end will ever report: the loop opens with a
        // status, then the scope, and a single match must not add a second scope report.
        let deadline = std::time::Instant::now() + Duration::from_millis(900);
        let mut opened = None;
        let mut reports = 0;
        while std::time::Instant::now() < deadline {
            match event_rx.try_recv() {
                Ok(Ok(Event::Scope(scope))) => {
                    if opened.is_none() {
                        opened = Some(scope.is_none());
                    }
                    reports += 1;
                    assert!(scope.is_none(), "one match must not be adopted");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => panic!("carrier error {e:?}"),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert_eq!(opened, Some(true), "the opening report is unscoped");
        assert_eq!(reports, 1, "and it is the only one");
        drop(command_tx);
        worker.join().unwrap();
    }

    /// **A route request is answered by a flood carrying the path this end uses.** The request
    /// crossed repeaters to arrive, so the answer has to as well: a direct reply depends on one chain
    /// of repeaters and one lost hop is a phone that never learns a route. This is MeshCore's own
    /// rule for answering a flood (`ReplyRoute::REPLY_ROUTE_PATH_RETURN`), and the order on the wire
    /// has to be exact because the phone reads it out of its receive log.
    #[test]
    fn a_route_request_is_answered_along_the_path_it_arrived_by() {
        let peer = [0x42u8; 32];
        let own = [0x91u8; 32];
        // The peer's request arrived through two repeaters, one-byte hashes: 0x11 nearest the peer.
        let mut packet = ldmux_companion::build_route_request(1, &own, &peer).unwrap();
        packet[1] = ldmux_companion::codec::path_byte(1, 2).unwrap();
        packet.splice(2..2, [0x11, 0x22]);
        let req = frame_route_request(&log_frame(&packet), &own).expect("addressed to us");

        let mut link = MemoryLink::new();
        link.push_payload(&[0x00]); // the radio answers OK
        let mut companion = Companion::new(link);
        assert!(send_route_reply(&mut companion, &req, &own, None));

        // The frame the radio is handed: `[0x3C][u16 n][0x41][0][header][path byte][path][record]`.
        let written = companion.link_mut().written.clone();
        let body = &written[3..];
        assert_eq!(body[0], ldmux_companion::commands::SEND_RAW_PACKET);
        assert_eq!(body[1], 0, "priority");
        let view = parse_packet(&body[2..]).expect("a whole packet");
        assert_eq!(view.route, 1, "flood: the request crossed repeaters, so the answer does too");
        assert_eq!(view.payload_type, 0, "REQ, the vehicle a stock companion can flood");
        assert_eq!(view.path.hops, 0, "it starts here and each repeater appends its own hash");
        // The envelope is addressed to the peer, names this radio in full, and carries the reply.
        assert_eq!(view.payload[0], peer[0], "destination hash is the peer");
        assert_eq!(view.payload[1], own[0], "sender hash is this radio");
        assert_eq!(&view.payload[6..38], &own, "so the phone can tell its host from any other node");
        let reply = parse_route_reply(&view.payload[38..]).expect("a route reply");
        assert_eq!(reply.hops, 2);
        assert_eq!(reply.bytes, vec![0x22, 0x11], "the path we used, for the peer to reverse");
        // And the phone's own reader agrees with the builder, which is the contract that matters:
        // one end writes this layout and the other parses it out of a logged packet.
        let parsed = ldmux_companion::parse_route_reply_flood(&body[2..], &own).expect("reads back");
        assert_eq!(parsed.bytes, vec![0x22, 0x11]);
        assert_eq!((parsed.hash_size, parsed.hops), (1, 2));
    }

    /// **One rule for both directions.** The reply carries the sender's sending-order path and the
    /// reader reverses it, so a reply from the phone yields the host's route to it exactly as a
    /// reply from the host yields the phone's.
    #[test]
    fn a_route_reply_becomes_the_route_to_the_peer() {
        // What the peer wrote: the path it uses towards us, in sending order.
        let record = ldmux_companion::build_route_reply(1, 2, &[0x22, 0x11]).unwrap();
        let path = route_reply(&record).expect("a route");
        assert_eq!(path.bytes, vec![0x11, 0x22], "ours is the reverse");
        assert_eq!((path.hash_size, path.hops), (1, 2));
        // ...and a session record is not a route reply.
        assert!(route_reply(&[0x13, 1, 2, 3, 4, 5]).is_none());
    }

    /// A request that names another radio is not ours: answering it would flood a reply for a peer
    /// that never asked us, from every node in range at once.
    #[test]
    fn a_route_request_for_another_radio_is_not_ours() {
        let packet = ldmux_companion::build_route_request(1, &[0x42; 32], &[0x91; 32]).unwrap();
        assert!(frame_route_request(&log_frame(&packet), &[0x43; 32]).is_none());
        assert!(frame_route_request(&log_frame(&packet), &[0x42; 32]).is_some());
    }

    /// **A scope is followed, not invented.** The order is the radio's own answer, then an
    /// operator's pin, then nothing. `off` is a decision too, and it has to survive: an operator on
    /// a mesh where unscoped floods are better must be able to say so.
    #[test]
    fn a_scope_comes_from_the_radio_or_a_pin_and_never_from_a_guess() {
        let mut companion = Companion::new(MemoryLink::new());
        // SAFETY: these keys are read only by `ScopeChoice::resolve`, and the two tests that write
        // them take turns by owning the keys for their whole body.
        unsafe { std::env::remove_var("LDMUX_MESH_SCOPE") };
        let none = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(none.source, "none", "a radio that says nothing is unscoped");
        assert!(none.info().is_none());

        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_SCOPE", "off") };
        let off = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(off.source, "off");
        assert!(off.as_ref().is_none(), "off means every flood is unscoped");

        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_SCOPE", "#us-co") };
        let pinned = ScopeChoice::resolve(&mut companion, false);
        assert_eq!(pinned.source, "env");
        let scope = pinned.as_ref().expect("a pin is a scope");
        assert_eq!(scope.name, "us-co", "the leading # is not part of the name");
        assert_eq!(
            scope.key,
            ldmux_companion::FloodScope::from_name("us-co").key,
            "a pin derives the public key the firmware would"
        );
        // SAFETY: as above.
        unsafe { std::env::remove_var("LDMUX_MESH_SCOPE") };
    }

    /// A scoped request is what a mesh that denies unscoped floods needs, and the scope a flood
    /// proves is what this end adopts: the name is never on the air, so the code is the evidence.
    #[test]
    fn a_scoped_request_is_built_and_can_be_attributed() {
        let peer = [0x31u8; 32];
        let own = [0x72u8; 32];
        let scope = ldmux_companion::FloodScope::from_name("us-co");
        let packet = ldmux_companion::build_route_request_in_scope(1, &peer, &own, Some(&scope))
            .expect("builds");
        assert_eq!(packet[0], ldmux_companion::ROUTE_FLOOD_SCOPED_HEADER);
        let view = parse_packet(&packet).expect("a packet");
        assert!(view.transport_codes.is_some(), "a scoped flood carries its code");
        assert_eq!(
            ldmux_companion::identify_scope(&view, ldmux_companion::KNOWN_SCOPE_TAGS)
                .map(|s| s.name),
            Some("us-co".to_owned())
        );
        // The unscoped form is still a plain flood, and proves nothing.
        let plain = ldmux_companion::build_route_request(1, &peer, &own).expect("builds");
        let view = parse_packet(&plain).expect("a packet");
        assert!(view.transport_codes.is_none());
        assert!(ldmux_companion::identify_scope(&view, ldmux_companion::KNOWN_SCOPE_TAGS).is_none());
    }

    /// **The peer's key outlives a restart.** It is learned from a hint, a request or a reply, and
    /// without it a restarted daemon cannot address anything to the phone, so it falls back to
    /// waiting for an advert — which is the five-minute wait this change exists to remove.
    #[test]
    fn the_peers_radio_key_is_remembered_across_a_restart() {
        let dir = std::env::temp_dir().join(format!("ldmux-peer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mesh-peer");
        assert!(load_peer(&path).is_none(), "nothing recorded yet");
        let key = [0x5Au8; 32];
        save_peer(&path, &key);
        assert_eq!(load_peer(&path), Some(key), "what the last contact learned");
        std::fs::write(&path, "not a key").unwrap();
        assert!(load_peer(&path).is_none(), "a malformed record is no key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_route_hint_names_the_peers_radio() {
        let mut record = ROUTE_HINT_MAGIC.to_vec();
        record.extend_from_slice(&[7u8; 32]);
        assert_eq!(route_hint(&record), Some([7u8; 32]));
        // Anything else is a session record: a short one, a long one, or a different magic.
        assert_eq!(route_hint(&[0u8; 36]), None);
        assert_eq!(route_hint(&record[..35]), None);
        assert_eq!(route_hint(&[9u8; 37]), None);
    }

    #[test]
    fn another_protocols_packet_is_not_a_record() {
        // An advert: DIRECT route, payload type 4, version 0 -> 0x12.
        let advert = log_frame(&[0x12, 0x00, 0x01, 0x02]);
        assert_eq!(incoming_record(&advert), None);
    }

    #[test]
    fn a_frame_that_is_not_a_log_push_is_not_a_record() {
        assert_eq!(incoming_record(&[0x05, 0x01, 0x02, 0x03, 0x04]), None);
    }

    #[test]
    fn a_truncated_path_is_refused_rather_than_sliced_wrongly() {
        // Path byte says eight hops of two bytes; the packet has four bytes.
        let packet = vec![0x3E, 0b01_001000, 0x01, 0x02];
        assert_eq!(incoming_record(&log_frame(&packet)), None);
    }

    #[test]
    fn the_budget_comes_from_the_preset_and_the_band() {
        let eu = 869.525;
        let us = 910.525;
        // **The law where there is one, and nothing where there is not.** The US band, where the
        // MeshCore preset sits, has no duty cycle for this transmitter, so it has no allowance to
        // spend either — the meter is a measurement there, not a gate.
        assert_eq!(airtime_budget_ms(us), None);
        assert_eq!(airtime_budget_ms(eu), Some(36_000.0));
        // **The allowance is the law, or nothing.** 910.525 MHz is in no duty-cycled window, so
        // there is no cap at all; the European windows keep their own limits.
        assert_eq!(budget_per_hour(602.6, us), None);
        assert_eq!(budget_per_hour(75.3, us), None);
        assert_eq!(budget_per_hour(602.6, eu), Some(59));
        assert_eq!(budget_per_hour(602.6, 868.1), Some(59), "1 % by law");
        assert_eq!(budget_per_hour(602.6, 868.9), Some(5), "0.1 % by law");
        // The published byte saturates without ever looking spent.
        assert_eq!(left(Some(597), 0), Some(255));
        assert_eq!(left(Some(597), 500), Some(97));
        assert_eq!(left(Some(597), 597), Some(0));
        assert_eq!(left(None, 12_345), None, "no limit is not a countdown");
        // Twice the airtime is half the records, in a band that has a limit.
        assert!(budget_per_hour(7_315.5, eu) < budget_per_hour(602.6, eu));
    }

    #[test]
    fn a_refusal_maps_to_the_outcome_the_retry_ladder_expects() {
        assert_eq!(
            refused(&SessionError::Rejected { code: 2, meaning: "NOT_FOUND" }),
            SendOutcome::ErrNotFound
        );
        assert_eq!(
            refused(&SessionError::Rejected { code: 6, meaning: "ILLEGAL_ARG" }),
            SendOutcome::ErrTooLong
        );
        assert_eq!(
            refused(&SessionError::Rejected { code: 4, meaning: "BAD_STATE" }),
            SendOutcome::ErrBusy
        );
    }

    /// **The radio floods an advert so a peer can learn a path.** Without one, a phone behind a
    /// repeater has no route to this host at all: a direct raw packet with an empty path reaches
    /// only the radios in earshot.
    #[test]
    fn the_radio_advertises_flooded() {
        let mut link = MemoryLink::new();
        link.push_payload(&[0x00]); // OK for the advert
        let mut companion = Companion::new(link);
        companion.send_self_advert(true).unwrap();
        // `[0x3C][u16 len][0x07][1]`: CMD_SEND_SELF_ADVERT with the flood mode.
        let written = &companion.link_mut().written;
        assert_eq!(written[0], 0x3C, "frame start");
        let body = &written[3..];
        assert_eq!(body[0], ldmux_companion::commands::SEND_SELF_ADVERT);
        assert_eq!(body[1], 1, "flood, not zero-hop");
    }

    /// Off means off, and a malformed value falls back to the default rather than silently
    /// disabling adverts.
    #[test]
    fn the_advert_interval_is_configurable() {
        // SAFETY: this key is read only by `advert_interval`, and this test is the only place in
        // the binary that writes it, so there is nothing to race with.
        unsafe { std::env::remove_var("LDMUX_MESH_ADVERT_SECS") };
        assert_eq!(advert_interval(), None, "off by default, as a companion ships");
        // SAFETY: as above; this test owns the key for its whole body.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "0") };
        assert_eq!(advert_interval(), None, "off means off");
        // SAFETY: as above.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "60") };
        assert_eq!(advert_interval(), Some(Duration::from_secs(60)));
        // SAFETY: as above. A malformed value falls back to the default, which is off: a typo must
        // not turn a broadcast on.
        unsafe { std::env::set_var("LDMUX_MESH_ADVERT_SECS", "often") };
        assert_eq!(advert_interval(), None);
        // SAFETY: as above.
        unsafe { std::env::remove_var("LDMUX_MESH_ADVERT_SECS") };
    }

    /// **A record's size is a property of the route.** The builder needs `2 + path + record <= 174`,
    /// and the radio does not push a frame it cannot fit in 176 bytes, so the receive ceiling is one
    /// byte lower than the send ceiling. Both limits are pinned here because the arithmetic is the
    /// whole reason a direct link can carry 171 and a 20-hop one only 131.
    #[test]
    fn the_record_limit_is_the_path_and_the_receive_ceiling() {
        let path = |width: usize, hops: usize| PeerPath {
            hash_size: u8::try_from(width).unwrap_or(1),
            hops: u8::try_from(hops).unwrap_or(1),
            bytes: vec![0x11; width * hops],
        };
        assert_eq!(record_limit(None), 171, "no path: the receive ceiling");
        // One hop at one byte costs the record one byte, one at two costs two.
        assert_eq!(record_limit(Some(&path(1, 1))), 171);
        assert_eq!(record_limit(Some(&path(2, 1))), 170);
        // The band this mesh actually uses: 18-29 hops at two bytes.
        assert_eq!(record_limit(Some(&path(2, 20))), 132);
        assert_eq!(record_limit(Some(&path(2, 29))), 114);
        // The firmware's longest legal path is 64 bytes, which is the floor's reason to exist.
        assert_eq!(
            record_limit(Some(&path(2, 32))),
            u8::try_from(ldmux_mesh::RECORD_FLOOR).unwrap_or(255)
        );
        // A path cannot take the record below one byte, whatever a hostile path claims.
        assert_eq!(record_limit(Some(&path(3, 64))), 1);
    }

    /// **Shortest wins; stale is no route at all.** A flood is re-forwarded by every repeater in
    /// range, so the same request arrives again by longer paths — measured on the bench 2026-11-08:
    /// a phone in the same room logged at 0, then 1, then 4 hops — and taking the last one made
    /// every reply take a four-repeater detour.
    #[test]
    fn a_route_is_replaced_by_a_shorter_or_a_fresher_one() {
        let path = |hops: usize| PeerPath {
            hash_size: 2,
            hops: u8::try_from(hops).unwrap_or(1),
            bytes: vec![0x22; hops * 2],
        };
        let now = Instant::now();
        // Nothing in hand: anything is accepted.
        assert!(prefer_route(None, None, &path(4)));
        // A shorter path displaces a longer one, and equal stays.
        assert!(prefer_route(Some(&path(4)), Some(now), &path(0)));
        assert!(prefer_route(Some(&path(4)), Some(now), &path(4)));
        // A longer one does not, while the route in hand is fresh.
        assert!(!prefer_route(Some(&path(0)), Some(now), &path(4)));
        // Stale is no route at all: a longer path is accepted once two minutes have passed.
        let stale = now - PEER_ROUTE_STALE - std::time::Duration::from_secs(1);
        assert!(prefer_route(Some(&path(0)), Some(stale), &path(4)));
        // A route whose age was never recorded is treated as stale, not as fresh.
        assert!(prefer_route(Some(&path(0)), None, &path(4)));
    }

    /// **The countdown is republished while nothing is sent.** The session gates every send on it,
    /// so a status tied to transmission was a status that could never say the hour had rolled over:
    /// at zero the session polled nothing, the radio sent nothing, and the phone waited on a host
    /// whose budget would never come back (measured 2026-11-06, `held back 3959 times`).
    #[test]
    fn the_budget_is_republished_while_the_link_is_idle() {
        let companion = Companion::new(MemoryLink::new());
        let (_command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 137.9,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
        tx_power_dbm: 22,
                        public_key: [7u8; 32],
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });

        // The opening status, then another with no command in between: the refresh.
        let mut statuses = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        while statuses < 2 && Instant::now() < deadline {
            match event_rx.blocking_recv() {
                Some(Ok(Event::Status(_))) => statuses += 1,
                Some(Ok(_)) => {}
                other => panic!("expected a status, got {other:?}"),
            }
        }
        assert!(statuses >= 2, "the countdown refreshes on its own");
        drop(_command_tx);
        let _ = worker.join();
    }

    #[test]
    fn a_record_is_sent_and_its_answer_and_budget_are_reported() {
        let mut link = MemoryLink::new();
        link.push_payload(&[0x00]); // the radio accepts the raw send
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 602.6,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                    preset: RadioPreset {
                        // The European sub-band, where the allowance is one percent: 59 records an
                        // hour at this airtime, which fits the published byte and so shows the
                        // spend decrementing.
                        freq_mhz: 869.525,
                        bw_khz: 62.5,
                        sf: 7,
                        cr: 5,
                        tx_power_dbm: 14,
                        public_key: [7u8; 32],
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });

        command_tx
            .send(Command::Record { seq: 7, record: vec![1, 2, 3] })
            .unwrap();
        // A status first: the loop starts with no budget, and unknown is zero.
        match event_rx.blocking_recv() {
            Some(Ok(Event::Status(s))) => assert_eq!(s.ops_left_this_hour, Some(59)),
            other => panic!("expected an opening status, got {other:?}"),
        }
        // ...then the scope this node's floods are proved with (none here: the radio has no
        // region and nothing scoped was overheard), and only then the answer to the record.
        match event_rx.blocking_recv() {
            Some(Ok(Event::Scope(scope))) => assert!(scope.is_none(), "unscoped by default"),
            other => panic!("expected the opening scope, got {other:?}"),
        }
        // ...the largest record this path allows (no path yet, so the receiver's cap), and only
        // then the answer to the record.
        match event_rx.blocking_recv() {
            Some(Ok(Event::Limit { record_max: limit, .. })) => {
                assert_eq!(limit, RECEIVED_RECORD_MAX as u8, "a bare record, capped by the receiver")
            }
            other => panic!("expected the record limit, got {other:?}"),
        }
        match event_rx.blocking_recv() {
            Some(Ok(Event::SendResult(r))) => {
                assert_eq!(r.seq, 7);
                assert_eq!(r.outcome, SendOutcome::SentDirect);
            }
            other => panic!("expected a send result, got {other:?}"),
        }
        match event_rx.blocking_recv() {
            Some(Ok(Event::Status(s))) => assert_eq!(
                s.ops_left_this_hour,
                Some(58),
                "one record spent one of the hour's budget"
            ),
            other => panic!("expected a status after the send, got {other:?}"),
        }

        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_received_packet_reaches_the_loop_as_its_record() {
        let packet = build_raw_direct(1, &[], &[9, 8, 7]).unwrap();
        let mut link = MemoryLink::new();
        link.push_payload(&log_frame(&packet)); // framed as the radio frames it
        let companion = Companion::new(link);
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 137.9,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
        tx_power_dbm: 22,
                        public_key: [7u8; 32],
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });

        // The opening status comes first; the record follows on the next turn of
        // the loop, without any command being sent.
        let mut record = None;
        // **A budget, not a contract.** One pass publishes a status, the scope and the record limit
        // before the packet itself, and the loop refreshes a status every few seconds, so the record
        // is a handful of events in rather than the second one.
        for _ in 0..12 {
            match event_rx.blocking_recv() {
                Some(Ok(Event::Recv(r))) => {
                    record = Some(r);
                    break;
                }
                Some(Ok(_)) => continue,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(record, Some(vec![9, 8, 7]));

        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn letting_go_of_the_carrier_ends_the_radio_thread() {
        let companion = Companion::new(MemoryLink::new());
        let (command_tx, command_rx) = channel();
        let (event_tx, mut event_rx) = async_mpsc::channel(8);
        let worker = std::thread::spawn(move || {
            serve(
                companion,
                Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 137.9,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
        tx_power_dbm: 22,
                        public_key: [7u8; 32],
                        hash_size: 1,
                    },
                },
                command_rx,
                event_tx,
                Arc::new(AtomicBool::new(false)),
            );
        });
        let _ = event_rx.blocking_recv(); // the opening status
        drop(command_tx);
        worker.join().unwrap();
    }

    #[test]
    fn a_status_carries_a_budget_so_the_loop_can_send() {
        let settings = Settings {
                    hash_size: 1,
                    repeat_enabled: None,
                    airtime_ms: 137.9,
                    window_record: None,
                    peer_record: None,
                    peer_key: None,
                    scope: ScopeChoice { scope: None, source: "none" },
                preset: RadioPreset {
                        freq_mhz: 914.25,
                        bw_khz: 500.0,
                        sf: 8,
                        cr: 5,
        tx_power_dbm: 22,
                        public_key: [7u8; 32],
                        hash_size: 1,
                    },
                };
        let status = status(&settings, Some(255), 0);
        assert_eq!(status.ops_left_this_hour, Some(255));
        assert_eq!(status.radio_state, RadioState::Connected);
    }
}
