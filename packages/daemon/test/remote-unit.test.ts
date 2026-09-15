import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RemoteServer, type RemoteServerOptions } from "../src/remote.ts";
import { HEADER_SIZE } from "../../protocol/src/frames.ts";
import type { PaneStream, StreamSubscriber } from "../src/stream.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";
import { setTimeout as sleep } from "node:timers/promises";
import { request } from "node:http";
import { connect as netConnect } from "node:net";

/**
 * The remote layer with a stub stream: no tmux, no daemon. These cover the
 * paths that are impractical to provoke through a real pane (the backpressure
 * close) and the ones that must not depend on tmux at all.
 */

const session = (id: string): SessionRecord => ({
  id,
  tmuxName: `tmux-${id}`,
  kind: "shell",
  cwd: "/tmp",
  title: id,
  createdAt: 0,
  updatedAt: 0,
  createdBy: "test",
  status: "running",
});

/** A stream that records what the server asks of it and can be told to emit. */
function stubStream() {
  const subs = new Set<StreamSubscriber>();
  const calls: string[] = [];
  const stream = {
    subscriberCount: subs.size,
    async subscribe(sub: StreamSubscriber) {
      subs.add(sub);
      stream.subscriberCount = subs.size;
      calls.push("subscribe");
      return () => {
        subs.delete(sub);
        stream.subscriberCount = subs.size;
        calls.push("unsubscribe");
      };
    },
    async snapshot() {
      return {
        screen: "STUB-SCREEN",
        state: {
          cursorX: 0,
          cursorY: 0,
          alternateOn: false,
          paneInMode: false,
          mouse: false,
          appCursor: false,
          bracketedPaste: false,
          historySize: 0,
          cols: 80,
          rows: 24,
          dead: false,
        },
        epoch: 1,
      };
    },
    async snapshotWithHistory() {
      const snap = await stream.snapshot();
      return { ...snap, history: true };
    },
    async inputText(text: string) {
      calls.push(`input:${text}`);
    },
    async sendKey(keys: string[]) {
      calls.push(`key:${keys.join(",")}`);
    },
    close() {
      calls.push("close");
    },
    /** Test hook: push output to every subscriber. */
    emit(text: string) {
      for (const sub of subs) sub.onOutput(text);
    },
    reset(reason: string) {
      for (const sub of subs) sub.onReset(reason);
    },
  };
  return { stream, calls, subscribers: () => subs.size };
}

const servers: RemoteServer[] = [];

async function start(opts: Partial<RemoteServerOptions> = {}) {
  const { stream, calls, subscribers } = stubStream();
  const server = new RemoteServer({
    hostname: "127.0.0.1",
    port: 0,
    token: "test-token",
    getSession: (id) => (id === "s1" ? session("s1") : undefined),
    listSessions: () => [],
    acquireStream: () => stream as unknown as PaneStream,
    webRoot: undefined,
    ...opts,
  });
  const info = await server.start();
  servers.push(server);
  return { server, info, stream, calls, subscribers };
}

afterAll(() => {
  for (const server of servers) {
    try {
      server.stop();
    } catch {
      /* already stopped */
    }
  }
});

async function connect(info: { hostname: string; port: number }, path = "/ws", token = "test-token") {
  const ws = new WebSocket(`ws://${info.hostname}:${info.port}${path}?token=${token}`);
  // Node's WebSocket defaults binary payloads to a Blob, which cannot be read
  // as bytes; ldmux frames are binary, so ask for the ArrayBuffer.
  ws.binaryType = "arraybuffer";
  const text: any[] = [];
  const output: string[] = [];
  const closed: number[] = [];
  ws.addEventListener("message", (ev: any) => {
    if (typeof ev.data === "string") text.push(JSON.parse(ev.data));
    // Strip the 8-byte ldmux frame header: these are payloads, not wire frames.
    else output.push(new TextDecoder().decode(new Uint8Array(ev.data).subarray(HEADER_SIZE)));
  });
  ws.addEventListener("close", (ev: any) => closed.push(ev.code));
  await new Promise<void>((resolve, reject) => {
    ws.addEventListener("open", () => resolve());
    ws.addEventListener("error", () => reject(new Error("ws error")));
  });
  return { ws, text, output, closed };
}

const waitFor = async (pred: () => boolean, ms = 4000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (pred()) return true;
    await sleep(20);
  }
  return false;
};

