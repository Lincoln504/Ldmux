/**
 * Daemon entrypoint:
 *   bun run packages/daemon/src/main.ts
 * Environment: LDMUX_STATE_DIR, LDMUX_TMUX_SOCK, LDMUX_SOCK
 * (resolved via @ldmux/protocol `paths`).
 */

import { resolveStateDir, resolveTmuxSocket } from "../../protocol/src/paths.ts";
import { LdmuxDaemon } from "./daemon.ts";

const daemon = new LdmuxDaemon({
  stateDir: resolveStateDir(),
  tmuxSocket: resolveTmuxSocket(),
  remoteHost: process.env.LDMUX_REMOTE_HOST ?? "127.0.0.1",
  remotePort: Number(process.env.LDMUX_REMOTE_PORT ?? 7623),
});

try {
  await daemon.start();
  console.log(`ldmuxd listening on ${daemon.socketPath} (tmux -L ${daemon.cp.socket})`);} catch (err) {
  const message = (err as Error).message;
  // Another daemon won the startup race — not an error for the spawner.
  if (message === "ldmuxd already running") {
    console.log("ldmuxd already running");
    process.exit(0);
  }
  console.error(`ldmuxd: ${message}`);
  process.exit(1);
}

process.on("SIGINT", () => {
  daemon.stop();
  process.exit(0);
});
process.on("SIGTERM", () => {
  daemon.stop();
  process.exit(0);
});