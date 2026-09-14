import { test, expect, afterAll } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

const tmuxSocket = `ldmux-clitest-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const stateDir = mkdtempSync(join(tmpdir(), "ldmux-cli-"));
const daemon = new LdmuxDaemon({ stateDir, tmuxSocket, remotePort: 0 });
await daemon.start();

afterAll(() => {
  daemon.stop();
  killTmuxServer(tmuxSocket);
  rmSync(stateDir, { recursive: true, force: true });
});

const cliPath = new URL("../src/cli.ts", import.meta.url).pathname;
/** A private settings file: a test must never touch the user's real pi config. */
const piSettings = join(stateDir, "pi-settings.json");

/**
 * Run the real CLI as a child process, exactly as a user would.
 *
 * Asynchronous on purpose: the daemon lives in *this* process, so blocking the
 * event loop (spawnSync) would deadlock — the child waits for a daemon that
 * cannot run. This is also a genuine end-to-end path: separate process, real
 * argv, real socket.
 */
async function cli(...args: string[]) {
  const proc = Bun.spawn(["bun", "run", cliPath, ...args], {
    env: {
      ...process.env,
      LDMUX_STATE_DIR: stateDir,
      LDMUX_TMUX_SOCK: tmuxSocket,
      LDMUX_SOCK: join(stateDir, "ldmuxd.sock"),
      LDMUX_PI_SETTINGS: piSettings,
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [out, err, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  return { code: code ?? -1, out, err };
}

test("ping prints the daemon's JSON answer", async () => {
  const res = await cli("ping");
  expect(res.code).toBe(0);
  expect(JSON.parse(res.out).pong).toBe(true);
}, 20000);

test("new prints an id, list shows the session, and kill removes it", async () => {
  const created = await cli("new", "shell", tmpdir());
  expect(created.code).toBe(0);
  const id = created.out.trim();
  expect(id).toMatch(/^[0-9a-f]{10}$/);

  const listed = await cli("list");
  expect(listed.code).toBe(0);
  expect(listed.out).toContain(id);
  expect(listed.out).toContain("running");
  expect(listed.out).toContain("shell");

  const killed = await cli("kill", id);
  expect(killed.code).toBe(0);
  expect(killed.out).toContain(`killed ${id}`);
  expect((await cli("list")).out).toContain("dead");
}, 30000);

test("send/key/capture round-trip through the daemon", async () => {
  const id = (await cli("new", "shell", tmpdir())).out.trim();
  expect((await cli("send", id, "printf 'CLI_MARKER\\n'")).code).toBe(0);
  expect((await cli("key", id, "Enter")).code).toBe(0);
  let screen = "";
  for (let i = 0; i < 60; i++) {
    screen = (await cli("capture", id)).out;
    if (screen.includes("CLI_MARKER")) break;
    await Bun.sleep(50);
  }
  expect(screen).toContain("CLI_MARKER");

  const snap = await cli("snapshot", id);
  expect(snap.code).toBe(0);
  const parsed = JSON.parse(snap.out);
  expect(parsed.state.cols).toBe(120);
  expect(typeof parsed.screen).toBe("string");
  await cli("kill", id);
}, 30000);

test("an unknown command exits 2 with usage, and a daemon error exits 1", async () => {
  const bad = await cli("definitely-not-a-command");
  expect(bad.code).toBe(2);
  expect(bad.err).toContain("usage: ldmux");

  const missing = await cli("capture", "nosuchsession");
  expect(missing.code).toBe(1);
  expect(missing.err).toContain("unknown session");
});

test("install-extension edits a settings file without needing a daemon", async () => {
  const dry = await cli("install-extension", "--dry-run");
  expect(dry.code).toBe(0);
  expect(dry.out).toContain("would add");
  expect(existsSync(piSettings)).toBe(false); // dry run touches nothing

  const installed = await cli("install-extension");
  expect(installed.code).toBe(0);
  expect(installed.out).toContain("entries:");
  const entries = JSON.parse(readFileSync(piSettings, "utf8")).extensions as string[];
  expect(entries.length).toBe(1);
  expect(entries[0]).toEndWith("/packages/extension");
  expect(entries[0].startsWith("/")).toBe(true);

  // A second install is a no-op (no clobbering, no second rewrite).
  const again = await cli("link-extension");
  expect(again.code).toBe(0);
  expect(again.out).toContain("already present");
  expect(JSON.parse(readFileSync(piSettings, "utf8")).extensions).toEqual(entries);

  // Removal takes the entry back out.
  const removed = await cli("install-extension", "--remove");
  expect(removed.code).toBe(0);
  expect(JSON.parse(readFileSync(piSettings, "utf8")).extensions).toEqual([]);
  expect(readdirSync(stateDir).some((f) => f.includes(".bak-ldmux-"))).toBe(true);
}, 30000);

test("the daemon still answers after an unusual CLI invocation", async () => {
  // An empty capture id must not take the daemon down, and the next call works.
  const empty = await cli("capture", "");
  expect(empty.code).toBe(1);
  expect((await cli("ping")).code).toBe(0);
});
