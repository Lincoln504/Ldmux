import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { PaneStream } from "../src/stream.ts";
import { TmuxControlPlane } from "../src/tmux/control-plane.ts";

const tmuxSocket = `ldmux-streamtest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-stream-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();

const cp = new TmuxControlPlane(tmuxSocket);

afterAll(() => {
  daemon.stop();
  Bun.spawnSync(["tmux", "-L", tmuxSocket, "kill-server"]);
  rmSync(stateDir, { recursive: true, force: true });
});

const sleep = (ms: number) => Bun.sleep(ms);

async function waitFor(pred: () => boolean | Promise<boolean>, ms = 6000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await pred()) return true;
    await sleep(50);
  }
  return false;
}

test("PaneStream fans out live output and snapshots the screen", async () => {
  const name = "ldmux-stream-a";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const stream = new PaneStream(tmuxSocket, name, cp);
  const chunks: string[] = [];
  const resets: string[] = [];
  const unsub = await stream.subscribe({
    onOutput: (t) => chunks.push(t),
    onReset: (r) => resets.push(r),
  });
  expect(stream.subscriberCount).toBe(1);

  await cp.sendText(name, "printf 'STREAM_MARKER\\n'");
  await cp.sendKey(name, "Enter");
  const got = await waitFor(() => chunks.join("").includes("STREAM_MARKER"));
  expect(got).toBe(true);

  // The visible screen may lag the stream (shell startup noise); poll it.
  expect(await waitFor(async () => (await stream.snapshot()).screen.includes("STREAM_MARKER"))).toBe(true);
  const snap = await stream.snapshot();
  expect(snap.state.cols).toBe(80);
  expect(snap.epoch).toBeGreaterThan(0);

  unsub();
  expect(stream.subscriberCount).toBe(0);
  expect(resets).toEqual([]);
  stream.close();
});

test("PaneStream survives subscriber churn (ref-counted control client)", async () => {
  const name = "ldmux-stream-b";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const stream = new PaneStream(tmuxSocket, name, cp);
  const a: string[] = [];
  const b: string[] = [];
  const unsubA = await stream.subscribe({ onOutput: (t) => a.push(t), onReset: () => {} });
  const unsubB = await stream.subscribe({ onOutput: (t) => b.push(t), onReset: () => {} });
  expect(stream.subscriberCount).toBe(2);
  await cp.sendText(name, "echo CHURN_OK");
  await cp.sendKey(name, "Enter");
  expect(await waitFor(() => a.join("").includes("CHURN_OK") && b.join("").includes("CHURN_OK"))).toBe(true);
  unsubA();
  unsubB();
  stream.close();
});
test("closing a stream while it is connecting rejects cleanly (no leaked client)", async () => {
  const name = "ldmux-stream-c";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const stream = new PaneStream(tmuxSocket, name, cp);
  const pending = stream.subscribe({ onOutput: () => {}, onReset: () => {} });
  stream.close("session ended");
  await expect(pending).rejects.toThrow(/stream closed/);
  expect(stream.subscriberCount).toBe(0);
  await expect(stream.subscribe({ onOutput: () => {}, onReset: () => {} })).rejects.toThrow(/stream closed/);
});
