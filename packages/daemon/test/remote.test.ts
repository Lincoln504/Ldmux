import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";
import { call } from "../../protocol/src/socket-client.ts";
import { daemonCall } from "../../protocol/src/rpc.ts";

import { decodeFrames, FrameType, type Frame } from "../../protocol/src/frames.ts";
import { setTimeout as sleep } from "node:timers/promises";

const tmuxSocket = `ldmux-remotetest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-remote-"));

// **Start from a server that does not exist.**
//
// The daemon writes its option profile and every tmux it runs passes `-f`, but tmux
// reads `-f` only when a server *starts* — so a server left over from a previous run of
// this file keeps whatever options it had, and the profile silently does not apply.
//
// That is not a test artefact, it is how the product behaves too: a long-lived server
// keeps the profile it was born with, which is why the phone's own server had to have
// the new options applied to it by hand rather than by restarting the daemon. A test
// that depends on machine state is not a test, and the failure it produced was the one
// this suite has been carrying for a while: `window-size manual` missing means the pane
// follows the attached client's width, so a line that is supposed to *wrap* fits on one
// row, and a test about wrapped lines fails for a reason that has nothing to do with
// wrapping.
killTmuxServer(tmuxSocket);

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

/**
 * Poll until [pred] is true, or give up after [ms].
 *
 * The predicate may be sync or async, and the `await` is deliberately
 * unconditional. An async predicate used to be a trap here: it returns a Promise,
 * a Promise is truthy, so the very first check passed and the wait did nothing at
 * all — a test that cannot fail is worse than a failing one. Awaiting a plain
 * boolean is a no-op, so both shapes are safe and neither is silently defeated.
 */
async function waitFor(pred: () => boolean | Promise<boolean>, ms = 6000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await pred()) return true;
    await sleep(50);
  }
  return false;
}

async function connect(info: RemoteInfo, token: string) {
  const ws = new WebSocket(`ws://${info.hostname}:${info.port}${info.path}?token=${token}`);
  // Node's WebSocket defaults binary payloads to a Blob, which cannot be read
  // as bytes; ldmux frames are binary, so ask for the ArrayBuffer.
  ws.binaryType = "arraybuffer";
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

test("remote endpoint rejects a missing/incorrect token", { timeout: 20000 }, async () => {
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

test("serves no files at all, and says what the endpoint is", async () => {
  // This used to be "serves the bundled web client, its assets, and the session
  // list". The client is deleted (native KMP is the client), and with it a second
  // renderer, a second snapshot implementation and a static file server. What is
  // left to check is the negative: nothing under any path is a file, and the one
  // thing a person pasting this URL into a browser is told is where the socket is.
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const base = `http://${info.hostname}:${info.port}`;
  for (const path of ["/", "/index.html", "/core.js", "/app.css", "/sw.js", "/nope.js"]) {
    const res = await fetch(`${base}${path}`);
    expect(res.status).toBe(404);
    const body = await res.text();
    expect(body).toContain("/ws");
    expect(body).not.toContain("<html");
  }
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
  //
  // **Prove the send took effect before asserting what did not arrive.** An
  // assertion that something *did not* arrive passes for any reason at all — a slow
  // machine, a wedged socket, a send that never happened — so it gets greener as
  // conditions get worse and can never fail to reveal the problem. This used to sleep
  // 600 ms and assert absence, with the only positive check coming afterwards.
  //
  // So the fact is established directly: `capture` reads A's own pane through the
  // daemon, independently of the client, and the sentinel appearing there **twice**
  // (the shell's echo of the command, then its output) proves the command really ran.
  // Only then does the absence mean something: those bytes existed and did not reach
  // this client.
  await call(daemon.socketPath, "send", { id: a.id, text: "printf 'SUPERSEDED_A\\n'", keys: ["Enter"] });
  const ranInPane = await waitFor(async () => {
    const { screen } = await call<{ screen: string }>(daemon.socketPath, "capture", { id: a.id });
    return screen.split("SUPERSEDED_A").length - 1 >= 2;
  });
  expect(ranInPane).toBe(true);
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
});

test("session list carries an activity clock and a cwd-derived title", { timeout: 20000 }, async () => {
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
});

test("protocol errors are reported per-message without dropping the connection", { timeout: 20000 }, async () => {
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
  // not a socket kill from the transport's frame limit).
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
});

test("key names that would be read as tmux flags are dropped, not forwarded", { timeout: 20000 }, async () => {
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
});

test("a malformed binary frame from a client does not break the connection", { timeout: 20000 }, async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { ws, text } = await connect(info, info.token);
  ws.send(new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]));
  ws.send(new Uint8Array(0));
  ws.send(JSON.stringify({ type: "ping", id: 5 }));
  expect(await waitFor(() => text.some((m) => m.type === "pong" && m.id === 5))).toBe(true);
  ws.close();
});

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