test("a client that falls behind is closed with too_far_behind so it resyncs", async () => {
  // A negative threshold forces the comparison to be true on the first frame
  // (real code uses 4 MiB): what is under test is the path itself — that it
  // reports too_far_behind, closes the socket, and releases the subscription.
  const { info, stream, calls } = await start({ maxBufferedBytes: -1 });
  const { ws, text, output, closed } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  stream.emit("hello from the pane");
  expect(await waitFor(() => output.length > 0)).toBe(true);
  expect(await waitFor(() => text.some((m) => m.code === "too_far_behind"))).toBe(true);
  expect(await waitFor(() => closed.length > 0)).toBe(true);
  // The subscription is released: no leak into the next attach.
  expect(await waitFor(() => calls.includes("unsubscribe"))).toBe(true);
});

test("a normal client is not closed by the default backpressure threshold", async () => {
  const { info, stream } = await start();
  const { ws, text, output, closed } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  for (let i = 0; i < 20; i++) stream.emit(`line ${i}\n`);
  // Output is coalesced, so what matters is that the content arrives, not that
  // each write became its own frame.
  expect(await waitFor(() => output.join("").includes("line 19"))).toBe(true);
  expect(closed).toEqual([]);
  expect(text.some((m) => m.code === "too_far_behind")).toBe(false);
  ws.close();
});

test("a burst of pane output is coalesced, but every byte still arrives in order", async () => {
  // The pane is fed in many small writes; sending each as its own frame costs a
  // packet and a render on the phone for what is usually one visible line.
  const { info, stream } = await start();
  const { ws, text, output } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  const expected = Array.from({ length: 50 }, (_, i) => `x${i}`).join("");
  // The screen rides in the snapshot's JSON message, so every binary frame here
  // is live pane output.
  const base = output.length;
  for (let i = 0; i < 50; i++) stream.emit(`x${i}`);
  expect(await waitFor(() => output.join("").includes("x49"))).toBe(true);
  // Nothing lost, nothing reordered...
  expect(output.slice(base).join("")).toBe(expected);
  // ...and not one frame per write.
  expect(output.length - base).toBeLessThan(50);
  ws.close();
});

test("overlapping attaches leave exactly one subscriber", async () => {
  const { info, stream, subscribers, calls } = await start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.filter((m) => m.type === "snapshot").length >= 2)).toBe(true);
  // The superseded attach must have released its subscription.
  expect(subscribers()).toBe(1);
  expect(calls.filter((c) => c === "unsubscribe").length).toBe(1);
  ws.close();
});

test("a stream reset is surfaced as a resync so the client re-attaches", async () => {
  const { info, stream } = await start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  stream.reset("pane paused");
  expect(await waitFor(() => text.some((m) => m.code === "resync" && m.message === "pane paused"))).toBe(true);
  ws.close();
});

