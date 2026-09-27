import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LdmuxDaemon } from "../src/daemon.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";
import { setTimeout as sleep } from "node:timers/promises";

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
/**
 * Run the real CLI as a child process, exactly as a user would.
 *
 * Asynchronous on purpose: the daemon lives in *this* process, so blocking the
 * event loop (spawnSync) would deadlock — the child waits for a daemon that
 * cannot run. This is also a genuine end-to-end path: separate process, real
 * argv, real socket.
 */
async function cli(...args: string[]) {
  const proc = spawn("node", [cliPath, ...args], {
    env: {
      ...process.env,
      LDMUX_STATE_DIR: stateDir,
      LDMUX_TMUX_SOCK: tmuxSocket,
      LDMUX_SOCK: join(stateDir, "ldmuxd.sock"),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const drained = Promise.all([readAll(proc.stdout), readAll(proc.stderr)]);
  const exited = new Promise<number>((resolve) => {
    proc.once("close", (code, signal) => resolve(signal ? -1 : (code ?? -1)));
    proc.once("error", () => resolve(-1));
  });
  const [out, err] = await drained;
  return { code: await exited, out, err };
}

/** Collect a stream as text; a broken pipe is not a test failure. */
function readAll(stream: NodeJS.ReadableStream | null): Promise<string> {
  if (!stream) return Promise.resolve("");
  return new Promise((resolve) => {
    const chunks: Buffer[] = [];
    stream.on("data", (chunk: Buffer) => chunks.push(chunk));
    stream.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    stream.on("error", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}

test("ping prints the daemon's JSON answer", { timeout: 20000 }, async () => {
  const res = await cli("ping");
  expect(res.code).toBe(0);
  expect(JSON.parse(res.out).pong).toBe(true);
});

test("new prints an id, list shows the session, and kill removes it", { timeout: 30000 }, async () => {
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
});

test("send/key/capture round-trip through the daemon", { timeout: 30000 }, async () => {
  const id = (await cli("new", "shell", tmpdir())).out.trim();
  expect((await cli("send", id, "printf 'CLI_MARKER\\n'")).code).toBe(0);
  expect((await cli("key", id, "Enter")).code).toBe(0);
  let screen = "";
  for (let i = 0; i < 60; i++) {
    screen = (await cli("capture", id)).out;
    if (screen.includes("CLI_MARKER")) break;
    await sleep(50);
  }
  expect(screen).toContain("CLI_MARKER");

  const snap = await cli("snapshot", id);
  expect(snap.code).toBe(0);
  const parsed = JSON.parse(snap.out);
  expect(parsed.state.cols).toBe(120);
  expect(typeof parsed.screen).toBe("string");
  await cli("kill", id);
});

test("an unknown command exits 2 with usage, and a daemon error exits 1", { timeout: 20000 }, async () => {
  const bad = await cli("definitely-not-a-command");
  expect(bad.code).toBe(2);
  expect(bad.err).toContain("usage: ldmux");

  const missing = await cli("capture", "nosuchsession");
  expect(missing.code).toBe(1);
  expect(missing.err).toContain("unknown session");
});

test("the daemon still answers after an unusual CLI invocation", async () => {
  // An empty capture id must not take the daemon down, and the next call works.
  const empty = await cli("capture", "");
  expect(empty.code).toBe(1);
  expect((await cli("ping")).code).toBe(0);
});

test("send -- types a leading-dash string instead of swallowing it as a flag", async () => {
  const created = await cli("new", "shell", tmpdir());
  const id = created.out.trim();
  const sent = await cli("send", id, "--", "--dash-cli-marker");
  expect(sent.code).toBe(0);

  // The literal text must reach the pane (previously the `--`-prefixed argument
  // was dropped by option parsing, sending nothing).
  const { call } = await import("../../protocol/src/socket-client.ts");
  const end = Date.now() + 6000;
  let screen = "";
  while (Date.now() < end) {
    screen = (await call<{ screen: string }>(daemon.socketPath, "capture", { id })).screen;
    if (screen.includes("--dash-cli-marker")) break;
    await sleep(60);
  }
  expect(screen).toContain("--dash-cli-marker");
  await cli("kill", id);
});

test("an unknown flag is refused instead of silently ignored", { timeout: 20000 }, async () => {
  // `flag()` finds a name and returns the next argv element, and ignores every option
  // it does not recognise. So `ldmux shell --nam foo` used the default name and the
  // command's own output said nothing about the flag having been dropped. That is not
  // one bad case — it is every mistyped flag in every command — so the check is one
  // place, before the switch and before any socket is opened.
  const before = (await cli("list")).out;
  const bad = await cli("shell", "--nam", "typo");
  expect(bad.code).toBe(2);
  expect(bad.err).toContain("unknown option --nam");
  // The point of the check: no session appeared.
  expect((await cli("list")).out).toBe(before);
});

test("--help explains the tool and starts nothing", { timeout: 20000 }, async () => {
  // `ldmux shell --help` used to **create a session**, which then had to be killed by
  // id — the session was the only evidence the flag had been dropped anywhere.
  const before = (await cli("list")).out;

  const verb = await cli("shell", "--help");
  expect(verb.code).toBe(0);
  expect(verb.err).toContain("usage: ldmux");
  expect((await cli("list")).out).toBe(before);

  const whole = await cli("--help");
  expect(whole.code).toBe(0);
  expect(whole.err).toContain("usage: ldmux");
  expect((await cli("list")).out).toBe(before);

  // `--` still ends option scanning, so a leading-dash *argument* is still typeable.
  const created = await cli("new", "shell", tmpdir());
  const id = created.out.trim();
  expect((await cli("send", id, "--", "--not-a-flag")).code).toBe(0);
  await cli("kill", id);
});

test("code and pair refuse a daemon a phone cannot reach, and say what to set", { timeout: 20000 }, async () => {
  // This daemon listens on loopback (tests), which is exactly the case a QR code
  // must not be shown for: the phone would scan it and connect to itself.
  for (const verb of ["code", "pair"]) {
    const r = await cli(verb);
    expect(r.code).toBe(1);
    expect(r.err).toContain("LDMUX_REMOTE_HOST");
    expect(r.out).toBe("");
  }
});
