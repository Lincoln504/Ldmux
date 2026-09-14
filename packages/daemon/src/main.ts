/**
 * Daemon entrypoint:
 *   bun run packages/daemon/src/main.ts
 * Environment:
 *   LDMUX_STATE_DIR  (default $XDG_RUNTIME_DIR/ldmux or /tmp/ldmux)
 *   LDMUX_TMUX_SOCK  (default "ldmux")
 */

import { join } from "node:path";
import { LdmuxDaemon } from "./daemon.ts";

const stateDir = process.env.LDMUX_STATE_DIR ?? join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ldmux");
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket: process.env.LDMUX_TMUX_SOCK ?? "ldmux" });
daemon.start();
console.log(`ldmuxd listening on ${daemon.socketPath} (tmux -L ${daemon.cp.socket})`);

const shutdown = () => {
  daemon.stop();
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);