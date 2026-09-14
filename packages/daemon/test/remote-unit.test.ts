import { test, expect, afterAll } from "bun:test";
import { RemoteServer, type RemoteServerOptions } from "../src/remote.ts";
import type { PaneStream, StreamSubscriber } from "../src/stream.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";

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

function start(opts: Partial<RemoteServerOptions> = {}) {
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
  const info = server.start();
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
  const text: any[] = [];
  const output: string[] = [];
  const closed: number[] = [];
  ws.addEventListener("message", (ev: any) => {
    if (typeof ev.data === "string") text.push(JSON.parse(ev.data));
    else output.push(new TextDecoder().decode(ev.data));
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
    await Bun.sleep(20);
  }
  return false;
};

test("a client that falls behind is closed with too_far_behind so it resyncs", async () => {
  // A negative threshold forces the comparison to be true on the first frame
  // (real code uses 4 MiB): what is under test is the path itself — that it
  // reports too_far_behind, closes the socket, and releases the subscription.
  const { info, stream, calls } = start({ maxBufferedBytes: -1 });
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
  const { info, stream } = start();
  const { ws, text, output, closed } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  for (let i = 0; i < 20; i++) stream.emit(`line ${i}\n`);
  expect(await waitFor(() => output.length >= 20)).toBe(true);
  expect(closed).toEqual([]);
  expect(text.some((m) => m.code === "too_far_behind")).toBe(false);
  ws.close();
});

test("overlapping attaches leave exactly one subscriber", async () => {
  const { info, stream, subscribers, calls } = start();
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
  const { info, stream } = start();
  const { ws, text } = await connect(info);
  ws.send(JSON.stringify({ type: "attach", sessionId: "s1" }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  stream.reset("pane paused");
  expect(await waitFor(() => text.some((m) => m.code === "resync" && m.message === "pane paused"))).toBe(true);
  ws.close();
});

test("input and keys are forwarded to the stream, and oversized input is not", async () => {
  const { info, calls } = start();
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

test("a bad token is rejected before any upgrade", async () => {
  const { info, calls } = start();
  const res = await fetch(`http://${info.hostname}:${info.port}/ws?token=wrong`);
  expect(res.status).toBe(401);
  expect(calls).toEqual([]);
});

test("a token in the Authorization header is accepted (not just the query)", async () => {
  const { info } = start();
  const res = await fetch(`http://${info.hostname}:${info.port}/ws`, {
    headers: { Upgrade: "websocket", Connection: "Upgrade", Authorization: "Bearer test-token" },
  });
  // Bun's fetch cannot complete a WS upgrade, so assert it is not a 401.
  expect(res.status).not.toBe(401);
});