test("asking for a shell answers with which shell, and a bad path answers with why", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { ws, text } = await connect(info, info.token);
  const lastSessions = () => [...text].reverse().find((m) => m.type === "sessions")?.sessions ?? [];

  // The id, not just the list. A list cannot say which row you asked for, so the
  // client used to match on the directory it had requested — which fails for a
  // blank path (the daemon chose the directory) and for a directory that already
  // has a shell (the match finds the old one). What that produced was a client
  // that had just asked for a shell, staring at "No sessions".
  ws.send(JSON.stringify({ type: "new", kind: "shell", cwd: tmpdir() }));
  expect(await waitFor(() => text.some((m) => m.type === "created"))).toBe(true);
  const msg = [...text].reverse().find((m) => m.type === "created");
  expect(typeof msg.sessionId).toBe("string");
  // It is the session that exists, not an id invented for the reply.
  expect(await waitFor(() => lastSessions().some((s: any) => s.id === msg.sessionId))).toBe(true);
  expect(msg.cwd).toBe(await realpath(tmpdir()));

  // A path that is not a directory is refused out loud, and refusing does not
  // take the connection down.
  ws.send(JSON.stringify({ type: "new", kind: "shell", cwd: "/definitely/not/here" }));
  expect(
    await waitFor(() => text.some((m) => m.type === "error" && /not a directory/.test(m.message))),
  ).toBe(true);
  ws.close();
});

test("a client opens and kills a session over the WebSocket (FR-10)", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { ws, text } = await connect(info, info.token);

  const lastSessions = () => [...text].reverse().find((m) => m.type === "sessions")?.sessions ?? [];

  ws.send(JSON.stringify({ type: "new", kind: "shell", cwd: tmpdir(), title: "ws-created" }));
  expect(await waitFor(() => lastSessions().some((s: any) => s.title === "ws-created"))).toBe(true);
  const created = lastSessions().find((s: any) => s.title === "ws-created");

  ws.send(JSON.stringify({ type: "kill", sessionId: created.id }));
  expect(await waitFor(() => lastSessions().find((s: any) => s.id === created.id)?.status === "dead")).toBe(true);

  // A bad kill reports an error rather than taking the connection down.
  ws.send(JSON.stringify({ type: "kill", sessionId: "nope" }));
  expect(await waitFor(() => text.some((m) => m.type === "error"))).toBe(true);
  expect((await call(daemon.socketPath, "ping") as { pong: boolean }).pong).toBe(true);
  ws.close();
});

test("attach carries the client's grid, and resize re-sizes the pane", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);

  // The client states its own geometry on attach, so the first snapshot is
  // already the size it will paint (no 120x40 frame followed by a reflow).
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id, cols: 60, rows: 20 }));
  expect(
    await waitFor(() => text.some((m) => m.type === "snapshot" && m.cols === 60 && m.rows === 20)),
  ).toBe(true);

  // A viewport change: the pane follows the client, and the answer is a full
  // snapshot of the *new* grid (a diff computed against the old one would be
  // garbage).
  const before = text.filter((m) => m.type === "snapshot").length;
  ws.send(JSON.stringify({ type: "resize", cols: 88, rows: 26 }));
  expect(
    await waitFor(() => text.some((m) => m.type === "caps" && m.cols === 88 && m.rows === 26)),
  ).toBe(true);
  // Wait for the SNAPSHOT, not just for the `caps` that precedes it. The two
  // arrive as separate messages and the snapshot is the slower one (it needs a
  // capture), so asserting on the count the instant `caps` lands is a race the
  // test created for itself. It failed under parallel load and looked like a
  // resize bug.
  expect(
    await waitFor(() => text.filter((m) => m.type === "snapshot").length > before),
  ).toBe(true);
  ws.close();
});

test("resize before attach is refused; an absurd size is clamped, not rejected", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);

  ws.send(JSON.stringify({ type: "resize", cols: 50, rows: 20 }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "not_attached"))).toBe(true);

  // A hostile/buggy size must not be a way to make tmux fail the session: it is
  // clamped into the pane bounds and the attach still succeeds.
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id, cols: 100000, rows: 0 }));
  expect(
    await waitFor(() => text.some((m) => m.type === "snapshot" && m.cols <= 500 && m.rows >= 5)),
  ).toBe(true);
  ws.close();
});

