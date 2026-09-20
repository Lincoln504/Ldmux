/**
 * Daemon entrypoint:
 *   node packages/daemon/src/main.ts
 *
 * Environment: LDMUX_STATE_DIR, LDMUX_SOCK, LDMUX_TMUX_SOCK (resolved via
 * `protocol/paths.ts`), plus LDMUX_REMOTE_HOST and LDMUX_REMOTE_PORT for the
 * loopback WebSocket (docs/REMOTE.md).
 */

import { resolveStateDir, resolveTmuxSocket } from "../../protocol/src/paths.ts";
import { LdmuxDaemon } from "./daemon.ts";

/**
 * Last-resort backstops.
 *
 * The daemon holds every client's terminal; exiting because one request or one
 * promise went wrong is the worst outcome available. So an unhandled rejection
 * or a stray exception is logged loudly and survived, and each specific path
 * that could raise one is still fixed at its source — these are a net, not the
 * mechanism.
 */
process.on("unhandledRejection", (reason) => {
  console.error(`ldmuxd: unhandled rejection (survived): ${format(reason)}`);
});
process.on("uncaughtException", (err) => {
  console.error(`ldmuxd: uncaught exception (survived): ${format(err)}`);
});

function format(value: unknown): string {
  if (value instanceof Error) return `${value.name}: ${value.message}\n${value.stack ?? ""}`;
  return String(value);
}

// A port that is not a number used to disable the remote transport *silently*.
// `Number("abc")` is NaN, `listen(NaN)` throws, `startRemote`'s catch logs one line
// to stderr and carries on — which is right for EADDRINUSE and wrong for a typo — and
// this file then printed "listening on …" and exited 0. Under systemd the unit was
// `active (running)` with no WebSocket at all, and every phone simply could not
// connect, with nothing anywhere saying why. A port that is not a port is
// unambiguously a mistake, so it fails here, loudly, the way `resolveTmuxSocket()`
// already refuses a socket path it does not recognise.
const remotePort = Number(process.env.LDMUX_REMOTE_PORT ?? 7623);
if (!Number.isInteger(remotePort) || remotePort < 1 || remotePort > 65535) {
  console.error(
    `ldmuxd: LDMUX_REMOTE_PORT must be a port number, got ${JSON.stringify(process.env.LDMUX_REMOTE_PORT ?? "")}`,
  );
  process.exit(1);
}

const daemon = new LdmuxDaemon({
  stateDir: resolveStateDir(),
  tmuxSocket: resolveTmuxSocket(),
  remoteHost: process.env.LDMUX_REMOTE_HOST ?? "127.0.0.1",
  remotePort,
  // `control` (default) or `pipe`. Runtime-selectable so the two transports can
  // be compared on a real device without a rebuild (docs/TMUX-DEPENDENCY.md §7).
  streamTransport: process.env.LDMUX_STREAM_TRANSPORT === "pipe" ? "pipe" : "control",
});

try {
  await daemon.start();
  console.log(`ldmuxd listening on ${daemon.socketPath} (tmux -L ${daemon.cp.socket})`);
} catch (err) {
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