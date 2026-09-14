import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../../daemon/src/daemon.ts";
import { daemonCall } from "../../protocol/src/rpc.ts";
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
  tm("kill-server");
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