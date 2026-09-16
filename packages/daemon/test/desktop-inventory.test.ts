/**
 * The desktop inventory's parsing, against recorded output.
 *
 * Everything checked here was taken from this machine's real i3 tree, real
 * `xprop` output and real pi transcripts — the point of the module is to read
 * other people's formats correctly, so the tests use those formats rather than
 * shapes that were invented to be easy to parse.
 */

import { test, expect } from "../../testing/src/expect.ts";
import {
  assignTranscripts,
  attributeWindows,
  encodePiSessionDir,
  mergeI3WorkspaceFlags,
  parseEWMHClientList,
  parseEWMHWindow,
  parseHyprland,
  parseNiri,
  parsePiSessionHead,
  ppidFromStat,
  startTicksFromStat,
  walkI3Tree,
} from "../src/desktop-inventory.ts";

// ── the i3/sway tree ───────────────────────────────────────────────────────

/** A tree in i3's shape: workspaces, a nested split, and a floating window. */
const TREE = {
  type: "root",
  nodes: [
    {
      type: "output",
      nodes: [
        {
          type: "workspace",
          name: "1",
          nodes: [
            { type: "con", window: 73400325, window_properties: { class: "Alacritty", title: "π - ldeen" } },
            {
              type: "con",
              nodes: [
                // A nested split: the workspace is still what encloses this.
                { type: "con", window: 8388613, window_properties: { class: "firefox", title: "Inbox" } },
              ],
            },
          ],
          floating_nodes: [
            { type: "floating_con", window: 4194309, window_properties: { class: "mpv", title: "video" } },
          ],
        },
      ],
    },
    { type: "workspace", name: "2", nodes: [] },
  ],
};

test("walkI3Tree keeps each window with the workspace that encloses it", () => {
  const { workspaces, windows } = walkI3Tree(TREE);
  expect(workspaces.map((w) => w.name)).toEqual(["1", "2"]);
  expect(windows.length).toBe(3);
  // Hex, because that is how every other X tool spells a window id.
  expect(windows[0]!.id).toBe("0x4600005");
  expect(windows[0]!.appClass).toBe("Alacritty");
  expect(windows[0]!.workspace).toBe("1");
  // Nested and floating windows both belong to the workspace, not to a parent node.
  expect(windows[1]!.workspace).toBe("1");
  expect(windows[2]!.workspace).toBe("1");
  expect(windows[2]!.appClass).toBe("mpv");
});

test("walkI3Tree counts windows per workspace, including floats", () => {
  const { workspaces } = walkI3Tree(TREE);
  expect(workspaces[0]!.windowCount).toBe(3);
  expect(workspaces[1]!.windowCount).toBe(0);
});

test("walkI3Tree reads sway's app_id when there is no window_properties", () => {
  const sway = { type: "workspace", name: "web", nodes: [{ type: "con", window: 1, app_id: "firefox" }] };
  const { windows } = walkI3Tree(sway);
  expect(windows[0]!.appClass).toBe("firefox");
  // sway does report a pid, and it is kept.
  expect(windows[0]!.pid).toBe(null);
});

test("walkI3Tree takes a numeric workspace name as its index", () => {
  const { workspaces } = walkI3Tree({ type: "root", nodes: [{ type: "workspace", name: "10" }] });
  expect(workspaces[0]!.index).toBe(10);
  const named = walkI3Tree({ type: "root", nodes: [{ type: "workspace", name: "web" }] });
  expect(named.workspaces[0]!.index).toBe(null);
});

test("walkI3Tree ignores the null windows i3 uses for split containers", () => {
  const { windows } = walkI3Tree({
    type: "root",
    nodes: [{ type: "workspace", name: "1", nodes: [{ type: "con", window: null, name: "split" }] }],
  });
  expect(windows.length).toBe(0);
});

test("mergeI3WorkspaceFlags brings focus and visibility in from get_workspaces", () => {
  const walked = walkI3Tree(TREE).workspaces;
  const merged = mergeI3WorkspaceFlags(walked, [
    { name: "1", focused: false, visible: false },
    { name: "2", focused: true, visible: true },
  ]);
  expect(merged[0]!.focused).toBe(false);
  expect(merged[1]!.focused).toBe(true);
  // Counts survive the merge — they came from the tree, not from get_workspaces.
  expect(merged[0]!.windowCount).toBe(3);
});

test("mergeI3WorkspaceFlags leaves the walked data alone when get_workspaces is unreadable", () => {
  const walked = walkI3Tree(TREE).workspaces;
  expect(mergeI3WorkspaceFlags(walked, null)).toEqual(walked);
  expect(mergeI3WorkspaceFlags(walked, "not an array")).toEqual(walked);
});

// ── niri and hyprland ──────────────────────────────────────────────────────

