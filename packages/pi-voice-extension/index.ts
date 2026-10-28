import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { speak } from "./src/daemon-client.ts";
import { SAY_INSTRUCTIONS, assistantText, extractSay, stripSay } from "./src/say.ts";

/** Persisted through `appendEntry`; excluded from the model's context. */
export const ENTRY_TYPE = "ldmux-voice";

export interface Environment {
  LDMUX?: string;
  LDMUX_SOCKET?: string;
  LDMUX_SESSION_ID?: string;
}

/**
 * Voice mode for a pi running inside an ldmux shell.
 *
 * Active only when `LDMUX=1` and the socket and session id the daemon exports are present
 * (`crates/daemon/src/daemon.rs`), so a pi outside ldmux is untouched: nothing is registered.
 * The daemon relays and holds no speech logic; this extension decides what is said.
 */
export default function ldmuxVoice(pi: ExtensionAPI, env: Environment = process.env as Environment) {
  const socket = env.LDMUX_SOCKET;
  const sessionId = env.LDMUX_SESSION_ID;
  if (env.LDMUX !== "1" || !socket || !sessionId) return;

  let enabled = false;
  let counter = 0;
  let warned = false;

  const restore = (ctx: { sessionManager: { getBranch(): unknown[] } }) => {
    enabled = false;
    for (const e of ctx.sessionManager.getBranch() as Array<{ type?: string; customType?: string; data?: { enabled?: boolean } }>) {
      if (e.type === "custom" && e.customType === ENTRY_TYPE) enabled = e.data?.enabled === true;
    }
  };

  pi.on("session_start", async (_event, ctx) => restore(ctx as never));

  pi.registerCommand("voice", {
    description: "Voice mode: /voice on | off | (status)",
    handler: async (args, ctx) => {
      const a = args.trim().toLowerCase();
      if (a === "on" || a === "off") {
        enabled = a === "on";
        pi.appendEntry(ENTRY_TYPE, { enabled });
        warned = false;
      } else if (a !== "") {
        ctx.ui.notify("usage: /voice on | off", "warning");
        return;
      }
      ctx.ui.notify(enabled ? "voice mode on: answers end with a spoken summary" : "voice mode off", "info");
    },
  });

  pi.on("before_agent_start", async (event) => {
    if (!enabled) return undefined;
    return { systemPrompt: event.systemPrompt + SAY_INSTRUCTIONS };
  });

  pi.on("message_end", async (event, ctx) => {
    if (!enabled) return undefined;
    const text = assistantText(event.message as { role?: string; content?: unknown });
    const say = text ? extractSay(text) : null;
    if (!say) return undefined;
    const result = await speak(socket, { sessionId, text: say, id: `${sessionId}-${Date.now().toString(36)}-${counter++}`, final: true });
    if (!result.ok && !warned) {
      warned = true; // once per toggle: a down daemon must not nag on every turn
      ctx.ui.notify(`voice: could not speak (${result.error})`, "warning");
    }
    return undefined;
  });

  // Hide the spoken copy from the transcript (and while it is still streaming in).
  pi.registerMarkdownTransformer((markdown) => (enabled ? stripSay(markdown) : markdown));
}
