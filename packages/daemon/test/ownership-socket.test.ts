import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";
import { setTimeout as sleep } from "node:timers/promises";
import { daemonCall } from "../../protocol/src/rpc.ts";

/**
 * Everything here goes through the daemon's own API rather than a private
 * method: a test that reaches inside proves the internals agree with each other,
 * not that a client can do the thing.
 */
async function newShell(title: string): Promise<string> {
  const r = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp", title });
  return r.session.id;
}

async function remoteInfo() {
  return await daemonCall(daemon.socketPath, "remote_info", {});
}

/**
 * Exclusive ownership at the socket (ADR-0010).
 *
 * The rules themselves are stated in ownership.test.ts. What is tested *here* is
 * the wiring, because every one of these rules is worthless if the transport does
 * not apply it: a refusal that is not sent is a silent steal, and a claim that is
 * not released on disconnect is a lockout no restart short of the daemon's will
 * clear.
 */

const tmuxSocket = `ldmux-owntest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-own-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();

afterAll(() => {
  daemon.stop();
  killTmuxServer(tmuxSocket);
  rmSync(stateDir, { recursive: true, force: true });
});

interface RemoteInfo { hostname: string; port: number; path: string; token: string }

async function waitFor(pred: () => boolean, ms = 8000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (pred()) return true;
    await sleep(40);
  }
  return false;
}

/** A client that has said `hello` and can be driven message by message. */
async function client(info: RemoteInfo, device: { id: string; name: string; kind: string }) {
  const ws = new WebSocket(`ws://${info.hostname}:${info.port}${info.path}?token=${info.token}`);
  ws.binaryType = "arraybuffer";
  const text: any[] = [];
  ws.addEventListener("message", (ev: any) => {
    if (typeof ev.data === "string") text.push(JSON.parse(ev.data));
  });
  await new Promise<void>((resolve, reject) => {
    ws.addEventListener("open", () => resolve());
    ws.addEventListener("error", () => reject(new Error("ws error")));
  });
  ws.send(JSON.stringify({ type: "hello", device }));
  await sleep(50);
  return {
    ws,
    text,
    send: (msg: unknown) => ws.send(JSON.stringify(msg)),
    owned: () => text.filter((m) => m.type === "owned"),
    sessions: () => text.filter((m) => m.type === "sessions").at(-1)?.sessions ?? [],
    close: () => ws.close(),
  };
}

const phone = { id: "pixel-test", name: "Pixel 8", kind: "phone" };
const tablet = { id: "tab-test", name: "Tab S9", kind: "phone" };

test("ownership: a second device is refused and told who holds the shell", { timeout: 30000 }, async () => {
  const info = await remoteInfo();
  const id = await newShell("owned-shell");

  const a = await client(info, phone);
  a.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => a.text.some((m) => m.type === "caps"))).toBe(true);

  const b = await client(info, tablet);
  b.send({ type: "attach", sessionId: id });
  // The refusal has to be explicit. Silence here would leave the second device
  // believing it had attached while being sent nothing.
  expect(await waitFor(() => b.owned().length > 0)).toBe(true);
  expect(b.owned()[0].owner.name).toBe("Pixel 8");
  expect(b.text.some((m) => m.type === "caps")).toBe(false);

  a.close();
  b.close();
});

test("ownership: takeover displaces the holder and tells it", { timeout: 30000 }, async () => {
  const info = await remoteInfo();
  const id = await newShell("takeover-shell");

  const a = await client(info, phone);
  a.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => a.text.some((m) => m.type === "caps"))).toBe(true);

  const b = await client(info, tablet);
  b.send({ type: "attach", sessionId: id, takeover: true });

  // The taker gets in...
  expect(await waitFor(() => b.text.some((m) => m.type === "caps"))).toBe(true);
  // ...and the holder is told, rather than discovering it from silence. This is
  // the message that lets a desktop show "open on your phone" instead of just
  // freezing, or a phone show "taken by your desktop".
  expect(await waitFor(() => a.owned().length > 0)).toBe(true);
  expect(a.owned()[0].owner.name).toBe("Tab S9");

  a.close();
  b.close();
});

