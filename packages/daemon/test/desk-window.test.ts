import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { LdmuxDaemon } from "../src/daemon.ts";
import { daemonCall } from "../../protocol/src/rpc.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

/**
 * A desk terminal as a desk window (`ldmux window`, what `$mod+Return` runs).
 *
 * The contract pinned here is the one the user met as "the phone opens a new
 * shell instead of mine": a desk terminal has to be a shell the daemon knows is
 * on the desk, so that the phone is asked before taking it, the desk stops typing
 * into it when it is taken, and closing the window ends it.
 *
 * The window is a real tmux client on a real pty (`script`), running the shim the
 * daemon wrote — the same file the terminal execs.
 */

const tmuxSocket = `ldmux-desk-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-desk-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();
const windows: ChildProcess[] = [];

afterAll(() => {
  for (const w of windows) {
    try {
      process.kill(-w.pid!, "SIGKILL");
    } catch {
      /* already gone */
    }
  }
  daemon.stop();
  killTmuxServer(tmuxSocket);
  rmSync(stateDir, { recursive: true, force: true });
});

async function waitFor(pred: () => boolean | Promise<boolean>, ms = 8000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await pred()) return true;
    await sleep(50);
  }
  return false;
}

/** Open a desk window on a new shell, the way ldmux-terminal does. */
async function deskWindow(): Promise<{ id: string; tmuxName: string; win: ChildProcess }> {
  const { session } = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp", createdBy: "local" });
  return openWindow(session);
}

/** Open a desk window on an existing shell, the way `ldmux shell` does. */
async function openWindow(session: { id: string; tmuxName: string }): Promise<{ id: string; tmuxName: string; win: ChildProcess }> {
  const res = await daemonCall(daemon.socketPath, "deskWindow", { id: session.id });
  expect(res.ok).toBe(true);
  const win = spawn("script", ["-qfc", `/bin/sh ${res.shimPath}`, "/dev/null"], {
    detached: true,
    stdio: ["pipe", "ignore", "ignore"],
    env: { ...process.env, TERM: "xterm-256color" },
  });
  windows.push(win);
  // Attached means tmux lists the tty the shim recorded.
  const attached = await waitFor(async () => (await clientTtys(session.tmuxName)).length > 0);
  expect(attached).toBe(true);
  return { id: session.id, tmuxName: session.tmuxName, win };
}

async function clientTtys(tmuxName: string): Promise<string[]> {
  return daemon.cp.listClientTtys(tmuxName);
}

interface RemoteInfo { hostname: string; port: number; path: string; token: string }

async function phoneClient(device: { id: string; name: string; kind: string }) {
  const info = (await daemonCall(daemon.socketPath, "remote_info", {})) as RemoteInfo;
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
  return { ws, text, send: (m: unknown) => ws.send(JSON.stringify(m)) };
}

function ownerOf(id: string): Promise<string | undefined> {
  return daemonCall(daemon.socketPath, "list", {}).then(
    (r) => (r.sessions.find((s: any) => s.id === id) as any)?.owner?.kind,
  );
}

const phone = { id: "desk-test-phone", name: "Test phone", kind: "phone" };

/** Phone takes a desk window's shell; resolves once the window is detached. */
async function phoneTakes(id: string, tmuxName: string, deviceId: string) {
  const p = await phoneClient({ ...phone, id: deviceId });
  p.send({ type: "attach", sessionId: id, cols: 80, rows: 24, takeover: true });
  expect(await waitFor(() => p.text.some((m) => m.type === "caps"))).toBe(true);
  expect(await waitFor(async () => (await clientTtys(tmuxName)).length === 0)).toBe(true);
  return p;
}

async function statusOf(id: string): Promise<string | undefined> {
  const r = await daemonCall(daemon.socketPath, "list", {});
  return r.sessions.find((s) => s.id === id)?.status;
}


test("a desk window holds its shell: the phone is asked before it can have it", async () => {
  const { id } = await deskWindow();
  expect(await ownerOf(id)).toBe("desktop");
  const p = await phoneClient(phone);
  p.send({ type: "attach", sessionId: id, cols: 80, rows: 24 });
  expect(await waitFor(() => p.text.some((m) => m.type === "owned" && m.sessionId === id))).toBe(true);
  p.ws.close();
});

test("taking a desk window's shell detaches the window, and the desk does not snatch it back", async () => {
  const { id, tmuxName } = await deskWindow();
  const p = await phoneClient({ ...phone, id: "desk-test-phone-2" });
  p.send({ type: "attach", sessionId: id, cols: 80, rows: 24, takeover: true });
  expect(await waitFor(() => p.text.some((m) => m.type === "caps"))).toBe(true);
  // The window's client goes: it is showing "open on another device", not typing.
  expect(await waitFor(async () => (await clientTtys(tmuxName)).length === 0)).toBe(true);
  // Past the reclaim guard, the phone still holds it: nothing reclaimed on a
  // window nobody touched.
  await sleep(3500);
  await daemonCall(daemon.socketPath, "list", {});
  expect(await ownerOf(id)).toBe("phone");
  p.ws.close();
});

test("pressing a key in the desk window takes the shell back, and the phone is told and stops typing", async () => {
  const { id, tmuxName, win } = await deskWindow();
  const p = await phoneClient({ ...phone, id: "desk-test-phone-reclaim" });
  p.send({ type: "attach", sessionId: id, cols: 80, rows: 24, takeover: true });
  expect(await waitFor(() => p.text.some((m) => m.type === "caps"))).toBe(true);
  expect(await waitFor(async () => (await clientTtys(tmuxName)).length === 0)).toBe(true);
  await sleep(3200); // past the post-takeover guard
  win.stdin!.write("x");
  expect(await waitFor(() => p.text.some((m) => m.type === "owned" && m.sessionId === id && m.owner?.kind === "desktop"))).toBe(true);
  expect(await ownerOf(id)).toBe("desktop");
  // The phone's input no longer reaches the pane.
  p.send({ type: "input", data: "echo PHONE-AFTER-RECLAIM\n" });
  await sleep(400);
  const screen = (await daemonCall(daemon.socketPath, "capture", { id })).screen;
  expect(screen.includes("PHONE-AFTER-RECLAIM")).toBe(false);
  p.ws.close();
});

test("closing a desk window ends its shell", async () => {
  const { id, win } = await deskWindow();
  process.kill(-win.pid!, "SIGKILL");
  const ended = await waitFor(async () => {
    const r = await daemonCall(daemon.socketPath, "list", {});
    return r.sessions.find((s) => s.id === id)?.status === "dead";
  }, 10000);
  expect(ended).toBe(true);
});

test("closing a desk window ends its shell even while the phone is using it", async () => {
  // "Closing a desktop window closes the session totally" — and the phone, which
  // was showing it, is told it is gone.
  const { id, tmuxName, win } = await deskWindow();
  const p = await phoneTakes(id, tmuxName, "desk-test-phone-close");
  p.send({ type: "list" });
  process.kill(-win.pid!, "SIGKILL");
  expect(await waitFor(async () => (await statusOf(id)) === "dead", 10000)).toBe(true);
  expect(
    await waitFor(() =>
      p.text.some((m: any) => m.type === "sessions" && m.sessions.some((s: any) => s.id === id && s.status === "dead")),
    ),
  ).toBe(true);
  p.ws.close();
});

test("when the phone lets go, the desk window takes its shell back by itself", async () => {
  // Going back to the launcher, being swiped away, losing the link: the desk gets
  // its shell back without anybody touching it, and the next phone is asked.
  const { id, tmuxName } = await deskWindow();
  const p = await phoneTakes(id, tmuxName, "desk-test-phone-return");
  p.ws.close();
  expect(await waitFor(async () => (await clientTtys(tmuxName)).length === 1)).toBe(true);
  expect(await waitFor(async () => (await ownerOf(id)) === "desktop")).toBe(true);
  const q = await phoneClient({ ...phone, id: "desk-test-phone-return-2" });
  q.send({ type: "attach", sessionId: id, cols: 80, rows: 24 });
  expect(await waitFor(() => q.text.some((m) => m.type === "owned" && m.sessionId === id))).toBe(true);
  q.ws.close();
});

test("a pid left by an earlier window cannot end a new window's shell", async () => {
  // A dead pid in the session made the new window look closed before its own
  // shim had written anything.
  const { session } = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp", createdBy: "local" });
  await daemon.cp.setSessionOption(session.tmuxName, "@ldmux_live_pid", "999999");
  await openWindow(session);
  await sleep(3000);
  expect(await statusOf(session.id)).toBe("running");
  expect(await ownerOf(session.id)).toBe("desktop");
});

test("a shell cannot have two desk windows", async () => {
  const { id } = await deskWindow();
  const res = await daemonCall(daemon.socketPath, "deskWindow", { id });
  expect(res.ok).toBe(false);
});

test("a phone that goes back to the launcher lets go of the shell it was showing", async () => {
  // The launcher socket stays open the whole time, which is what used to keep the
  // claim alive: only the device's *last* socket released anything.
  const device = { ...phone, id: "desk-test-phone-4" };
  const launcher = await phoneClient(device);
  const { session } = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp" });
  const page = await phoneClient(device);
  page.send({ type: "attach", sessionId: session.id, cols: 80, rows: 24 });
  expect(await waitFor(() => page.text.some((m) => m.type === "caps"))).toBe(true);
  expect(await ownerOf(session.id)).toBe("phone");
  page.ws.close();
  expect(await waitFor(async () => (await ownerOf(session.id)) === undefined)).toBe(true);
  launcher.ws.close();
});

test("a shell that exits ends its session", async () => {
  const { session } = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp" });
  await daemonCall(daemon.socketPath, "send", { id: session.id, text: "exit\n" });
  const ended = await waitFor(async () => {
    const r = await daemonCall(daemon.socketPath, "list", {});
    return r.sessions.find((s) => s.id === session.id)?.status === "dead";
  });
  expect(ended).toBe(true);
});

test("a desk pi is linked to the shell it runs in, and the shell reports where it is now", async () => {
  // A `pi` that is a descendant of a pane: comm is the script's name, which is
  // how the inventory recognises pi.
  const bin = mkdtempSync(join(tmpdir(), "ldmux-fakepi-"));
  writeFileSync(join(bin, "pi"), "#!/bin/sh\nsleep 60\n", { mode: 0o755 });
  const work = mkdtempSync(join(tmpdir(), "ldmux-work-"));
  const { session } = await daemonCall(daemon.socketPath, "new", { kind: "shell", cwd: "/tmp", createdBy: "local" });
  await daemonCall(daemon.socketPath, "send", { id: session.id, text: `cd ${work} && ${join(bin, "pi")}\n` });
  const linked = await waitFor(async () => {
    const inv = await daemonCall(daemon.socketPath, "desktop", {});
    return inv.piSessions.some((p) => p.ldmuxSessionId === session.id && p.cwd === work);
  });
  expect(linked).toBe(true);
  const r = await daemonCall(daemon.socketPath, "list", {});
  const row = r.sessions.find((s) => s.id === session.id)!;
  expect(row.cwd).toBe(work);
  await daemonCall(daemon.socketPath, "kill", { id: session.id });
  rmSync(bin, { recursive: true, force: true });
  rmSync(work, { recursive: true, force: true });
});

test("a tmux session the registry does not know about does not make the daemon broadcast forever", async () => {
  // `forget`, or a hand-made session on ldmux's server, leaves one. Every
  // reconcile reported it as new, and every report saved, broadcast, and — via
  // the broadcast's own session list — reconciled again.
  const { spawn: spawnAsync } = await import("node:child_process");
  await new Promise<void>((resolve) => {
    spawnAsync(daemon.cp.bin, ["-L", tmuxSocket, "new-session", "-d", "-s", "stray", "sleep 60"]).on("exit", () => resolve());
  });
  const p = await phoneClient({ ...phone, id: "desk-test-phone-loop" });
  p.send({ type: "list" });
  await sleep(1500);
  const lists = p.text.filter((m) => m.type === "sessions").length;
  expect(lists).toBeLessThan(5);
  p.ws.close();
});

test("a restarted daemon takes back its own shells that the registry lost", async () => {
  // A crash between `new-session` and saving the record, or a corrupt registry,
  // left shells running that nothing could see.
  const { spawn: spawnAsync } = await import("node:child_process");
  await new Promise<void>((resolve) => {
    spawnAsync(daemon.cp.bin, ["-L", tmuxSocket, "new-session", "-d", "-s", "ldmux-lost0001", "-c", "/tmp"]).on("exit", () => resolve());
  });
  daemon.stop();
  const again = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
  await again.start();
  try {
    const r = await daemonCall(again.socketPath, "list", {});
    const row = r.sessions.find((s) => s.id === "lost0001");
    expect(row?.status).toBe("running");
    expect(row?.tmuxName).toBe("ldmux-lost0001");
  } finally {
    again.stop();
  }
  await daemon.start();
});

test("a desk window closed while the daemon was down ends its shell when it comes back", async () => {
  const { id, win } = await deskWindow();
  daemon.stop();
  process.kill(-win.pid!, "SIGKILL");
  await sleep(300);
  const again = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
  await again.start();
  try {
    const r = await daemonCall(again.socketPath, "list", {});
    expect(r.sessions.find((s) => s.id === id)?.status).toBe("dead");
  } finally {
    again.stop();
  }
  await daemon.start();
});

test("a restarted daemon finds the desk windows it had", async () => {
  const { id } = await deskWindow();
  daemon.stop();
  const again = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
  await again.start();
  try {
    const r = await daemonCall(again.socketPath, "list", {});
    expect((r.sessions.find((s: any) => s.id === id) as any)?.owner?.kind).toBe("desktop");
  } finally {
    again.stop();
  }
});
