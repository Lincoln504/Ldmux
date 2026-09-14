import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../../daemon/src/daemon.ts";
import { killTmuxServer } from "../../daemon/test/tmux-cleanup.ts";

/**
 * The pi extension, loaded the way pi loads it (its own bundled jiti), driven
 * with a mock pi API against a real daemon. This is the only test that exercises
 * the factory, the event handlers and the tool together; without it the shipped
 * extension behaviour would rest on a manual smoke run.
 *
 * jiti resolves the relative .ts imports for real, which is exactly the property
 * the "no build step" architecture depends on.
 */

const tmuxSocket = `ldmux-extlife-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-extlife-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });

process.env.LDMUX_STATE_DIR = stateDir;
process.env.LDMUX_SOCK = join(stateDir, "ldmuxd.sock");
process.env.LDMUX_TMUX_SOCK = tmuxSocket;
delete process.env.PI_FORK_PROVIDER;
delete process.env.PI_FORK_ALL_TOOLS;

await daemon.start();

afterAll(() => {
  daemon.stop();
  killTmuxServer(tmuxSocket);
  rmSync(stateDir, { recursive: true, force: true });
});

const piRoot = "/home/ldeen/.config/nvm/versions/node/v25.8.2/lib/node_modules/@earendil-works/pi-coding-agent";

interface MockPi {
  handlers: Record<string, (...args: any[]) => any>;
  tools: any[];
  commands: string[];
  api: any;
}

/** Load the extension through pi's bundled jiti and capture what it registers. */
async function loadExtension(): Promise<MockPi> {
  const jitiModulePath = join(piRoot, "node_modules", "jiti", "lib", "jiti.cjs");
  const { createJiti } = await import(jitiModulePath);
  const jiti = createJiti(import.meta.url, { moduleCache: false });
  const mod = await jiti.import(new URL("../src/index.ts", import.meta.url).pathname);
  const factory = (mod as any).default ?? mod;
  expect(typeof factory).toBe("function");

  const mock: MockPi = { handlers: {}, tools: [], commands: [], api: null };
  mock.api = {
    on: (event: string, handler: (...args: any[]) => any) => {
      mock.handlers[event] = handler;
    },
    registerTool: (tool: any) => mock.tools.push(tool),
    registerCommand: (name: string) => mock.commands.push(name),
    registerShortcut: () => {},
  };
  await factory(mock.api);
  return mock;
}

/** The minimum ExtensionContext (ctx) the extension actually uses. */
function mockCtx(sessionId: string) {
  const notifications: string[] = [];
  return {
    hasUI: false,
    cwd: tmpdir(),
    ui: {
      notify: (message: string) => notifications.push(message),
      setStatus: () => {},
    },
    sessionManager: {
      getSessionId: () => sessionId,
      getName: () => "lifecycle-test",
    },
    notifications,
  } as any;
}

test("the factory registers exactly the events, tool and command we ship", async () => {
  const pi = await loadExtension();
  expect(Object.keys(pi.handlers).sort()).toEqual(["session_shutdown", "session_start"]);
  expect(pi.tools.map((t) => t.name)).toEqual(["ldmux"]);
  expect(pi.commands).toEqual(["ldmux"]);
  // The tool's schema is plain JSON Schema (no typebox dependency), and the
  // action enum must match what the implementation handles.
  const tool = pi.tools[0];
  expect(tool.parameters.additionalProperties).toBe(false);
  expect(tool.parameters.properties.action.enum).toEqual(["list", "snapshot", "send"]);
  expect(tool.parameters.required).toContain("action");
});

test("session_start registers the root pi session as RPC-only and running", async () => {
  const pi = await loadExtension();
  await pi.handlers.session_start({ reason: "startup" }, mockCtx("pi-life-root"));
  const { sessions } = await daemonCallList();
  const mine = sessions.find((s: any) => s.piSessionId === "pi-life-root");
  expect(mine).toBeDefined();
  expect(mine.kind).toBe("pi");
  // A pi session started outside a ldmux-created tmux session has no terminal.
  expect(mine.tmuxName).toBe("");
  expect(mine.status).toBe("running");
  expect(mine.createdBy).toBe("pi-extension");
});

test("a fork/subagent child never registers", async () => {
  process.env.PI_FORK_PROVIDER = "openrouter";
  try {
    const pi = await loadExtension();
    await pi.handlers.session_start({ reason: "startup" }, mockCtx("pi-life-child"));
    const { sessions } = await daemonCallList();
    expect(sessions.some((s: any) => s.piSessionId === "pi-life-child")).toBe(false);
  } finally {
    delete process.env.PI_FORK_PROVIDER;
  }
});

test("session_shutdown only ends the session for reason 'quit'", async () => {
  const pi = await loadExtension();
  const ctx = mockCtx("pi-life-transitions");
  await pi.handlers.session_start({ reason: "startup" }, ctx);
  expect(await statusOf("pi-life-transitions")).toBe("running");

  // /new, /resume, /reload and /fork keep the same process alive: not an end.
  for (const reason of ["new", "resume", "reload", "fork"]) {
    await pi.handlers.session_shutdown({ reason }, ctx);
    expect(await statusOf("pi-life-transitions")).toBe("running");
  }

  await pi.handlers.session_shutdown({ reason: "quit" }, ctx);
  expect(await statusOf("pi-life-transitions")).toBe("dead");
});

test("the ldmux tool lists and snapshots through the daemon", async () => {
  const pi = await loadExtension();
  const tool = pi.tools[0];
  const ctx = mockCtx("pi-life-tool");
  await pi.handlers.session_start({ reason: "startup" }, ctx);

  const listed = await tool.execute("call-1", { action: "list" }, undefined, undefined, ctx);
  const text = JSON.stringify(listed.content);
  // The listing names sessions by their ldmux id, which we look up.
  const mine = (await daemonCallList()).sessions.find((s: any) => s.piSessionId === "pi-life-tool");
  expect(mine).toBeDefined();
  expect(text).toContain(mine.id);

  // A snapshot of an RPC-only pi session must explain itself rather than fail
  // with a tmux transport error about an empty target.
  await expect(
    tool.execute("call-2", { action: "snapshot", id: mine.id }, undefined, undefined, ctx),
  ).rejects.toThrow(/no terminal/);

  // missing id is an error, not a silent no-op
  await expect(tool.execute("call-3", { action: "send", text: "x" }, undefined, undefined, ctx)).rejects.toThrow(/id/);
  // an unknown action must not degrade into a send
  await expect(
    tool.execute("call-4", { action: "nonsense", id: mine.id }, undefined, undefined, ctx),
  ).rejects.toThrow(/unknown action/);
});

async function daemonCallList(): Promise<{ sessions: any[] }> {
  const { daemonCall } = await import("../../protocol/src/rpc.ts");
  return daemonCall(join(stateDir, "ldmuxd.sock"), "list", {});
}

async function statusOf(piSessionId: string): Promise<string | undefined> {
  const { sessions } = await daemonCallList();
  return sessions.find((s: any) => s.piSessionId === piSessionId)?.status;
}
test("the ldmux tool sends into a real terminal session", async () => {
  const pi = await loadExtension();
  const tool = pi.tools[0];
  const ctx = mockCtx("pi-life-tool-send");
  await pi.handlers.session_start({ reason: "startup" }, ctx);

  // Create a real shell session through the daemon, then drive it via the tool.
  const { daemonCall } = await import("../../protocol/src/rpc.ts");
  const sock = join(stateDir, "ldmuxd.sock");
  const created = await daemonCall(sock, "new", { kind: "shell", cwd: tmpdir() });
  const session = (created as { session: { id: string } }).session;

  const sent = await tool.execute(
    "call-send",
    { action: "send", id: session.id, text: "printf 'TOOL_SEND_OK\\n'", keys: ["Enter"] },
    undefined,
    undefined,
    ctx,
  );
  expect(JSON.stringify(sent.content)).toContain(session.id);

  const end = Date.now() + 6000;
  let screen = "";
  while (Date.now() < end) {
    const cap = await daemonCall(sock, "capture", { id: session.id });
    screen = (cap as { screen: string }).screen;
    if (screen.includes("TOOL_SEND_OK")) break;
    await Bun.sleep(60);
  }
  expect(screen).toContain("TOOL_SEND_OK");
  await daemonCall(sock, "kill", { id: session.id });
});