test("scrollback with lines 0 re-sends the visible screen, not 5000 lines", async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id, cols: 40, rows: 10 }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  // Give the shell something that scrolled off the top.
  ws.send(JSON.stringify({ type: "input", data: "for i in $(seq 1 60); do echo line$i; done" }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter"] }));
  // Let the flood finish and scroll the pane.
  await new Promise((r) => setTimeout(r, 1500));

  const before = text.filter((m) => m.type === "snapshot").length;
  ws.send(JSON.stringify({ type: "scrollback", sessionId: session.id, lines: 0 }));
  expect(await waitFor(() => text.filter((m) => m.type === "snapshot").length > before)).toBe(true);
  const last = text.filter((m) => m.type === "snapshot").at(-1) as any;
  // The plain screen is exactly the pane's rows; history paging would be far
  // taller and would carry the history flag.
  expect(last.rows).toBe(10);
  expect(last.history).toBeUndefined();
  ws.close();
});

test("a wrapped line comes back as the screen has it, so replaying it does not scroll", async () => {
  // This test used to assert the opposite — that the capture *joined* a wrapped
  // line into one row — and that was wrong in two ways.
  //
  // The reasoning was that a client cannot tell "the terminal wrapped here" from
  // "the user pressed Enter", and the symptom was a cursor that would not move
  // back up past a wrap. That symptom was real but the cause was elsewhere: the
  // session shells were running a bash built without readline, which is fixed by
  // taking the login shell from the host, not by joining rows.
  //
  // And joining rows actively breaks the screen. Measured on a device: a
  // 105-column line came back as ONE row, the client re-wrapped it into two, the
  // capture had one row more than the pane had, and the emulator scrolled by one
  // — silently deleting the top line. On a 20-row pane whose first row is the
  // prompt, the prompt is simply gone.
  //
  // A client is not reading the rows, it is replaying them into an emulator of
  // the same width, and the wrap is a consequence of that width. So the capture
  // must be the raw screen.
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text, output } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id, cols: 40, rows: 24 }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);

  // 50 characters on a 40-column pane, and the exact length matters.
  //
  // This was 40 characters, which is precisely one row wide: `capture-pane` returned
  // it joined into a single row because it *was* a single row, and the assertion below
  // — that the logical line must not come back joined — failed on a line that never
  // wrapped in the first place. The test had been failing for that reason rather than
  // for the one it is about.
  const long = "AAAAAAAAAABBBBBBBBBBCCCCCCCCCCDDDDDDDDDDEEEEEEEEEE";
  ws.send(JSON.stringify({ type: "input", data: `echo ${long}` }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter"] }));
  expect(await waitFor(() => output.join("").includes("DDDDDDDDDD"))).toBe(true);
  await new Promise((r) => setTimeout(r, 400));

  const before = text.filter((m) => m.type === "snapshot").length;
  ws.send(JSON.stringify({ type: "scrollback", sessionId: session.id, lines: 0 }));
  expect(await waitFor(() => text.filter((m) => m.type === "snapshot").length > before)).toBe(true);
  const snap = text.filter((m) => m.type === "snapshot").at(-1) as any;
  const screen = snap.screen as string;
  // The logical line must come back the way the pane holds it: wrapped at the
  // pane's width, so the client's emulator reproduces the same screen.
  expect(long.length).toBeGreaterThan(40);
  expect(screen).not.toContain(long);
  expect(screen).toContain("AAAAAAAAA");
  // And the screen must not have grown a row — that is the scroll which deletes
  // the top line. Counting breaks rather than pieces: a blank bottom row leaves a
  // trailing empty element, which would read as one row too many.
  expect((screen.match(/\n/g) ?? []).length).toBeLessThanOrEqual(24);
  ws.close();
});

test("a click on a pane that never enabled mouse reporting is refused", async () => {
  // Sending an SGR sequence to a program that did not ask for one would just
  // print garbage in the session.
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.send(JSON.stringify({ type: "mouseclick", col: 3, row: 1 }));
  expect(await waitFor(() => text.some((m) => m.type === "error" && m.code === "no_mouse"))).toBe(true);
  ws.close();
});

