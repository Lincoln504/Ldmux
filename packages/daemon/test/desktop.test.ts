import { test, expect } from "../../testing/src/expect.ts";
import {
  closeLive,
  findWindowIdByClass,
  isShellForeground,
  liveArgv,
  liveExecArgv,
  liveWindowClass,
  mirrorArgv,
  mirrorCommand,
  mirrorTmuxArgs,
  placeLive,
  safeWorkspace,
} from "../src/desktop.ts";
import { resolveTmuxBin } from "../src/tmux/control-plane.ts";

test("a shell cannot use a mouse click, so it is never sent one", () => {
  // The mouse mode is sticky: a program that enables reporting and then exits
  // (or is killed) leaves it on, so the pane flags still say "mouse" while a
  // shell is what would receive the bytes — and a shell prints them.
  for (const sh of ["bash", "sh", "zsh", "fish", "dash", "nu"]) {
    expect(isShellForeground(sh)).toBe(true);
  }
  // tmux reports the login shell with a leading dash.
  expect(isShellForeground("-bash")).toBe(true);
  expect(isShellForeground("BASH")).toBe(true);
});

test("a program that can use a click is not mistaken for a shell", () => {
  for (const p of ["vim", "less", "htop", "pi", "claude", "codex", "node", "lazygit", ""]) {
    expect(isShellForeground(p)).toBe(false);
  }
});

test("mirror is read-only by default (input stays with the phone)", () => {
  expect(mirrorTmuxArgs("sock", "ldmux-x")).toEqual(["-L", "sock", "attach", "-r", "-t", "ldmux-x"]);
});

test("mirror can be made writable with readOnly:false", () => {
  expect(mirrorTmuxArgs("sock", "ldmux-x", { readOnly: false })).toEqual(["-L", "sock", "attach", "-t", "ldmux-x"]);
});

test("mirrorArgv wraps tmux in the terminal with an i3-targetable class", () => {
  expect(mirrorArgv("sock", "ldmux-x")).toEqual([
    "alacritty",
    "--class",
    "ldmux-mirror",
    "-e",
    "tmux",
    "-L",
    "sock",
    "attach",
    "-r",
    "-t",
    "ldmux-x",
  ]);
});

test("mirrorCommand renders a copy-pasteable command line", () => {
  expect(mirrorCommand("sock", "ldmux-x")).toBe(
    "alacritty --class ldmux-mirror -e tmux -L sock attach -r -t ldmux-x",
  );
  expect(mirrorCommand("sock", "ldmux-x", { terminal: "kitty", windowClass: "my-class" })).toContain("kitty --class my-class");
});
test("mirrorCommand shell-quotes every argument, so i3-msg exec cannot inject", () => {
  // i3-msg exec runs its argument through a shell, and it only takes one
  // space-free metacharacter to escape. Each of these must be quoted.
  const cmd = mirrorCommand("sock", "x$(id)", { terminal: "kitty; rm -rf /", windowClass: "c`whoami`" });
  expect(cmd).toContain("'x$(id)'");
  expect(cmd).toContain("'kitty; rm -rf /'");
  expect(cmd).toContain("'c`whoami`'");
  // A single quote inside a single-quoted string is escaped, not left dangling.
  expect(mirrorCommand("sock", "a'b")).toContain(`'a'\\''b'`);
  // Ordinary args stay bare for readability.
  expect(mirrorCommand("sock", "ldmux-x")).toBe("alacritty --class ldmux-mirror -e tmux -L sock attach -r -t ldmux-x");
});

// ── openMirror against a stub `i3-msg` ────────────────────────────────────
// The IO half of desktop.ts was left untested when the pure helpers were
// extracted. Now that run() honours an explicit env, it can be driven with a
// fake i3-msg on PATH (no X11 needed).
import { afterAll, beforeEach } from "../../testing/src/expect.ts";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openMirror } from "../src/desktop.ts";

const binDir = mkdtempSync(join(tmpdir(), "ldmux-fake-i3-"));
const realPath = process.env.PATH ?? "";
const realDisplay = process.env.DISPLAY;

