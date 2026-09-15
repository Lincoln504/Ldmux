/**
 * Manual probe: stream a tmux session's pane output via control mode.
 *
 *   nix develop --command node packages/daemon/src/probe.ts <socket> <session>
 *
 * Ctrl-C to stop. Proves the ADR-0001 transport end to end.
 */

import { TmuxControlClient } from "./tmux/control-client.ts";

const [socket = "default", session] = process.argv.slice(2);
if (!session) {
  console.error("usage: node packages/daemon/src/probe.ts <socket> <session>");
  process.exit(2);
}

console.error(`attaching to ${session} on -L ${socket} (Ctrl-C to stop)…`);
const client = await TmuxControlClient.connect(socket, session, {
  onOutput: (pane, text) => process.stdout.write(text),
  onNotification: (line) => console.error(`\n[notify] ${line}`),
  onStderr: (chunk) => process.stderr.write(chunk),
  onExit: () => {
    console.error("\n[session exited]");
    process.exit(0);
  },
});

process.on("SIGINT", () => {
  client.close();
  process.exit(0);
});