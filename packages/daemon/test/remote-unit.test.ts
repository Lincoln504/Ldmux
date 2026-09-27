import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RemoteServer, type RemoteServerOptions } from "../src/remote.ts";
import { HEADER_SIZE } from "../../protocol/src/frames.ts";
import type { PaneStream, StreamSubscriber } from "../src/stream.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";
import { setTimeout as sleep } from "node:timers/promises";
import { WebSocket as WsLib } from "ws";
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
          // A program, not a shell: the shell guard is exercised separately.
          foreground: "vim",
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
test("the endpoint serves no files, and says so", async () => {
  // This server used to be a static file server for a browser client. That client
  // is gone (native KMP is the client), and with it a whole class of concern: path
  // confinement, symlink escapes, content types. What must not go with it is the
  // *answer* — a person who pastes the URL into a browser should be told what this
  // port is, not handed an empty page or a directory listing.
  const { info } = await start({});
  const base = `http://${info.hostname}:${info.port}`;
  for (const path of ["/", "/index.html", "/../etc/passwd", "/nope.js"]) {
    const res = await fetch(`${base}${path}`);
    expect(res.status).toBe(404);
    expect(await res.text()).toContain("/ws");
    // Never a file, whatever was asked for.
    expect(await (await fetch(`${base}${path}`)).text()).not.toContain("root:");
  }
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
  // The daemon is still answering, not dead. A 404 is the right answer now — it
  // serves no files — so the probe asserts that the request was *served*, not that
  // it returned any particular page.
  const after = await fetch(`http://${info.hostname}:${info.port}/`);
  expect(after.status).toBe(404);
  expect(await after.text()).toContain("/ws");
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
  // Still alive and answering.
  expect((await fetch(`http://${info.hostname}:${info.port}/`)).status).toBe(404);
});

test("a connection that has said nothing for too long is taken away, and its claim with it", async () => {
  // The failure this exists for: a phone loses signal, no FIN is sent, the socket
  // stays open — and the daemon goes on holding that device's claim, so the shell
  // is refused to everyone, including the phone that vanished. Stale State,
  // arriving from the direction `ownership.ts` does not cover.
  const claims = new Map<string, string>();
  const { info } = await start({
    idleMs: 300,
    // The reap is what is under test, not the reconnect grace that follows it.
    releaseGraceMs: 0,
    claim: (sessionId, device) => {
      const holder = claims.get(sessionId);
      if (holder && holder !== device.id) {
        return { ok: false as const, heldBy: { id: holder, name: holder } as unknown as never };
      }
      claims.set(sessionId, device.id);
      return { ok: true as const };
    },
    releaseClaim: (sessionId, deviceId) =>
      claims.get(sessionId) === deviceId && claims.delete(sessionId),
    releaseDevice: (deviceId) =>
      [...claims]
        .filter(([, d]) => d === deviceId)
        .map(([s]) => {
          claims.delete(s);
          return s;
        }),
  });
  // `autoPong: false` is what makes this test mean anything. Node's built-in
  // WebSocket, and the `ws` library by default, answer a ping in the transport
  // without the application being involved — which is exactly the cooperation a
  // liveness rule must not depend on, and which would leave this socket alive
  // forever and this test green either way.
  const client = new WsLib(`ws://${info.hostname}:${info.port}/ws?token=test-token`, {
    autoPong: false,
  });
  await new Promise<void>((resolve, reject) => {
    client.on("open", () => resolve());
    client.on("error", (err) => reject(err));
  });
  const closed: number[] = [];
  client.on("close", () => closed.push(1));
  const hello = (id: string) =>
    JSON.stringify({ type: "hello", device: { id, name: id, kind: "phone" } });
  client.send(hello("phone-1"));
  client.send(JSON.stringify({ type: "attach", sessionId: "s1", cols: 80, rows: 24 }));
  expect(await waitFor(() => claims.get("s1") === "phone-1")).toBe(true);

  // ...and then it says nothing at all, and answers nothing.
  expect(await waitFor(() => closed.length > 0, 5000)).toBe(true);
  expect(claims.has("s1")).toBe(false);
});

test("an attach that fails to subscribe gives its claim back", async () => {
  // The claim is taken before the stream is subscribed, deliberately: a refusal
  // has to reach the client before anything is torn down. The price of that
  // ordering is that any later failure holds a claim nobody is using — and a
  // claim's whole purpose is to refuse the next attach, so the device would be
  // refused by itself and read it as the app being broken.
  const claims = new Map<string, string>();
  const { info } = await start({
    claim: (sessionId, device) => {
      claims.set(sessionId, device.id);
      return { ok: true as const };
    },
    releaseClaim: (sessionId, deviceId) =>
      claims.get(sessionId) === deviceId && claims.delete(sessionId),
    acquireStream: (() => ({
      subscribe: async () => {
        throw new Error("cannot subscribe");
      },
    })) as unknown as RemoteServerOptions["acquireStream"],
  });
  const c = await connect(info);
  c.ws.send(JSON.stringify({ type: "hello", device: { id: "phone-2", name: "p", kind: "phone" } }));
  c.ws.send(JSON.stringify({ type: "attach", sessionId: "s1", cols: 80, rows: 24 }));

  // The code is carried in an `error` envelope, not as the message type.
  expect(
    await waitFor(() => c.text.some((m) => m.type === "error" && m.code === "attach_failed")),
  ).toBe(true);
  expect(claims.has("s1")).toBe(false);
});
