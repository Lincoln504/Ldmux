/**
 * TmuxPipeClient — the measured claims, pinned as tests.
 *
 * These are the assertions the transport decision rests on (docs/TMUX-DEPENDENCY.md
 * §8). If one of them stops being true, the reason to prefer this transport over
 * control mode has gone and the A/B should say so rather than the tests pass quietly.
 *
 * Every test runs on its own scratch tmux server and kills it afterwards. The
 * user's server is never touched.
 */

import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { TmuxControlPlane } from "../src/tmux/control-plane.ts";
import { TmuxPipeClient, pipeFifoPath } from "../src/tmux/pipe-client.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

const tmuxSocket = `ldmux-pipetest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const tmp = mkdtempSync(join(tmpdir(), "ldmux-pipe-"));
const pipeDir = join(tmp, "pipes");
const cp = new TmuxControlPlane(tmuxSocket);

afterAll(() => {
  killTmuxServer(tmuxSocket);
  rmSync(tmp, { recursive: true, force: true });
});

async function waitFor(pred: () => boolean | Promise<boolean>, ms = 8000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await pred()) return true;
    await sleep(40);
  }
  return false;
}

let seq = 0;
async function newPipeSession(): Promise<string> {
  const name = `ldmux-pipe-${++seq}`;
  await cp.ensureServer();
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  return name;
}

test("the FIFO path is derived from the session name and is not the name itself", () => {
  const p = pipeFifoPath(pipeDir, "a/b c:d");
  expect(p.startsWith(pipeDir)).toBe(true);
  expect(p.endsWith(".fifo")).toBe(true);
  expect(p.includes("a/b")).toBe(false);
  // Stable: the same session always maps to the same pipe.
  expect(pipeFifoPath(pipeDir, "a/b c:d")).toBe(p);
});

test("pipe delivers pane output, byte-verbatim", { timeout: 20000 }, async () => {
  const name = await newPipeSession();
  const got: string[] = [];
  const client = await TmuxPipeClient.connect({
    tmuxSocket,
    sessionName: name,
    pipeDir,
    onOutput: (t) => got.push(t),
    onExit: () => {},
  });
  try {
    // The proof that nothing is escaped is POSITIVE, not negative.
    //
    // A negative assertion (`the literal \033 must not appear`) cannot work here,
    // because the shell echoes the command it was given and the command text
    // contains those four characters. A test written that way passes or fails on
    // the echo rather than on the transport. What actually distinguishes an
    // escaped stream is that the four characters appear INSTEAD OF the byte: so
    // asserting the real byte is present, for sequences that cannot otherwise
    // occur, is the same claim and cannot be fooled.
    await cp.sendText(name, 'printf "SGR:\\033[31mRED\\033[0m OSC:\\033]52;c;TERDTElQMTIz\\007 UNI:\\303\\251\\360\\237\\230\\200\\n"');
    await cp.sendKey(name, "Enter");
    const seen = await waitFor(() => got.join("").includes("\u001b[31m"));
    expect(seen).toBe(true);
    const all = got.join("");
    // Each of these is a real control byte. Octal escaping would replace every
    // one of them with printable text.
    expect(all.includes("\u001b[31m")).toBe(true);
    expect(all.includes("\u001b[0m")).toBe(true);
    expect(all.includes("\u001b]52;c;TERDTElQMTIz\u0007")).toBe(true);
    // And the multibyte payload survives the byte -> string decode intact.
    expect(all.includes("é")).toBe(true);
    expect(all.includes("😀")).toBe(true);
  } finally {
    client.close();
  }
});

test("output arrives in order", { timeout: 20000 }, async () => {
  const name = await newPipeSession();
  const got: string[] = [];
  const client = await TmuxPipeClient.connect({
    tmuxSocket,
    sessionName: name,
    pipeDir,
    onOutput: (t) => got.push(t),
    onExit: () => {},
  });
  try {
    for (let i = 1; i <= 5; i++) {
      await cp.sendText(name, `echo ORDER_${i}`);
      await cp.sendKey(name, "Enter");
      await sleep(120);
    }
    expect(await waitFor(() => got.join("").includes("ORDER_5"))).toBe(true);
    const idx = [1, 2, 3, 4, 5].map((i) => got.join("").indexOf(`ORDER_${i}`));
    for (const i of idx) expect(i).toBeGreaterThanOrEqual(0);
    // Strictly increasing: the stream is ordered.
    for (let i = 1; i < idx.length; i++) expect(idx[i]!).toBeGreaterThan(idx[i - 1]!);
  } finally {
    client.close();
  }
});

test("a burst is not lost", { timeout: 30000 }, async () => {
  const name = await newPipeSession();
  const got: string[] = [];
  const client = await TmuxPipeClient.connect({
    tmuxSocket,
    sessionName: name,
    pipeDir,
    onOutput: (t) => got.push(t),
    onExit: () => {},
  });
  try {
    // 20,000 numbered lines: a countable, checkable payload.
    //
    // The wait is on the COUNT and not on a sentinel word, because the shell
    // echoes the command it was given and a sentinel would therefore be present
    // in the stream the instant the command was typed — before any of it ran.
    await cp.sendText(
      name,
      "seq 1 20000 | sed 's/^/L/;s/$/|END/'",
    );
    await cp.sendKey(name, "Enter");
    const count = (): number => {
      const seen = new Set<number>();
      for (const m of got.join("").matchAll(/\bL(\d+)\|END/g)) seen.add(Number(m[1]));
      return seen.size;
    };
    const complete = await waitFor(() => count() === 20000, 25000);
    expect(complete).toBe(true);
  } finally {
    client.close();
  }
});

test("the pane outlives the pipe, and the FIFO is cleaned up", { timeout: 20000 }, async () => {
  const name = await newPipeSession();
  const client = await TmuxPipeClient.connect({
    tmuxSocket,
    sessionName: name,
    pipeDir,
    onOutput: () => {},
    onExit: () => {},
  });
  const fifo = pipeFifoPath(pipeDir, name);
  expect(await waitFor(() => existsSync(fifo))).toBe(true);
  client.close();
  // Closing stops the pipe and removes the FIFO. The session is untouched.
  expect(await waitFor(() => !existsSync(fifo), 6000)).toBe(true);
  const sessions = (await cp.listSessions()) ?? [];
  expect(sessions.some((s) => s.name === name)).toBe(true);
});