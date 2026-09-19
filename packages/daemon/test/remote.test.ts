import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync } from "node:fs";
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

  // Something on the shell's own screen, which must never appear in a snapshot of
  // the program's screen.
  await daemonCall(daemon.socketPath, "send", { id: session.id, text: "echo PRIMARY-MARKER" });
  await daemonCall(daemon.socketPath, "send", { id: session.id, keys: ["Enter"] });
  await sleep(400);

  // `tput smcup` emits the real sequence; a hand-written escape through send-keys
  // does not survive the round trip and silently tests nothing (measured: alt=0).
  await daemonCall(daemon.socketPath, "send", {
    id: session.id,
    text: "tput smcup; echo ALTERNATE-MARKER",
  });
  await daemonCall(daemon.socketPath, "send", { id: session.id, keys: ["Enter"] });
  await sleep(600);

  const state = await daemonCall(daemon.socketPath, "snapshot", { id: session.id });
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

