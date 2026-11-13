//! The daemon as the version-15 service sees it, and the two pure rules that outlived the frame
//! loop.
//!
//! **The frame loop is gone** (`docs/RADIO-PLAN.md` §10a). What used to be here — a differ, a hold,
//! a coalescing window, a retry of unsent rows, a page byte — existed to stream a screen over a
//! byte-stream session. A version-15 `Snapshot` is the whole screen, so none of it has a subject.
//! What is left is the part that was never about the transport:
//!
//! * [`DaemonHost`], the daemon's own calls, which is the seam the service asks through;
//! * [`paste_bytes`], the one rule for what a paste is on a pty;
//! * [`history_page`], the keeper's line arithmetic, which is where the bug was;
//! * [`BridgeStats`], the counters `mesh status` prints.

use std::sync::Arc;

use ldmux_protocol::keeper::SnapshotResult;
use ldmux_protocol::mesh_term::Screen;

use crate::mesh_rpc::{ClaimOutcome, MeshHost};

/// The device id this link's claims are recorded under.
///
/// The claim table spells the radio route `"mesh"`, and a desk window that sees that holder knows
/// the phone has the shell. One constant so the three places that compare it cannot disagree.
pub const MESH_DEVICE: &str = "mesh-link";

/// What the service has done, for `mesh status`.
///
/// Published by the link task ([`crate::mesh::MeshRuntime::publish_bridge`]); zero until the first
/// message arrives. The counters that matter are the ones that tell a blank screen apart from a
/// host that could not answer: `host_unanswered` is a failed call, which is not "no shells".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BridgeStats {
    /// Messages handed to the transaction layer.
    pub messages_sent: u32,
    /// Commands written to a pty.
    pub inputs: u32,
    /// Key names encoded for a pane.
    pub keys: u32,
    /// Grids applied to a pane.
    pub resizes: u32,
    /// Messages that would not decode.
    pub malformed: u32,
    /// Shells the phone started, ended, and put on the desk.
    pub created: u32,
    pub closed: u32,
    pub made_live: u32,
    /// Listing or liveness questions the host could not answer. Counted, and answered with
    /// **silence**: an empty list is a fact ("no shells are running") and a failed call is not, so
    /// sending one for the other told the phone its shells had all ended (measured 2026-11-03: the
    /// launcher emptied on a drop while every shell was alive).
    pub host_unanswered: u32,
    /// Writes refused because another device had taken the shell. Counted because it is the one
    /// visible sign of a takeover from the mesh side: the phone sees `HELD`, the desk sees its own
    /// claim, and this is what says the rule fired.
    pub held_writes: u32,
    /// Claims the host refused to give back. A claim that cannot be released keeps the shell held by
    /// a device that is not there, which is the state that stops a desk window reclaiming it.
    pub release_failed: u32,
}