function stubI3(script: string): void {
  const path = join(binDir, "i3-msg");
  writeFileSync(path, `#!/bin/sh\necho "$@" >> ${join(binDir, "args.log")}\n${script}\n`);
  chmodSync(path, 0o755);
  process.env.PATH = `${binDir}:${realPath}`;
}

beforeEach(() => {
  process.env.PATH = realPath;
  process.env.DISPLAY = ":0.0";
  rmSync(join(binDir, "args.log"), { force: true });
});

afterAll(() => {
  process.env.PATH = realPath;
  if (realDisplay === undefined) delete process.env.DISPLAY;
  else process.env.DISPLAY = realDisplay;
  rmSync(binDir, { recursive: true, force: true });
});

test("openMirror without DISPLAY reports it and runs nothing", async () => {
  delete process.env.DISPLAY;
  const res = await openMirror("sock", "ldmux-x");
  expect(res.ok).toBe(false);
  expect(res.detail).toContain("DISPLAY");
});

test("openMirror switches workspace then execs the shell-quoted mirror command", async () => {
  stubI3("exit 0");
  const res = await openMirror("sock", "ldmux-x", { workspace: 7 });
  expect(res.ok).toBe(true);
  const log = readFileSync(join(binDir, "args.log"), "utf8");
  expect(log).toContain("workspace number 7");
  // The exec line carries the quoted command, so a metacharacter cannot inject.
  expect(log).toContain("exec -- alacritty --class ldmux-mirror -e tmux -L sock attach -r -t ldmux-x");
});

test("openMirror reports a failure when i3-msg is unavailable", async () => {
  process.env.PATH = join(binDir, "definitely-empty");
  const res = await openMirror("sock", "ldmux-x");
  expect(res.ok).toBe(false);
  expect(res.detail).toContain("i3-msg");
});

// ── the live window: placed, never followed ───────────────────────────────

test("liveWindowClass is derived from the session id and cannot carry i3 syntax", () => {
  expect(liveWindowClass("0de567c644")).toBe("ldmux-live-0de567c644");
  // An adopted session's id is caller-supplied, and the class is interpolated
  // into an i3 criterion. A quote here would end `[class="…"]` and turn the rest
  // of the argument into i3 commands.
  const nasty = liveWindowClass('x"] kill; exec -- rm -rf /');
  expect(nasty).toBe("ldmux-live-xkillexec--rm-rf");
  expect(nasty).not.toContain('"');
  expect(nasty).not.toContain(" ");
  expect(nasty).not.toContain("[");
  // Bounded, so nothing enormous reaches the window manager.
  expect(liveWindowClass("a".repeat(200)).length).toBeLessThanOrEqual(51);
});

test("safeWorkspace accepts real workspace names and refuses i3 syntax", () => {
  for (const ok of ["1", "10", "2", "web", "code-2", "a.b"]) {
    expect(safeWorkspace(ok)).toBe(ok);
  }
  // A workspace name also reaches i3 as a criterion in closeLive, so anything
  // that could be syntax is refused rather than escaped.
  for (const bad of ["", "  ", "1; workspace 2", 'x"] kill', "$(id)", "a/b", "a".repeat(33)]) {
    expect(safeWorkspace(bad)).toBe(null);
  }
  expect(safeWorkspace("  7  ")).toBe("7");
});

test("liveArgv attaches writably, because 'live' means it can be typed in", () => {
  // Not `-r`: the point of putting a shell on the desktop is to use it there.
  // Geometry is safe because the pane owns its size (window-size manual).
  expect(liveArgv("sock", "ldmux-x", "abc")).toEqual([
    "alacritty",
    "--class",
    "ldmux-live-abc",
    "-e",
    "tmux",
    "-L",
    "sock",
    "attach",
    "-t",
    "ldmux-x",
  ]);
});

test("findWindowIdByClass finds a window nested in a split or floating", () => {
  const tree = JSON.stringify({
    type: "root",
    nodes: [
      {
        type: "workspace",
        name: "1",
        nodes: [
          { type: "con", window: 1, window_properties: { class: "firefox" } },
          { type: "con", nodes: [{ type: "con", window: 73400325, window_properties: { class: "ldmux-live-abc" } }] },
        ],
        floating_nodes: [],
      },
    ],
  });
  expect(findWindowIdByClass(tree, "ldmux-live-abc")).toBe("0x4600005");
});

