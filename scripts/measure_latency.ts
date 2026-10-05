/**
 * Round-trip echo latency measurement.
 *
 * The perceived-latency number that matters: how long after a keystroke does the
 * *echo* come back? The tty echoes a typed line immediately, so this measures
 * client -> daemon -> keeper -> pty -> shell -> back without waiting for command
 * execution. See docs/PREDICTIVE-ECHO.md §6.
 *
 *   node scripts/measure_latency.ts --session <id> [--runs 20]
 *   node scripts/measure_latency.ts --url ws://host:port/ws?token=... [--runs 20]
 *
 * Needs a running daemon (LDMUX_SOCK, like the CLI) for the --session form.
 * Reports p50/p95/p99, which is the shape the dooboxes latency SLA is written
 * in.
 *
 * NOTE: over `adb reverse` this measures loopback and is a floor, not a mobile
 * number. Run it against the real (tailnet) address for the figure that matters.
 */

import { connect } from "node:net";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

// ── The wire codec, inlined ────────────────────────────────────────────────
// This script used to import these from packages/protocol, the TypeScript tree
// the daemon was ported out of and which was deleted at D4. The format is the
// daemon's and is pinned by the Rust crates; only what this script needs is
// repeated here. Frame: 8-byte header, u8 type / u8 flags / u16 reserved /
// u32 length (little-endian).
const HEADER_SIZE = 8;
const TYPE_OUTPUT = 0x10;
const MAX_FRAME_PAYLOAD = 16 * 1024 * 1024;

interface Frame {
  type: number;
  payload: Uint8Array;
}

/** Decode as many complete frames as `buf` contains; return the trailing partial. */
function decodeFrames(buf: Uint8Array): { frames: Frame[]; rest: Uint8Array } {
  const frames: Frame[] = [];
  let offset = 0;
  while (buf.length - offset >= HEADER_SIZE) {
    const view = new DataView(buf.buffer, buf.byteOffset + offset, HEADER_SIZE);
    const type = view.getUint8(0);
    const length = view.getUint32(4);
    if (length > MAX_FRAME_PAYLOAD) throw new RangeError(`frame payload too large: ${length}`);
    if (buf.length - offset - HEADER_SIZE < length) break;
    const start = offset + HEADER_SIZE;
    frames.push({ type, payload: buf.subarray(start, start + length) });
    offset = start + length;
  }
  return { frames, rest: buf.subarray(offset) };
}

/** The daemon's Unix socket. Same chain as the daemon resolves: never drift. */
function resolveSocketPath(): string {
  if (process.env.LDMUX_SOCK) return process.env.LDMUX_SOCK;
  const state = process.env.LDMUX_STATE_DIR ?? `${process.env.XDG_RUNTIME_DIR ?? "/tmp"}/ldmux`;
  return join(state, "ldmuxd.sock");
}

/** One newline-delimited JSON call on the daemon's socket: one connection each. */
function call<T>(method: string, params: Record<string, unknown> = {}, timeoutMs = 8000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const sock = connect(resolveSocketPath());
    let buf = "";
    let done = false;
    const decoder = new TextDecoder();
    const finish = (fn: () => void) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      sock.destroy();
      fn();
    };
    const timer = setTimeout(
      () => finish(() => reject(new Error(`ldmux request timed out: ${method}`))),
      timeoutMs,
    );
    sock.on("connect", () => sock.write(JSON.stringify({ id: 1, method, params }) + "\n"));
    sock.on("data", (chunk: Buffer) => {
      buf += decoder.decode(chunk, { stream: true });
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      const line = buf.slice(0, nl);
      finish(() => {
        try {
          const res = JSON.parse(line) as { ok: boolean; result?: T; error?: string };
          if (res.ok) resolve(res.result as T);
          else reject(new Error(res.error || "ldmux request failed"));
        } catch (err) {
          reject(err as Error);
        }
      });
    });
    sock.on("error", (err) => finish(() => reject(err)));
    sock.on("close", () => finish(() => reject(new Error("ldmux socket closed"))));
  });
}

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
  console.error("usage: node scripts/measure_latency.ts --session <id> | --url <ws-url> [--runs N]");
  process.exit(2);
}

const wsUrl = explicitUrl ?? await (async () => {
  const info = await call<RemoteInfo>("remote_info");
  return `ws://${info.hostname}:${info.port}${info.path}?token=${encodeURIComponent(info.token)}`;
})();
const socket = new WebSocket(wsUrl);
// Binary frames arrive as a Blob unless we ask for bytes.
socket.binaryType = "arraybuffer";
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
  for (const f of frames) {
    if (f.type === TYPE_OUTPUT) onOutput?.(new TextDecoder().decode(f.payload));
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
const settle = (ms: number) => sleep(ms);

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