/// What the pane is owed for one `Input` record: a paste, encoded for its own paste mode.
///
/// Typed text passes through unchanged. Text carrying a newline is a paste, and there are only
/// two honest answers for it:
///
///  - **the pane negotiated bracketed paste** — wrap it in `CSI 200 ~` … `CSI 201 ~`, and the
///    program inserts every line at the prompt instead of running them. ESC is stripped first:
///    a clipboard that contains `ESC [ 2 0 1 ~` would otherwise end the paste early and the rest
///    would run as commands (the classic paste injection);
///  - **it did not** — give it carriage returns. There is no way to hand such a program a
///    newline without its being Enter, so the paste is typed, line by line, which is what every
///    terminal does when a program has not asked for the markers.
///
/// `CRLF` is folded to `LF` first: a paste copied on a system that writes both would otherwise
/// arrive as a carriage return followed by a newline, i.e. a blank line after every line.
///
/// **A newline is the paste marker, and a carriage return is not.** Typed Enter arrives as `\r`,
/// so treating `\r` as a paste would wrap the person's own Enter in the markers — the shell would
/// insert a newline into the line instead of running it. A clipboard whose lines end in bare `\r`
/// therefore still types itself line by line, which is what a terminal does when the markers are
/// absent; anything that ends lines the ordinary way is a paste and is inserted.
#[must_use]
pub fn paste_bytes(bytes: &[u8], bracketed_paste: bool) -> Vec<u8> {
    if !bytes.contains(&b'\n') {
        return bytes.to_vec();
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.replace("\r\n", "\n");
    if bracketed_paste {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.bytes().filter(|b| *b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}


/// Slice a history page out of a keeper snapshot, in the keeper's own line numbering.
///
/// **Three counts, and confusing them is a silently empty page.** `total` is every line the pane
/// has (history first, live last); `available` is how many history lines exist; `captured` is how
/// many of them the keeper put in `text`, which is `available` clipped to what was asked for. The
/// live screen is the last `rows` lines of `total`.
///
/// A request above the live screen serves only history; one that reaches the live screen is clamped
/// back so a page never re-sends rows the phone already has; one older than what was fetched serves
/// from the oldest line in hand, so the phone's next press walks up from there rather than skipping.
///
/// A pure function because this is where the bug was: with the measured pane (`total 9912`,
/// `available 9895`, `rows 17`) the old arithmetic used `available`'s slot for `captured` and
/// answered every request with nothing.
fn history_page(
    total: u32,
    rows: u32,
    captured: u32,
    available: u32,
    from: u32,
    want: u32,
    lines: &[String],
) -> (u32, bool, Vec<String>) {
    let live_top = total.saturating_sub(rows);
    // **`historySize` and `historyLines` count lines above the live screen**, not lines from the
    // top of the pane: the keeper's numbering is `[live_top - history_size, live_top)` for what
    // exists and `[live_top - history_lines, live_top)` for what it sent. Subtracting from `total`
    // instead puts both counters `rows` too high, which is a page that misses by a screen — seen
    // on the device as `history 9598+17 -> 9615`.
    let oldest_captured = live_top.saturating_sub(captured);
    let oldest_available = live_top.saturating_sub(available);
    let start = from.max(oldest_captured).min(live_top);
    let end = start.saturating_add(want).min(live_top);
    // **A row is clamped to what a message can carry.** The wire's line field is one byte and the
    // protocol's segment bound is 120, so a wider pane would have its page silently shortened by a
    // failed encode; the phone's own grid is never wider than this anyway.
    let out = if end > start {
        let a = start.saturating_sub(oldest_captured) as usize;
        let b = end.saturating_sub(oldest_captured) as usize;
        lines
            .get(a..b)
            .map(|slice| {
                slice
                    .iter()
                    .map(|l| ldmux_protocol::mesh_term::clamp_row(l))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (start, start > oldest_available, out)
}


#[derive(Clone)]
pub struct DaemonHost(pub Arc<tokio::sync::Mutex<crate::daemon::LdmuxDaemon>>);

impl MeshHost for DaemonHost {
    async fn listing(&mut self) -> Option<Vec<ldmux_protocol::mesh_term::SessionSummary>> {
        // **The reason a listing failed was thrown away.** `ok()?` turned a keeper error, a busy
        // daemon and a malformed reply into the same `None`, and the phone saw only that no answer
        // came. Measured 2026-11-13: a List acknowledged by the host and never answered while the
        // desk showed seventeen shells. A journal line is the difference between "the host has no
        // shells" and "the host could not ask".
        let v = match self
            .0
            .lock()
            .await
            .handle("list", serde_json::json!({}))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("ldmuxd: the mesh list could not be read: {e}");
                return None;
            }
        };
        let rows = v
            .get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("status").and_then(|x| x.as_str()) == Some("running"))
                    .filter_map(|s| {
                        let id = s.get("id").and_then(|x| x.as_str())?;
                        let label = s.get("title").and_then(|x| x.as_str()).unwrap_or("");
                        // The daemon's `list` already describes the pane's live directory (the
                        // keeper's fact, not the directory the session was created in).
                        let cwd = s.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
                        // Who started it (`SessionInfo.created_by`): "phone", "desktop", "local".
                        // The mesh launcher shows the phone's own shells' actions, so it needs the
                        // same fact the tailnet launcher has always had.
                        let origin = s.get("createdBy").and_then(|x| x.as_str()).unwrap_or("");
                        // Who holds it now (`owner.kind`): `desktop`, `phone`, `mesh`, or absent.
                        // A shell with a holder is one this phone must ask to take.
                        let holder = s
                            .get("owner")
                            .and_then(|o| o.get("kind"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        // The shell the session was created with (`bash`), for the row's second
                        // line: the folder and the shell, always.
                        let shell = s.get("shell").and_then(|x| x.as_str()).unwrap_or("");
                        Some(ldmux_protocol::mesh_term::SessionSummary {
                            id: id.to_owned(),
                            label: label.to_owned(),
                            cwd: cwd.to_owned(),
                            origin: origin.to_owned(),
                            owner: holder.to_owned(),
                            shell: shell.to_owned(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Some(rows)
    }

    async fn marks(&mut self) -> Vec<(String, u8)> {
        let v = self
            .0
            .lock()
            .await
            .handle("marks", serde_json::json!({}))
            .await;
        let Ok(v) = v else { return Vec::new() };
        v.get("marks")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let id = m.get("session").and_then(|x| x.as_str())?.to_owned();
                        // A kind this build does not know is skipped, not folded into `done`: a
                        // word added later would otherwise arrive as the wrong mark instead of as
                        // no mark.
                        let kind = match m.get("kind").and_then(|x| x.as_str()) {
                            Some("done") => 0,
                            Some("attention") => 1,
                            _ => return None,
                        };
                        Some((id, kind))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn holder(&mut self, id: &str) -> Option<String> {
        let v = self
            .0
            .lock()
            .await
            .handle("holder", serde_json::json!({ "id": id }))
            .await
            .ok()?;
        Some(
            v.get("holder")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_owned(),
        )
    }

    async fn history(&mut self, id: &str, from: u32, rows: u8) -> Option<(u32, bool, Vec<String>)> {
        // **Two reads, because the keeper's `historyLines` counts back from the live screen.**
        // Asking for "a page plus a margin" is wrong the moment the operator scrolls further back
        // than the margin: the keeper would return the newest N lines above live, the page would
        // not join what the client holds, and the missing rows would go unnoticed. The first read
        // is the geometry (`totalRows`, `rows`, `historySize`), which the keeper reports whether or
        // not it sends any history; the second asks for exactly the lines between `from` and the
        // live screen. Both are local socket calls, so the extra one costs microseconds and not a
        // single byte of airtime.
        let want = u32::from(rows.max(1));
        let info: SnapshotResult = serde_json::from_value(
            self.0
                .lock()
                .await
                .handle("snapshot", serde_json::json!({ "id": id }))
                .await
                .ok()?,
        )
        .ok()?;
        let total = u32::try_from(info.total_rows).unwrap_or(u32::MAX);
        let live_top = total.saturating_sub(u32::try_from(info.rows).unwrap_or(0));
        let available = u32::try_from(info.history_size).unwrap_or(0);
        // **Nothing to serve, but two different reasons, and `older` tells them apart.** A request
        // at or past the live screen never receives live rows, and whether the pane still holds
        // history above that point is exactly what the caller needs to know: `older` false means
        // "you are at the oldest row this shell kept", true means "ask again lower down".
        //
        // The test is "does this pane have any history at all", because the request is already at
        // the live screen: `live_top > total - available` reads as "more history than one screen",
        // which is a different question and answers `false` for a pane that kept less than a
        // screenful.
        let above = available > 0;
        if from >= live_top {
            return Some((live_top, above, Vec::new()));
        }
        let need = live_top.saturating_sub(from).min(available);
        if need == 0 {
            return Some((live_top, above, Vec::new()));
        }
        let snap: SnapshotResult = serde_json::from_value(
            self.0
                .lock()
                .await
                .handle(
                    "snapshot",
                    serde_json::json!({ "id": id, "historyLines": need }),
                )
                .await
                .ok()?,
        )
        .ok()?;
        // **Second answer, second geometry.** The pane may have scrolled between the two reads, and
        // this snapshot is the one the text belongs to, so every count is taken from it.
        let all: Vec<String> = snap
            .text
            .split('\n')
            .map(ldmux_protocol::mesh_term::plain_row)
            .collect();
        Some(history_page(
            u32::try_from(snap.total_rows).unwrap_or(u32::MAX),
            u32::try_from(snap.rows).unwrap_or(0),
            u32::try_from(snap.history_lines).unwrap_or(0),
            u32::try_from(snap.history_size).unwrap_or(0),
            from,
            want,
            &all,
        ))
    }

    async fn screen(&mut self, id: &str) -> Result<Screen, String> {
        let v = self
            .0
            .lock()
            .await
            .handle("snapshot", serde_json::json!({ "id": id }))
            .await?;
        let s: SnapshotResult = serde_json::from_value(v).map_err(|e| e.to_string())?;
        Ok(Screen::from_keeper(
            &s.text,
            s.cols,
            s.rows,
            (s.cursor_x, s.cursor_y),
            s.alternate_on,
            s.dead,
            // The keeper numbers its lines: history first, then the live screen. A keeper too
            // old to say reports nothing, and a phone treats that as "no line identity".
            s.total_rows.saturating_sub(s.rows).min(u64::from(u32::MAX)) as u32,
        )
        // The pane's own answer to "may a paste be given newlines": see [`Screen`].
        .with_paste_mode(s.bracketed_paste))
    }

    async fn input(&mut self, id: &str, bytes: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "input is not UTF-8".to_owned())?;
        self.0
            .lock()
            .await
            .handle("send", serde_json::json!({ "id": id, "text": text }))
            .await
            .map(|_| ())
    }

    /// The same `send` the CLI and the tailnet clients use: it reads the pane's cursor-key mode
    /// and Kitty flags from the keeper and encodes each name with `key_name_to_bytes_in_mode`,
    /// refusing a name it does not know instead of typing it into the shell.
    async fn keys(&mut self, id: &str, names: &[String]) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("send", serde_json::json!({ "id": id, "keys": names }))
            .await
            .map(|_| ())
    }

    async fn resize(&mut self, id: &str, cols: u8, rows: u8) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle(
                "resizePane",
                serde_json::json!({ "id": id, "cols": cols, "rows": rows }),
            )
            .await
            .map(|_| ())
    }

    async fn claim(&mut self, id: &str, takeover: bool) -> Result<ClaimOutcome, String> {
        let v = self
            .0
            .lock()
            .await
            .handle(
                "claim",
                serde_json::json!({
                    "id": id,
                    "device": { "id": MESH_DEVICE, "name": "the mesh link", "kind": "mesh" },
                    "takeover": takeover,
                }),
            )
            .await?;
        if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            Ok(ClaimOutcome::Claimed)
        } else {
            Ok(ClaimOutcome::Held {
                holder: v
                    .get("holder")
                    .and_then(|h| h.as_str())
                    .unwrap_or("")
                    .to_owned(),
            })
        }
    }

    async fn release(&mut self, id: &str) -> bool {
        let answer = self
            .0
            .lock()
            .await
            // A bare device id, not the device object `reclaim` takes: the daemon's `release`
            // reads `device` as a string, so the object the bridge used to send parsed as `""` and
            // every release returned `{"ok":false}` — the phone let go and the claim was kept for
            // ever, which is why the desk window never got its shell (and its size) back.
            .handle("release", serde_json::json!({ "id": id, "device": MESH_DEVICE }))
            .await;
        match answer {
            Ok(_) => true,
            Err(e) => {
                // Said out loud here; counted by the bridge, which owns the counters. A claim that
                // cannot be given back keeps a desk window from ever reclaiming the shell, and the
                // bug that caused it once (a device object where a string was expected) was silent
                // because this answer was discarded.
                eprintln!("ldmuxd: mesh: release {id} refused by the host: {e}");
                false
            }
        }
    }

    /// The same `new` the tailnet's WebSocket handler calls, with `createdBy` set to the phone so the
    /// record says who started it. That field is what the launcher reads to keep a phone-opened
    /// shell's actions (close, open on the desktop) after the app restarts, instead of letting the
    /// row pass for a desk shell.
    async fn create(&mut self, cwd: &str, cols: u8, rows: u8) -> Result<String, String> {
        let cwd = if cwd.is_empty() { "." } else { cwd };
        let v = self
            .0
            .lock()
            .await
            .handle(
                "new",
                serde_json::json!({
                    "kind": "shell",
                    "cwd": cwd,
                    "createdBy": "phone",
                    "cols": cols,
                    "rows": rows,
                }),
            )
            .await?;
        v.get("session")
            .and_then(|s| s.get("id"))
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .ok_or_else(|| "the host did not say which shell it made".to_owned())
    }

    /// The daemon's `close`: a shell a desk window holds is put down, one nobody holds is ended.
    /// Destroying a desk shell from a phone is not this app's decision to make (ADR-0010).
    async fn close(&mut self, id: &str) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("close", serde_json::json!({ "id": id }))
            .await
            .map(|_| ())
    }

    async fn make_live(&mut self, id: &str) -> Result<(), String> {
        self.0
            .lock()
            .await
            .handle("makeLive", serde_json::json!({ "id": id, "workspace": "" }))
            .await
            .map(|_| ())
    }
}