/**
 * The screen and its scrollback come from the same buffer, or they are two
 * different screens.
 *
 * Written against a real failure. tmux is unambiguous about this once measured:
 * with a full-screen program running (`alternate_on=1`),
 *
 *     capture-pane                          -> the program's screen
 *     capture-pane -a                       -> the shell's screen
 *     capture-pane -S -100                  -> the shell's *scrollback*
 *     capture-pane -a -S -100               -> the shell's screen
 *
 * So `-a` does not mean "the alternate one", it means "the other one" — and
 * scrollback is not a property of the alternate screen at all, because that
 * screen has none: it belongs to the primary, which is not on screen. Asking for
 * either while a TUI is up hands the client the shell's prompt and history, and
 * the client — told `alternate_on=1` — draws them on the alternate screen. That
 * is a full-screen program with its own shell showing through it, which is
 * exactly what it looked like.
 */
test("a snapshot of an alternate-screen pane is that screen and no scrollback", { timeout: 30000 }, async () => {
  const { session } = await daemonCall(daemon.socketPath, "new", {
    kind: "shell",
    cwd: "/tmp",
    title: "altscreen",
  });
  const name = session.tmuxName;

  // Poll on the pane's own state rather than sleeping. This test used fixed
  // `sleep(400)` / `sleep(600)`, and under the suite's full concurrency — four test
  // files at once, each with its own tmux server — a loaded machine simply did not
  // finish starting `tput` inside 600 ms, so the snapshot was taken before the
  // alternate screen existed and the failure read as "the feature is broken". It
  // passed alone every time and failed in the suite, which is the signature of a
  // test that depends on how busy the machine is.
  const screenNow = async (): Promise<string> =>
    (await daemonCall(daemon.socketPath, "snapshot", { id: session.id })).screen;
  const snap = async () => await daemonCall(daemon.socketPath, "snapshot", { id: session.id });
  const send = async (text: string) => {
    await daemonCall(daemon.socketPath, "send", { id: session.id, text });
    await daemonCall(daemon.socketPath, "send", { id: session.id, keys: ["Enter"] });
  };
  const countOf = (haystack: string, needle: string) => haystack.split(needle).length - 1;

  // Something on the shell's own screen, which must never appear in a snapshot of
  // the program's screen.
  //
  // Waited for *twice*, deliberately. The shell echoes the command it is given, so
  // the literal text is on screen the instant it is typed, before anything has run
  // — one occurrence is the echo and two is the output. Waiting for one would let
  // the next command be typed into a shell that had not finished the first, and the
  // two would interleave.
  await send("echo PRIMARY-MARKER");
  const sawPrimary = await waitFor(async () => countOf(await screenNow(), "PRIMARY-MARKER") >= 2);
  expect(sawPrimary).toBe(true);

  // `tput smcup` emits the real sequence; a hand-written escape through send-keys
  // does not survive the round trip and silently tests nothing (measured: alt=0).
  await send("tput smcup; echo ALTERNATE-MARKER");
  // Waited on the **flag**, not the marker. This is the same echo trap: a wait on
  // the marker is satisfied by the echoed command line within milliseconds, before
  // `tput` has run, so the test then asserts against a screen that never switched
  // and fails in 187 ms. `alternateOn` is the engine's own state and no echo can
  // fake it.
  const switched = await waitFor(async () => (await snap()).state.alternateOn === true);
  expect(switched).toBe(true);

  const state = await snap();
  expect(state.state.alternateOn).toBe(true);
  expect(state.screen.includes("ALTERNATE-MARKER")).toBe(true);
  expect(state.screen.includes("PRIMARY-MARKER")).toBe(false);
});

/**
 * A click that was taken is answered, so the client knows the tap is over.
 *
 * Written against a measured double move. The client keeps a fallback for panes
 * that will not take a click (the mouse mode is sticky, so a shell can be left
 * with it on). Nothing cancelled that fallback when the click *worked*, so one
 * tap moved the cursor twice: `micro` clamped the click to the end of its line
 * (`col 41` -> `col 7`), and 1.2 s later the fallback walked back toward the
 * tapped cell from a stale frame cursor, finished on an empty line and clamped
 * to the gutter. The tap ended at `(2,4)` instead of `(7,2)`.
 *
 * The pane here enables mouse reporting and is left at a non-shell foreground,
 * which is the shape the guard accepts. `cat` rather than the shell, because the
 * shell foreground is refused by design: those bytes would be printed as junk.
 */