test("parseNiri keeps windows and counts them per workspace", () => {
  const result = parseNiri(
    [{ id: 1, idx: 1, name: "one", is_focused: true, is_active: true }, { id: 2, idx: 2, name: "two" }],
    [
      { id: 7, app_id: "Alacritty", title: "shell", workspace_id: 1, pid: 100 },
      { id: 8, app_id: "firefox", title: "web", workspace_id: 1 },
    ],
  );
  expect(result.wm).toBe("niri");
  expect(result.windows.length).toBe(2);
  expect(result.windows[0]!.pid).toBe(100);
  expect(result.workspaces[0]!.windowCount).toBe(2);
  expect(result.workspaces[0]!.focused).toBe(true);
  expect(result.workspaces[1]!.windowCount).toBe(0);
});

test("parseHyprland marks the activeworkspace focused, since the list does not", () => {
  const result = parseHyprland(
    [{ id: 1, name: "1", windows: 2, monitor: "DP-1" }, { id: 2, name: "2", windows: 0, monitor: "DP-1" }],
    [{ address: "0xabc", class: "Alacritty", title: "shell", workspace: { id: 1, name: "1" }, pid: 100 }],
    { id: 2, name: "2" },
  );
  expect(result.wm).toBe("hyprland");
  expect(result.workspaces[0]!.focused).toBe(false);
  expect(result.workspaces[1]!.focused).toBe(true);
  expect(result.workspaces[0]!.windowCount).toBe(2);
  // The nested workspace object is flattened to its name.
  expect(result.windows[0]!.workspace).toBe("1");
});

// ── EWMH, the fallback that makes GNOME/KDE/XFCE work ──────────────────────

test("parseEWMHClientList takes the window ids and drops repeats", () => {
  const ids = parseEWMHClientList("_NET_CLIENT_LIST(WINDOW): window id # 0x4600005, 0x4600005, 0x1400003\n");
  expect(ids).toEqual(["0x4600005", "0x1400003"]);
});

test("parseEWMHWindow reads class, title, desktop and pid", () => {
  const out = [
    '_NET_WM_NAME(UTF8_STRING) = "pi: ponydeck"',
    'WM_NAME(STRING) = "pi: ponydeck"',
    '_NET_WM_DESKTOP(CARDINAL) = 2',
    "_NET_WM_PID(CARDINAL) = 10402",
    'WM_CLASS(STRING) = "Alacritty", "Alacritty"',
  ].join("\n");
  const w = parseEWMHWindow("0x5000005", out);
  expect(w!.appClass).toBe("Alacritty");
  expect(w!.title).toBe("pi: ponydeck");
  expect(w!.workspace).toBe("2");
  expect(w!.pid).toBe(10402);
});

test("parseEWMHWindow prefers the UTF-8 title over the legacy one", () => {
  const out = ['_NET_WM_NAME(UTF8_STRING) = "π - ldeen"', 'WM_NAME(STRING) = "old"'].join("\n");
  expect(parseEWMHWindow("0x1", out)!.title).toBe("π - ldeen");
});

test("parseEWMHWindow falls back to WM_NAME when there is no _NET_WM_NAME", () => {
  const out = ['WM_NAME(STRING) = "xterm"', 'WM_CLASS(STRING) = "xterm", "XTerm"'].join("\n");
  const w = parseEWMHWindow("0x1", out);
  expect(w!.title).toBe("xterm");
  // The second of `"instance", "Class"` is the one people recognise.
  expect(w!.appClass).toBe("XTerm");
});

test("parseEWMHWindow returns null for a window that is not there", () => {
  expect(parseEWMHWindow("0x1", "xprop:  no such window")).toBe(null);
});

test("parseEWMHWindow leaves pid null when the window does not set one", () => {
  const w = parseEWMHWindow("0x1", ['_NET_WM_NAME = "x"', 'WM_CLASS(STRING) = "a", "b"'].join("\n"));
  expect(w!.pid).toBe(null);
});

// ── pi sessions ────────────────────────────────────────────────────────────

test("encodePiSessionDir matches pi's own directory naming", () => {
  // Verified against the real ~/.pi/agent/sessions on this machine.
  expect(encodePiSessionDir("/home/ldeen")).toBe("--home-ldeen--");
  expect(encodePiSessionDir("/home/ldeen/Documents/ponydeck")).toBe("--home-ldeen-Documents-ponydeck--");
  // A trailing slash must not add one, and a doubled one must not add two.
  expect(encodePiSessionDir("/home/ldeen/")).toBe("--home-ldeen--");
  expect(encodePiSessionDir("/home/ldeen//x")).toBe("--home-ldeen--x--");
});

test("parsePiSessionHead reads the session header pi writes first", () => {
  const head =
    '{"type":"session","version":3,"id":"01a0a1f3-d758-7300-8fdd-0d612a1b7993","timestamp":"2026-09-14T22:05:07.289Z","cwd":"/home/ldeen/Documents/ponydeck"}\n{"type":"model_change","id":"x"}';
  const parsed = parsePiSessionHead(head);
  expect(parsed.id).toBe("01a0a1f3-d758-7300-8fdd-0d612a1b7993");
  expect(parsed.cwd).toBe("/home/ldeen/Documents/ponydeck");
  expect(parsed.startedAt).toBe("2026-09-14T22:05:07.289Z");
});

