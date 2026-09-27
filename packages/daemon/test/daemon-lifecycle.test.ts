import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { call } from "../../protocol/src/socket-client.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

const root = mkdtempSync(join(tmpdir(), "ldmux-lifecycle-"));
const tmuxSocket = `ldmux-life-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const live: LdmuxDaemon[] = [];

afterAll(() => {
  for (const daemon of live) {
    try {
      daemon.stop();
    } catch {
      /* already stopped */
    }
  }
  killTmuxServer(tmuxSocket);
  rmSync(root, { recursive: true, force: true });
});

function freshDaemon(extra: Record<string, unknown> = {}): LdmuxDaemon {
  const daemon = new LdmuxDaemon({
    stateDir: mkdtempSync(join(root, "state-")),
    tmuxSocket,
    remotePort: 0,
    ...extra,
  });
  live.push(daemon);
  return daemon;
}

test("start creates an owner-only state dir, socket and lock", async () => {
  const daemon = freshDaemon();
  await daemon.start();

  // Private by construction: the socket is full control of every session.
  expect(statSync(daemon.stateDir).mode & 0o777).toBe(0o700);
  expect(statSync(daemon.socketPath).mode & 0o777).toBe(0o600);
  expect(existsSync(join(daemon.stateDir, "ldmuxd.lock"))).toBe(true);
  expect(Number(readFileSync(join(daemon.stateDir, "ldmuxd.lock"), "utf8"))).toBe(process.pid);

  const res = await call<{ pong: boolean }>(daemon.socketPath, "ping");
  expect(res.pong).toBe(true);

  // The registry is written lazily: an empty daemon has nothing to persist, so
  // the file appears with the first session (and survives restarts).
  const { session } = await call<{ session: { id: string } }>(daemon.socketPath, "new", { kind: "shell", cwd: tmpdir() });
  expect(existsSync(join(daemon.stateDir, "registry.json"))).toBe(true);
  await call(daemon.socketPath, "kill", { id: session.id });
});

test("a second daemon refuses to start and leaves the first one's socket intact", async () => {
  const first = freshDaemon();
  await first.start();
  const second = new LdmuxDaemon({ stateDir: first.stateDir, tmuxSocket, remotePort: 0 });
  await expect(second.start()).rejects.toThrow(/already running/);
  // The loser must not unlink the winner's socket (the old startup bug).
  const res = await call<{ pong: boolean }>(first.socketPath, "ping");
  expect(res.pong).toBe(true);
});

test("a stale lock from a dead process is stolen", async () => {
  const dir = mkdtempSync(join(root, "stale-"));
  // A pid that cannot exist: no live process will match it.
  writeFileSync(join(dir, "ldmuxd.lock"), "999999");
  const daemon = new LdmuxDaemon({ stateDir: dir, tmuxSocket, remotePort: 0 });
  live.push(daemon);
  await daemon.start();
  expect(Number(readFileSync(join(dir, "ldmuxd.lock"), "utf8"))).toBe(process.pid);
});

test("a lock held by a live process is respected even for another pid", async () => {
  const dir = mkdtempSync(join(root, "held-"));
  writeFileSync(join(dir, "ldmuxd.lock"), String(process.pid)); // this test process is alive
  const daemon = new LdmuxDaemon({ stateDir: dir, tmuxSocket, remotePort: 0 });
  await expect(daemon.start()).rejects.toThrow(/already running/);
});

test("stop removes the socket and the lock, and leaves tmux alone", async () => {
  const daemon = freshDaemon();
  await daemon.start();
  const { session } = await call<{ session: { id: string; tmuxName: string } }>(daemon.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  const dir = daemon.stateDir;
  daemon.stop();

  expect(existsSync(daemon.socketPath)).toBe(false);
  expect(existsSync(join(dir, "ldmuxd.lock"))).toBe(false);
  // The session registry survives a restart (crash-only design)…
  expect(existsSync(join(dir, "registry.json"))).toBe(true);
  // …and tmux still owns the session: stopping the daemon must never kill it.
  const sessions = await daemon.cp.listSessions();
  expect(sessions?.map((s) => s.name)).toContain(session.tmuxName);

  // Requests now fail rather than hanging on a dead socket.
  await expect(call(daemon.socketPath, "ping")).rejects.toBeDefined();
  await call(daemon.socketPath, "kill", { id: session.id }).catch(() => {});
});

test("a restarted daemon adopts its own tmux sessions from the registry", async () => {
  const dir = mkdtempSync(join(root, "restart-"));
  const first = new LdmuxDaemon({ stateDir: dir, tmuxSocket, remotePort: 0 });
  live.push(first);
  await first.start();
  const { session } = await call<{ session: { id: string; tmuxName: string } }>(first.socketPath, "new", {
    kind: "shell",
    cwd: tmpdir(),
  });
  first.stop();

  const second = new LdmuxDaemon({ stateDir: dir, tmuxSocket, remotePort: 0 });
  live.push(second);
  await second.start();
  const { sessions } = await call<{ sessions: Array<{ id: string; status: string }> }>(second.socketPath, "list", {});
  const mine = sessions.find((s) => s.id === session.id);
  // The tmux session outlived the daemon, so reconcile must find it running.
  expect(mine?.status).toBe("running");
});

test("stop is idempotent and safe to call twice", async () => {
  const daemon = freshDaemon();
  await daemon.start();
  daemon.stop();
  expect(() => daemon.stop()).not.toThrow();
});

test("a remote bind failure only disables remote; local control keeps working", async () => {
  // Take a port with one daemon, then make a second daemon want it.
  const holder = freshDaemon();
  await holder.start();
  const takenPort = holder.remoteInfoForTest().port;
  expect(takenPort).toBeGreaterThan(0);

  const blocked = freshDaemon({ remotePort: takenPort });
  await blocked.start(); // must not throw

  // Local RPC is unaffected…
  const res = await call<{ pong: boolean }>(blocked.socketPath, "ping");
  expect(res.pong).toBe(true);
  // …and remote_info reports that remote is unavailable rather than a lie.
  const info = await call<{ port: number }>(blocked.socketPath, "remote_info");
  expect(info.port).toBe(0);
});

test("a state directory that already exists is tightened, not trusted", async () => {
  // `mkdirSync` with `recursive` applies `mode` only when it *creates* the directory.
  // A directory that already existed kept whatever the umask gave it, so that line was
  // not the guarantee it looked like — the running daemon's own `/run/user/1000/ldmux`
  // was 0775, and `resolveStateDir()` falls back to `/tmp/ldmux` when `XDG_RUNTIME_DIR`
  // is unset. The token, the registry and the socket path all live here.
  //
  // The assertion in the test above could never have caught this: `mkdtempSync` hands
  // back 0700, so it passed for the wrong reason. This fixture is deliberately loose
  // first, and says so, so a failure here is about the code and not about the fixture.
  const dir = join(root, "loose-state");
  mkdirSync(dir, { recursive: true, mode: 0o775 });
  chmodSync(dir, 0o775);
  expect(statSync(dir).mode & 0o077).toBe(0o075);

  const daemon = freshDaemon({ stateDir: dir });
  await daemon.start();
  expect(statSync(daemon.stateDir).mode & 0o777).toBe(0o700);
  // And it is still usable afterwards, which is the point of tightening rather than
  // refusing: the shell did not stop being the owner's because a mode was wrong.
  expect(existsSync(daemon.socketPath)).toBe(true);
});

test("a remote bind that fails because the address is busy comes up once it frees", async () => {
  // The boot case is EADDRNOTAVAIL (the tailnet address is not assigned yet),
  // which a test cannot produce and then clear; EADDRINUSE takes the same retry
  // path and can be. Without the retry the second daemon stays at port 0 forever.
  const holder = freshDaemon();
  await holder.start();
  const port = holder.remoteInfoForTest().port;

  const waiting = freshDaemon({ remotePort: port, remoteRetryMs: 50 });
  await waiting.start();
  expect(waiting.remoteInfoForTest().port).toBe(0);

  holder.stop();
  const deadline = Date.now() + 4000;
  while (waiting.remoteInfoForTest().port === 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 25));
  }
  expect(waiting.remoteInfoForTest().port).toBe(port);
});

test("a stopped daemon does not go on retrying its remote bind", async () => {
  const holder = freshDaemon();
  await holder.start();
  const port = holder.remoteInfoForTest().port;
  const waiting = freshDaemon({ remotePort: port, remoteRetryMs: 50 });
  await waiting.start();
  waiting.stop();
  holder.stop();
  await new Promise((r) => setTimeout(r, 200));
  expect(waiting.remoteInfoForTest().port).toBe(0);
});
