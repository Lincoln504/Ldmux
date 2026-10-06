//! Desktop inventory: what is on this machine's desktop, and which of it is a pi
//! session.
//!
//! Port of `packages/daemon/src/desktop-inventory.ts` (671 lines). The launcher is
//! a menu of *places to go*: the desktop workspaces, the windows in them, and
//! the pi agent sessions running inside those windows.
//!
//! The shape is the shared wire contract (`DesktopInventory`, `DesktopPiSession`,
//! `DesktopWindow`, `DesktopWorkspace`), defined once in the protocol package.
//!
//! Window managers are probed in order, and the first that answers wins. The i3
//! and sway trees share one shape, so they share one walker. EWMH (`xprop`) is
//! the fallback and the reason GNOME/KDE/XFCE on X11 work at all.
//!
//! A pi process cannot be asked what session it is (it rewrites its own `comm`),
//! so the session is found the way pi itself lays it out:
//! `~/.pi/agent/sessions/--<cwd with / → ->--`, newest transcript first. A window
//! is linked to a pi session by *process ancestry*: walk the pi process's
//! parents until one of them is the pid the WM reports for a window.
//!
//! Everything that parses is pure and testable. Only `read_desktop()` and
//! `scan_pi_sessions()` touch the machine.

use std::collections::HashMap;

use serde::Deserialize;

use ldmux_protocol::rpc::{DesktopInventory, DesktopPiSession, DesktopWindow, DesktopWorkspace};

// ── window-manager trees: i3 and sway ───────────────────────────────────────

/// One node in the i3/sway tree (a loose shape, serde-parsed).
/// The JSON uses snake_case (`app_id`, `window_properties`, `floating_nodes`).
#[derive(Deserialize, Default)]
struct I3LikeNode {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    window: Option<u64>,
    #[serde(default)]
    pid: Option<u64>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    window_properties: Option<WindowProperties>,
    #[serde(default)]
    nodes: Vec<I3LikeNode>,
    #[serde(default)]
    floating_nodes: Vec<I3LikeNode>,
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct WindowProperties {
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

fn class_of(node: &I3LikeNode) -> String {
    node.app_id
        .clone()
        .or_else(|| {
            node.window_properties
                .as_ref()
                .and_then(|w| w.class.clone())
        })
        .unwrap_or_default()
}

fn title_of(node: &I3LikeNode) -> String {
    node.window_properties
        .as_ref()
        .and_then(|w| w.title.clone())
        .or_else(|| node.name.clone())
        .unwrap_or_default()
}

/// Flatten an i3/sway tree into workspaces and the windows on them.
///
/// Recursive on purpose: a container can nest arbitrarily deep, and the
/// workspace a window belongs to is whatever workspace node encloses it.
pub fn walk_i3_tree(root: &serde_json::Value) -> (Vec<DesktopWorkspace>, Vec<DesktopWindow>) {
    let mut workspaces: Vec<DesktopWorkspace> = Vec::new();
    let mut windows: Vec<DesktopWindow> = Vec::new();
    let mut counts: HashMap<String, u64> = HashMap::new();

    fn visit(
        node: &I3LikeNode,
        workspace: &str,
        workspaces: &mut Vec<DesktopWorkspace>,
        windows: &mut Vec<DesktopWindow>,
        counts: &mut HashMap<String, u64>,
    ) {
        let mut ws = workspace;
        if node.kind.as_deref() == Some("workspace") {
            let name = node.name.as_deref().unwrap_or(workspace);
            ws = name;
            workspaces.push(DesktopWorkspace {
                name: name.to_string(),
                index: name.parse::<i64>().ok(),
                focused: false,
                visible: false,
                window_count: 0,
            });
        }
        if node.window.is_some_and(|w| w != 0) {
            let w = node.window.unwrap();
            windows.push(DesktopWindow {
                id: format!("0x{:x}", w),
                app_class: class_of(node),
                title: title_of(node),
                workspace: ws.to_string(),
                pid: node.pid.map(|p| p as i64),
            });
            *counts.entry(ws.to_string()).or_insert(0) += 1;
        }
        for child in node.nodes.iter().chain(node.floating_nodes.iter()) {
            visit(child, ws, workspaces, windows, counts);
        }
    }

    if let Ok(root_node) = serde_json::from_value::<I3LikeNode>(root.clone()) {
        visit(&root_node, "", &mut workspaces, &mut windows, &mut counts);
    }

    for ws in &mut workspaces {
        ws.window_count = counts.get(&ws.name).copied().unwrap_or(0);
    }

    (workspaces, windows)
}

/// Merge `get_workspaces` flags (focused/visible) into a walked tree.
pub fn merge_i3_workspace_flags(
    walked: Vec<DesktopWorkspace>,
    reported: &serde_json::Value,
) -> Vec<DesktopWorkspace> {
    let by_name: HashMap<String, (bool, bool)> = reported
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|w| {
                    let name = w.get("name")?.as_str()?.to_string();
                    let focused = w.get("focused").and_then(|v| v.as_bool()).unwrap_or(false);
                    let visible = w.get("visible").and_then(|v| v.as_bool()).unwrap_or(false);
                    Some((name, (focused, visible)))
                })
                .collect()
        })
        .unwrap_or_default();

    walked
        .into_iter()
        .map(|ws| {
            if let Some((focused, visible)) = by_name.get(&ws.name) {
                DesktopWorkspace {
                    focused: *focused,
                    visible: *visible,
                    ..ws
                }
            } else {
                ws
            }
        })
        .collect()
}