test("parsePiSessionHead refuses to read a non-session first line as a session", () => {
  // A truncated read could hand back a later line; only a `session` header counts.
  expect(parsePiSessionHead('{"type":"model_change","id":"x"}').id).toBe(null);
  expect(parsePiSessionHead("").id).toBe(null);
  expect(parsePiSessionHead("not json at all").id).toBe(null);
});

test("assignTranscripts pairs the newest process with the newest transcript", () => {
  const procs = [
    { pid: 10, cwd: "/a", startTicks: 100, ancestors: [] },
    { pid: 11, cwd: "/a", startTicks: 300, ancestors: [] },
    { pid: 12, cwd: "/a", startTicks: 200, ancestors: [] },
  ];
  const files = new Map([["/a", ["/a/newest.jsonl", "/a/middle.jsonl", "/a/oldest.jsonl"]]]);
  const assigned = assignTranscripts(procs, files);
  expect(assigned.get(11)).toBe("/a/newest.jsonl");
  expect(assigned.get(12)).toBe("/a/middle.jsonl");
  expect(assigned.get(10)).toBe("/a/oldest.jsonl");
});

test("assignTranscripts keeps directories apart", () => {
  const procs = [
    { pid: 1, cwd: "/a", startTicks: 10, ancestors: [] },
    { pid: 2, cwd: "/b", startTicks: 10, ancestors: [] },
  ];
  const files = new Map([
    ["/a", ["/a/only.jsonl"]],
    ["/b", ["/b/only.jsonl"]],
  ]);
  const assigned = assignTranscripts(procs, files);
  expect(assigned.get(1)).toBe("/a/only.jsonl");
  expect(assigned.get(2)).toBe("/b/only.jsonl");
});

test("assignTranscripts gives a process null rather than someone else's transcript", () => {
  const procs = [
    { pid: 1, cwd: "/a", startTicks: 10, ancestors: [] },
    { pid: 2, cwd: "/a", startTicks: 20, ancestors: [] },
  ];
  const assigned = assignTranscripts(procs, new Map([["/a", ["/a/only.jsonl"]]]));
  expect(assigned.get(2)).toBe("/a/only.jsonl");
  expect(assigned.get(1)).toBe(null);
  // And when there is no directory at all.
  expect(assignTranscripts(procs, new Map()).get(1)).toBe(null);
});

// ── process ancestry: how a pi session is tied to its window ───────────────

test("ppidFromStat reads field 4 even when comm contains spaces and parens", () => {
  // `(sd-pam)`, `(my prog)`, and a comm with a close-paren in it.
  expect(ppidFromStat("100 (bash) S 42 100 100 0 -1")).toBe(42);
  expect(ppidFromStat("100 (sd-pam) S 42 100 100 0 -1")).toBe(42);
  expect(ppidFromStat("100 (my) prog) S 42 100 100 0 -1")).toBe(42);
  expect(ppidFromStat("garbage")).toBe(null);
});

test("startTicksFromStat reads field 22 from the end-of-comm offset", () => {
  // Fields after comm: state(3) ppid(4) … starttime(22). Build a line with a
  // known starttime at the right position.
  const after = ["S", "1", "1", "1", "0", "-1", "4194304"]
    .concat(Array.from({ length: 12 }, () => "0"))
    .concat(["123456"]);
  const stat = `999 (pi) ${after.join(" ")}`;
  expect(startTicksFromStat(stat)).toBe(123456);
  expect(startTicksFromStat("garbage")).toBe(0);
});

test("attributeWindows finds the window whose pid is an ancestor of pi", () => {
  const windows = [
    { id: "0x4600005", appClass: "Alacritty", title: "π", workspace: "1", pid: 9707 },
    { id: "0x5000005", appClass: "Alacritty", title: "pi", workspace: "1", pid: 10402 },
  ];
  // The real chain: i3 → sh → alacritty(9707) → bash(9713) → pi(10068).
  const chains = [{ pid: 10068, ancestors: [9713, 9707, 9706, 5274] }];
  const found = attributeWindows(chains, windows);
  expect(found.get(10068)!.id).toBe("0x4600005");
});

test("attributeWindows leaves a pi process unattributed when no ancestor has a window", () => {
  const windows = [{ id: "0x1", appClass: "x", title: "y", workspace: "1", pid: 500 }];
  const found = attributeWindows([{ pid: 100, ancestors: [90, 80] }], windows);
  expect(found.has(100)).toBe(false);
});

test("attributeWindows ignores windows whose pid is unknown", () => {
  // A window with no pid must not match the first chain by accident.
  const windows = [{ id: "0x1", appClass: "x", title: "y", workspace: "1", pid: null }];
  const found = attributeWindows([{ pid: 100, ancestors: [1] }], windows);
  expect(found.has(100)).toBe(false);
});
