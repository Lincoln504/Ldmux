/**
 * ldmux pi extension.
 *
 * Responsibilities (deliberately small — the daemon owns everything durable):
 *   1. ensure ldmuxd is running;
 *   2. bind the current pi session to its ldmux session record;
 *   3. expose a `/ldmux` status command and an `ldmux` tool for the agent.
 *
 * Lifecycle discipline (docs/adr/0003-session-lifecycle.md): session_shutdown
 * fires for /new, /resume, /fork, /reload AND /quit. Only "quit" means the pi
 * process is going away — so we end the ldmux session record only then, and
 * never stop the daemon or kill tmux from here.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { daemonCall } from "../../protocol/src/rpc.ts";
import { ensureDaemon, isChildAgentSession, ping, resolveConfig, type LdmuxConfig } from "./config.ts";

// Plain JSON Schema (TypeBox-compatible) — avoids a typebox dependency.
const ldmuxToolParams = {
  type: "object",
  properties: {
    action: {
      type: "string",
      enum: ["list", "snapshot", "send"],
      description: "list sessions; snapshot a session's screen; send input to a session",
    },
    id: { type: "string", description: "ldmux session id (for snapshot/send)" },
    text: { type: "string", description: "literal text to type (for send)" },
    keys: { type: "array", items: { type: "string" }, description: "named keys, e.g. Enter, C-c (for send)" },
  },
  required: ["action"],
  additionalProperties: false,
} as const;

async function reportStatus(cfg: LdmuxConfig): Promise<string> {
  if (!(await ping(cfg))) return "ldmux: daemon not running";
  const { sessions } = await daemonCall(cfg.socketPath, "list", {});
  const running = sessions.filter((s) => s.status === "running").length;
  const lines = sessions.slice(0, 10).map((s) => {
    const attach = s.tmuxName ? s.tmuxName : "rpc";
    return `  ${s.id}  ${s.status.padEnd(7)} ${s.kind.padEnd(5)} ${s.cwd}  (${attach})`;
  });
  return [`ldmux: ${running} running / ${sessions.length} total`, ...lines].join("\n");
}

export default function ldmuxExtension(pi: ExtensionAPI): void {
  const cfg = resolveConfig();
  // The daemon expires RPC-only records that go quiet, so the extension
  // re-registers as a heartbeat; cleared on every transition and on quit.
  let heartbeat: ReturnType<typeof setInterval> | null = null;
  const stopHeartbeat = () => {
    if (heartbeat) clearInterval(heartbeat);
    heartbeat = null;
  };

  pi.on("session_start", async (_event, ctx) => {
    // Only root pi sessions are ldmux sessions; fork/subagent children are not.
    if (isChildAgentSession()) return;
    stopHeartbeat();
    const register = () =>
      daemonCall(cfg.socketPath, "register", {
        piSessionId: ctx.sessionManager.getSessionId(),
        cwd: ctx.cwd,
        kind: "pi",
        tmuxName: process.env.LDMUX_TMUX_NAME || undefined,
        title: ctx.sessionManager.getName?.() ?? undefined,
      });
    try {
      if (!(await ensureDaemon(cfg))) {
        if (ctx.hasUI) ctx.ui.notify("ldmux: daemon unavailable (is `bun` on PATH?)", "warning");
        return;
      }
      await register();
      heartbeat = setInterval(() => void register().catch(() => {}), 5 * 60 * 1000);
      heartbeat.unref?.();
    } catch (err) {
      const message = `ldmux: ${(err as Error).message}`;
      if (ctx.hasUI) ctx.ui.notify(message, "warning");
      else console.error(message);
    }
  });

  // Only "quit" ends the session; other reasons are session transitions that
  // survive in the same pi process (the next session_start re-registers).
  pi.on("session_shutdown", async (event, ctx) => {
    stopHeartbeat();
    if (event.reason !== "quit") return;
    try {
      await daemonCall(cfg.socketPath, "session_end", { piSessionId: ctx.sessionManager.getSessionId() });
    } catch {
      /* daemon already gone */
    }
  });

  pi.registerCommand("ldmux", {
    description: "Show ldmux session status",
    handler: async (_args, ctx) => {
      const status = await reportStatus(cfg);
      if (ctx.hasUI) ctx.ui.notify(status, "info");
      else console.log(status);
    },
  });

  pi.registerTool({
    name: "ldmux",
    label: "ldmux",
    description:
      "Inspect and control ldmux terminal sessions on this machine (list, snapshot a screen, send input).",
    parameters: ldmuxToolParams as never,
    async execute(_toolCallId, params) {
      const { action, id, text, keys } = params as {
        action: "list" | "snapshot" | "send";
        id?: string;
        text?: string;
        keys?: string[];
      };
      if (action === "list") {
        const status = await reportStatus(cfg);
        return { content: [{ type: "text" as const, text: status }], details: {} };
      }
      if (action !== "snapshot" && action !== "send") {
        // An unknown action must not silently degrade into a send.
        throw new Error(`ldmux: unknown action "${String(action)}" (expected list, snapshot or send)`);
      }
      if (!id) throw new Error(`ldmux: \`id\` is required for ${action}`);
      if (action === "snapshot") {
        const { screen } = await daemonCall(cfg.socketPath, "snapshot", { id });
        return { content: [{ type: "text" as const, text: screen }], details: {} };
      }
      await daemonCall(cfg.socketPath, "send", { id, text, keys });
      return { content: [{ type: "text" as const, text: `sent to ${id}` }], details: {} };
    },
  });
}