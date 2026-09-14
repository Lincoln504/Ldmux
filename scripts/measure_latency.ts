/**
 * Round-trip echo latency measurement.
 *
 * The perceived-latency number that matters: how long after a keystroke does the
 * *echo* come back? The tty echoes a typed line immediately, so this measures
 * client -> daemon -> tmux -> pty -> shell -> back without waiting for command
 * execution. See docs/PREDICTIVE-ECHO.md §6.
 *
 *   bun run scripts/measure_latency.ts --session <id> [--runs 20]
 *   bun run scripts/measure_latency.ts --url ws://host:port/ws?token=... [--runs 20]
 *
 * Needs a running daemon (LDMUX_SOCK, like the CLI) for the --session form.
 * Reports p50/p95/p99, which is the shape the dooboxes latency SLA is written
 * in.
 *
 * NOTE: over `adb reverse` this measures loopback and is a floor, not a mobile
 * number. Run it against the real (tailnet) address for the figure that matters.
 */

import { call } from "../packages/protocol/src/socket-client.ts";
import { resolveSocketPath } from "../packages/protocol/src/paths.ts";
import { decodeFrames, FrameType, type Frame } from "../packages/protocol/src/frames.ts";

interface RemoteInfo {
  hostname: string;
  port: number;
  path: string;
  token: string;
}

function arg(name: string, fallback?: string): string | undefined {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback;
}

const sessionId = arg("session");
const runs = Number(arg("runs", "20"));
const explicitUrl = arg("url");
if (!sessionId && !explicitUrl) {
  console.error("usage: bun run scripts/measure_latency.ts --session <id> | --url <ws-url> [--runs N]");
  process.exit(2);
}

const wsUrl = explicitUrl ?? await (async () => {
  const info = await call<RemoteInfo>(resolveSocketPath(), "remote_info");
  return `ws://${info.hostname}:${info.port}${info.path}?token=${encodeURIComponent(info.token)}`;
})();
const socket = new WebSocket(wsUrl);
let tail: Uint8Array = new Uint8Array(0);
let onOutput: ((text: string) => void) | null = null;

socket.addEventListener("message", (ev: any) => {
  const data = ev.data;
  if (typeof data === "string") return;
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
  const merged = new Uint8Array(tail.length + bytes.length);
  merged.set(tail);
  merged.set(bytes, tail.length);
  const { frames, rest } = decodeFrames(merged);
  tail = rest;
  for (const f of frames as Frame[]) {
    if (f.type === FrameType.Output) onOutput?.(new TextDecoder().decode(f.payload));
  }
});

await new Promise<void>((resolve, reject) => {
  socket.addEventListener("open", () => resolve());
  socket.addEventListener("error", () => reject(new Error("websocket error")));
});

/** Resolve on the first output frame whose text includes `needle`. */
function waitForOutput(needle: string, timeoutMs: number): Promise<number> {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const timer = setTimeout(() => {
      onOutput = null;
      reject(new Error(`timeout after ${timeoutMs}ms waiting for ${needle}`));
    }, timeoutMs);
    onOutput = (text) => {
      if (!text.includes(needle)) return;
      clearTimeout(timer);
      onOutput = null;
      resolve(performance.now() - started);
    };
  });
}

const send = (msg: unknown) => socket.send(JSON.stringify(msg));
const settle = (ms: number) => Bun.sleep(ms);

// Attach (the --session form) and let the stream settle before timing: an
// un-attached socket has no pane to type into.
if (sessionId) send({ type: "attach", sessionId });
await settle(sessionId ? 600 : 200);

const samples: number[] = [];
for (let i = 0; i < runs; i++) {
  // Clear the line (C-u) so the marker cannot match an earlier echo.
  send({ type: "key", keys: ["C-u"] });
  await settle(60);
  const marker = `lmx${i}x`;
  const t0 = performance.now();
  send({ type: "input", data: marker });
  const dt = await waitForOutput(marker, 5000);
  samples.push(dt);
  void t0;
  await settle(40);
}
// Leave the shell as we found it.
send({ type: "key", keys: ["C-u"] });
await settle(50);

samples.sort((a, b) => a - b);
const pct = (p: number) => samples[Math.min(samples.length - 1, Math.floor((p / 100) * samples.length))];
const fmt = (n: number) => `${n.toFixed(1)}ms`;

console.log(`round-trip echo latency over ${samples.length} runs (${sessionId ?? wsUrl})`);
console.log(`  min ${fmt(samples[0])}   p50 ${fmt(pct(50))}   p95 ${fmt(pct(95))}   max ${fmt(samples[samples.length - 1])}`);
console.log(`  target: p95 < 100ms (dooboxes TESTING_STRATEGY.md:541; launch blocker)`);
if (wsUrl.includes("127.0.0.1") || wsUrl.includes("localhost")) {
  console.log("  (loopback: this is a floor — measure the tailnet address for the real number)");
}

socket.close();
process.exit(0);