test("ownership: a disconnect frees the shell for anyone, with no takeover", { timeout: 30000 }, async () => {
  // The Stale State failure, end to end. A claim that outlived its connection
  // would make this attach come back `owned` — a phone that lost signal would
  // lock its own shell until the daemon was restarted.
  const info = await remoteInfo();
  const id = await newShell("stale-shell");

  const a = await client(info, phone);
  a.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => a.text.some((m) => m.type === "caps"))).toBe(true);
  a.close();

  // Wait for the claim to actually be gone, instead of sleeping and hoping.
  //
  // This was `await sleep(300)`, and it failed roughly one run in five — always
  // here, never on the `caps` wait above. The daemon releases on the socket's close
  // event, which is prompt, so 300 ms was normally plenty; on a loaded machine
  // running the whole suite it was not. The test then reported a Stale State bug
  // that was not there, which is the worst thing a test can do.
  //
  // Polling for the condition removes the timing assumption without weakening the
  // assertion: `list` carries the owner as a fact, so if a claim ever did outlive
  // its connection this would spin out its timeout and fail — and it would fail on
  // the real thing rather than on a race in the test.
  const b = await client(info, tablet);
  const cleared = await waitFor(() => {
    b.send({ type: "list" });
    const row = b.sessions().find((s: any) => s.id === id);
    return row !== undefined && row.owner == null;
  });
  if (!cleared) {
    const row = b.sessions().find((s: any) => s.id === id);
    throw new Error(`claim survived the disconnect: ${JSON.stringify(row)}`);
  }

  b.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => b.text.some((m) => m.type === "caps"))).toBe(true);
  expect(b.owned().length).toBe(0);

  b.close();
});

test("ownership: the session list carries the owner as a fact", { timeout: 30000 }, async () => {
  // The launcher shows *where* a shell is open. That has to come from the
  // server: a client-side guess cannot be right about a fact it cannot see.
  const info = await remoteInfo();
  const id = await newShell("listed-shell");

  const a = await client(info, phone);
  a.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => a.text.some((m) => m.type === "caps"))).toBe(true);

  a.send({ type: "list" });
  expect(await waitFor(() => a.sessions().length > 0)).toBe(true);
  const row = a.sessions().find((s: any) => s.id === id);
  expect(row.owner.name).toBe("Pixel 8");
  expect(row.owner.kind).toBe("phone");

  a.close();
});

test("close: a shell with no desktop window ends", { timeout: 30000 }, async () => {
  const id = await newShell("plain-close");

  const r = await daemonCall(daemon.socketPath, "close", { id });
  expect(r.ended).toBe(true);
  expect(daemon.registry.get(id)?.status).toBe("dead");
});

test("kill: still an unconditional end, so the CLI is not negotiable", { timeout: 30000 }, async () => {
  // `close` is the polite one. `kill` must stay absolute: a desktop window being
  // closed and the CLI both mean it, and neither should be second-guessed by a
  // rule about who is holding what.
  const id = await newShell("hard-kill");
  const r = await daemonCall(daemon.socketPath, "kill", { id });
  expect(r.ok).toBe(true);
  expect(daemon.registry.get(id)?.status).toBe("dead");
});

test("ownership: a device's claim survives the close of its other socket", { timeout: 30000 }, async () => {
  // The reconnect overlap, which is the normal case rather than a corner one: a
  // phone holds one socket for the session list and one per open shell, and a
  // reconnect opens the new socket before the old one is reaped. Releasing the
  // device's claims on *any* close let the dead socket free the claim its own
  // replacement had just made — the shell went quietly claimable by anyone, with
  // no takeover and no message, which is the same failure as the Stale State this
  // release exists to prevent, pointing the other way.
  const info = await remoteInfo();
  const id = await newShell("overlap-shell");

  const list = await client(info, phone);
  const pane = await client(info, phone);
  pane.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => pane.text.some((m) => m.type === "caps"))).toBe(true);

  // The list socket goes. Same device, still here.
  list.close();
  await sleep(200);

  const other = await client(info, tablet);
  other.send({ type: "attach", sessionId: id });
  expect(await waitFor(() => other.owned().length > 0)).toBe(true);
  expect(other.text.some((m) => m.type === "caps")).toBe(false);

  // ...and when the last one goes, the claim goes with it, which is the property
  // the release was for in the first place.
  pane.close();
  expect(await waitFor(() => {
    other.send({ type: "attach", sessionId: id, takeover: true });
    return other.text.some((m) => m.type === "caps");
  })).toBe(true);

  other.close();
});
