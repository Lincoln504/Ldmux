/**
 * ldmux CLI. Talks to the daemon over its Unix socket (shared @ldmux/protocol client).
 *
 *   ldmux list
 *   ldmux new <shell|pi> <cwd> [--title T]
 *   ldmux run <shell|pi> [cwd]        # create, then attach tmux
 *   ldmux send <id> <text>
 *   ldmux key <id> <Key...>
 *   ldmux capture <id> [--history]
 *   ldmux snapshot <id>
 *   ldmux kill <id>
 *   ldmux forget <id>                 # drop a record without touching tmux
 *   ldmux adopt <tmux-name>
 *   ldmux mirror <id> [--rw] [--workspace N]
 *   ldmux remote
 *   ldmux pair
 *   ldmux expose [--dry-run]
 */

import { spawnSync } from "node:child_process";
import { daemonCall, defaultSocketPath } from "../../protocol/src/rpc.ts";
import { call } from "../../protocol/src/socket-client.ts";
import type { PingResult, SessionRecord } from "../../protocol/src/rpc.ts";
import { DEFAULT_MIRROR_WORKSPACE, mirrorCommand, openMirror } from "./desktop.ts";
import { buildPairInfo, expose, tailnetHost } from "./pairing.ts";

function flag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

function positional(args: string[], skip: string[]): string[] {
  const out: string[] = [];
  // A bare `--` ends option scanning, so `ldmux send <id> -- --help` types the
  // literal `--help` instead of silently sending nothing.
  let endOfOptions = false;
  for (let i = 0; i < args.length; i++) {
    const a = args[i]!;
    if (!endOfOptions) {
      if (a === "--") {
        endOfOptions = true;
        continue;
      }
      if (skip.includes(a)) {
        i++;
        continue;
      }
      if (a.startsWith("--")) continue;
    }
    out.push(a);
  }
  return out;
}

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const sock = defaultSocketPath();
  switch (cmd) {
    case "ping":
      console.log(JSON.stringify(await daemonCall(sock, "ping", {})));
      break;

    case "list": {
      const { sessions } = await daemonCall(sock, "list", {});
      if (!sessions.length) return void console.log("(no sessions)");
      for (const s of sessions) {
        const attach = s.tmuxName ? `[${s.tmuxName}]` : "[rpc]";
        console.log(`${s.id}  ${s.status.padEnd(7)} ${s.kind.padEnd(5)} ${s.cwd}  "${s.title}"  ${attach}`);
      }
      break;
    }

    case "new": {
      const [kind, cwd] = positional(rest, ["--title"]);
      const { session } = await daemonCall(sock, "new", {
        kind: (kind as "shell" | "pi") ?? "shell",
        cwd: cwd ?? process.cwd(),
        title: flag(rest, "--title"),
      });
      console.log(session.id);
      break;
    }

    case "run": {
      const [kind, cwd] = positional(rest, []);
      const ping = await call<PingResult>(sock, "ping");
      const { session } = await daemonCall(sock, "new", {
        kind: (kind as "shell" | "pi") ?? "shell",
        cwd: cwd ?? process.cwd(),
      });
      console.error(`ldmux: session ${session.id} (${session.tmuxName}) — detach with Ctrl-b d`);
      const proc = spawnSync("tmux", ["-L", ping.tmuxSocket, "attach", "-t", session.tmuxName], {
        stdio: "inherit",
      });
      process.exit(proc.status ?? 0);
    }

    case "send": {
      const [id, ...text] = positional(rest, []);
      await daemonCall(sock, "send", { id, text: text.join(" ") });
      break;
    }

    case "key": {
      const [id, ...keys] = positional(rest, []);
      await daemonCall(sock, "send", { id, keys });
      break;
    }

    case "capture": {
      const [id] = positional(rest, []);
      const { screen } = await daemonCall(sock, "capture", { id, history: rest.includes("--history") });
      process.stdout.write(screen);
      break;
    }

    case "snapshot": {
      const [id] = positional(rest, []);
      console.log(JSON.stringify(await daemonCall(sock, "snapshot", { id }), null, 2));
      break;
    }

    case "kill": {
      const [id] = positional(rest, []);
      await daemonCall(sock, "kill", { id });
      console.log(`killed ${id}`);
      break;
    }

    case "forget": {
      const [id] = positional(rest, []);
      const res = await daemonCall(sock, "forget", { id });
      console.log(res.removed ? `forgot ${id}` : `no such session ${id ?? ""}`);
      break;
    }

    case "adopt": {
      const [tmuxName] = positional(rest, []);
      const { session } = await daemonCall(sock, "adopt", { tmuxName });
      console.log(`${session.id}  ${session.tmuxName}`);
      break;
    }

    case "remote": {
      const info = await daemonCall(sock, "remote_info", {});
      console.log(JSON.stringify(info, null, 2));
      console.log(`\nlocal:     ws://${info.hostname}:${info.port}${info.path}`);
      console.log(`expose:    tailscale serve --bg --https=443 http://${info.hostname}:${info.port}`);
      break;
    }

    case "mirror": {
      const [id] = positional(rest, ["--workspace", "--terminal"]);
      const { sessions } = await daemonCall(sock, "list", {});
      const session = sessions.find((s) => s.id === id);
      if (!session?.tmuxName) {
        console.error(`ldmux: no terminal-backed session ${id ?? ""}`);
        process.exit(1);
      }
      const ping = await call<PingResult>(sock, "ping");
      const opts = {
        readOnly: !rest.includes("--rw"),
        workspace: Number(flag(rest, "--workspace") ?? DEFAULT_MIRROR_WORKSPACE),
        terminal: flag(rest, "--terminal"),
      };
      const res = await openMirror(ping.tmuxSocket, session.tmuxName, opts);
      if (!res.ok) {
        console.error(`ldmux: mirror not opened (${res.detail})`);
        console.error(`run manually: ${mirrorCommand(ping.tmuxSocket, session.tmuxName, opts)}`);
        process.exit(1);
      }
      console.log(`mirror of ${session.tmuxName} on workspace ${res.workspace}`);
      break;
    }

    case "pair": {
      const info = await daemonCall(sock, "remote_info", {});
      const host = await tailnetHost();
      if (!host) {
        console.error("ldmux: tailscale unavailable; using localhost");
      }
      const pair = buildPairInfo({ host: host ?? "127.0.0.1", port: info.port, token: info.token });
      console.log(`url:        ${pair.url}`);
      console.log(`local:      ${pair.localUrl}`);
      console.log(`deep link:  ${pair.deepLink}`);
      console.log(`expose:     ${pair.serveCommand}`);
      break;
    }

    case "expose": {
      const info = await daemonCall(sock, "remote_info", {});
      if (rest.includes("--dry-run")) {
        console.log(`tailscale serve --bg --https=443 http://127.0.0.1:${info.port}`);
        break;
      }
      const res = await expose({ port: info.port });
      if (!res.ok) {
        console.error(`ldmux: ${res.detail}`);
        process.exit(1);
      }
      const pair = buildPairInfo({ host: (await tailnetHost()) ?? "127.0.0.1", port: info.port, token: info.token });
      console.log(`exposed on the tailnet: ${pair.url}`);
      break;
    }

    default:
      console.error(
        "usage: ldmux <ping|list|new|run|send|key|capture|snapshot|kill|forget|adopt|mirror|remote|pair|expose> [args]",
      );
      process.exit(2);
  }
}

main().catch((err) => {
  console.error(`ldmux: ${(err as Error).message}`);
  process.exit(1);
});