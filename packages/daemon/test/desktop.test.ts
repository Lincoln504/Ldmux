import { test, expect } from "bun:test";
import { mirrorArgv, mirrorCommand, mirrorTmuxArgs } from "../src/desktop.ts";

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
import { afterAll, beforeEach } from "bun:test";
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
