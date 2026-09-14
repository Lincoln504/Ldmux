import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";
import { call } from "../../protocol/src/socket-client.ts";
import { decodeFrames, FrameType, type Frame } from "../../protocol/src/frames.ts";

const tmuxSocket = `ldmux-remotetest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-remote-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();

afterAll(() => {
  daemon.stop();
  killTmuxServer(tmuxSocket);
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
  let tail: Uint8Array = new Uint8Array(0);
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
  const html = await index.text();
  expect(html).toContain("app.js");
  expect(html).toContain("core.js");
  const core = await fetch(`${base}/core.js`);
  expect(core.status).toBe(200);
  expect(core.headers.get("content-type") || "").toContain("javascript");

  const xterm = await fetch(`${base}/vendor/xterm.js`);
  expect(xterm.status).toBe(200);
  expect(xterm.headers.get("content-type")).toContain("javascript");

  const missing = await fetch(`${base}/nope.js`);
  expect(missing.status).toBe(404);

  const manifest = await fetch(`${base}/manifest.webmanifest`);
  expect(manifest.status).toBe(200);
  expect(manifest.headers.get("content-type")).toContain("manifest+json");
  expect(((await manifest.json()) as { orientation: string }).orientation).toBe("landscape");

  for (const asset of ["/sw.js", "/icon.svg", "/app.css"]) {
    expect((await fetch(`${base}${asset}`)).status).toBe(200);
  }

  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "list" }));
  expect(await waitFor(() => text.some((m) => m.type === "sessions"))).toBe(true);
  const msg = text.find((m) => m.type === "sessions");
  expect(Array.isArray(msg.sessions)).toBe(true);
  ws.close();
});
test("attaching to a dead session is rejected with session_dead", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  await call(daemon.socketPath, "kill", { id: session.id });
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "session_dead"))).toBe(true);
  ws.close();
});

test("a superseded attach does not leak a subscription or mix panes", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const a = (await call<{ session: { id: string } }>(daemon.socketPath, "new", { kind: "shell", cwd: tmpdir() }))
    .session;
  const b = (await call<{ session: { id: string } }>(daemon.socketPath, "new", { kind: "shell", cwd: tmpdir() }))
    .session;
  const { ws, output, text } = await connect(info, info.token);

  // Attach A then immediately B: A must never commit.
  ws.send(JSON.stringify({ type: "attach", sessionId: a.id }));
  ws.send(JSON.stringify({ type: "attach", sessionId: b.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot" && m.sessionId === b.id))).toBe(true);

  // Drive A (the superseded session) and confirm none of its output arrives.
  await call(daemon.socketPath, "send", { id: a.id, text: "printf 'SUPERSEDED_A\\n'", keys: ["Enter"] });
  await sleep(600);
  expect(output.join("").includes("SUPERSEDED_A")).toBe(false);

  // B still streams.
  await call(daemon.socketPath, "send", { id: b.id, text: "printf 'ACTIVE_B\\n'", keys: ["Enter"] });
  expect(await waitFor(() => output.join("").includes("ACTIVE_B"))).toBe(true);
  ws.close();
});

test("scrollback request returns paged history as a snapshot", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text, output } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  // Ask before attaching: the server must say so rather than guess.
  const bare = await connect(info, info.token);
  bare.ws.send(JSON.stringify({ type: "scrollback", sessionId: session.id }));
  expect(await waitFor(() => bare.text.some((m) => m.type === "error"))).toBe(true);
  expect(bare.text.find((m) => m.type === "error").code).toBe("not_attached");
  bare.ws.close();

  await call(daemon.socketPath, "send", { id: session.id, text: "echo HISTORY_MARKER", keys: ["Enter"] });
  expect(await waitFor(() => output.join("").includes("HISTORY_MARKER"))).toBe(true);
  ws.send(JSON.stringify({ type: "scrollback", sessionId: session.id, lines: 500 }));
  expect(
    await waitFor(() =>
      text.some((m) => m.type === "snapshot" && m.history === true && String(m.screen).includes("HISTORY_MARKER")),
    ),
  ).toBe(true);
  const caps = text.find((m) => m.type === "caps");
  expect(caps.capabilities.scrollback).toBe(true);
  ws.close();
}, 20000);

test("session list carries an activity clock and a cwd-derived title", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const cwd = mkdtempSync(join(tmpdir(), "ldmux-titled-"));
  const { session } = await call<{ session: { id: string; tmuxName: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd,
  });
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "list" }));
  expect(await waitFor(() => text.some((m) => m.type === "sessions"))).toBe(true);
  const info2 = text.find((m) => m.type === "sessions").sessions.find((s: any) => s.id === session.id);
  expect(info2.title).toBe(cwd.split("/").pop());
  expect(info2.lastActivityAt).toBeGreaterThan(0);
  expect(info2.tmuxName).toBe(session.tmuxName);
  ws.close();
  rmSync(cwd, { recursive: true, force: true });
}, 20000);

test("protocol errors are reported per-message without dropping the connection", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);

  ws.send("{ not json");
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "bad_json"))).toBe(true);

  ws.send(JSON.stringify({ type: "nonsense" }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "bad_message"))).toBe(true);

  // Oversized input is refused with a typed error (not a silent truncation and
  // not a socket kill from Bun's frame limit).
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.send(JSON.stringify({ type: "input", data: "x".repeat(20_000) }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "input_too_large"))).toBe(true);

  // A scrollback request without an attach is refused with a typed error.
  const bare = await connect(info, info.token);
  bare.ws.send(JSON.stringify({ type: "scrollback", sessionId: session.id }));
  expect(await waitFor(() => bare.text.some((m) => m.code === "not_attached"))).toBe(true);
  bare.ws.close();

  // The original connection is still healthy after all of that.
  ws.send(JSON.stringify({ type: "ping", id: 99 }));
  expect(await waitFor(() => text.some((m) => m.type === "pong" && m.id === 99))).toBe(true);
  ws.close();
}, 20000);

test("key names that would be read as tmux flags are dropped, not forwarded", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text, output } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  // Type a command with no newline, then send Enter *plus* hostile "keys".
  ws.send(JSON.stringify({ type: "input", data: "printf 'KEYFILTER_OK\\n'" }));
  ws.send(JSON.stringify({ type: "key", keys: ["-t", "%999", "-l", "x", "Enter"] }));
  // If the dashes were forwarded, tmux would treat them as flags (different
  // target, literal mode) and the command would never run.
  expect(await waitFor(() => output.join("").includes("KEYFILTER_OK"))).toBe(true);
  ws.close();
}, 20000);

test("a malformed binary frame from a client does not break the connection", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { ws, text } = await connect(info, info.token);
  ws.send(new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]));
  ws.send(new Uint8Array(0));
  ws.send(JSON.stringify({ type: "ping", id: 5 }));
  expect(await waitFor(() => text.some((m) => m.type === "pong" && m.id === 5))).toBe(true);
  ws.close();
}, 20000);

test("rpc snapshot and history capture agree with the live pane", async () => {
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  await call(daemon.socketPath, "send", { id: session.id, text: "seq 1 60", keys: ["Enter"] });
  // Wait for the command to actually produce output before asserting.
  let hist = { screen: "" };
  for (let i = 0; i < 80; i++) {
    hist = await call<{ screen: string }>(daemon.socketPath, "capture", { id: session.id, history: true });
    if (hist.screen.includes("\n60\n")) break;
    await sleep(50);
  }
  const snap = await call<{ screen: string; state: { cols: number; rows: number; dead: boolean } }>(
    daemon.socketPath,
    "snapshot",
    { id: session.id },
  );
  expect(snap.state.cols).toBe(120);
  expect(snap.state.rows).toBe(40);
  expect(snap.state.dead).toBe(false);

  // The 40-row pane shows the tail; history reaches the beginning of the run.
  expect(snap.screen).toContain("60");
  expect(hist.screen).toContain("1\n");
  expect(hist.screen).toContain("60");
});
