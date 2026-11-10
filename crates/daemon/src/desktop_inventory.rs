//! Desktop inventory: the workspaces on this machine and the windows on them.
//!
//! What the phone's "send to desktop" picker chooses between
//! (`LdmuxClient.kt` `DesktopPickerModal`): a workspace, with the titles of the
//! windows already there so "the one with the editor" is recognisable. The shape is
//! the shared wire contract (`DesktopInventory`, `DesktopWindow`,
//! `DesktopWorkspace`), defined once in the protocol package.
//!
//! Window managers are probed in order, and the first that answers wins: i3 and
//! sway share one tree shape and one walker, niri and hyprland answer JSON, and
//! EWMH (`xprop`) is the fallback and the reason GNOME/KDE/XFCE on X11 work at all.
//!
//! Everything that parses is pure and testable. Only `read_desktop()` touches the
//! machine.

use std::collections::HashMap;

use serde::Deserialize;

use ldmux_protocol::rpc::{DesktopInventory, DesktopWindow, DesktopWorkspace};

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
        if let Some(w) = node.window.filter(|w| *w != 0) {
            windows.push(DesktopWindow {
                id: format!("0x{:x}", w),
                app_class: class_of(node),
                title: title_of(node),
                workspace: ws.to_string(),
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
    })
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

// ── the one call the RPC makes ─────────────────────────────────────────────

/// Read the desktop inventory: ask the window manager what is on screen.
#[must_use]
pub async fn read_desktop() -> DesktopInventory {
    let env = crate::desktop::session_env().await;
    let probe = probe_in(&env).await;
    DesktopInventory {
        wm: probe.wm,
        workspaces: probe.workspaces,
        windows: probe.windows,
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
    }

    #[test]
    fn parse_ewmh_client_list_extracts_hex_ids() {
        let stdout = "_NET_CLIENT_LIST: 0x100001, 0x100002, 0x100003\n";
        let ids = parse_ewmh_client_list(stdout);
        assert_eq!(ids, vec!["0x100001", "0x100002", "0x100003"]);
    }

    #[test]
    fn parse_ewmh_window_extracts_title_class_and_desktop() {
        // xprop output format: KEY(TYPE) = "value"
        let stdout = "\
WM_CLASS(STRING) = \"term\", \"Alacritty\"\n\
_NET_WM_NAME(UTF-8 String) = \"Terminal\"\n\
_NET_WM_DESKTOP(CARDINAL) = 0\n";
        let w = parse_ewmh_window("0x100001", stdout).unwrap();
        assert_eq!(w.title, "Terminal");
        assert_eq!(w.app_class, "Alacritty");
        assert_eq!(w.workspace, "0");
    }
}
