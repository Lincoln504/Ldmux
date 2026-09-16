/**
 * What is on this machine's desktop, and which of it is a pi session.
 *
 * The launcher is a menu of *places to go*: the desktop workspaces, the windows
 * in them, and the pi agent sessions running inside those windows. Worth knowing
 * up front:
 *
 *   - Window managers are probed in order, and the first that answers wins. The
 *     i3 and sway trees share one shape, so they share one walker.
 *   - EWMH (`xprop`) is the fallback and the reason GNOME/KDE/XFCE on X11 work at
 *     all: it is a standard, not a WM feature.
 *   - A pi process cannot be asked what session it is (it rewrites its own
 *     `comm`, and its argv is not readable), so the session is found the way pi
 *     itself lays it out: `~/.pi/agent/sessions/--<cwd with / → ->--`, newest
 *     transcript first.
 *   - A window is linked to a pi session by *process ancestry*: walk the pi
 *     process's parents until one of them is the pid the WM reports for a window.
 *     That is a fact, not a guess from a window title.
 *
 * Everything that parses is pure and tested against recorded output. Only
 * `readDesktop()` and `scanPiSessions()` touch the machine.
 */

import { closeSync, openSync, readdirSync, readFileSync, readlinkSync, readSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { run } from "./exec.ts";

// The shape is the shared wire contract, not this module's private business: the
// phone renders it, so it is defined once, in the protocol package.
export type {
  DesktopInventory,
  DesktopPiSession,
  DesktopWindow,
  DesktopWorkspace,
} from "../../protocol/src/rpc.ts";
import type {
  DesktopInventory,
  DesktopPiSession,
  DesktopWindow,
  DesktopWorkspace,
} from "../../protocol/src/rpc.ts";

// ── the shape the launcher consumes ────────────────────────────────────────

/** Local aliases, so the body of this module reads short. */
type InventoryWorkspace = DesktopWorkspace;
type InventoryWindow = DesktopWindow;
type InventoryPiSession = DesktopPiSession;

// ── window-manager trees: i3 and sway ──────────────────────────────────────

interface I3LikeNode {
  type?: string;
  name?: string;
  window?: number | null;
  pid?: number | null;
  app_id?: string | null;
  window_properties?: { class?: string; instance?: string; title?: string } | null;
  nodes?: I3LikeNode[];
  floating_nodes?: I3LikeNode[];
}

function classOf(node: I3LikeNode): string {
  // sway reports `app_id`; i3/X11 reports `window_properties.class`.
  return node.app_id ?? node.window_properties?.class ?? "";
}

function titleOf(node: I3LikeNode): string {
  return node.window_properties?.title ?? node.name ?? "";
}

/**
 * Flatten an i3/sway tree into workspaces and the windows on them.
 *
 * Recursive on purpose: a container can nest arbitrarily deep, and the workspace
 * a window belongs to is whatever workspace node encloses it — not something the
 * leaf itself knows.
 */
export function walkI3Tree(root: unknown): {
  workspaces: InventoryWorkspace[];
  windows: InventoryWindow[];
} {
  const workspaces: InventoryWorkspace[] = [];
  const windows: InventoryWindow[] = [];
  const counts = new Map<string, number>();

  const visit = (node: I3LikeNode, workspace: string): void => {
    let ws = workspace;
    if (node.type === "workspace") {
      ws = node.name ?? workspace;
      workspaces.push({
        name: ws,
        index: /^\d+$/.test(ws) ? Number(ws) : null,
        focused: false, // filled in from get_workspaces, which knows
        visible: false,
        windowCount: 0,
      });
    }
    if (typeof node.window === "number" && node.window !== 0) {
      windows.push({
        id: `0x${node.window.toString(16)}`,
        appClass: classOf(node),
        title: titleOf(node),
        workspace: ws,
        // i3 sets this on Wayland; on X11 it is null and xprop supplies it.
        pid: typeof node.pid === "number" ? node.pid : null,
      });
      counts.set(ws, (counts.get(ws) ?? 0) + 1);
    }
    for (const child of [...(node.nodes ?? []), ...(node.floating_nodes ?? [])]) visit(child, ws);
  };

  if (root && typeof root === "object") visit(root as I3LikeNode, "");
  for (const ws of workspaces) ws.windowCount = counts.get(ws.name) ?? 0;
  return { workspaces, windows };
}

/** Merge `get_workspaces` flags (focused/visible) into a walked tree. */
export function mergeI3WorkspaceFlags(
  walked: InventoryWorkspace[],
  reported: unknown,
): InventoryWorkspace[] {
  if (!Array.isArray(reported)) return walked;
  const byName = new Map<string, { focused?: boolean; visible?: boolean }>();
  for (const raw of reported) {
    const w = raw as { name?: unknown; focused?: unknown; visible?: unknown };
    if (typeof w.name === "string") {
      byName.set(w.name, { focused: Boolean(w.focused), visible: Boolean(w.visible) });
    }
  }
  return walked.map((ws) => {
    const flags = byName.get(ws.name);
    return flags ? { ...ws, focused: Boolean(flags.focused), visible: Boolean(flags.visible) } : ws;
  });
}

// ── window-manager probes ──────────────────────────────────────────────────

interface ProbeResult {
  wm: string;
  workspaces: InventoryWorkspace[];
  windows: InventoryWindow[];
}

/** i3 and sway: same tree, different binary. */
async function probeI3Like(binary: string, wm: string): Promise<ProbeResult | null> {
  const tree = await run([binary, "-t", "get_tree"], { timeoutMs: 3000 });
  if (tree.code !== 0 || !tree.out.trim()) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(tree.out);
  } catch {
    return null;
  }
  const walked = walkI3Tree(parsed);
  const ws = await run([binary, "-t", "get_workspaces"], { timeoutMs: 3000 });
  let reported: unknown = null;
  try {
    reported = JSON.parse(ws.out);
  } catch {
    /* flags are a nicety; a tree without them is still useful */
  }
  return { wm, workspaces: mergeI3WorkspaceFlags(walked.workspaces, reported), windows: walked.windows };
}

