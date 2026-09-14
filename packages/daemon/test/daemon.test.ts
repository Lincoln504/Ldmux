import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { Registry } from "../src/registry.ts";
import { call } from "../../protocol/src/socket-client.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";

const tmuxSocket = `ldmux-daemontest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-daemon-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();

const tm = (...a: string[]) => Bun.spawnSync(["tmux", "-L", tmuxSocket, ...a]);

afterAll(() => {
  daemon.stop();
  tm("kill-server");
  rmSync(stateDir, { recursive: true, force: true });
});

async function waitFor(pred: () => Promise<boolean>, ms = 6000): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await pred()) return true;
    await Bun.sleep(60);
  }
  return false;
}

test("ping reports the tmux socket", async () => {
  const res = await call<{ pong: boolean; tmuxSocket: string }>(daemon.socketPath, "ping");
  expect(res.pong).toBe(true);
  expect(res.tmuxSocket).toBe(tmuxSocket);
});

test("new -> list -> send -> capture -> snapshot -> kill lifecycle", async () => {
  const created = await call<{ session: SessionRecord }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
    title: "lifecycle",
  });
  const id = created.session.id;
  expect(created.session.tmuxName.startsWith("ldmux-")).toBe(true);
  expect(created.session.status).toBe("running");

  const list = await call<{ sessions: SessionRecord[] }>(daemon.socketPath, "list");
  expect(list.sessions.some((s) => s.id === id)).toBe(true);

  await call(daemon.socketPath, "send", { id, text: "printf 'lifecycle-ok\\n'" });
  await call(daemon.socketPath, "send", { id, keys: ["Enter"] });
  const sawOutput = await waitFor(async () => {
    const { screen } = await call<{ screen: string }>(daemon.socketPath, "capture", { id });
    return screen.includes("lifecycle-ok");
  });
  expect(sawOutput).toBe(true);

  const snap = await call<{ state: { cols: number; rows: number } }>(daemon.socketPath, "snapshot", { id });
  expect(snap.state.cols).toBe(120);
  expect(snap.state.rows).toBe(40);

  await call(daemon.socketPath, "kill", { id });
  const after = await call<{ sessions: SessionRecord[] }>(daemon.socketPath, "list");
  expect(after.sessions.find((s) => s.id === id)?.status).toBe("dead");
});

test("reconcile marks an externally killed session dead", async () => {
  const created = await call<{ session: SessionRecord }>(daemon.socketPath, "new", { kind: "shell", cwd: tmpdir() });
  const id = created.session.id;
  tm("kill-session", "-t", created.session.tmuxName);
  const list = await call<{ sessions: SessionRecord[] }>(daemon.socketPath, "list");
  expect(list.sessions.find((s) => s.id === id)?.status).toBe("dead");
});

test("adopt registers an existing tmux session", async () => {
  tm("new-session", "-d", "-s", "external-one", "-c", tmpdir());
  const adopted = await call<{ session: SessionRecord }>(daemon.socketPath, "adopt", { tmuxName: "external-one" });
  expect(adopted.session.tmuxName).toBe("external-one");
  tm("kill-session", "-t", "external-one");
});

test("unknown method and unknown session are errors", async () => {
  await expect(call(daemon.socketPath, "nope")).rejects.toThrow();
  await expect(call(daemon.socketPath, "capture", { id: "does-not-exist" })).rejects.toThrow();
});

test("socket is owner-only (0600)", () => {
  expect(statSync(daemon.socketPath).mode & 0o777).toBe(0o600);
});

test("a second daemon on the same state dir refuses to start", async () => {
  const second = new LdmuxDaemon({ stateDir, tmuxSocket, enableRemote: false });
  await expect(second.start()).rejects.toThrow("already running");
});

test("new injects the child env contract", async () => {
  const { session } = await call<{ session: SessionRecord }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  await call(daemon.socketPath, "send", { id: session.id, text: "printf 'ENV:%s\n' \"$LDMUX_SESSION_ID\"" });
  await call(daemon.socketPath, "send", { id: session.id, keys: ["Enter"] });
  const ok = await waitFor(async () => {
    const { screen } = await call<{ screen: string }>(daemon.socketPath, "capture", { id: session.id });
    return screen.includes(`ENV:${session.id}`);
  });
  expect(ok).toBe(true);
});

test("register binds a pi session and session_end marks it dead", async () => {
  const piSessionId = "pi-01HMOCKULID00000000000000";
  const { session } = await call<{ session: SessionRecord }>(daemon.socketPath, "register", {
    piSessionId,
    cwd: tmpdir(),
    kind: "pi",
  });
  expect(session.piSessionId).toBe(piSessionId);
  expect(session.tmuxName).toBe("");

  // RPC-only sessions have no tmux counterpart and must survive reconcile.
  const list = await call<{ sessions: SessionRecord[] }>(daemon.socketPath, "list");
  expect(list.sessions.find((s) => s.id === session.id)?.status).toBe("running");

  // Re-registering is idempotent (same record).
  const again = await call<{ session: SessionRecord }>(daemon.socketPath, "register", {
    piSessionId,
    cwd: tmpdir(),
  });
  expect(again.session.id).toBe(session.id);

  await call(daemon.socketPath, "session_end", { piSessionId });
  const after = await call<{ sessions: SessionRecord[] }>(daemon.socketPath, "list");
  expect(after.sessions.find((s) => s.id === session.id)?.status).toBe("dead");
});
test("concurrent requests on one connection are answered in request order", async () => {
  // handle() awaits tmux now; without the per-connection queue two handlers
  // could interleave and reorder responses. Pipelining is the observable check.
  const lines: string[] = [];
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out")), 8000);
    Bun.connect({
      unix: daemon.socketPath,
      socket: {
        open: (s: any) => {
          // Three requests in a single write: they land in one read chunk.
          s.write(
            ["a", "b", "c"].map((id) => JSON.stringify({ id, method: "ping" })).join("\n") + "\n",
          );
        },
        data: (_s: any, chunk: Uint8Array) => {
          lines.push(new TextDecoder().decode(chunk));
          if (lines.join("").split("\n").filter(Boolean).length >= 3) {
            clearTimeout(timer);
            resolve();
          }
        },
        error: (_s: any, err: Error) => {
          clearTimeout(timer);
          reject(err);
        },
      },
    });
  });
  const replies = lines
    .join("")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l) as { id: string; ok: boolean });
  expect(replies.map((r) => r.id)).toEqual(["a", "b", "c"]);
  expect(replies.every((r) => r.ok)).toBe(true);
});
