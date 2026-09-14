import { test, expect, afterAll } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TmuxControlPlane } from "../src/tmux/control-plane.ts";

const dir = mkdtempSync(join(tmpdir(), "ldmux-faketmux-"));
const realPath = process.env.PATH ?? "";

afterAll(() => {
  process.env.PATH = realPath;
  rmSync(dir, { recursive: true, force: true });
});

/**
 * A tmux that ignores SIGTERM and never exits: without the SIGKILL-and-deadline
 * escalation in run(), the caller (and, in the daemon, a whole connection)
 * would hang forever.
 */
function installStubbornFakeTmux(): void {
  const script = join(dir, "tmux");
  writeFileSync(script, '#!/bin/sh\ntrap "" TERM\necho started\nsleep 300\n');
  chmodSync(script, 0o755);
  process.env.PATH = `${dir}:${realPath}`;
}

test("a tmux that ignores SIGTERM still returns (bounded, code -1)", async () => {
  installStubbornFakeTmux();
  const cp = new TmuxControlPlane("test-socket", 300);
  const started = Date.now();
  const sessions = await cp.listSessions();
  const elapsed = Date.now() - started;
  // code -1 must read as "unknown", never as "no sessions".
  expect(sessions).toBeNull();
  // 300ms SIGTERM + 150ms grace + 500ms deadline is the worst case.
  expect(elapsed).toBeLessThan(3000);
});

test("hard deadline returns even if the pipes never close", async () => {
  installStubbornFakeTmux();
  const cp = new TmuxControlPlane("test-socket", 100);
  const started = Date.now();
  const sessions = await cp.listSessions();
  expect(sessions).toBeNull();
  expect(Date.now() - started).toBeLessThan(2500);
});

test("a normal tmux command is unaffected", async () => {
  process.env.PATH = realPath;
  const socket = `ldmux-cp-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
  const cp = new TmuxControlPlane(socket);
  await cp.ensureServer({ historyLimit: 1000 });
  await cp.newSession({ name: "t", cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const sessions = await cp.listSessions();
  expect(sessions?.map((s) => s.name)).toContain("t");
  expect(await cp.hasSession("t")).toBe(true);
  expect(await cp.hasSession("nope")).toBe(false);
  await cp.killSession("t");
  Bun.spawnSync(["tmux", "-L", socket, "kill-server"]);
});