/** niri speaks JSON if asked. */
export function parseNiri(workspacesJson: unknown, windowsJson: unknown): ProbeResult {
  const rawWs = Array.isArray(workspacesJson) ? workspacesJson : [];
  const rawWin = Array.isArray(windowsJson) ? windowsJson : [];
  const windows: InventoryWindow[] = rawWin.map((w: Record<string, unknown>) => ({
    id: String(w.id ?? ""),
    appClass: String(w.app_id ?? ""),
    title: String(w.title ?? ""),
    workspace: w.workspace_id != null ? String(w.workspace_id) : "",
    pid: typeof w.pid === "number" ? w.pid : null,
  }));
  const workspaces: InventoryWorkspace[] = rawWs.map((w: Record<string, unknown>) => {
    const name = String(w.name ?? w.idx ?? "");
    return {
      name,
      index: typeof w.idx === "number" ? w.idx : null,
      focused: Boolean(w.is_focused),
      visible: Boolean(w.is_active),
      windowCount: windows.filter((x) => x.workspace === String(w.id)).length,
    };
  });
  return { wm: "niri", workspaces, windows };
}

/**
 * Hyprland's `hyprctl -j` is already JSON, twice over.
 *
 * A workspace object does not say whether it is focused — that is a separate
 * `activeworkspace` call — so [active] (its `id`) is what marks one.
 */
export function parseHyprland(
  workspacesJson: unknown,
  clientsJson: unknown,
  active?: { id?: unknown; name?: unknown },
): ProbeResult {
  const rawWs = Array.isArray(workspacesJson) ? workspacesJson : [];
  const rawWin = Array.isArray(clientsJson) ? clientsJson : [];
  const activeId = typeof active?.id === "number" ? active.id : null;
  const windows: InventoryWindow[] = rawWin.map((w: Record<string, unknown>) => ({
    id: String(w.address ?? ""),
    appClass: String(w.class ?? ""),
    title: String(w.title ?? ""),
    workspace: w.workspace && typeof w.workspace === "object"
      ? String((w.workspace as Record<string, unknown>).name ?? "")
      : "",
    pid: typeof w.pid === "number" ? w.pid : null,
  }));
  const workspaces: InventoryWorkspace[] = rawWs.map((w: Record<string, unknown>) => {
    const name = String(w.name ?? w.id ?? "");
    const id = typeof w.id === "number" ? w.id : null;
    return {
      name,
      index: id,
      focused: id !== null && id === activeId,
      visible: typeof w.monitor === "string" && w.monitor.length > 0,
      windowCount: typeof w.windows === "number" ? w.windows : 0,
    };
  });
  return { wm: "hyprland", workspaces, windows };
}

