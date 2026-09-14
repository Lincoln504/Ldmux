import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { call } from "../../protocol/src/socket-client.ts";
import { decodeFrames, FrameType, type Frame } from "../../protocol/src/frames.ts";

const tmuxSocket = `ldmux-remotetest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-remote-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
daemon.start();

afterAll(() => {
  daemon.stop();
  Bun.spawnSync(["tmux", "-L", tmuxSocket, "kill-server"]);
  rmSync(stateDir, { recursive: true, force: true });
});

interface RemoteInfo {
  hostname: string;
  port: number;
  path: string;
  token: string;
}

const sleep = (ms: number) => Bun.sleep(ms);
async function waitFor(pred: () => boolean, ms = 6000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (pred()) return true;
    await sleep(50);
  }
  return false;
}

async function connect(info: RemoteInfo, token: string) {
  const ws = new WebSocket(`ws://${info.hostname}:${info.port}${info.path}?token=${token}`);
  const text: any[] = [];
  const output: string[] = [];
  let tail = new Uint8Array(0);
  ws.addEventListener("message", (ev: any) => {
    const data = ev.data;
    if (typeof data === "string") {
      text.push(JSON.parse(data));
      return;
    }
    const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
    const merged = new Uint8Array(tail.length + bytes.length);
    merged.set(tail);
    merged.set(bytes, tail.length);
    const { frames, rest } = decodeFrames(merged);
    tail = rest;
    for (const f of frames as Frame[]) {
      if (f.type === FrameType.Output) output.push(new TextDecoder().decode(f.payload));
    }
  });
  await new Promise<void>((resolve, reject) => {
    ws.addEventListener("open", () => resolve());
    ws.addEventListener("error", () => reject(new Error("ws error")));
    ws.addEventListener("close", (e: any) => reject(new Error(`ws closed ${e.code}`)));
  });
  return { ws, text, output };
}

test("remote endpoint rejects a missing/incorrect token", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const res = await fetch(`http://${info.hostname}:${info.port}/ws`);
  expect(res.status).toBe(401);
});

test("remote attach streams a snapshot then live output; input round-trips", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  expect(info.port).toBeGreaterThan(0);
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });

  const { ws, text, output } = await connect(info, info.token);

  ws.send(JSON.stringify({ type: "ping", id: 7 }));
  expect(await waitFor(() => text.some((m) => m.type === "pong" && m.id === 7))).toBe(true);

  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "caps"))).toBe(true);
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  ws.send(JSON.stringify({ type: "input", data: "printf 'REMOTE_MARKER\\n'" }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter"] }));
  expect(await waitFor(() => output.join("").includes("REMOTE_MARKER"))).toBe(true);

  ws.close();
});

test("remote attach to an unknown session errors", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: "nope" }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "unknown_session"))).toBe(true);
  ws.close();
});

test("killing an attached session resyncs the client and evicts the stream", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "caps"))).toBe(true);
  await call(daemon.socketPath, "kill", { id: session.id });
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "resync"))).toBe(true);
  ws.close();
});

test("serves the bundled web client, its assets, and the session list", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const base = `http://${info.hostname}:${info.port}`;

  const index = await fetch(`${base}/`);
  expect(index.status).toBe(200);
  expect(index.headers.get("content-type")).toContain("text/html");
  expect(await index.text()).toContain("app.js");

  const xterm = await fetch(`${base}/vendor/xterm.js`);
  expect(xterm.status).toBe(200);
  expect(xterm.headers.get("content-type")).toContain("javascript");

  const missing = await fetch(`${base}/nope.js`);
  expect(missing.status).toBe(404);

  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "list" }));
  expect(await waitFor(() => text.some((m) => m.type === "sessions"))).toBe(true);
  const msg = text.find((m) => m.type === "sessions");
  expect(Array.isArray(msg.sessions)).toBe(true);
  ws.close();
});