test("a click a mouse-aware program can take is answered", { timeout: 30000 }, async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  // 1000 = report button presses, 1006 = SGR coordinates. Then `cat`, so the
  // pane is not sitting at a shell prompt.
  await daemonCall(daemon.socketPath, "send", {
    id: session.id,
    text: "printf '\\033[?1000h\\033[?1006h'; cat",
  });
  await daemonCall(daemon.socketPath, "send", { id: session.id, keys: ["Enter"] });
  // Wait for the pane to actually BE in SGR mouse mode rather than sleeping a
  // fixed 700 ms. The click is refused with `no_mouse` when the mode is not on
  // yet, so a fixed sleep turns a slow shell start into a failed test that looks
  // like a mouse-reporting bug. Same trap as the fixed sleeps elsewhere: wait for
  // the state, not for a duration.
  //
  // Polled explicitly rather than through `waitFor`, which in this file takes a
  // synchronous predicate: an `async` one returns a Promise, a Promise is truthy,
  // and the wait passes instantly without ever having checked anything.
  let mouseReady = false;
  for (let i = 0; i < 150 && !mouseReady; i++) {
    const st = await daemonCall(daemon.socketPath, "snapshot", { id: session.id });
    mouseReady = st.state.mouse === true && st.state.mouseSgr === true;
    if (!mouseReady) await sleep(100);
  }
  expect(mouseReady).toBe(true);

  const { ws, text } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.send(JSON.stringify({ type: "mouseclick", col: 3, row: 1 }));
  expect(await waitFor(() => text.some((m) => m.type === "mousedelivered"))).toBe(true);
  // And not a refusal: the two answers are different outcomes and must not be
  // sent for the same click.
  expect(text.some((m) => m.type === "error" && m.code === "no_mouse")).toBe(false);
  ws.close();
});


/**
 * A modified character reaches a program that enabled extended keys.
 *
 * pi and Claude Code turn on modifyOtherKeys (`CSI > 4;2 m`). In that mode tmux
 * encodes keys itself and **drops** a literal control byte sent as text: the phone
 * used to send Ctrl-a as the byte 0x01, and in pi it did nothing, while the C/c
 * button (sent by name) worked. The client now sends every modified character by
 * name (`Modifiers.kt`); this pins that the names it sends reach the program in the
 * extended form, and — the half that made the bug — that the literal byte does not.
 */
test("modified keys by name reach an extended-keys program; a literal control byte does not", { timeout: 30000 }, async () => {
  const info = await call<RemoteInfo>(daemon.socketPath, "remote_info");
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  // A program that asks for modifyOtherKeys and prints each read as <hex>.
  const script = join(stateDir, "keydump.cjs");
  writeFileSync(
    script,
    `process.stdout.write("\\x1b[>4;2mKEYS_" + "READY\\n");
process.stdin.setRawMode(true);
process.stdin.on("data", (d) => {
  process.stdout.write("<" + d.toString("hex") + ">");
  if (d.includes(0x71)) { process.stdout.write("\\x1b[>4m"); process.exit(0); }
});`,
  );
  const { ws, text, output } = await connect(info, info.token);
  ws.send(JSON.stringify({ type: "attach", sessionId: session.id }));
  expect(await waitFor(() => text.some((m) => m.type === "snapshot"))).toBe(true);
  ws.send(JSON.stringify({ type: "input", data: `${process.execPath} ${script}` }));
  ws.send(JSON.stringify({ type: "key", keys: ["Enter"] }));
  expect(await waitFor(() => output.join("").includes("KEYS_READY"), 10000)).toBe(true);

  const reads = () => [...output.join("").matchAll(/<([0-9a-f]+)>/g)].map((m) => m[1]).join("");
  const csi = (s: string) => Buffer.from(`\x1b[${s}`).toString("hex");
  // The literal byte first, then a marker, so its absence is observed rather than
  // assumed: if 0x01 were delivered it would be read before the `x`.
  ws.send(JSON.stringify({ type: "input", data: "\x01x" }));
  expect(await waitFor(() => reads().includes("78"))).toBe(true);
  expect(reads().startsWith("78")).toBe(true);

  for (const [key, seq] of [["C-a", "97;5u"], ["M-f", "102;3u"], ["M-C-a", "97;7u"], ["M-|", "124;3u"]] as const) {
    ws.send(JSON.stringify({ type: "key", keys: [key] }));
    expect(await waitFor(() => reads().includes(csi(seq)))).toBe(true);
  }
  ws.send(JSON.stringify({ type: "input", data: "q" }));
  ws.close();
});
