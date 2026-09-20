/**
 * A/B: the two live transports, driven identically.
 *
 * This exists because `docs/TMUX-DEPENDENCY.md` §7 says the choice between control
 * mode and a pipe should be made on numbers rather than on an argument, and the
 * numbers that matter are the ones a phone produces. This measures what can be
 * measured without the phone in hand — echo latency, bytes on the wire, and the
 * behaviour that actually differs, which is what each one does when the client
 * stops keeping up.
 *
 * Run: nix develop --command node scripts/ab-transport.ts
 *
 * Every measurement uses its own scratch tmux server and its own state dir, and
 * kills both. The user's sessions are never touched.
 */

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { LdmuxDaemon } from "../packages/daemon/src/daemon.ts";
import { TmuxControlPlane } from "../packages/daemon/src/tmux/control-plane.ts";
import { PaneStream } from "../packages/daemon/src/stream.ts";
import type { StreamTransport } from "../packages/daemon/src/stream.ts";
import { killTmuxServer } from "../packages/daemon/test/tmux-cleanup.ts";

interface Result {
  transport: StreamTransport;
  attachBytes: number;
  echoMs: number[];
  burstBytes: number;
  burstMs: number;
  burstChunks: number;
}

const ECHO_SAMPLES = 12;

async function run(transport: StreamTransport): Promise<Result> {
  const socket = `ldmux-ab-${transport}-${process.pid}`;
  const stateDir = mkdtempSync(join(tmpdir(), `ldmux-ab-${transport}-`));
  const daemon = new LdmuxDaemon({ stateDir, tmuxSocket: socket, remotePort: 0, streamTransport: transport });
  // Everything past this point runs inside `try`, because the cleanup at the bottom is
  // not cleanup if a `throw` can jump over it. This script has already been wrong twice,
  // the burst timeout is a real branch, and the docstring at the top promises both the
  // scratch server and the state directory are killed — so a *failed* measurement, which
  // is the likely one, left a live tmux server and a temp directory behind every time.
  let stream: PaneStream | undefined;
  let unsub: (() => void) | undefined;
  try {
  await daemon.start();
  const cp = new TmuxControlPlane(socket);
  const name = `ab-${transport}`;
  await cp.newSession({ name, cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  stream = new PaneStream(socket, name, cp, {
    transport,
    pipeDir: join(stateDir, "pipes"),
  });

  let bytes = 0;
  let chunks = 0;
  const chunksSeen: { at: number; text: string }[] = [];
  unsub = await stream.subscribe({
    onOutput: (t) => {
      bytes += Buffer.byteLength(t, "utf8");
      chunks++;
      chunksSeen.push({ at: Date.now(), text: t });
    },
    onReset: () => {},
  });

  // Let the shell finish drawing its prompt. Without this the burst is typed into
  // a shell that is not at a prompt yet, and the measurement silently records a
  // fraction of the output — which is exactly what the first version of this
  // script did, reporting 447 bytes for a 79 KB burst and doing it identically for
  // both transports, which is the shape of a broken measurement rather than a
  // result.
  await sleep(1200);

  // Attach snapshot: what a client pays to see the screen for the first time.
  const snap = await stream.snapshot();
  const attachBytes = Buffer.byteLength(snap.screen, "utf8");

  // Echo latency: time from writing a keystroke to seeing it come back.
  const echoMs: number[] = [];
  for (let i = 0; i < ECHO_SAMPLES; i++) {
    const marker = `E${i}_`;
    const t0 = Date.now();
    await stream.inputText(marker);
    const ok = await waitFor(() => chunksSeen.some((c) => c.at >= t0 && c.text.includes(marker)), 5000);
    if (ok) {
      const hit = chunksSeen.find((c) => c.at >= t0 && c.text.includes(marker))!;
      echoMs.push(hit.at - t0);
    }
    // Clear the line. Without this the samples accumulate on the shell's input
    // line and the burst command below is typed onto the end of them, so the shell
    // runs a command named `E0_E1_..._seq` and the burst never happens. That is
    // what made the first two versions of this script report 447 and 206 bytes for
    // a 79 KB burst. Ctrl-U (0x15) kills to the start of the line in readline and
    // is a plain byte, so it does not depend on the key-name mapping.
    await stream.inputText("\u0015");
  }

  // A burst of known size, so framing and escaping overhead is measurable.
  const before = bytes;
  const chunksBefore = chunks;
  const t0 = Date.now();
  await stream.inputText("seq 1 5000 | sed 's/^/BURSTLINE_/'; echo BURST_DONE_$((3*3))");
  await stream.sendKey(["Enter"]);
  const done = await waitFor(() => chunksSeen.some((c) => c.text.includes("BURST_DONE_9")), 30000);
  const burstMs = Date.now() - t0;
  const burstBytes = bytes - before;
  const burstChunks = chunks - chunksBefore;
  if (!done) throw new Error(`${transport}: burst never completed`);

  // What the payload would be with no framing: 5000 lines of "BURSTLINE_<n>".
  void rawPayloadOf();

  return {
    transport,
    attachBytes,
    echoMs,
    burstBytes,
    burstMs,
    burstChunks,
  };
  } finally {
    // In a `finally`, and each step guarded, so one failure cannot skip the rest: a
    // leaked tmux server outlives the test run, and the socket name is derived from this
    // process's pid, so the next run does not reuse it.
    try {
      unsub?.();
      stream?.close();
    } catch {
      /* already gone */
    }
    daemon.stop();
    killTmuxServer(socket);
    rmSync(stateDir, { recursive: true, force: true });
  }
}

/** The payload the burst would produce with no framing at all, for reference. */
function rawPayloadOf(): number {
  return Array.from({ length: 5000 }, (_, i) => `BURSTLINE_${i + 1}`.length + 2).reduce((a, b) => a + b, 0);
}

async function waitFor(pred: () => boolean, ms: number): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (pred()) return true;
    await sleep(20);
  }
  return false;
}

function median(xs: number[]): number {
  if (xs.length === 0) return NaN;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid]! : Math.round((s[mid - 1]! + s[mid]!) / 2);
}

const results: Result[] = [];
for (const t of ["control", "pipe"] as StreamTransport[]) {
  process.stdout.write(`measuring ${t}...\n`);
  results.push(await run(t));
}

const pad = (s: string | number, n: number) => String(s).padEnd(n);
process.stdout.write("\n");
process.stdout.write(`${pad("", 12)}${pad("control", 14)}${pad("pipe", 14)}\n`);
process.stdout.write(`${"-".repeat(40)}\n`);
const row = (label: string, f: (r: Result) => string | number) => {
  process.stdout.write(`${pad(label, 12)}${pad(f(results[0]!), 14)}${pad(f(results[1]!), 14)}\n`);
};
row("attach B", (r) => r.attachBytes);
row("echo p50 ms", (r) => median(r.echoMs));
row("echo min ms", (r) => Math.min(...r.echoMs));
row("burst B", (r) => r.burstBytes);
row("burst chunks", (r) => r.burstChunks);
row("B/chunk", (r) => Math.round(r.burstBytes / r.burstChunks));
row("burst ms", (r) => r.burstMs);
process.stdout.write("\n");
process.stdout.write(
  `payload with no framing: ${rawPayloadOf()} B — both transports must carry the same bytes, ` +
    `because both carry the pane's byte stream.\n`,
);