// ── niri / hyprland / EWMH parsers ───────────────────────────────────────────

/// niri speaks JSON if asked.
pub fn parse_niri(
    workspaces_json: &serde_json::Value,
    windows_json: &serde_json::Value,
) -> (Vec<DesktopWorkspace>, Vec<DesktopWindow>) {
    let raw_ws = workspaces_json.as_array().cloned().unwrap_or_default();
    let raw_win = windows_json.as_array().cloned().unwrap_or_default();

    let windows: Vec<DesktopWindow> = raw_win
        .iter()
        .map(|w| DesktopWindow {
            id: w.get("id").map(|v| v.to_string()).unwrap_or_default(),
            app_class: w
                .get("app_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            title: w
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            workspace: w
                .get("workspace_id")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            pid: w.get("pid").and_then(|v| v.as_u64()).map(|v| v as i64),
        })
        .collect();

    let workspaces: Vec<DesktopWorkspace> = raw_ws
        .iter()
        .map(|w| {
            let name = w
                .get("name")
                .or_else(|| w.get("idx"))
                .map(|v| v.to_string())
                .unwrap_or_default();
            let ws_id = w.get("id").map(|v| v.to_string()).unwrap_or_default();
            DesktopWorkspace {
                name: name.clone(),
                index: w.get("idx").and_then(|v| v.as_i64()),
                focused: w
                    .get("is_focused")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                visible: w
                    .get("is_active")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                window_count: windows.iter().filter(|x| x.workspace == ws_id).count() as u64,
            }
        })
        .collect();

    (workspaces, windows)
}

/// Hyprland's `hyprctl -j` is already JSON, twice over.
///
/// A workspace object does not say whether it is focused — that is a separate
/// `activeworkspace` call — so [active] (its `id`) is what marks one.
pub fn parse_hyprland(
    workspaces_json: &serde_json::Value,
    clients_json: &serde_json::Value,
    active: Option<&serde_json::Value>,
) -> (Vec<DesktopWorkspace>, Vec<DesktopWindow>) {
    let raw_ws = workspaces_json.as_array().cloned().unwrap_or_default();
    let raw_win = clients_json.as_array().cloned().unwrap_or_default();
    let active_id = active.and_then(|a| a.get("id")).and_then(|v| v.as_i64());

    let windows: Vec<DesktopWindow> = raw_win
        .iter()
        .map(|w| DesktopWindow {
            id: w.get("address").map(|v| v.to_string()).unwrap_or_default(),
            app_class: w
                .get("class")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            title: w
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            workspace: w
                .get("workspace")
                .and_then(|w| w.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            pid: w.get("pid").and_then(|v| v.as_u64()).map(|v| v as i64),
        })
        .collect();

    let workspaces: Vec<DesktopWorkspace> = raw_ws
        .iter()
        .map(|w| {
            let name = w
                .get("name")
                .or_else(|| w.get("id"))
                .map(|v| v.to_string())
                .unwrap_or_default();
            let id = w.get("id").and_then(|v| v.as_i64());
            DesktopWorkspace {
                name,
                index: id,
                focused: id.is_some() && id == active_id,
                visible: w
                    .get("monitor")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
                window_count: w.get("windows").and_then(|v| v.as_u64()).unwrap_or(0),
            }
        })
        .collect();

    (workspaces, windows)
}

/// Parse `xprop -root _NET_CLIENT_LIST`.
///
/// EWMH is the standard every X11 desktop implements, which is what makes
/// GNOME, KDE and XFCE work without three more probes: they do not expose a
/// tree, but they all publish their windows here.
pub fn parse_ewmh_client_list(stdout: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let bytes = stdout.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'0' && i + 1 < bytes.len() && bytes[i + 1] == b'x' {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            ids.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
        } else {
            i += 1;
        }
    }
    // Deduplicate, preserving order.
    let mut seen = std::collections::HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids
}

/// Parse the `xprop -id <id>` output for one window.
pub fn parse_ewmh_window(id: &str, stdout: &str) -> Option<DesktopWindow> {
    let line = |key: &str| -> String {
        stdout
            .lines()
            .find(|l| l.starts_with(key) && l.contains('='))
            .and_then(|l| {
                let eq = l.find('=')?;
                Some(l[eq + 1..].trim().to_string())
            })
            .unwrap_or_default()
    };

    let title_raw = line("_NET_WM_NAME");
    let title = title_raw.trim_matches('"').to_string();
    let title = if title.is_empty() {
        line("WM_NAME").trim_matches('"').to_string()
    } else {
        title
    };

    // WM_CLASS is `"instance", "Class"`; the second is what people recognise.
    let app_class = stdout
        .lines()
        .find(|l| l.starts_with("WM_CLASS"))
        .and_then(|l| {
            let val = l.split_once('=')?.1.trim();
            let parts: Vec<&str> = val.split('"').filter(|s| !s.trim().is_empty()).collect();
            parts.last().copied()
        })
        .unwrap_or_default();

    let pid_str = line("_NET_WM_PID");
    let pid: Option<i64> = pid_str.parse().ok();

    let desktop = line("_NET_WM_DESKTOP");
    if title.is_empty() && app_class.is_empty() {
        return None;
    }

    Some(DesktopWindow {
        id: id.to_string(),
        app_class: app_class.to_string(),
        title,
        workspace: if desktop.is_empty() || !desktop.chars().all(|c| c.is_ascii_digit()) {
            String::new()
        } else {
            desktop
        },
        pid,
    })
}

// ── pi sessions ─────────────────────────────────────────────────────────────

/// Where pi keeps the transcripts for a directory.
///
/// `--home-ldeen-Documents-ponydeck--`: a leading and trailing `--` around the
/// path with every `/` turned into `-`. Encoded exactly as pi does it.
///
/// The leading (and any trailing) separator is *dropped* rather than turned
/// into a dash, so the `--` delimiters are the only thing outside the path
/// components.
pub fn encode_pi_session_dir(cwd: &str) -> String {
    let trimmed = cwd.trim_matches('/');
    let inner = trimmed.replace('/', "-");
    format!("--{}--", inner)
}

/// Read the `{"type":"session",...}` header pi writes as a transcript's first
/// line.
pub fn parse_pi_session_head(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let first_line = text.lines().next().unwrap_or("");
    let obj: Result<serde_json::Value, _> = serde_json::from_str(first_line);
    let obj = match obj {
        Ok(v) if v.get("type").and_then(|t| t.as_str()) == Some("session") => v,
        _ => {
            return (None, None, None);
        }
    };
    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let cwd = obj
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let started_at = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (id, cwd, started_at)
}

/// Parse the ppid out of `/proc/<pid>/stat`. The comm may contain spaces and
/// parens.
pub fn ppid_from_stat(stat: &str) -> Option<u32> {
    let close = stat.rfind(')')?;
    let rest = &stat[close + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // field 0 is the state char (S/R/D/...), field 1 is the ppid.
    let ppid = fields.get(1)?.parse::<u32>().ok()?;
    Some(ppid)
}

/// `/proc/<pid>/stat` field 22: start time in clock ticks since boot.
pub fn start_ticks_from_stat(stat: &str) -> u64 {
    let close = match stat.rfind(')') {
        Some(i) => i,
        None => return 0,
    };
    let rest = &stat[close + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields
        .get(19)
        .and_then(|f| f.parse::<u64>().ok())
        .unwrap_or(0)
}

/// A pi process's start time and cwd, as read from /proc.
#[derive(Debug, Clone)]
pub struct PiProc {
    pub pid: i64,
    pub cwd: String,
    /// Process start, in clock ticks since boot.
    pub start_ticks: u64,
    pub ancestors: Vec<u32>,
}

/// Walk a process's parents, and say which of them is a window-manager window.
///
/// This is how a pi process is tied to the window it runs in: the window's
/// pid is an *ancestor* of pi, so the walk meets the window part-way up.
pub fn attribute_windows(
    chains: &[(i64, Vec<u32>)],
    windows: &[DesktopWindow],
) -> HashMap<i64, DesktopWindow> {
    let by_pid: HashMap<i64, &DesktopWindow> = windows
        .iter()
        .filter(|w| w.pid.is_some())
        .map(|w| (w.pid.unwrap(), w))
        .collect();

    let mut found = HashMap::new();
    for &(pid, ref ancestors) in chains {
        for &ancestor in ancestors {
            if let Some(&window) = by_pid.get(&(ancestor as i64)) {
                found.insert(pid, window.clone());
                break;
            }
        }
    }
    found
}

/// Pair each pi process with the transcript it is writing.
///
/// Both sides are sorted newest-first and zipped, per directory. This cannot
/// be proof — two pi processes in one directory started at the same moment are
/// indistinguishable from outside — but it is right whenever the sessions were
/// opened at different times, and it always yields *distinct* sessions rather
/// than showing the same transcript three times.
pub fn assign_transcripts(
    procs: &[PiProc],
    transcripts_by_cwd: &HashMap<String, Vec<String>>,
) -> HashMap<i64, Option<String>> {
    let mut out = HashMap::new();
    let by_cwd: HashMap<&str, Vec<&PiProc>> = procs.iter().fold(HashMap::new(), |mut acc, p| {
        acc.entry(p.cwd.as_str()).or_default().push(p);
        acc
    });

    for (cwd, group) in &by_cwd {
        let mut fresh: Vec<&PiProc> = group.to_vec();
        fresh.sort_by_key(|p| std::cmp::Reverse(p.start_ticks));
        let files = transcripts_by_cwd
            .get(*cwd)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for (i, p) in fresh.iter().enumerate() {
            out.insert(p.pid, files.get(i).cloned());
        }
    }
    out
}

/// Build a `PiSession` from a proc + transcript + window.
fn build_pi_session(
    proc: &PiProc,
    transcript: Option<&str>,
    window: Option<&DesktopWindow>,
) -> DesktopPiSession {
    let (session_id, head_cwd, started_at) = transcript
        .map(parse_pi_session_head)
        .unwrap_or((None, None, None));

    let dir = if proc.cwd.is_empty() {
        head_cwd.unwrap_or_default()
    } else {
        proc.cwd.clone()
    };

    DesktopPiSession {
        pid: proc.pid,
        cwd: dir,
        session_id,
        ldmux_session_id: None,
        started_at,
        transcript: transcript.map(|s| s.to_string()),
        window_id: window.map(|w| w.id.clone()),
        workspace: window.map(|w| w.workspace.clone()),
        label: proc
            .cwd
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "pi".to_string()),
    }
}

// ── the probe result ────────────────────────────────────────────────────────

/// The result of probing a window manager.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub wm: String,
    pub workspaces: Vec<DesktopWorkspace>,
    pub windows: Vec<DesktopWindow>,
    pub detail: Option<String>,
}

// ── async probes (call external commands) ───────────────────────────────────

use crate::exec::{self, ExecOpts};

/// Every probe runs in the session environment (`desktop::session_env`): a daemon
/// started before login has no DISPLAY/SWAYSOCK/… of its own, and a probe without
/// them reads as "no window manager" on a machine that has one.
type Env = Vec<(String, String)>;

fn exec_opts(ms: u64, env: &Env) -> ExecOpts {
    ExecOpts {
        timeout_ms: Some(ms),
        env: Some(env.clone()),
    }
}

fn argv<T: AsRef<str>>(args: &[T]) -> Vec<String> {
    args.iter().map(|a| a.as_ref().to_string()).collect()
}

/// Probe i3 or sway: same tree, different binary.
async fn probe_i3_like(env: &Env, binary: &str, wm: &str) -> Option<ProbeResult> {
    let tree = exec::run(argv(&[binary, "-t", "get_tree"]), exec_opts(3000, env)).await;
    if tree.code != 0 || tree.out.trim().is_empty() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_str(&tree.out).ok()?;
    let (mut workspaces, windows) = walk_i3_tree(&parsed);

    let ws = exec::run(
        argv(&[binary, "-t", "get_workspaces"]),
        exec_opts(3000, env),
    )
    .await;
    if let Ok(reported) = serde_json::from_str::<serde_json::Value>(&ws.out) {
        workspaces = merge_i3_workspace_flags(workspaces, &reported);
    }

    Some(ProbeResult {
        wm: wm.to_string(),
        workspaces,
        windows,
        detail: None,
    })
}

/// Fill in window pids the WM did not report.
///
/// i3 sets `pid` on Wayland windows only; on X11 every window comes back with
/// `null`, which would leave every pi session unattributed. `_NET_WM_PID` is
/// the EWMH property that answers it.
async fn enrich_window_pids(env: &Env, windows: &mut [DesktopWindow]) {
    let missing: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.pid.is_none())
        .map(|(i, _)| i)
        .collect();
    if missing.is_empty() {
        return;
    }
    let handles: Vec<_> = missing
        .into_iter()
        .map(|i| {
            let id = windows[i].id.clone();
            let env = env.clone();
            tokio::spawn(async move {
                let out = exec::run(
                    argv(&["xprop", "-id", &id, "_NET_WM_PID"]),
                    exec_opts(2000, &env),
                )
                .await;
                if out.code != 0 {
                    return None;
                }
                let pid: i64 = out
                    .out
                    .split('=')
                    .nth(1)
                    .and_then(|s| s.trim().parse().ok())?;
                Some(pid)
            })
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        if let Ok(Some(pid)) = h.await {
            windows[i].pid = Some(pid);
        }
    }
}

/// Probe EWMH: the standard every X11 desktop implements.
async fn probe_ewmh(env: &Env) -> Option<ProbeResult> {
    let list = exec::run(
        argv(&["xprop", "-root", "_NET_CLIENT_LIST"]),
        exec_opts(3000, env),
    )
    .await;
    if list.code != 0 {
        return None;
    }
    let ids = parse_ewmh_client_list(&list.out);

    let windows: Vec<DesktopWindow> = futures_util::future::join_all(ids.iter().map(|id| {
        let id = id.clone();
        async move {
            let out = exec::run(
                argv(&[
                    "xprop",
                    "-id",
                    &id,
                    "WM_CLASS",
                    "_NET_WM_NAME",
                    "WM_NAME",
                    "_NET_WM_DESKTOP",
                    "_NET_WM_PID",
                ]),
                exec_opts(3000, env),
            )
            .await;
            if out.code == 0 {
                parse_ewmh_window(&id, &out.out)
            } else {
                None
            }
        }
    }))
    .await
    .into_iter()
    .flatten()
    .collect();

    let current = exec::run(
        argv(&["xprop", "-root", "_NET_CURRENT_DESKTOP"]),
        exec_opts(2000, env),
    )
    .await;
    let active = current
        .out
        .split('=')
        .nth(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut by_desktop: HashMap<String, u64> = HashMap::new();
    for w in &windows {
        if w.workspace.is_empty() {
            continue;
        }
        *by_desktop.entry(w.workspace.clone()).or_insert(0) += 1;
    }
    if !active.is_empty() && !by_desktop.contains_key(&active) {
        by_desktop.insert(active.clone(), 0);
    }

    let mut workspaces: Vec<DesktopWorkspace> = by_desktop
        .iter()
        .map(|(name, count)| DesktopWorkspace {
            name: name.clone(),
            index: name.parse::<i64>().ok(),
            focused: *name == active,
            visible: *name == active,
            window_count: *count,
        })
        .collect();
    workspaces.sort_by(|a, b| a.name.cmp(&b.name));

    Some(ProbeResult {
        wm: "ewmh".to_string(),
        workspaces,
        windows,
        detail: None,
    })
}

/// Ask each window manager in turn. The order is "most specific first": a WM
/// with a real tree gives workspace nesting, EWMH only gives flat desktops.
pub async fn probe_window_manager() -> ProbeResult {
    let env = crate::desktop::session_env().await;
    let env = &env;
    probe_in(env).await
}

async fn probe_in(env: &Env) -> ProbeResult {
    if let Some(r) = probe_i3_like(env, "i3-msg", "i3").await {
        return r;
    }
    if let Some(r) = probe_i3_like(env, "swaymsg", "sway").await {
        return r;
    }

    let niri_ws = exec::run(
        argv(&["niri", "msg", "--json", "workspaces"]),
        exec_opts(3000, env),
    )
    .await;
    let niri_win = exec::run(
        argv(&["niri", "msg", "--json", "windows"]),
        exec_opts(3000, env),
    )
    .await;
    if niri_ws.code == 0
        && niri_win.code == 0
        && let (Ok(ws), Ok(win)) = (
            serde_json::from_str::<serde_json::Value>(&niri_ws.out),
            serde_json::from_str::<serde_json::Value>(&niri_win.out),
        )
    {
        let (workspaces, windows) = parse_niri(&ws, &win);
        return ProbeResult {
            wm: "niri".to_string(),
            workspaces,
            windows,
            detail: None,
        };
    }

    let hypr_ws = exec::run(argv(&["hyprctl", "-j", "workspaces"]), exec_opts(3000, env)).await;
    let hypr_clients = exec::run(argv(&["hyprctl", "-j", "clients"]), exec_opts(3000, env)).await;
    if hypr_ws.code == 0
        && hypr_clients.code == 0
        && let (Ok(ws), Ok(clients)) = (
            serde_json::from_str::<serde_json::Value>(&hypr_ws.out),
            serde_json::from_str::<serde_json::Value>(&hypr_clients.out),
        )
    {
        let active = exec::run(
            argv(&["hyprctl", "-j", "activeworkspace"]),
            exec_opts(2000, env),
        )
        .await;
        let active_val: Option<serde_json::Value> = if active.code == 0 {
            serde_json::from_str(&active.out).ok()
        } else {
            None
        };
        let active_ref = active_val.as_ref();
        let (workspaces, windows) = parse_hyprland(&ws, &clients, active_ref);
        return ProbeResult {
            wm: "hyprland".to_string(),
            workspaces,
            windows,
            detail: None,
        };
    }

    if let Some(r) = probe_ewmh(env).await {
        return r;
    }

    ProbeResult {
        wm: "unknown".to_string(),
        workspaces: vec![],
        windows: vec![],
        detail: Some(
            "no window manager could be enumerated (i3/sway/niri/hyprland need their msg tool on PATH; other X11 desktops need xprop)"
                .to_string(),
        ),
    }
}

// ── pi session scanning ─────────────────────────────────────────────────────

/// Pids whose `comm` is exactly `pi`.
pub fn scan_pi_pids(proc_root: &str) -> Vec<i64> {
    let mut pids = Vec::new();
    let entries = match std::fs::read_dir(proc_root) {
        Ok(entries) => entries,
        Err(_) => return pids,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let comm_path = std::path::Path::new(proc_root).join(&name).join("comm");
        match std::fs::read_to_string(&comm_path) {
            Ok(content) => {
                if content.trim() == "pi" {
                    pids.push(name.parse::<i64>().unwrap_or(0));
                }
            }
            Err(_) => {
                // the process exited between readdir and read: expected
            }
        }
    }
    pids
}

/// Walk a process's parents (up to `max_depth`).
pub fn ancestry(pid: u32, proc_root: &str, max_depth: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut current = pid;
    for _ in 0..max_depth {
        let stat_path = std::path::Path::new(proc_root)
            .join(current.to_string())
            .join("stat");
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(s) => s,
            Err(_) => break,
        };
        let ppid = match ppid_from_stat(&stat) {
            Some(p) if p > 1 => p,
            _ => break,
        };
        out.push(ppid);
        current = ppid;
    }
    out
}

/// Read `/proc/<pid>/cwd` (following the symlink).
pub fn read_cwd(pid: i64, proc_root: &str) -> String {
    let path = std::path::Path::new(proc_root)
        .join(pid.to_string())
        .join("cwd");
    std::fs::read_link(&path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Read `/proc/<pid>/stat` field 22: start time in clock ticks since boot.
fn read_start_ticks(pid: i64, proc_root: &str) -> u64 {
    let path = std::path::Path::new(proc_root)
        .join(pid.to_string())
        .join("stat");
    match std::fs::read_to_string(&path) {
        Ok(stat) => start_ticks_from_stat(&stat),
        Err(_) => 0,
    }
}

/// Read the first `bytes` of a file without reading the whole thing.
pub fn read_file_head(path: &str, bytes: usize) -> String {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut buf = vec![0u8; bytes];
    let mut total = 0;
    while total < bytes {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(k) => total += k,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf[..total]).to_string()
}

/// One directory's transcripts, newest first.
pub fn list_transcripts(dir: &str) -> Vec<String> {
    let names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
            .map(|e| e.path().to_string_lossy().to_string())
            .collect(),
        Err(_) => return vec![],
    };

    let mut stamped: Vec<(String, std::time::SystemTime)> = Vec::new();
    for path in names {
        if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
            stamped.push((path, mtime));
        }
    }
    stamped.sort_by_key(|b| std::cmp::Reverse(b.1)); // descending by mtime
    stamped.into_iter().map(|(p, _)| p).collect()
}

/// The newest transcript for one directory, or null when there is none.
pub fn newest_transcript(dir: &str) -> Option<String> {
    list_transcripts(dir).into_iter().next()
}

/// Every running pi process, with the session it is writing to.
pub fn scan_pi_sessions(
    home: &str,
    proc_root: &str,
    windows: &[DesktopWindow],
) -> Vec<DesktopPiSession> {
    let procs: Vec<PiProc> = scan_pi_pids(proc_root)
        .into_iter()
        .filter_map(|pid| {
            let cwd = read_cwd(pid, proc_root);
            if cwd.is_empty() {
                return None;
            }
            Some(PiProc {
                pid,
                cwd,
                start_ticks: read_start_ticks(pid, proc_root),
                ancestors: ancestry(pid as u32, proc_root, 24),
            })
        })
        .collect();

    let sessions_root = std::path::Path::new(home)
        .join(".pi")
        .join("agent")
        .join("sessions");
    let cwd_set: std::collections::HashSet<String> = procs.iter().map(|p| p.cwd.clone()).collect();
    let transcripts: HashMap<String, Vec<String>> = cwd_set
        .iter()
        .map(|cwd| {
            let dir = sessions_root.join(encode_pi_session_dir(cwd));
            (
                cwd.clone(),
                list_transcripts(dir.to_string_lossy().as_ref()),
            )
        })
        .collect();

    let assigned = assign_transcripts(&procs, &transcripts);
    let attributed = attribute_windows(
        &procs
            .iter()
            .map(|p| (p.pid, p.ancestors.clone()))
            .collect::<Vec<_>>(),
        windows,
    );

    procs
        .into_iter()
        .map(|p| {
            let transcript = assigned.get(&p.pid).and_then(|o| o.as_deref());
            let window = attributed.get(&p.pid);
            build_pi_session(&p, transcript, window)
        })
        .collect()
}

/// Fill in which ldmux session each running pi runs inside, by walking the pi's
/// ancestry to the session's shell pid.
///
/// The inventory can see a pi and cannot see that it is *in ldmux*: a desk
/// window's terminal is not in the pi's ancestry — the pi runs in the keeper's
/// pty, whose parent is the daemon, while the window is a separate `alacritty`
/// running `ldmuxd attach` — so the window attribution finds nothing and the pi
/// reads as "no window" while its shell fills one. The session's shell pid *is*
/// in its ancestry, which is exactly what the TypeScript used (tmux's
/// `#{pane_pid}`; `origin/main:packages/daemon/src/daemon.ts:1096-1106`), and
/// without it the phone could only guess by directory — and a guess that missed
/// opened a new shell instead of the running one.
///
/// `by_shell_pid` maps a session's shell pid to its session id.
pub fn link_pi_sessions(inv: &mut DesktopInventory, by_shell_pid: &HashMap<i32, String>) {
    if by_shell_pid.is_empty() {
        return;
    }
    for pi in inv.pi_sessions.iter_mut() {
        // The pi itself is never a session's shell (that is the pane's `bash`),
        // so the parents are the whole chain, exactly as the TS's `ancestorIn`
        // walked it after its first lookup.
        pi.ldmux_session_id = ancestry(pi.pid as u32, "/proc", 64)
            .into_iter()
            .find_map(|p| by_shell_pid.get(&(p as i32)).cloned());
    }
}

// ── the one call the RPC makes ─────────────────────────────────────────────

/// Read the full desktop inventory: probe the WM, enrich pids, scan pi
/// sessions.
pub async fn read_desktop(home: &str) -> DesktopInventory {
    let env = crate::desktop::session_env().await;
    let mut probe = probe_in(&env).await;
    enrich_window_pids(&env, &mut probe.windows).await;
    let pi_sessions = scan_pi_sessions(home, "/proc", &probe.windows);
    DesktopInventory {
        wm: probe.wm,
        workspaces: probe.workspaces,
        windows: probe.windows,
        pi_sessions,
        detail: probe.detail,
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_i3_tree_flattens_workspaces_and_windows() {
        let tree: serde_json::Value = serde_json::from_str(
            r#"{
                "type": "root",
                "nodes": [
                    {
                        "type": "workspace",
                        "name": "1",
                        "nodes": [
                            {"type": "con", "nodes": [
                                {"type": "con", "nodes": [
                                    {"type": "con", "window": 1234, "pid": 456, "app_id": "Alacritty", "window_properties": {"class": "Alacritty", "title": "test"}}
                                ]}
                            ]}
                        ]
                    },
                    {
                        "type": "workspace",
                        "name": "2",
                        "nodes": [
                            {"type": "con", "nodes": [
                                {"type": "con", "window": 789, "app_id": "firefox", "window_properties": {"class": "firefox", "title": "Firefox"}}
                            ]}
                        ],
                        "floating_nodes": [
                            {"type": "con", "window": 999, "pid": 777, "app_id": "kitty"}
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();

        let (workspaces, windows) = walk_i3_tree(&tree);
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].name, "1");
        assert_eq!(workspaces[0].index, Some(1));
        assert_eq!(workspaces[1].name, "2");
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].id, "0x4d2");
        assert_eq!(windows[0].app_class, "Alacritty");
        assert_eq!(windows[0].workspace, "1");
        assert_eq!(windows[0].pid, Some(456));
        assert_eq!(windows[1].app_class, "firefox");
        assert_eq!(windows[1].workspace, "2");
        assert_eq!(windows[2].id, "0x3e7");
        assert_eq!(windows[2].app_class, "kitty");
        assert_eq!(workspaces[0].window_count, 1);
        assert_eq!(workspaces[1].window_count, 2);
    }

    #[test]
    fn merge_i3_workspace_flags_sets_focused_and_visible() {
        let walked = vec![
            DesktopWorkspace {
                name: "1".into(),
                index: Some(1),
                focused: false,
                visible: false,
                window_count: 0,
            },
            DesktopWorkspace {
                name: "2".into(),
                index: Some(2),
                focused: false,
                visible: false,
                window_count: 0,
            },
        ];
        let reported: serde_json::Value =
            serde_json::from_str(r#"[{"name":"1","focused":true,"visible":true},{"name":"2","focused":false,"visible":true}]"#)
                .unwrap();
        let merged = merge_i3_workspace_flags(walked, &reported);
        assert!(merged[0].focused);
        assert!(merged[0].visible);
        assert!(!merged[1].focused);
        assert!(merged[1].visible);
    }

    #[test]
    fn parse_niri_maps_workspaces_and_windows() {
        let ws: serde_json::Value = serde_json::from_str(
            r#"[{"id":0,"name":"1","is_focused":true,"is_active":true},{"id":1,"name":"2","is_focused":false,"is_active":false}]"#,
        )
        .unwrap();
        let win: serde_json::Value = serde_json::from_str(
            r#"[{"id":100,"app_id":"Alacritty","title":"test","workspace_id":0,"pid":123},{"id":200,"app_id":"firefox","title":"FF","workspace_id":1}]"#,
        )
        .unwrap();
        let (workspaces, windows) = parse_niri(&ws, &win);
        assert_eq!(workspaces.len(), 2);
        assert!(workspaces[0].focused);
        assert_eq!(workspaces[0].window_count, 1);
        assert_eq!(windows[0].id, "100");
        assert_eq!(windows[0].pid, Some(123));
        assert_eq!(windows[0].workspace, "0");
    }

    #[test]
    fn parse_hyprland_maps_workspaces_and_windows() {
        let ws: serde_json::Value = serde_json::from_str(
            r#"[{"id":0,"name":"1","monitor":"D-01","windows":2},{"id":1,"name":"2","monitor":"","windows":0}]"#,
        )
        .unwrap();
        let clients: serde_json::Value = serde_json::from_str(
            r#"[{"address":500,"class":"Alacritty","title":"t","workspace":{"name":"1"},"pid":999}]"#,
        )
        .unwrap();
        let active: serde_json::Value = serde_json::from_str(r#"{"id":0}"#).unwrap();
        let (workspaces, windows) = parse_hyprland(&ws, &clients, Some(&active));
        assert_eq!(workspaces.len(), 2);
        assert!(workspaces[0].focused);
        assert!(workspaces[0].visible);
        assert!(!workspaces[1].visible);
        assert_eq!(windows[0].id, "500");
        assert_eq!(windows[0].pid, Some(999));
    }

    #[test]
    fn parse_ewmh_client_list_extracts_hex_ids() {
        let stdout = "_NET_CLIENT_LIST: 0x100001, 0x100002, 0x100003\n";
        let ids = parse_ewmh_client_list(stdout);
        assert_eq!(ids, vec!["0x100001", "0x100002", "0x100003"]);
    }

    #[test]
    fn parse_ewmh_window_extracts_title_class_pid() {
        // xprop output format: KEY(TYPE) = "value"
        let stdout = "\
WM_CLASS(STRING) = \"term\", \"Alacritty\"\n\
_NET_WM_NAME(UTF-8 String) = \"Terminal\"\n\
_NET_WM_PID(CARDINAL) = 12345\n\
_NET_WM_DESKTOP(CARDINAL) = 0\n";
        let w = parse_ewmh_window("0x100001", stdout).unwrap();
        assert_eq!(w.title, "Terminal");
        assert_eq!(w.app_class, "Alacritty");
        assert_eq!(w.pid, Some(12345));
        assert_eq!(w.workspace, "0");
    }

    #[test]
    fn encode_pi_session_dir_encodes_cwd() {
        assert_eq!(
            encode_pi_session_dir("/home/ldeen/Documents/ponydeck"),
            "--home-ldeen-Documents-ponydeck--"
        );
        assert_eq!(encode_pi_session_dir("/home/ldeen"), "--home-ldeen--");
        // Leading/trailing slashes are dropped, not turned into dashes.
        assert_eq!(encode_pi_session_dir("/home/"), "--home--");
    }

    #[test]
    fn parse_pi_session_head_reads_the_header() {
        let text = "{\"type\":\"session\",\"id\":\"abc123\",\"cwd\":\"/home/test\",\"timestamp\":\"2026-01-01T00:00:00Z\"}\n{...}\n";
        let (id, cwd, started) = parse_pi_session_head(text);
        assert_eq!(id, Some("abc123".into()));
        assert_eq!(cwd, Some("/home/test".into()));
        assert_eq!(started, Some("2026-01-01T00:00:00Z".into()));
    }

    #[test]
    fn parse_pi_session_head_rejects_non_session() {
        let text = "{\"type\":\"message\",\"id\":\"abc\"}\n";
        let (id, cwd, started) = parse_pi_session_head(text);
        assert_eq!(id, None);
        assert_eq!(cwd, None);
        assert_eq!(started, None);
    }

    #[test]
    fn ppid_from_stat_parses_the_ppid() {
        // The comm may contain spaces and parens.
        let stat = "(pi) S 1234 1234 1234 0 -1 4194304 0 0 0 0 0 100 200 0 0 1 0 12345 123456789 0";
        let ppid = ppid_from_stat(stat).unwrap();
        assert_eq!(ppid, 1234);
    }

    #[test]
    fn start_ticks_from_stat_parses_field_22() {
        let stat = "(pi) S 1234 1234 1234 0 -1 4194304 0 0 0 0 0 100 200 0 0 1 0 12345 987654321 0 0 0 0 0 0 0 0 0 0 0 0";
        let ticks = start_ticks_from_stat(stat);
        assert_eq!(ticks, 987654321);
    }

    #[test]
    fn attribute_windows_finds_the_ancestor_window() {
        let windows = vec![DesktopWindow {
            id: "0x100001".into(),
            app_class: "Alacritty".into(),
            title: "test".into(),
            workspace: "1".into(),
            pid: Some(111),
        }];
        // pi (pid 999) → bash (444) → alacritty (111) → i3 (55)
        let chains = vec![(999i64, vec![444, 111, 55])];
        let attributed = attribute_windows(&chains, &windows);
        assert_eq!(attributed.get(&999).unwrap().id, "0x100001");
    }

    #[test]
    fn assign_transcripts_pairs_newest_proc_with_newest_transcript() {
        let procs = vec![
            PiProc {
                pid: 100,
                cwd: "/home/a".into(),
                start_ticks: 1000,
                ancestors: vec![],
            },
            PiProc {
                pid: 200,
                cwd: "/home/a".into(),
                start_ticks: 2000,
                ancestors: vec![],
            },
        ];
        // Transcripts are sorted newest-first (as `listTranscripts` returns them).
        let transcripts: HashMap<String, Vec<String>> = [(
            "/home/a".to_string(),
            vec![
                "/home/a/t2.jsonl".to_string(), // newest
                "/home/a/t1.jsonl".to_string(), // older
            ],
        )]
        .into_iter()
        .collect();
        let assigned = assign_transcripts(&procs, &transcripts);
        // Newest proc (200, ticks 2000) gets newest transcript (t2).
        assert_eq!(assigned.get(&200), Some(&Some("/home/a/t2.jsonl".into())));
        // Older proc (100, ticks 1000) gets older transcript (t1).
        assert_eq!(assigned.get(&100), Some(&Some("/home/a/t1.jsonl".into())));
    }

    /// The walk finds the session by a *real* process chain, which is the whole
    /// mechanism: spawn a child of a child and give the grandparent's pid to a
    /// session, exactly as the keeper reports its shell's pid.
    #[test]
    fn a_pi_is_linked_to_the_session_its_ancestry_runs_in() {
        // `sh -c "sleep 30; :"`: a single command would be *exec'd* by the shell
        // (no child at all), so the trailing no-op keeps the sh around as the
        // parent of the sleep. The sh's own parent is this test process.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30; :")
            .spawn()
            .expect("spawn sh");
        let sh_pid = child.id() as i32;
        // The grandchild: a sleep that is a child of the sh.
        let grandchild = {
            let mut found = None;
            for _ in 0..200 {
                if let Ok(entries) = std::fs::read_dir("/proc") {
                    for e in entries.flatten() {
                        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok())
                        else {
                            continue;
                        };
                        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                            continue;
                        };
                        if ppid_from_stat(&stat) == Some(sh_pid as u32)
                            && let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                            && comm.trim() == "sleep"
                        {
                            found = Some(pid);
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            found.expect("the sh's sleep child")
        };

        let mut inv = DesktopInventory {
            wm: "test".into(),
            workspaces: Vec::new(),
            windows: Vec::new(),
            pi_sessions: vec![DesktopPiSession {
                pid: grandchild as i64,
                cwd: "/tmp".into(),
                session_id: None,
                ldmux_session_id: None,
                started_at: None,
                transcript: None,
                window_id: None,
                workspace: None,
                label: "sleep".into(),
            }],
            detail: None,
        };
        let mut by_shell_pid = HashMap::new();
        // The keeper reports the *shell's* pid; here the sh stands in for it.
        by_shell_pid.insert(sh_pid, "abc123".to_string());
        link_pi_sessions(&mut inv, &by_shell_pid);
        assert_eq!(
            inv.pi_sessions[0].ldmux_session_id.as_deref(),
            Some("abc123")
        );

        // A pid whose chain holds no session is left alone, and an empty map
        // costs nothing.
        by_shell_pid.clear();
        by_shell_pid.insert(2_000_000_000, "nope".to_string());
        link_pi_sessions(&mut inv, &by_shell_pid);
        assert_eq!(inv.pi_sessions[0].ldmux_session_id, None);
        // The test process's own pid is an ancestor too: a map with it links as
        // well, which is what makes the walk start at the pi and go up.
        by_shell_pid.clear();
        by_shell_pid.insert(std::process::id() as i32, "me".to_string());
        link_pi_sessions(&mut inv, &by_shell_pid);
        assert_eq!(inv.pi_sessions[0].ldmux_session_id.as_deref(), Some("me"));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn build_pi_session_builds_the_session() {
        let proc = PiProc {
            pid: 42,
            cwd: "/home/test/proj".into(),
            start_ticks: 100,
            ancestors: vec![1, 2],
        };
        let transcript = "/home/.pi/agent/sessions/--home-test-proj--/t.jsonl";
        let window = Some(DesktopWindow {
            id: "0x100001".into(),
            app_class: "Alacritty".into(),
            title: "test".into(),
            workspace: "1".into(),
            pid: Some(111),
        });
        let session = build_pi_session(&proc, Some(transcript), window.as_ref());
        assert_eq!(session.pid, 42);
        assert_eq!(session.cwd, "/home/test/proj");
        assert_eq!(session.window_id, Some("0x100001".into()));
        assert_eq!(session.workspace, Some("1".into()));
        assert_eq!(session.label, "proj");
    }
}
