import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../../daemon/src/daemon.ts";
import { daemonCall } from "../../protocol/src/rpc.ts";
import { killTmuxServer } from "../../daemon/test/tmux-cleanup.ts";
import { ensureDaemon, isChildAgentSession, ping, resolveConfig } from "../src/config.ts";

const tmuxSocket = `ldmux-exttest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-ext-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });

process.env.LDMUX_STATE_DIR = stateDir;
process.env.LDMUX_SOCK = join(stateDir, "ldmuxd.sock");
process.env.LDMUX_TMUX_SOCK = tmuxSocket;

await daemon.start();

const tm = (...a: string[]) => Bun.spawnSync(["tmux", "-L", tmuxSocket, ...a]);

afterAll(() => {
  daemon.stop();
  killTmuxServer(tmuxSocket);
  rmSync(stateDir, { recursive: true, force: true });
});

test("resolveConfig honours LDMUX_* environment", () => {
  const cfg = resolveConfig();
  expect(cfg.socketPath).toBe(join(stateDir, "ldmuxd.sock"));
  expect(cfg.tmuxSocket).toBe(tmuxSocket);
});

test("ping succeeds against the running daemon", async () => {
  const res = await ping(resolveConfig());
  expect(res?.pong).toBe(true);
});

test("ensureDaemon is a no-op when the daemon is already up", async () => {
  expect(await ensureDaemon(resolveConfig())).toBe(true);
});

test("register binds the pi session and list reports it", async () => {
  const cfg = resolveConfig();
  const { session } = await daemonCall(cfg.socketPath, "register", {
    piSessionId: "pi-ext-test-session",
    cwd: tmpdir(),
    kind: "pi",
  });
  expect(session.piSessionId).toBe("pi-ext-test-session");
  const { sessions } = await daemonCall(cfg.socketPath, "list", {});
  expect(sessions.some((s) => s.piSessionId === "pi-ext-test-session")).toBe(true);
});

test("only root pi sessions register: fork/subagent children are filtered", () => {
  expect(isChildAgentSession({} as NodeJS.ProcessEnv)).toBe(false);
  expect(isChildAgentSession({ PI_FORK_PROVIDER: "openrouter" } as NodeJS.ProcessEnv)).toBe(true);
  expect(isChildAgentSession({ PI_FORK_ALL_TOOLS: "1" } as NodeJS.ProcessEnv)).toBe(true);
  expect(
    isChildAgentSession({ PI_FORK_PROVIDER: "openrouter", LDMUX_REGISTER_CHILD: "1" } as NodeJS.ProcessEnv),
  ).toBe(false);
});
test("ensureDaemon spawns a detached daemon and attaches to it", async () => {
  const dir2 = mkdtempSync(join(tmpdir(), "ldmux-ext-spawn-"));
  const sock2 = join(dir2, "ldmuxd.sock");
  const tmux2 = `ldmux-extspawn-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
  const cfg = { stateDir: dir2, socketPath: sock2, tmuxSocket: tmux2 };
  try {
    expect(await ensureDaemon(cfg as never)).toBe(true);
    expect((await ping(cfg as never))?.pong).toBe(true);
  } finally {
    // The daemon is detached; reap it by the pid it recorded in its lockfile.
    try {
      const pid = Number(readFileSync(join(dir2, "ldmuxd.lock"), "utf8").trim());
      if (Number.isFinite(pid) && pid > 0) process.kill(pid, "SIGTERM");
    } catch {
      /* never started */
    }
    await Bun.sleep(200);
    killTmuxServer(tmux2);
    rmSync(dir2, { recursive: true, force: true });
  }
}, 20000);

test("ensureDaemon reports false when the daemon cannot be spawned", async () => {
  const dir3 = mkdtempSync(join(tmpdir(), "ldmux-ext-fail-"));
  const cfg = { stateDir: dir3, socketPath: join(dir3, "ldmuxd.sock"), tmuxSocket: "ldmux-extfail" };
  const prev = process.env.LDMUX_BUN;
  process.env.LDMUX_BUN = "/nonexistent-bun-for-ldmux";
  try {
    expect(await ensureDaemon(cfg as never)).toBe(false);
  } finally {
    if (prev === undefined) delete process.env.LDMUX_BUN;
    else process.env.LDMUX_BUN = prev;
    rmSync(dir3, { recursive: true, force: true });
  }
}, 20000);