test("findWindowIdByClass reads sway's app_id and returns null when absent", () => {
  const sway = JSON.stringify({ type: "con", window: 5, app_id: "ldmux-live-abc" });
  expect(findWindowIdByClass(sway, "ldmux-live-abc")).toBe("0x5");
  expect(findWindowIdByClass(JSON.stringify({ type: "con", window: 5, app_id: "other" }), "ldmux-live-abc")).toBe(null);
  // Unparseable output is not a crash: i3 may be mid-restart.
  expect(findWindowIdByClass("not json", "x")).toBe(null);
});

test("placeLive refuses a workspace it cannot safely name, before touching i3", async () => {
  const res = await placeLive("abc", '1; exec -- rm -rf /');
  expect(res.ok).toBe(false);
  expect(res.detail).toContain("workspace");
});

test("closeLive reports whether i3 could be asked at all", async () => {
  const previous = process.env.PATH;
  process.env.PATH = join(binDir, "definitely-empty");
  try {
    const res = await closeLive("abc");
    expect(res.ok).toBe(false);
    expect(res.detail).toContain("i3-msg");
  } finally {
    process.env.PATH = previous;
  }
});

test("liveExecArgv passes the command as separate arguments, never one string", () => {
  // Measured on this machine, same session and class, back to back:
  //   i3-msg "exec -- alacritty …"   -> no window (i3-msg still says success)
  //   i3-msg exec -- "alacritty …"   -> window
  // So the split form is the only one that works, and this test is what keeps it.
  const argv = liveExecArgv(["alacritty", "--class", "ldmux-live-x", "-e", "/nix/store/tmux/bin/tmux"]);
  expect(argv[0]).toBe("i3-msg");
  expect(argv[1]).toBe("exec");
  expect(argv[2]).toBe("--");
  // The command is expanded into the argv, not joined into one element: 5
  // command words become 5 elements after `i3-msg exec --`.
  expect(argv.length).toBe(3 + 5);
  expect(argv[3]).toBe("alacritty");
  expect(argv[5]).toBe("ldmux-live-x");
  expect(argv[7]).toBe("/nix/store/tmux/bin/tmux");
  expect(argv.some((a) => a.includes(" "))).toBe(false);
});

test("liveExecArgv shell-quotes each element, because i3 re-parses through sh", () => {
  // i3-msg joins its arguments back into one string and i3 runs it with `sh -c`,
  // so quoting happens per element and an injection cannot ride in on one.
  const argv = liveExecArgv(["alacritty", "--class", "c$(id)"]);
  expect(argv).toContain("'c$(id)'");
  // i3-msg's own `--` is the only one; a second would be for the wrong parser.
  expect(argv.filter((a) => a === "--").length).toBe(1);
});

test("resolveTmuxBin prefers LDMUX_TMUX_BIN, then PATH order, keeping an absolute path", () => {
  // The absolute path is the point: the live window is launched through the
  // window manager, whose PATH is not this process's, and a bare `tmux` there
  // resolves to the system one — two tmux versions cannot share a server, so the
  // window would exit on arrival with a protocol mismatch.
  expect(
    resolveTmuxBin({ LDMUX_TMUX_BIN: "/opt/tmux/bin/tmux", PATH: "/usr/bin" }, () => true),
  ).toBe("/opt/tmux/bin/tmux");
  // An override that does not exist is not honoured.
  expect(resolveTmuxBin({ LDMUX_TMUX_BIN: "/nope/tmux", PATH: "/a:/b" }, (p) => p === "/b/tmux")).toBe("/b/tmux");
  // First match on PATH wins, in order.
  expect(resolveTmuxBin({ PATH: "/a:/b" }, (p) => p === "/a/tmux" || p === "/b/tmux")).toBe("/a/tmux");
  // Nothing found falls back to the bare name rather than failing here.
  expect(resolveTmuxBin({ PATH: "/a" }, () => false)).toBe("tmux");
  expect(resolveTmuxBin({}, () => false)).toBe("tmux");
  // An empty PATH entry must not become "/tmux".
  expect(resolveTmuxBin({ PATH: ":/a" }, (p) => p === "/tmux" || p === "/a/tmux")).toBe("/a/tmux");
});
