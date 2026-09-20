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

import { basename, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { daemonCall, defaultSocketPath } from "../../protocol/src/rpc.ts";
import { call } from "../../protocol/src/socket-client.ts";
import type { PingResult, SessionRecord } from "../../protocol/src/rpc.ts";
import { DEFAULT_MIRROR_WORKSPACE, mirrorCommand, openMirror } from "./desktop.ts";
import { resolveTmuxBin } from "./tmux/control-plane.ts";
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

/** Every long option the CLI understands, across all commands. */
const KNOWN_FLAGS = new Set([
  "--cwd",
  "--dry-run",
  "--history",
  "--json",
  "--name",
  "--rw",
  "--terminal",
  "--title",
  "--workspace",
]);

/**
 * The one usage string.
 *
 * It used to be an argument to `console.error` inside the switch's `default:` arm,
 * so nothing else could print it and a `--help` had nowhere to go but into the
 * command it was meant to explain.
 */
function printUsage(): void {
  console.error(
    "usage: ldmux <ping|list|desktop|new|shell|run|send|key|capture|snapshot|kill|close|forget|adopt|mirror|live|unlive|remote|pair|expose> [args]",
  );
}

/**
 * The first option no command knows, or null.
 *
 * `flag()` finds a name and returns the next argv element, and silently ignores
 * every option it does not recognise — so `ldmux shell --nam foo` used the default
 * name, and `ldmux shell --help` **built a session**, which then had to be killed by
 * id, because the session was the only evidence the flag had been dropped. That is
 * not one bad case, it is every mistyped flag in every command, so the check belongs
 * here: once, before any command runs, before any socket is opened, and before
 * anything with a side effect can happen.
 *
 * `--` still ends option scanning, so `ldmux send <id> -- --help` types the text.
 */
function rejectUnknownFlag(rest: string[]): string | null {
  for (const arg of rest) {
    if (arg === "--") return null;
    if (!arg.startsWith("-") || arg === "-") continue;
    const name = arg.includes("=") ? arg.slice(0, arg.indexOf("=")) : arg;
    if (KNOWN_FLAGS.has(name)) continue;
    return name;
  }
  return null;
}

async function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  const sock = defaultSocketPath();

  // `--help` is a request about the tool, not a flag for a command, so it is
  // answered before the command is looked up and never reaches one.
  if (cmd === "--help" || cmd === "-h") {
    printUsage();
    process.exit(0);
  }
  const bad = rejectUnknownFlag(rest);
  if (bad === "--help" || bad === "-h") {
    printUsage();
    process.exit(0);
  }
  if (bad) {
    console.error(`ldmux ${cmd ?? ""}: unknown option ${bad}`);
    printUsage();
    process.exit(2);
  }

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
      const proc = spawnSync(resolveTmuxBin(), ["-L", ping.tmuxSocket, "attach", "-t", session.tmuxName], {
        stdio: "inherit",
      });
      process.exit(proc.status ?? 0);
    }

    /**
     * A shell in this terminal, that the phone can see and take over.
     *
     * This is the *tool*, not an installer: it changes nothing about your
     * window manager or your shell. Run it where you would have run `tmux`, and
     * the shell it starts is an ldmux session like any other.
     */
    case "shell": {
      const ping = await call<PingResult>(sock, "ping");
      const name = flag(rest, "--name");
      const wantCwd = resolve(flag(rest, "--cwd") ?? process.cwd());
      const { sessions } = await daemonCall(sock, "list", {});

      // Reuse before creating. Running `ldmux shell` twice in one directory is
      // almost always "get me back to it", not "give me a second one" — and a
      // second one would be a second shell in the same place, which is exactly
      // what the launcher's dedup-by-directory exists to prevent.
      //
      // This matched on `s.title === wanted` as well, and that made the whole rule
      // dead: a title is not stable. The phone used to open a directory row with
      // the *process name* as the title, so a shell in `~/Documents/RCNT` was
      // titled `pi`, and `wanted` is `basename(cwd)` — `RCNT`. The two never
      // agreed again, and `ldmux shell` in a directory that already had one grew a
      // second session beside it. A rule that depends on a value the thing itself
      // rewrites is not a rule. The directory is the identity; `--name` is the
      // escape hatch for wanting a second one on purpose.
      const wanted = name ?? basename(wantCwd);
      const existing = sessions.find(
        (s) =>
          s.kind === "shell" &&
          s.status !== "dead" &&
          s.tmuxName !== "" &&
          (name ? s.title === name : resolve(s.cwd) === wantCwd),
      );

      let target: string;
      if (existing) {
        target = existing.tmuxName;
        console.error(`ldmux: reusing ${existing.id} (${target})`);
      } else {
        const { session } = await daemonCall(sock, "new", {
          kind: "shell",
          cwd: wantCwd,
          title: wanted,
          createdBy: "local",
        });
        target = session.tmuxName;
        console.error(`ldmux: session ${session.id} (${target}) — detach with Ctrl-b d`);
      }
      const proc = // By absolute path: the daemon and this shell may not share a PATH, and
      // two tmux versions cannot share a server.
      spawnSync(resolveTmuxBin(), ["-L", ping.tmuxSocket, "attach", "-t", target], {
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

    case "desktop": {
      // What the launcher shows. `--json` is for piping into jq or a test.
      const inv = await daemonCall(sock, "desktop", {});
      if (flag(rest, "--json")) {
        console.log(JSON.stringify(inv, null, 2));
        break;
      }
      console.log(`wm: ${inv.wm}${inv.detail ? `  (${inv.detail})` : ""}`);
      for (const ws of inv.workspaces) {
        console.log(
          `  ${ws.focused ? "*" : " "} ${ws.name.padEnd(12)} ${ws.windowCount} window${ws.windowCount === 1 ? "" : "s"}`,
        );
      }
      if (inv.piSessions.length) {
        console.log("pi sessions:");
        for (const p of inv.piSessions) {
          const where = p.workspace ? `ws ${p.workspace}` : "no window";
          console.log(`  ${p.label.padEnd(18)} ${where.padEnd(10)} pid ${p.pid}  ${p.cwd}`);
        }
      }
      break;
    }

    case "live": {
      // `live <id> <workspace>` puts a session's terminal on the desktop's
      // screen; `unlive <id>` takes it away.
      const [id, workspace] = positional(rest, []);
      const res = await daemonCall(sock, "makeLive", { id: id ?? "", workspace: workspace ?? "" });
      console.log(res.ok ? `on the desktop (${workspace})` : `could not: ${res.detail ?? "failed"}`);
      if (!res.ok) process.exit(1);
      break;
    }

    case "unlive": {
      const [id] = positional(rest, []);
      const res = await daemonCall(sock, "closeLive", { id: id ?? "" });
      console.log(res.ok ? "closed" : `could not: ${res.detail ?? "failed"}`);
      if (!res.ok) process.exit(1);
      break;
    }

    case "clipboard-write": {
      // The off default is not a joke: honouring OSC 52 lets a program inside a
      // shell overwrite what the user copies anywhere on the device.
      const [arg] = positional(rest, []);
      if (arg !== "on" && arg !== "off" && arg !== undefined) {
        console.error("usage: ldmux clipboard-write on|off");
        process.exit(2);
      }
      const { on } = await daemonCall(sock, "clipboardWrite", {
        on: arg === "on",
      });
      console.log(`clipboard-write ${on ? "on" : "off"}`);
      break;
    }

    case "copy": {
      const [id, which] = positional(rest, []);
      const on = which !== "off";
      await daemonCall(sock, "copyMode", { id: id ?? "", on });
      console.log(on ? "copy-mode on" : "copy-mode off");
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
      printUsage();
      process.exit(2);
  }
}

main().catch((err) => {
  console.error(`ldmux: ${(err as Error).message}`);
  process.exit(1);
});
