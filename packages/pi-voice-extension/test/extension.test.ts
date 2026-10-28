import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import ldmuxVoice, { ENTRY_TYPE } from "../index.ts";
import { speak } from "../src/daemon-client.ts";

type Handler = (event: any, ctx: any) => Promise<any> | any;

function fakePi() {
  const handlers = new Map<string, Handler>();
  const commands = new Map<string, { handler: (a: string, c: any) => Promise<void> }>();
  const entries: Array<[string, unknown]> = [];
  let transformer: ((md: string) => string) | undefined;
  const api = {
    on: (n: string, h: Handler) => handlers.set(n, h),
    registerCommand: (n: string, c: any) => commands.set(n, c),
    appendEntry: (t: string, d: unknown) => entries.push([t, d]),
    registerMarkdownTransformer: (t: (md: string) => string) => (transformer = t),
  };
  return { api: api as any, handlers, commands, entries, transform: (m: string) => transformer!(m) };
}

const notes: string[] = [];
const ctx = (branch: unknown[] = []) => ({ ui: { notify: (m: string) => notes.push(m) }, sessionManager: { getBranch: () => branch } });

/** A control socket that answers `speech` like the daemon and records what it was sent. */
async function fakeDaemon(reply: object) {
  const dir = mkdtempSync(join(tmpdir(), "ldmux-pi-voice-"));
  const path = join(dir, "d.sock");
  const seen: any[] = [];
  const server = createServer((s) => {
    s.setEncoding("utf8");
    let b = "";
    s.on("data", (c: string) => {
      b += c;
      if (b.includes("\n")) {
        seen.push(JSON.parse(b.split("\n")[0]));
        s.write(JSON.stringify(reply) + "\n");
      }
    });
  });
  await new Promise<void>((r) => server.listen(path, r));
  return { path, seen, close: async () => { server.close(); rmSync(dir, { recursive: true, force: true }); } };
}

test("outside ldmux nothing is registered at all", () => {
  for (const env of [{}, { LDMUX: "0", LDMUX_SOCKET: "/x", LDMUX_SESSION_ID: "s" }, { LDMUX: "1" }, { LDMUX: "1", LDMUX_SOCKET: "/x" }]) {
    const p = fakePi();
    ldmuxVoice(p.api, env);
    assert.equal(p.handlers.size + p.commands.size, 0, JSON.stringify(env));
  }
});

test("/voice toggles, persists through appendEntry, and restores from the branch", async () => {
  const p = fakePi();
  ldmuxVoice(p.api, { LDMUX: "1", LDMUX_SOCKET: "/x", LDMUX_SESSION_ID: "s1" });
  await p.commands.get("voice")!.handler("on", ctx());
  assert.deepEqual(p.entries, [[ENTRY_TYPE, { enabled: true }]]);
  const start = p.handlers.get("session_start")!;
  await start({}, ctx([{ type: "custom", customType: ENTRY_TYPE, data: { enabled: false } }]));
  assert.equal(await p.handlers.get("before_agent_start")!({ systemPrompt: "base" }, ctx()), undefined, "restored to off");
  await start({}, ctx([{ type: "custom", customType: ENTRY_TYPE, data: { enabled: false } }, { type: "custom", customType: ENTRY_TYPE, data: { enabled: true } }]));
  const r = await p.handlers.get("before_agent_start")!({ systemPrompt: "base" }, ctx());
  assert.ok(r.systemPrompt.startsWith("base") && r.systemPrompt.includes("```say"));
});

test("the prompt section and the transcript hiding follow the toggle", async () => {
  const p = fakePi();
  ldmuxVoice(p.api, { LDMUX: "1", LDMUX_SOCKET: "/x", LDMUX_SESSION_ID: "s1" });
  assert.equal(p.transform("a\n```say\nhi\n```"), "a\n```say\nhi\n```", "off: untouched");
  await p.commands.get("voice")!.handler("on", ctx());
  assert.equal(p.transform("a\n```say\nhi\n```"), "a");
  await p.commands.get("voice")!.handler("off", ctx());
  assert.equal(await p.handlers.get("before_agent_start")!({ systemPrompt: "base" }, ctx()), undefined);
});

test("a finished answer's say block is spoken to the daemon with the session id", async () => {
  const d = await fakeDaemon({ id: 1, ok: true, result: { seq: 4, delivered: 1 } });
  try {
    const p = fakePi();
    ldmuxVoice(p.api, { LDMUX: "1", LDMUX_SOCKET: d.path, LDMUX_SESSION_ID: "sess-9" });
    await p.commands.get("voice")!.handler("on", ctx());
    await p.handlers.get("message_end")!({ message: { role: "assistant", content: [{ type: "text", text: "Long answer.\n```say\nAll done.\n```" }] } }, ctx());
    assert.equal(d.seen.length, 1);
    assert.equal(d.seen[0].method, "speech");
    assert.equal(d.seen[0].params.sessionId, "sess-9");
    assert.equal(d.seen[0].params.text, "All done.");
    assert.equal(d.seen[0].params.final, true);
    assert.match(d.seen[0].params.id, /^sess-9-/);
    // Nothing spoken for a user message, a turn without a block, or voice off.
    await p.handlers.get("message_end")!({ message: { role: "user", content: [{ type: "text", text: "```say\nx\n```" }] } }, ctx());
    await p.handlers.get("message_end")!({ message: { role: "assistant", content: [{ type: "text", text: "plain" }] } }, ctx());
    await p.commands.get("voice")!.handler("off", ctx());
    await p.handlers.get("message_end")!({ message: { role: "assistant", content: [{ type: "text", text: "```say\nx\n```" }] } }, ctx());
    assert.equal(d.seen.length, 1);
  } finally {
    await d.close();
  }
});

test("a down daemon is reported once, not thrown and not repeated", async () => {
  notes.length = 0;
  const p = fakePi();
  ldmuxVoice(p.api, { LDMUX: "1", LDMUX_SOCKET: "/nonexistent/sock", LDMUX_SESSION_ID: "s" });
  await p.commands.get("voice")!.handler("on", ctx());
  const turn = { message: { role: "assistant", content: [{ type: "text", text: "```say\nhi\n```" }] } };
  await p.handlers.get("message_end")!(turn, ctx());
  await p.handlers.get("message_end")!(turn, ctx());
  assert.equal(notes.filter((n) => n.startsWith("voice: could not speak")).length, 1);
});

test("the daemon client reports a refusal and never throws", async () => {
  const d = await fakeDaemon({ id: 1, ok: false, error: "no such session: s" });
  try {
    assert.deepEqual(await speak(d.path, { sessionId: "s", text: "x" }), { ok: false, error: "no such session: s" });
  } finally {
    await d.close();
  }
  const r = await speak("/nonexistent/sock", { sessionId: "s", text: "x" });
  assert.equal(r.ok, false);
});