test("input and keys are forwarded to the stream, and oversized input is not", async () => {
  const { info, calls } = await start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  ws.send(JSON.stringify({ type: "input", data: "typed text" }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter", "C-c"] }));
  expect(await waitFor(() => calls.includes("input:typed text"))).toBe(true);
  expect(calls).toContain("key:Enter,C-c");

  const before = calls.length;
  ws.send(JSON.stringify({ type: "input", data: "x".repeat(20_000) }));
  expect(await waitFor(() => text.some((m) => m.code === "input_too_large"))).toBe(true);
  expect(calls.length).toBe(before);
  ws.close();
});

test("the input cap counts bytes, so multi-byte text cannot slip through", async () => {
  const { info, calls } = await start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  // 5000 astral characters = 20000 UTF-8 bytes but only 10000 UTF-16 code units,
  // so a `.length` check would wave it through. It must be refused.
  const before = calls.length;
  ws.send(JSON.stringify({ type: "input", data: "\u{1f600}".repeat(5000) }));
  expect(await waitFor(() => text.some((m) => m.code === "input_too_large"))).toBe(true);
  expect(calls.length).toBe(before);
  ws.close();
});

test("a bad token is rejected before any upgrade", async () => {
  const { info, calls } = await start();
  const res = await fetch(`http://${info.hostname}:${info.port}/ws?token=wrong`);
  expect(res.status).toBe(401);
  expect(calls).toEqual([]);
});

test("a token in the Authorization header is accepted (not just the query)", async () => {
  const { info } = await start();
  // A raw request rather than fetch: `Upgrade`/`Connection` are forbidden header
  // names in fetch, so it cannot present a real upgrade handshake (and the point
  // here is exactly the handshake's header path).
  const status = await new Promise<number>((resolve, reject) => {
    const req = request(
      {
        host: info.hostname,
        port: info.port,
        path: "/ws",
        headers: { Upgrade: "websocket", Connection: "Upgrade", Authorization: "Bearer test-token" },
      },
      (res) => {
        res.resume();
        resolve(res.statusCode ?? 0);
      },
    );
    req.on("error", reject);
    req.end();
  });
  // The token was accepted: the request reached the upgrade path rather than
  // being refused. (A handshake that cannot be completed is not a 401.)
  expect(status).not.toBe(401);
});
test("static serving confines paths lexically, follows no escaping symlink, and 404s", async () => {
  const root = mkdtempSync(join(tmpdir(), "ldmux-webroot-"));
  const outside = mkdtempSync(join(tmpdir(), "ldmux-outside-"));
  writeFileSync(join(root, "index.html"), "<h1>ok</h1>");
  writeFileSync(join(outside, "secret.txt"), "SECRET");
  symlinkSync(join(outside, "secret.txt"), join(root, "link.txt"));
  const { server, info } = await start({ webRoot: root });
  const base = `http://${info.hostname}:${info.port}`;

  expect((await fetch(`${base}/`)).status).toBe(200);
  expect((await fetch(`${base}/index.html`)).status).toBe(200);
  // A raw traversal pathname is forbidden lexically (browser/fetch normalises the
  // URL first, so this branch is exercised directly against serveStatic).
  const lexical = await (server as unknown as { serveStatic(p: string): Promise<Response> }).serveStatic(
    "/../../etc/passwd",
  );
  expect(lexical.status).toBe(403);
  expect(await lexical.text()).not.toContain("root:");
  // A symlink *inside* the root that points outside is re-confined after realpath.
  const symlinked = await fetch(`${base}/link.txt`);
  expect(symlinked.status).toBe(403);
  expect(await symlinked.text()).not.toContain("SECRET");
  // A genuinely missing file is a 404, not a 403.
  expect((await fetch(`${base}/nope.js`)).status).toBe(404);
  rmSync(root, { recursive: true, force: true });
  rmSync(outside, { recursive: true, force: true });
});

test("closing a socket releases the stream for the record it was attached to", async () => {
  const released: string[] = [];
  const { info } = await start({ releaseStream: (r) => released.push(r.id) });
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.close();
  expect(await waitFor(() => released.includes("s1"))).toBe(true);
});

test("control messages before attach are silent no-ops; attach still works", async () => {
  const { info, calls } = await start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "input", data: "x" }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter"] }));
  ws.send(JSON.stringify({ type: "follow", mode: "anchored" })); // client-owned: no server effect
  await sleep(150);
  expect(text.filter((m) => m.type === "error")).toEqual([]);
  expect(calls).toEqual([]); // nothing was forwarded to a stream

  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.close();
});

test("an unknown content type is served as application/octet-stream", async () => {
  const root = mkdtempSync(join(tmpdir(), "ldmux-webroot-ct-"));
  writeFileSync(join(root, "blob.bin"), new Uint8Array([1, 2, 3]));
  const { info } = await start({ webRoot: root });
  const res = await fetch(`http://${info.hostname}:${info.port}/blob.bin`);
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toBe("application/octet-stream");
  rmSync(root, { recursive: true, force: true });
});

test("a malformed request target cannot take the daemon down", async () => {
  // `GET http:// HTTP/1.1` is accepted by Node's HTTP parser and reaches the
  // handler verbatim; `new URL("http://", base)` throws. That runs BEFORE the
  // token check, so an unguarded parse was an unauthenticated way to crash the
  // daemon (and every attached terminal with it).
  const { info } = await start();
  const raw = (request: string) =>
    new Promise<string>((resolve, reject) => {
      const socket = netConnect(info.port, info.hostname, () => socket.write(request));
      let buf = "";
      socket.on("data", (chunk) => {
        buf += chunk.toString("utf8");
      });
      socket.on("close", () => resolve(buf));
      socket.on("error", reject);
    });

  const absolute = await raw("GET http:// HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n");
  expect(absolute).toContain("400");
  // The daemon is still serving, not dead.
  expect((await fetch(`http://${info.hostname}:${info.port}/`)).status).toBe(200);
});

test("an unauthenticated absolute-form upgrade is refused, not fatal", async () => {
  const { info } = await start();
  const status = await new Promise<string>((resolve, reject) => {
    const socket = netConnect(info.port, info.hostname, () => {
      socket.write(
        "GET http:// HTTP/1.1\r\nHost: h\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n" +
          "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
      );
    });
    let buf = "";
    socket.on("data", (chunk) => {
      buf += chunk.toString("utf8");
    });
    socket.on("close", () => resolve(buf.split("\r\n")[0] ?? ""));
    socket.on("error", reject);
  });
  expect(status).toContain("401");
  // Still alive and serving.
  expect((await fetch(`http://${info.hostname}:${info.port}/`)).status).toBe(200);
});
