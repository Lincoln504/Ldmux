/**
 * Daemon supervision + config for the ldmux pi extension.
 *
 * The extension never owns long-lived resources: ldmuxd is a separate process
 * (a pi extension runs in-process and cannot outlive a pi session). The
 * extension only ensures the daemon is up and registers the pi session with it.
 */

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { daemonCall, type PingResult } from "../../protocol/src/rpc.ts";
import { resolveSocketPath, resolveStateDir, resolveTmuxSocket } from "../../protocol/src/paths.ts";

export interface LdmuxConfig {
  stateDir: string;
  socketPath: string;
  tmuxSocket: string;
}

export function resolveConfig(): LdmuxConfig {
  return {
    stateDir: resolveStateDir(),
    socketPath: resolveSocketPath(),
    tmuxSocket: resolveTmuxSocket(),
  };
}

/** Absolute path to the daemon entrypoint (this package lives in the monorepo). */
const DAEMON_MAIN = fileURLToPath(new URL("../../daemon/src/main.ts", import.meta.url));

export async function ping(cfg: LdmuxConfig, timeoutMs = 1000): Promise<PingResult | undefined> {
  try {
    return await daemonCall(cfg.socketPath, "ping", {}, timeoutMs);
  } catch {
    return undefined;
  }
}

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Ensure ldmuxd is reachable, spawning it detached if not. Idempotent and safe
 * to call from every session_start. Duplicate-spawn races are harmless: the
 * daemon takes a pid lockfile, so a loser exits and this loop attaches to the
 * winner's socket. A small randomized delay spreads concurrent starters out.
 */
export async function ensureDaemon(cfg: LdmuxConfig): Promise<boolean> {
  if (await ping(cfg)) return true;
  const bun = process.env.LDMUX_BUN ?? "bun";
  try {
    const child = spawn(bun, ["run", DAEMON_MAIN], {
      detached: true,
      stdio: "ignore",
      env: {
        ...process.env,
        LDMUX_STATE_DIR: cfg.stateDir,
        LDMUX_SOCK: cfg.socketPath,
        LDMUX_TMUX_SOCK: cfg.tmuxSocket,
      },
    });
    child.on("error", () => {
      /* spawn failed (e.g. bun missing); the poll below will report it */
    });
    child.unref();
  } catch {
    return false;
  }
  await delay(50 + Math.floor(Math.random() * 100));
  for (let attempt = 0; attempt < 25; attempt++) {
    await delay(150);
    if (await ping(cfg)) return true;
  }
  return false;
}