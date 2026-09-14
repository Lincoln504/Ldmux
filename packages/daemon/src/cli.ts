/**
 * ldmux CLI (Phase 1). Talks to the daemon over its Unix socket.
 *
 *   ldmux list
 *   ldmux new <shell|pi> <cwd> [--title T]
 *   ldmux send <id> <text>
 *   ldmux key <id> <Key...>
 *   ldmux capture <id> [--history]
 *   ldmux snapshot <id>
 *   ldmux kill <id>
 *   ldmux ping
 */

import { join } from "node:path";
import { call } from "./socket-client.ts";
import type { SessionRecord } from "./registry.ts";

const defaultSocket = () =>
  process.env.LDMUX_SOCK ?? join(process.env.XDG_RUNTIME_DIR ?? "/tmp", "ldmux", "ldmuxd.sock");

function flag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

function positional(args: string[], skip: string[]): string[] {
  const out: string[] = [];
  for (let i = 0; i < args.length; i++) {
    if (skip.includes(args[i])) {
      i++;
      continue;
    }
    if (args[i].startsWith("--")) continue;
    out.push(args[i]);
  }
  return out;
}

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const sock = defaultSocket();
  switch (cmd) {
    case "ping":
      console.log(JSON.stringify(await call(sock, "ping")));
      break;
    case "list": {
      const { sessions } = await call<{ sessions: SessionRecord[] }>(sock, "list");
      if (!sessions.length) return void console.log("(no sessions)");
      for (const s of sessions) {
        console.log(`${s.id}  ${s.status.padEnd(7)} ${s.kind.padEnd(5)} ${s.cwd}  "${s.title}"  [${s.tmuxName}]`);
      }
      break;
    }
    case "new": {
      const [kind, cwd] = positional(rest, ["--title"]);
      const { session } = await call<{ session: SessionRecord }>(sock, "new", {
        kind: kind ?? "shell",
        cwd: cwd ?? process.cwd(),
        title: flag(rest, "--title"),
      });
      console.log(session.id);
      break;
    }
    case "send": {
      const [id, ...text] = positional(rest, []);
      await call(sock, "send", { id, text: text.join(" ") });
      break;
    }
    case "key": {
      const [id, ...keys] = positional(rest, []);
      await call(sock, "send", { id, keys });
      break;
    }
    case "capture": {
      const [id] = positional(rest, []);
      const { screen } = await call<{ screen: string }>(sock, "capture", { id, history: rest.includes("--history") });
      process.stdout.write(screen);
      break;
    }
    case "snapshot": {
      const [id] = positional(rest, []);
      console.log(JSON.stringify(await call(sock, "snapshot", { id }), null, 2));
      break;
    }
    case "kill": {
      const [id] = positional(rest, []);
      await call(sock, "kill", { id });
      console.log(`killed ${id}`);
      break;
    }
    default:
      console.error("usage: ldmux <ping|list|new|send|key|capture|snapshot|kill> [args]");
      process.exit(2);
  }
}

main().catch((err) => {
  console.error(`ldmux: ${(err as Error).message}`);
  process.exit(1);
});