/**
 * Parse `xprop -root _NET_CLIENT_LIST`.
 *
 * EWMH is the standard every X11 desktop implements, which is what makes GNOME,
 * KDE and XFCE work without three more probes: they do not expose a tree, but
 * they all publish their windows here.
 */
export function parseEWMHClientList(stdout: string): string[] {
  const ids = stdout.match(/0x[0-9a-fA-F]+/g) ?? [];
  return [...new Set(ids)];
}

/** Parse the `xprop -id <id>` output for one window. */
export function parseEWMHWindow(id: string, stdout: string): InventoryWindow | null {
  const line = (key: string): string => {
    const m = stdout.match(new RegExp(`^${key}[^=]*= (.*)$`, "m"));
    return m ? m[1].trim() : "";
  };
  const title = line("_NET_WM_NAME").replace(/^"|"$/g, "") || line("WM_NAME").replace(/^"|"$/g, "");
  // WM_CLASS is `"instance", "Class"`; the second is what people recognise.
  const classMatch = stdout.match(/^WM_CLASS[^=]*= (.*)$/m);
  const appClass = classMatch
    ? (classMatch[1].match(/"([^"]*)"/g) ?? [])
        .map((s) => s.replace(/"/g, ""))
        .pop() ?? ""
    : "";
  const pid = Number(line("_NET_WM_PID"));
  const desktop = line("_NET_WM_DESKTOP");
  if (!title && !appClass) return null;
  return {
    id,
    appClass,
    title,
    workspace: desktop && /^\d+$/.test(desktop) ? desktop : "",
    pid: Number.isFinite(pid) && pid > 0 ? pid : null,
  };
}

/**
 * Fill in window pids the WM did not report.
 *
 * i3 sets `pid` on Wayland windows only; on X11 every window comes back with
 * `null`, which would leave every pi session unattributed. `_NET_WM_PID` is the
 * EWMH property that answers it, and it is exactly what ties a window to the
 * process tree.
 */
async function enrichWindowPids(windows: InventoryWindow[]): Promise<void> {
  const missing = windows.filter((w) => w.pid == null);
  if (missing.length === 0) return;
  await Promise.all(
    missing.map(async (w) => {
      const out = await run(["xprop", "-id", w.id, "_NET_WM_PID"], { timeoutMs: 2000 });
      if (out.code !== 0) return;
      const pid = Number((out.out.match(/=\s*(\d+)/) ?? [])[1]);
      if (Number.isFinite(pid) && pid > 0) w.pid = pid;
    }),
  );
}

async function probeEWMH(): Promise<ProbeResult | null> {
  const list = await run(["xprop", "-root", "_NET_CLIENT_LIST"], { timeoutMs: 3000 });
  if (list.code !== 0) return null;
  const ids = parseEWMHClientList(list.out);
  // Bounded: one xprop per window, in parallel, is fine for a desktop's worth of
  // windows and avoids a shell pipeline's quoting problems.
  const windows = (
    await Promise.all(
      ids.map(async (id) => {
        const out = await run(
          ["xprop", "-id", id, "WM_CLASS", "_NET_WM_NAME", "WM_NAME", "_NET_WM_DESKTOP", "_NET_WM_PID"],
          { timeoutMs: 3000 },
        );
        return out.code === 0 ? parseEWMHWindow(id, out.out) : null;
      }),
    )
  ).filter((w): w is InventoryWindow => w !== null);
  // Which desktop is on screen. `_NET_CURRENT_DESKTOP` is a 0-based index, while
  // `_NET_WM_DESKTOP` on a window is the same 0-based index — so they compare
  // directly, and both are reported as the bare number string.
  const current = await run(["xprop", "-root", "_NET_CURRENT_DESKTOP"], { timeoutMs: 2000 });
  const active = (current.out.match(/=\s*(\d+)/) ?? [])[1] ?? "";
  const byDesktop = new Map<string, number>();
  for (const w of windows) {
    // A window with no desktop set (sticky/pinned) is not on a desktop of its own.
    if (w.workspace === "") continue;
    byDesktop.set(w.workspace, (byDesktop.get(w.workspace) ?? 0) + 1);
  }
  if (active !== "" && !byDesktop.has(active)) byDesktop.set(active, 0);
  const workspaces: InventoryWorkspace[] = [...byDesktop.entries()]
    .sort((a, b) => Number(a[0]) - Number(b[0]))
    .map(([name, count]) => ({
      name,
      index: /^\d+$/.test(name) ? Number(name) : null,
      focused: name === active,
      visible: name === active,
      windowCount: count,
    }));
  return { wm: "ewmh", workspaces, windows };
}

/**
 * Ask each window manager in turn. The order is "most specific first": a WM with
 * a real tree gives workspace nesting, EWMH only gives flat desktops.
 */
export async function probeWindowManager(): Promise<ProbeResult & { detail?: string }> {
  const i3 = await probeI3Like("i3-msg", "i3");
  if (i3) return i3;
  const sway = await probeI3Like("swaymsg", "sway");
  if (sway) return sway;

  const niriWs = await run(["niri", "msg", "--json", "workspaces"], { timeoutMs: 3000 });
  const niriWin = await run(["niri", "msg", "--json", "windows"], { timeoutMs: 3000 });
  if (niriWs.code === 0 && niriWin.code === 0) {
    try {
      return parseNiri(JSON.parse(niriWs.out), JSON.parse(niriWin.out));
    } catch {
      /* fall through to the next probe */
    }
  }

  const hyprWs = await run(["hyprctl", "-j", "workspaces"], { timeoutMs: 3000 });
  const hyprClients = await run(["hyprctl", "-j", "clients"], { timeoutMs: 3000 });
  if (hyprWs.code === 0 && hyprClients.code === 0) {
    try {
      const active = await run(["hyprctl", "-j", "activeworkspace"], { timeoutMs: 2000 });
      return parseHyprland(
        JSON.parse(hyprWs.out),
        JSON.parse(hyprClients.out),
        active.code === 0 ? JSON.parse(active.out) : undefined,
      );
    } catch {
      /* fall through */
    }
  }

  const ewmh = await probeEWMH();
  if (ewmh) return ewmh;
  return { wm: "unknown", workspaces: [], windows: [], detail: "no window manager could be enumerated" };
}

// ── pi sessions ────────────────────────────────────────────────────────────

/**
 * Where pi keeps the transcripts for a directory.
 *
 * `--home-ldeen-Documents-ponydeck--`: a leading and trailing `--` around the
 * path with every `/` turned into `-`. Encoded exactly as pi does it, because
 * that is the only thing that finds the right folder.
 *
 * The leading (and any trailing) separator is *dropped* rather than turned into a
 * dash, so the `--` delimiters are the only thing outside the path components — a
 * naive `"--" + cwd.replaceAll("/", "-")` produces three dashes and finds
 * nothing. Interior separators do become dashes, doubles included.
 */
export function encodePiSessionDir(cwd: string): string {
  return `--${cwd.replace(/^\/+|\/+$/g, "").replaceAll("/", "-")}--`;
}

/** Read the `{"type":"session",...}` header pi writes as a transcript's first line. */
export function parsePiSessionHead(text: string): {
  id: string | null;
  cwd: string | null;
  startedAt: string | null;
} {
  const firstLine = text.split("\n", 1)[0] ?? "";
  try {
    const obj = JSON.parse(firstLine) as Record<string, unknown>;
    if (obj.type !== "session") return { id: null, cwd: null, startedAt: null };
    return {
      id: typeof obj.id === "string" ? obj.id : null,
      cwd: typeof obj.cwd === "string" ? obj.cwd : null,
      startedAt: typeof obj.timestamp === "string" ? obj.timestamp : null,
    };
  } catch {
    return { id: null, cwd: null, startedAt: null };
  }
}

/**
 * One directory's transcripts, newest first.
 *
 * The filename already begins with an ISO timestamp, but mtime is what is sorted
 * on: it is the actual last write, and a transcript whose clock-skewed name
 * disagreed with its contents would otherwise be ordered wrongly.
 */
export function listTranscripts(dir: string): string[] {
  let names: string[];
  try {
    names = readdirSync(dir).filter((n) => n.endsWith(".jsonl"));
  } catch {
    return [];
  }
  const stamped: { path: string; mtime: number }[] = [];
  for (const name of names) {
    const path = join(dir, name);
    try {
      stamped.push({ path, mtime: statSync(path).mtimeMs });
    } catch {
      /* a transcript that vanished mid-scan is not a failure */
    }
  }
  return stamped.sort((a, b) => b.mtime - a.mtime).map((t) => t.path);
}

/** The newest transcript for one directory, or null when there is none. */
export function newestTranscript(dir: string): string | null {
  return listTranscripts(dir)[0] ?? null;
}

/** Parse the ppid out of `/proc/<pid>/stat`. The comm may contain spaces and parens. */
export function ppidFromStat(stat: string): number | null {
  const close = stat.lastIndexOf(")");
  if (close < 0) return null;
  const ppid = Number(stat.slice(close + 2).split(" ")[1]);
  return Number.isFinite(ppid) ? ppid : null;
}

/** Pids whose `comm` is exactly `pi`. */
export function scanPiPids(procRoot = "/proc"): number[] {
  const pids: number[] = [];
  let entries: string[];
  try {
    entries = readdirSync(procRoot);
  } catch {
    return pids;
  }
  for (const name of entries) {
    if (!/^\d+$/.test(name)) continue;
    try {
      if (readFileSync(join(procRoot, name, "comm"), "utf8").trim() === "pi") pids.push(Number(name));
    } catch {
      /* the process exited between readdir and read: expected, not an error */
    }
  }
  return pids;
}

/**
 * Walk a process's parents, and say which of them is a window-manager window.
 *
 * This is how a pi process is tied to the window it runs in: the window's pid is
 * an *ancestor* of pi (i3 → sh → alacritty → bash → pi), so the walk meets the
 * window part-way up. Titles are a hint; ancestry is proof.
 */
export function attributeWindows(
  chains: { pid: number; ancestors: number[] }[],
  windows: InventoryWindow[],
): Map<number, InventoryWindow> {
  const byPid = new Map<number, InventoryWindow>();
  for (const w of windows) if (w.pid != null) byPid.set(w.pid, w);
  const found = new Map<number, InventoryWindow>();
  for (const chain of chains) {
    for (const ancestor of chain.ancestors) {
      const window = byPid.get(ancestor);
      if (window) {
        found.set(chain.pid, window);
        break;
      }
    }
  }
  return found;
}

function ancestry(pid: number, procRoot: string, maxDepth = 24): number[] {
  const out: number[] = [];
  let current = pid;
  for (let i = 0; i < maxDepth; i++) {
    let stat: string;
    try {
      stat = readFileSync(join(procRoot, String(current), "stat"), "utf8");
    } catch {
      break;
    }
    const ppid = ppidFromStat(stat);
    if (ppid == null || ppid <= 1) break;
    out.push(ppid);
    current = ppid;
  }
  return out;
}

/**
 * Every running pi process, with the session it is writing to.
 *
 * Which transcript a process is writing cannot be read from the process: pi
 * rewrites its own `comm` and cmdline, keeps nothing session-shaped in its
 * environment, and does not hold the file open. What is known is the *directory*
 * (from `/proc/<pid>/cwd`) and, from the transcripts' own timestamps, the order
 * in which they were last written. So within a directory the newest transcript is
 * paired with the newest process — see [assignTranscripts].
 */
export interface PiProc {
  pid: number;
  cwd: string;
  /** Process start, in clock ticks since boot (`/proc/<pid>/stat` field 22). */
  startTicks: number;
  ancestors: number[];
}

/**
 * Pair each pi process with the transcript it is writing.
 *
 * Both sides are sorted newest-first and zipped, per directory. This cannot be
 * proof — two pi processes in one directory started at the same moment are
 * indistinguishable from outside — but it is right whenever the sessions were
 * opened at different times, and it always yields *distinct* sessions rather
 * than showing the same transcript three times.
 *
 * Transcripts left over (more files than processes) are ignored; processes left
 * over get `null` rather than a transcript that belongs to someone else.
 */
export function assignTranscripts(
  procs: PiProc[],
  transcriptsByCwd: Map<string, string[]>,
): Map<number, string | null> {
  const out = new Map<number, string | null>();
  const byCwd = new Map<string, PiProc[]>();
  for (const p of procs) {
    const list = byCwd.get(p.cwd) ?? [];
    list.push(p);
    byCwd.set(p.cwd, list);
  }
  for (const [cwd, group] of byCwd) {
    const fresh = [...group].sort((a, b) => b.startTicks - a.startTicks);
    const files = transcriptsByCwd.get(cwd) ?? [];
    fresh.forEach((p, i) => out.set(p.pid, files[i] ?? null));
  }
  return out;
}

export function scanPiSessions(
  home = homedir(),
  procRoot = "/proc",
  windows: InventoryWindow[] = [],
): InventoryPiSession[] {
  const procs: PiProc[] = [];
  for (const pid of scanPiPids(procRoot)) {
    const cwd = readCwd(pid, procRoot);
    if (!cwd) continue;
    procs.push({
      pid,
      cwd,
      startTicks: readStartTicks(pid, procRoot),
      ancestors: ancestry(pid, procRoot),
    });
  }
  const sessionsRoot = join(home, ".pi", "agent", "sessions");
  const transcripts = new Map<string, string[]>();
  for (const cwd of new Set(procs.map((p) => p.cwd))) {
    transcripts.set(cwd, listTranscripts(join(sessionsRoot, encodePiSessionDir(cwd))));
  }
  const assigned = assignTranscripts(procs, transcripts);
  const attributed = attributeWindows(
    procs.map((p) => ({ pid: p.pid, ancestors: p.ancestors })),
    windows,
  );
  return procs.map((p) => buildPiSession(p, assigned.get(p.pid) ?? null, attributed.get(p.pid) ?? null));
}

function readCwd(pid: number, procRoot: string): string {
  // Following the symlink is the only way to learn a live process's cwd.
  try {
    return readlinkSync(join(procRoot, String(pid), "cwd"));
  } catch {
    return "";
  }
}

/** `/proc/<pid>/stat` field 22: start time in clock ticks since boot. */
export function startTicksFromStat(stat: string): number {
  const close = stat.lastIndexOf(")");
  if (close < 0) return 0;
  const ticks = Number(stat.slice(close + 2).split(" ")[19]);
  return Number.isFinite(ticks) ? ticks : 0;
}

function readStartTicks(pid: number, procRoot: string): number {
  try {
    return startTicksFromStat(readFileSync(join(procRoot, String(pid), "stat"), "utf8"));
  } catch {
    return 0;
  }
}

/**
 * Read the first line of a transcript without reading the whole thing.
 *
 * Transcripts grow to megabytes; the session header is the first line and there
 * is no reason to pull the rest through memory to find it.
 */
export function readFileHead(path: string, bytes = 4096): string {
  const fd = openSync(path, "r");
  try {
    const buffer = Buffer.alloc(bytes);
    const read = readSync(fd, buffer, 0, bytes, 0);
    return buffer.subarray(0, read).toString("utf8");
  } finally {
    closeSync(fd);
  }
}

function buildPiSession(
  proc: PiProc,
  transcript: string | null,
  window: InventoryWindow | null,
): InventoryPiSession {
  let head = { id: null as string | null, cwd: null as string | null, startedAt: null as string | null };
  if (transcript) {
    try {
      head = parsePiSessionHead(readFileHead(transcript));
    } catch {
      /* an unreadable transcript does not make the session unreal */
    }
  }
  const dir = proc.cwd || head.cwd || "";
  return {
    pid: proc.pid,
    cwd: dir,
    sessionId: head.id,
    startedAt: head.startedAt,
    transcript,
    windowId: window?.id ?? null,
    workspace: window?.workspace ?? null,
    label: dir.split("/").filter(Boolean).pop() ?? "pi",
  };
}

// ── the one call the RPC makes ─────────────────────────────────────────────

export async function readDesktop(home = homedir()): Promise<DesktopInventory> {
  const probe = await probeWindowManager();
  // A WM tree may not carry pids (i3 on X11 does not), and without them a pi
  // session cannot be tied to its window at all.
  await enrichWindowPids(probe.windows);
  // Windows first, so pi sessions can be tied to the window they run in.
  const piSessions = scanPiSessions(home, "/proc", probe.windows);
  return {
    wm: probe.wm,
    workspaces: probe.workspaces,
    windows: probe.windows,
    piSessions,
    ...(probe.detail ? { detail: probe.detail } : {}),
  };
}
