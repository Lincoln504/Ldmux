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
import { execFileSync, spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
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
/**
 * A tripwire on a **known gap**, not a passing guarantee.
 *
 * The pipe transport's liveness is read from `#{pane_pipe}`, and this module's own
 * docstring says tmux cannot answer that question: it does not notice a dead reader. This
 * test measures what actually happens when the reader is killed, and it asserts the gap so
 * that the day tmux starts reporting it — or the day someone teaches the client to — this
 * fails loudly and the comment above `checkReader` gets corrected instead of going on
 * claiming something untrue.
 *
 * Measured 2026-09-20: `#{pane_pipe}` still reports `1`, and the FIFO's read end does not
 * report EOF, so nothing tells the client. A frozen terminal with no resync.
 *
 * A first attempt closed it by combining socket EOF with the tmux poll. It never fired,
 * because of the fact above, and it was reverted rather than shipped. The mechanism that
 * *would* work is asking whether the reader process still exists — which is what the
 * `pgrep` below does, and it is why the kill is possible at all.
 */
test("GAP: a killed pipe reader is not noticed by tmux or by the socket", { timeout: 30000 }, async () => {
  const name = await newPipeSession();
  const fifo = pipeFifoPath(pipeDir, name);
  let exits = 0;
  const client = await TmuxPipeClient.connect({
    sessionName: name,
    tmuxSocket,
    pipeDir,
    onOutput: () => {},
    onExit: () => exits++,
  });

  // Prove it is live and delivering first, so what follows is about the kill and not about
  // a client that never worked.
  await cp.sendText(name, "printf 'PIPE_ALIVE\\n'");
  await cp.sendKey(name, "Enter");
  expect(await waitFor(() => client.bytesOut > 0)).toBe(true);

  // Matched on the FIFO's *filename*: tmux runs the command through a shell and the path
  // arrives single-quoted (`shQuote`), so `cat > <path>` matches nothing.
  let readers: string[] = [];
  try {
    readers = execFileSync("pgrep", ["-f", basename(fifo)], { encoding: "utf8" })
      .split("\n")
      .map((s) => s.trim())
      .filter(Boolean);
  } catch {
    // pgrep exits 1 when nothing matches; asserted below rather than left as a stack trace.
  }
  expect(readers.length).toBeGreaterThan(0);
  for (const pid of readers) process.kill(Number(pid), "SIGKILL");

  // The reader is gone, and tmux does not know. If this ever becomes "0", the gap is
  // closed and the comment in pipe-client.ts needs rewriting.
  const reported = await cp.displayMessage(name, "#{pane_pipe}");
  expect(reported.trim()).toBe("1");

  // And the socket does not say either. Long enough that a resync would have happened.
  await sleep(3000);
  expect(exits).toBe(0);
  client.close();
});

/**
 * The positive control the `GAP:` test above needs to mean anything.
 *
 * That test asserts a death is *not* reported, and an assertion about something not
 * happening passes for any reason — including the watcher never having run at all. It
 * proves tmux still claims the pipe, which makes the absence meaningful about *tmux*, but
 * nothing there proves the poll loop is live. So this drives the case tmux **does** report:
 * turning the pipe off makes `#{pane_pipe}` go to `0`, and the client must notice. If this
 * ever stops firing, the `GAP:` test above becomes vacuous too, silently.
 */
test("the liveness watcher reports death when tmux does notice", { timeout: 30000 }, async () => {
  const name = await newPipeSession();
  let exits = 0;
  const client = await TmuxPipeClient.connect({
    sessionName: name,
    tmuxSocket,
    pipeDir,
    onOutput: () => {},
    onExit: () => exits++,
  });

  await cp.sendText(name, "printf 'WATCHER_ALIVE\\n'");
  await cp.sendKey(name, "Enter");
  expect(await waitFor(() => client.bytesOut > 0)).toBe(true);

  // `pipe-pane` with no shell-command closes the current pipe. Driven through tmux
  // directly rather than through `TmuxControlPlane`, which has no such method: this is a
  // *fixture* for the client under test, and adding production API that only a test calls
  // is how an interface grows things nothing uses.
  const off = spawnSync("tmux", ["-L", tmuxSocket, "pipe-pane", "-t", name]);
  expect(off.status).toBe(0);
  expect(await waitFor(async () => (await cp.displayMessage(name, "#{pane_pipe}")).trim() === "0")).toBe(true);

  expect(await waitFor(() => exits > 0, 15000)).toBe(true);
});
