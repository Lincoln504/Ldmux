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

test("polling fallback keeps the pane visible when control mode is gone", async () => {
  const name = "ldmux-stream-poll";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const stream = new PaneStream(tmuxSocket, name, cp, 40);
  const chunks: string[] = [];
  const resets: string[] = [];
  await stream.subscribe({ onOutput: (t) => chunks.push(t), onReset: (r) => resets.push(r) });

  stream.startPolling("test");
  expect(stream.mode).toBe("polling");
  expect(resets).toContain("test");
  // Input still works with no control connection (goes through the CLI).
  await stream.inputText("printf 'POLL_MARKER\\n'");
  await stream.sendKey(["Enter"]);
  expect(await waitFor(() => chunks.join("").includes("POLL_MARKER"))).toBe(true);
  stream.close();
}, 20000);

test("snapshotWithHistory returns more lines than the visible screen", async () => {
  const name = "ldmux-stream-hist";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 40, rows: 6 });
  const stream = new PaneStream(tmuxSocket, name, cp);
  await stream.subscribe({ onOutput: () => {}, onReset: () => {} });
  await stream.inputText("for i in $(seq 1 40); do echo LINE_$i; done");
  await stream.sendKey(["Enter"]);
  expect(await waitFor(async () => (await stream.snapshot()).screen.includes("LINE_40"))).toBe(true);

  const plain = await stream.snapshot();
  const paged = await stream.snapshotWithHistory(500);
  expect(paged.history).toBe(true);
  expect(plain.history).toBeUndefined();
  // The 6-row pane shows the tail; the paged capture reaches further back.
  expect(paged.screen.length).toBeGreaterThan(plain.screen.length);
  expect(paged.screen).toContain("LINE_1\n");
  stream.close();
});

test("history paging is clamped to the capture bound", async () => {
  const name = "ldmux-stream-clamp";
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 40, rows: 6 });
  const stream = new PaneStream(tmuxSocket, name, cp);
  await stream.subscribe({ onOutput: () => {}, onReset: () => {} });
  const snap = await stream.snapshotWithHistory(10_000_000);
  expect(snap.history).toBe(true);
  stream.close();
});
